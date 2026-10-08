// commands/cortes.rs — Módulo de Cortes de Caja para el POS
//
// ─── MODELO DE CAJA (v0.1.39) ─────────────────────────────
//
// La caja es UNA CADENA CONTINUA DE CORTES:
//
//   - Cada corte (de turno o del día) cubre el intervalo
//       (momento del corte anterior, momento de este conteo]
//   - fondo_inicial de un corte = fondo_siguiente del corte anterior.
//   - Toda venta y todo movimiento cae en exactamente UN corte, sin importar
//     si se saltó un día de cierre o si hubo ventas después de cerrar.
//
// La apertura NO corta la cadena: es un conteo de verificación. Si lo que se
// cuenta al abrir difiere de lo esperado (fondo que se dejó + lo que pasó
// desde el último corte), se registra un movimiento de ajuste explícito, con
// nota obligatoria, que entra en el corte en curso. Así el faltante o sobrante
// de la noche queda documentado a nombre de quien abrió, y el corte del día
// no lo vuelve a reportar.
//
// Todo el cálculo vive aquí (servidor), en `calcular_periodo`. El frontend
// solo muestra. Al confirmar, `crear_corte` vuelve a calcular dentro de la
// transacción y rechaza si el esperado cambió mientras el modal estaba
// abierto, en lugar de guardar cifras viejas.
//
// Este modelo reemplaza al anterior, que tenía cuatro fugas comprobadas:
//   1. El período se recalculaba desde "el último corte de turno del día",
//      ignorando días sin cerrar → esas ventas no entraban en ningún corte.
//   2. Los movimientos sin corte se barrían sin filtro de fecha → retiros y
//      entradas viejos caían en el corte de hoy.
//   3. El cierre del día guardaba fecha_fin = 23:59:59 (futuro) → las ventas
//      posteriores al cierre ese mismo día quedaban dentro de un rango ya
//      cerrado y nunca se contaban.
//   4. El esperado lo mandaba el navegador y se guardaba sin revalidar.
//
// ─── CAJAS Y VENTAS QUE LLEGAN DE LA WEB (sync v2) ────────
//
// `caja` dice a qué cajón pertenece cada venta, movimiento, corte y apertura:
//   - 'principal' = el cajón físico de la tienda. Su cadena de cortes y
//     aperturas la escribe SOLO este escritorio. Las ventas y movimientos que
//     la web hace en modo espejo (origen 'web', caja 'principal') llegan por
//     sync y SÍ cuentan aquí: ese dinero está en este cajón.
//   - 'web' = caja aparte de la web. Sus ventas se guardan aquí para historial
//     e inventario, pero NUNCA cuentan en los cortes del escritorio.
// Toda consulta de la cadena filtra caja = 'principal'.
//
// Una venta o movimiento web puede llegar tarde (sync cada ~30 s, equipo sin
// internet): su `fecha` ya quedó dentro de un corte cerrado que no la contó.
// El sync le pone `registrado_at` (momento en que llegó) y la cadena usa
// MOMENTO = COALESCE(registrado_at, fecha), así entra al corte siguiente en
// vez de perderse. Las filas del escritorio dejan registrado_at en NULL
// (salvo con el reloj atrasado, ver abajo), así que para ellas MOMENTO =
// fecha y todo da exactamente lo mismo que antes.
//
// Las ventas que el servidor borró (deleted_at, borrado lógico del sync)
// nunca cuentan, igual que en el motor de caja del servidor.
//
// ─── RELOJ MONÓTONO: LA CADENA NUNCA RETROCEDE ────────────
//
// En 2026-09 la PC de la tienda arrancó con el reloj ~12 h atrasado: las
// ventas quedaron con `fecha` ANTERIOR al último corte, fuera del período
// abierto, y ningún corte las contó. Por eso:
//   - Toda fila del escritorio que entra a la cadena (venta, movimiento
//     manual, ajuste de apertura, reembolso de anulación o de devolución)
//     guarda en `fecha` el reloj real, pero si ese reloj quedó EN O ANTES del
//     inicio del período abierto se le pone `registrado_at` = piso de la
//     cadena (`piso_cadena` = fin del último corte + 1 s). Su MOMENTO cae en
//     el período abierto y el siguiente corte la cuenta.
//   - El corte se estampa en max(reloj, piso) (`ahora_cadena`): fecha_fin,
//     created_at, la regla de un cierre del día por fecha y la asignación
//     de movimientos. Dos cortes seguidos con el reloj atrasado quedan en
//     piso, piso + 1 s, … y la cadena sigue continua.
//   - La vista previa (`calcular_periodo`) y el esperado de la apertura usan
//     el mismo piso, así que vista previa y confirmación coinciden (sin
//     DATOS_CAMBIARON falsos).
// Con el reloj bien el piso nunca alcanza al reloj y nada cambia.
//
// ─── SIN CORTES TODAVÍA (instalación nueva / PC nueva) ────
//
// Los cortes nunca bajan del servidor. En una PC nueva el primer pull trae
// las ventas y movimientos web históricos de la caja 'principal'; si el
// período abierto empezara en el origen, el primer corte los esperaría en
// el cajón (faltante fantasma). Por eso, sin cortes, el período abierto
// empieza en la primera apertura de la tienda (su fecha − 1 s). Sin
// apertura tampoco, empieza en el origen ('0000-00-00 00:00:00').

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::State;
use super::auth::AppState;
use chrono::{Local, NaiveDateTime};

// ─── Structs ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct NuevoMovimiento {
    pub tipo: String,           // ENTRADA | RETIRO
    pub usuario_id: i64,
    pub monto: f64,
    pub concepto: String,
    pub autorizado_por: Option<i64>,
    pub pin_autorizacion: Option<String>,  // PIN del dueño para retiros > $500
}

/// Monto de retiro a partir del cual se requiere PIN del dueño.
const RETIRO_LIMITE_SIN_PIN: f64 = 500.0;

/// Tolerancia para comparar dinero (medio centavo).
const EPS: f64 = 0.005;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MovimientoCajaRs {
    pub id: i64,
    pub tipo: String,
    pub usuario_id: i64,
    pub usuario_nombre: String,
    pub monto: f64,
    pub concepto: String,
    pub autorizado_por: Option<i64>,
    pub corte_id: Option<i64>,
    pub fecha: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VendedorResumenRs {
    pub usuario_id: i64,
    pub usuario_nombre: String,
    pub num_ventas: i64,
    pub total_vendido: f64,
    pub hora_inicio: String,
    pub hora_fin: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DatosCorte {
    pub fecha_inicio: String,
    pub fecha_fin: String,
    pub fondo_inicial: f64,
    pub total_ventas_efectivo: f64,
    pub total_ventas_tarjeta: f64,
    pub total_ventas_transferencia: f64,
    pub total_ventas: f64,
    pub num_transacciones: i64,
    pub total_descuentos: f64,
    pub total_anulaciones: f64,
    pub total_entradas_efectivo: f64,
    pub total_retiros_efectivo: f64,
    pub efectivo_esperado: f64,
    pub cortes_parciales_hoy: i64,
    pub total_retirado_parciales: f64,
    pub movimientos: Vec<MovimientoCajaRs>,
    pub vendedores: Vec<VendedorResumenRs>,
}

#[derive(Deserialize)]
pub struct DenominacionInput {
    pub denominacion: f64,
    pub tipo: String,   // BILLETE | MONEDA
    pub cantidad: i64,
}

/// Retiro de efectivo que se hace al momento de un corte de turno
/// ("me llevo esto, dejo el resto de fondo"). Se registra DENTRO de la misma
/// transacción del corte, después de calcular el esperado, para que no
/// descuadre el corte que lo origina.
#[derive(Deserialize)]
pub struct RetiroCorte {
    pub monto: f64,
    pub concepto: String,
    pub pin_autorizacion: Option<String>,
}

#[derive(Deserialize)]
pub struct NuevoCorte {
    pub tipo: String,           // PARCIAL | DIA
    pub usuario_id: i64,
    // fecha_inicio/fecha_fin se siguen aceptando por compatibilidad con el
    // frontend (la versión web los usa), pero aquí se ignoran: el período lo
    // define la cadena de cortes.
    #[allow(dead_code)]
    pub fecha_inicio: String,
    #[allow(dead_code)]
    pub fecha_fin: String,
    pub datos: DatosCorte,
    pub efectivo_contado: f64,
    pub nota_diferencia: Option<String>,
    pub fondo_siguiente: f64,
    pub denominaciones: Option<Vec<DenominacionInput>>,
    #[serde(default)]
    pub retiro: Option<RetiroCorte>,
}

#[derive(Serialize)]
pub struct CorteCreado {
    pub id: i64,
    pub tipo: String,
    pub diferencia: f64,
    pub efectivo_esperado: f64,
    pub efectivo_contado: f64,
    pub fondo_siguiente: f64,
    pub created_at: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct CorteResumen {
    pub id: i64,
    pub tipo: String,
    pub usuario_nombre: String,
    pub created_at: String,
    pub fondo_inicial: f64,
    pub total_ventas_efectivo: f64,
    pub total_ventas_tarjeta: f64,
    pub total_ventas_transferencia: f64,
    pub total_ventas: f64,
    pub num_transacciones: i64,
    pub total_entradas_efectivo: f64,
    pub total_retiros_efectivo: f64,
    pub efectivo_esperado: f64,
    pub efectivo_contado: f64,
    pub diferencia: f64,
    pub nota_diferencia: Option<String>,
    pub fondo_siguiente: f64,
}

#[derive(Serialize)]
pub struct CorteDetalle {
    pub corte: CorteResumen,
    pub denominaciones: Vec<DenominacionDetalle>,
    pub movimientos: Vec<MovimientoCajaRs>,
    pub vendedores: Vec<VendedorResumenRs>,
}

#[derive(Serialize)]
pub struct DenominacionDetalle {
    pub denominacion: f64,
    pub tipo: String,
    pub cantidad: i64,
    pub subtotal: f64,
}

// ─── Utilidades ───────────────────────────────────────────

pub(crate) fn ahora_local() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Redondea a centavos. Evita que sumas de flotantes dejen diferencias
/// fantasma de 0.0000001 que exijan nota.
pub(crate) fn round2(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

/// Momento real del conteo de un corte, como expresión SQL.
///
/// MIN(fecha_fin, created_at) porque los datos viejos tienen dos formas
/// distintas de mentir sobre ese momento:
///   - cierres del día con fecha_fin = 23:59:59 aunque se contó a las 20:00
///     (created_at es el momento real);
///   - cierres extemporáneos con created_at al día siguiente pero fecha_fin
///     = fin del día que cubrían.
/// Desde v0.1.39 ambos campos valen lo mismo (el momento del conteo).
/// `replace(substr(..))` normaliza fechas que pudieran venir en ISO con 'T'.
pub(crate) const FIN_CORTE_SQL: &str =
    "MIN(replace(substr(fecha_fin,1,19),'T',' '), replace(substr(created_at,1,19),'T',' '))";

/// Momento en que una venta o movimiento entra a la cadena de cortes (ver el
/// encabezado). Para filas del escritorio es `fecha` (registrado_at NULL).
pub(crate) const MOMENTO_SQL: &str = "COALESCE(registrado_at, fecha)";
/// MOMENTO_SQL para consultas con alias `v` (ventas).
pub(crate) const MOMENTO_V_SQL: &str = "COALESCE(v.registrado_at, v.fecha)";
/// MOMENTO_SQL para consultas con alias `m` (movimientos_caja).
pub(crate) const MOMENTO_M_SQL: &str = "COALESCE(m.registrado_at, m.fecha)";

/// Último eslabón de la cadena: momento en que terminó y fondo que dejó.
pub(crate) struct UltimoCorte {
    pub fin: String,
    pub fondo_siguiente: f64,
}

/// Inicio de la cadena cuando no hay ni cortes ni aperturas de la tienda.
pub(crate) const INICIO_SIN_CORTES: &str = "0000-00-00 00:00:00";

const FORMATO_MARCA: &str = "%Y-%m-%d %H:%M:%S";

/// Marca 'YYYY-MM-DD HH:MM:SS': primeros 19 caracteres y 'T' → ' ' (lo mismo
/// que hace FIN_CORTE_SQL en SQL).
pub(crate) fn normalizar_marca(marca: &str) -> String {
    marca.chars().take(19).collect::<String>().replace('T', " ")
}

/// `marca` + `segundos`. None si la marca no es una fecha real (p. ej. el
/// origen '0000-00-00 00:00:00').
fn sumar_segundos(marca: &str, segundos: i64) -> Option<String> {
    NaiveDateTime::parse_from_str(&normalizar_marca(marca), FORMATO_MARCA)
        .ok()
        .map(|t| (t + chrono::Duration::seconds(segundos)).format(FORMATO_MARCA).to_string())
}

/// Piso de la cadena a partir del inicio (exclusivo) del período abierto:
/// el primer segundo que todavía cae dentro del período.
fn piso_desde_inicio(inicio: &str) -> Option<String> {
    sumar_segundos(inicio, 1)
}

/// Piso de la cadena: fin del último corte de la tienda + 1 s (sin cortes,
/// la fecha de la primera apertura; ver `inicio_y_fondo`). None si el
/// período abierto empieza en el origen. Ver "RELOJ MONÓTONO" arriba.
pub(crate) fn piso_cadena(db: &Connection) -> Option<String> {
    piso_desde_inicio(&inicio_y_fondo(db).0)
}

/// `ahora` (reloj real) pero nunca antes de `piso`.
fn con_piso(ahora: &str, piso: Option<&str>) -> String {
    match piso {
        Some(p) if p > normalizar_marca(ahora).as_str() => p.to_string(),
        _ => ahora.to_string(),
    }
}

/// Momento de la cadena para un corte o una vista previa hechos en `ahora`:
/// max(reloj, piso de la cadena).
pub(crate) fn ahora_cadena(db: &Connection, ahora: &str) -> String {
    con_piso(ahora, piso_cadena(db).as_deref())
}

/// `registrado_at` para una fila del escritorio que entra a la cadena en
/// `ahora`: None con el reloj bien (MOMENTO = fecha, como siempre); el piso
/// de la cadena si el reloj quedó en o antes del fin del último corte.
pub(crate) fn registrado_at_para(db: &Connection, ahora: &str) -> Option<String> {
    piso_cadena(db).filter(|p| p.as_str() > normalizar_marca(ahora).as_str())
}

/// Último corte de la caja de la tienda (caja 'principal').
pub(crate) fn ultimo_corte(db: &Connection) -> Option<UltimoCorte> {
    db.query_row(
        &format!(
            "SELECT {f}, fondo_siguiente FROM cortes \
             WHERE deleted_at IS NULL AND caja = 'principal' \
             ORDER BY {f} DESC, id DESC LIMIT 1",
            f = FIN_CORTE_SQL
        ),
        [],
        |r| Ok(UltimoCorte { fin: r.get(0)?, fondo_siguiente: r.get(1)? }),
    ).ok()
}

/// Primera apertura de la tienda: (fecha normalizada, fondo declarado).
fn primera_apertura(db: &Connection) -> Option<(String, f64)> {
    db.query_row(
        "SELECT fecha, fondo_declarado FROM aperturas_caja \
         WHERE deleted_at IS NULL AND caja = 'principal' \
         ORDER BY fecha ASC, id ASC LIMIT 1",
        [],
        |r| Ok((normalizar_marca(&r.get::<_, String>(0)?), r.get::<_, f64>(1)?)),
    ).ok()
}

/// Inicio (exclusivo) del período abierto y fondo con el que arrancó.
/// Si nunca ha habido un corte, el período empieza en la primera apertura de
/// la tienda (su fecha − 1 s) con el fondo que se declaró ahí: lo anterior
/// (p. ej. ventas web históricas que trae el primer pull en una PC nueva) no
/// es dinero de este cajón. Sin apertura tampoco, empieza en el origen.
pub(crate) fn inicio_y_fondo(db: &Connection) -> (String, f64) {
    if let Some(u) = ultimo_corte(db) {
        return (u.fin, u.fondo_siguiente);
    }
    match primera_apertura(db) {
        Some((fecha, fondo)) => (
            sumar_segundos(&fecha, -1).unwrap_or_else(|| INICIO_SIN_CORTES.to_string()),
            fondo,
        ),
        None => (INICIO_SIN_CORTES.to_string(), 0.0),
    }
}

/// Desde cuándo existe la cadena de esta PC: lo más antiguo entre un segundo
/// antes de la primera apertura de la tienda y el inicio del primer corte
/// (origen si no hay ninguno). Lo que la web vendió antes no pasó por este
/// cajón; la auditoría no lo reporta como fuga.
pub(crate) fn origen_de_la_cadena(db: &Connection) -> String {
    let por_apertura = primera_apertura(db).and_then(|(fecha, _)| sumar_segundos(&fecha, -1));
    let por_corte: Option<String> = db.query_row(
        "SELECT MIN(replace(substr(fecha_inicio,1,19),'T',' ')) FROM cortes \
         WHERE deleted_at IS NULL AND caja = 'principal'",
        [],
        |r| r.get(0),
    ).ok().flatten();
    match (por_apertura, por_corte) {
        (Some(a), Some(c)) => std::cmp::min(a, c),
        (a, c) => a.or(c).unwrap_or_else(|| INICIO_SIN_CORTES.to_string()),
    }
}

/// Efectivo que entró y salió de la caja en (inicio, fin].
struct FlujoEfectivo {
    ventas_efectivo: f64,
    entradas: f64,
    retiros: f64,
}

fn flujo_efectivo(db: &Connection, inicio: &str, fin: &str) -> Result<FlujoEfectivo, String> {
    let ventas_efectivo: f64 = db.query_row(
        &format!(
            "SELECT COALESCE(SUM(total), 0) FROM ventas \
             WHERE caja = 'principal' AND deleted_at IS NULL AND {mo} > ? AND {mo} <= ? \
               AND anulada = 0 AND metodo_pago = 'efectivo'",
            mo = MOMENTO_SQL
        ),
        rusqlite::params![inicio, fin],
        |r| r.get(0),
    ).map_err(|e| e.to_string())?;

    let (entradas, retiros): (f64, f64) = db.query_row(
        &format!(
            "SELECT \
               COALESCE(SUM(CASE WHEN tipo = 'ENTRADA' THEN monto ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN tipo = 'RETIRO'  THEN monto ELSE 0 END), 0) \
             FROM movimientos_caja \
             WHERE caja = 'principal' AND corte_id IS NULL AND deleted_at IS NULL \
               AND {mo} > ? AND {mo} <= ?",
            mo = MOMENTO_SQL
        ),
        rusqlite::params![inicio, fin],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).map_err(|e| e.to_string())?;

    Ok(FlujoEfectivo { ventas_efectivo, entradas, retiros })
}

/// Valida la autorización de un retiro y devuelve quién lo autorizó.
///
/// Retiros mayores a RETIRO_LIMITE_SIN_PIN requieren el PIN de un dueño
/// (se resuelve aquí, no se confía en el id que mande el cliente). Si quien
/// retira es admin, se autoriza a sí mismo.
pub(crate) fn resolver_autorizacion_retiro(
    db: &Connection,
    usuario_id: i64,
    monto: f64,
    pin: Option<&str>,
    autorizado_por_cliente: Option<i64>,
) -> Result<Option<i64>, String> {
    if monto <= RETIRO_LIMITE_SIN_PIN {
        return Ok(autorizado_por_cliente);
    }

    let solicitante_es_admin: bool = db.query_row(
        "SELECT COALESCE(r.es_admin, 0) FROM usuarios u JOIN roles r ON r.id = u.rol_id WHERE u.id = ?",
        rusqlite::params![usuario_id],
        |row| row.get::<_, i64>(0),
    ).map(|v| v == 1).unwrap_or(false);

    if solicitante_es_admin {
        return Ok(Some(usuario_id));
    }

    let pin = pin.unwrap_or("").trim();
    if pin.is_empty() {
        return Err(format!(
            "Retiros mayores a ${:.0} requieren PIN del dueño",
            RETIRO_LIMITE_SIN_PIN
        ));
    }

    let mut stmt = db.prepare(
        "SELECT u.id, u.pin FROM usuarios u JOIN roles r ON r.id = u.rol_id
         WHERE r.es_admin = 1 AND u.activo = 1"
    ).map_err(|e| e.to_string())?;
    let rows: Vec<(i64, String)> = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    }).map_err(|e| e.to_string())?
      .filter_map(|r| r.ok())
      .collect();

    match rows.into_iter().find(|(_, hash)| bcrypt::verify(pin, hash).unwrap_or(false)) {
        Some((id, _)) => Ok(Some(id)),
        None => Err("PIN del dueño incorrecto".to_string()),
    }
}

fn leer_movimientos(
    db: &Connection,
    where_sql: &str,
    params: &[&dyn rusqlite::ToSql],
    orden: &str,
) -> Result<Vec<MovimientoCajaRs>, String> {
    let sql = format!(
        "SELECT m.id, m.tipo, m.usuario_id, COALESCE(u.nombre_completo, 'Desconocido'), m.monto,
                m.concepto, m.autorizado_por, m.corte_id, m.fecha
         FROM movimientos_caja m
         LEFT JOIN usuarios u ON u.id = m.usuario_id
         WHERE {where_sql}
         ORDER BY {mo} {orden}, m.id {orden}",
        mo = MOMENTO_M_SQL
    );
    let mut stmt = db.prepare(&sql).map_err(|e| e.to_string())?;
    let items = stmt.query_map(params, |row| {
        Ok(MovimientoCajaRs {
            id: row.get(0)?,
            tipo: row.get(1)?,
            usuario_id: row.get(2)?,
            usuario_nombre: row.get(3)?,
            monto: row.get(4)?,
            concepto: row.get(5)?,
            autorizado_por: row.get(6)?,
            corte_id: row.get(7)?,
            fecha: row.get(8)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();
    Ok(items)
}

// ─── Núcleo: cálculo del período abierto ──────────────────

/// Calcula todo lo que un corte hecho en `ahora` debe cubrir.
///
/// Es la ÚNICA fuente de verdad del esperado: la usan tanto la vista previa
/// (`calcular_datos_corte`) como la confirmación (`crear_corte`), así que lo
/// que se muestra y lo que se guarda salen de la misma fórmula.
///
/// El período termina en max(ahora, piso de la cadena): con el reloj
/// atrasado, las filas que quedaron en el piso entran igual a la vista
/// previa, y `fecha_fin` es el momento con el que se guardará el corte.
pub(crate) fn calcular_periodo(db: &Connection, ahora: &str) -> Result<DatosCorte, String> {
    let (inicio, fondo_inicial) = inicio_y_fondo(db);
    let fin = con_piso(ahora, piso_desde_inicio(&inicio).as_deref());

    let flujo = flujo_efectivo(db, &inicio, &fin)?;

    let (tarjeta, transferencia, num_transacciones, total_ventas, total_descuentos): (f64, f64, i64, f64, f64) =
        db.query_row(
            &format!(
                "SELECT \
                   COALESCE(SUM(CASE WHEN metodo_pago = 'tarjeta' THEN total ELSE 0 END), 0), \
                   COALESCE(SUM(CASE WHEN metodo_pago = 'transferencia' THEN total ELSE 0 END), 0), \
                   COUNT(*), COALESCE(SUM(total), 0), COALESCE(SUM(descuento), 0) \
                 FROM ventas WHERE caja = 'principal' AND deleted_at IS NULL \
                   AND {mo} > ? AND {mo} <= ? AND anulada = 0",
                mo = MOMENTO_SQL
            ),
            rusqlite::params![inicio, fin],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        ).map_err(|e| e.to_string())?;

    let total_anulaciones: f64 = db.query_row(
        &format!(
            "SELECT COALESCE(SUM(total), 0) FROM ventas \
             WHERE caja = 'principal' AND deleted_at IS NULL AND {mo} > ? AND {mo} <= ? \
               AND anulada = 1",
            mo = MOMENTO_SQL
        ),
        rusqlite::params![inicio, fin],
        |r| r.get(0),
    ).unwrap_or(0.0);

    let movimientos = leer_movimientos(
        db,
        &format!(
            "m.caja = 'principal' AND m.corte_id IS NULL AND m.deleted_at IS NULL \
             AND {mo} > ?1 AND {mo} <= ?2",
            mo = MOMENTO_M_SQL
        ),
        &[&inicio, &fin],
        "ASC",
    )?;

    let efectivo_esperado = round2(
        fondo_inicial + flujo.ventas_efectivo + flujo.entradas - flujo.retiros
    );

    // Informativo: cortes de turno que ya se hicieron hoy y lo retirado en ellos.
    let dia = &fin[..10];
    let cortes_parciales_hoy: i64 = db.query_row(
        &format!(
            "SELECT COUNT(*) FROM cortes WHERE tipo = 'PARCIAL' AND deleted_at IS NULL \
             AND caja = 'principal' AND substr({}, 1, 10) = ?",
            FIN_CORTE_SQL
        ),
        rusqlite::params![dia],
        |r| r.get(0),
    ).unwrap_or(0);

    // Lo que se sacó en cada corte de turno = contado − fondo que se dejó.
    let total_retirado_parciales: f64 = db.query_row(
        &format!(
            "SELECT COALESCE(SUM(efectivo_contado - fondo_siguiente), 0) FROM cortes \
             WHERE tipo = 'PARCIAL' AND deleted_at IS NULL AND caja = 'principal' \
             AND substr({}, 1, 10) = ?",
            FIN_CORTE_SQL
        ),
        rusqlite::params![dia],
        |r| r.get(0),
    ).unwrap_or(0.0);

    // hora_inicio/hora_fin muestran la fecha real de la venta; el rango usa MOMENTO.
    let mut vstmt = db.prepare(&format!(
        r#"SELECT v.usuario_id, COALESCE(u.nombre_completo, 'Desconocido'),
                  COUNT(*), COALESCE(SUM(v.total), 0),
                  MIN(v.fecha), MAX(v.fecha)
           FROM ventas v
           LEFT JOIN usuarios u ON u.id = v.usuario_id
           WHERE v.caja = 'principal' AND v.deleted_at IS NULL
             AND {mo} > ? AND {mo} <= ? AND v.anulada = 0
           GROUP BY v.usuario_id
           ORDER BY 4 DESC"#,
        mo = MOMENTO_V_SQL
    )).map_err(|e| e.to_string())?;

    let vendedores: Vec<VendedorResumenRs> = vstmt.query_map(
        rusqlite::params![inicio, fin],
        |row| Ok(VendedorResumenRs {
            usuario_id: row.get(0)?,
            usuario_nombre: row.get(1)?,
            num_ventas: row.get(2)?,
            total_vendido: row.get(3)?,
            hora_inicio: row.get(4)?,
            hora_fin: row.get(5)?,
        }),
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(DatosCorte {
        fecha_inicio: inicio,
        fecha_fin: fin,
        fondo_inicial,
        total_ventas_efectivo: flujo.ventas_efectivo,
        total_ventas_tarjeta: tarjeta,
        total_ventas_transferencia: transferencia,
        total_ventas,
        num_transacciones,
        total_descuentos,
        total_anulaciones,
        total_entradas_efectivo: flujo.entradas,
        total_retiros_efectivo: flujo.retiros,
        efectivo_esperado,
        cortes_parciales_hoy,
        total_retirado_parciales,
        movimientos,
        vendedores,
    })
}

/// Prefijo del error que el frontend reconoce para recargar el corte en vez
/// de mostrar un error genérico.
pub(crate) const ERR_DATOS_CAMBIARON: &str = "DATOS_CAMBIARON";

/// Guarda un corte. Recalcula todo dentro de la transacción y rechaza si el
/// esperado ya no coincide con lo que vio el cajero.
///
/// `reloj` es la hora real; el corte se estampa en max(reloj, piso de la
/// cadena) (ver "RELOJ MONÓTONO" en el encabezado), el mismo momento que
/// usa la vista previa.
pub(crate) fn crear_corte_core(
    db: &Connection,
    datos: NuevoCorte,
    reloj: &str,
) -> Result<CorteCreado, String> {
    if datos.tipo != "PARCIAL" && datos.tipo != "DIA" {
        return Err("Tipo de corte inválido".to_string());
    }
    if datos.efectivo_contado < 0.0 {
        return Err("El efectivo contado no puede ser negativo".to_string());
    }
    let contado = round2(datos.efectivo_contado);

    db.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;

    // Dentro de la transacción: el piso depende del último corte.
    let momento_corte = ahora_cadena(db, reloj);
    let ahora: &str = &momento_corte;
    // El retiro del corte guarda el reloj real en `fecha`, como toda fila.
    let registrado_retiro = registrado_at_para(db, reloj);

    let resultado = (|| -> Result<CorteCreado, String> {
        // Un solo cierre del día por fecha calendario (del momento del conteo).
        if datos.tipo == "DIA" {
            let existe: i64 = db.query_row(
                &format!(
                    "SELECT COUNT(*) FROM cortes WHERE tipo = 'DIA' AND deleted_at IS NULL \
                     AND caja = 'principal' AND substr({}, 1, 10) = ?",
                    FIN_CORTE_SQL
                ),
                rusqlite::params![&ahora[..10]],
                |r| r.get(0),
            ).unwrap_or(0);
            if existe > 0 {
                return Err(format!(
                    "Ya se hizo el cierre de caja del {}. Las ventas posteriores se incluirán \
                     en el próximo corte.",
                    &ahora[..10]
                ));
            }
        }

        let calc = calcular_periodo(db, ahora)?;

        // El cajero contó contra un esperado concreto. Si entre que abrió el
        // modal y confirmó entró una venta o un movimiento, ese esperado ya
        // es otro: guardar el viejo es justo lo que descuadraba los cortes.
        if (calc.efectivo_esperado - round2(datos.datos.efectivo_esperado)).abs() > EPS {
            return Err(format!(
                "{}: El efectivo esperado cambió de ${:.2} a ${:.2} porque se registraron \
                 ventas o movimientos mientras hacías el corte. Revisa el nuevo esperado y \
                 vuelve a confirmar.",
                ERR_DATOS_CAMBIARON, datos.datos.efectivo_esperado, calc.efectivo_esperado
            ));
        }

        // Retiro al momento del corte (solo de turno) y fondo que queda.
        let (fondo_siguiente, retiro_validado) = match &datos.retiro {
            Some(r) if r.monto > EPS => {
                if datos.tipo != "PARCIAL" {
                    return Err("El retiro solo aplica en cortes de turno".to_string());
                }
                let monto = round2(r.monto);
                if monto > contado + EPS {
                    return Err("No puedes retirar más efectivo del que hay contado en caja".to_string());
                }
                if r.concepto.trim().is_empty() {
                    return Err("El concepto del retiro es obligatorio".to_string());
                }
                let autorizado = resolver_autorizacion_retiro(
                    db, datos.usuario_id, monto, r.pin_autorizacion.as_deref(), None,
                )?;
                (round2(contado - monto), Some((monto, r.concepto.trim().to_string(), autorizado)))
            }
            _ => {
                let f = round2(datos.fondo_siguiente);
                if f < 0.0 {
                    return Err("El fondo que se deja no puede ser negativo".to_string());
                }
                if f > contado + EPS {
                    return Err(format!(
                        "No puedes dejar ${:.2} de fondo si solo contaste ${:.2}",
                        f, contado
                    ));
                }
                (f, None)
            }
        };

        let diferencia = round2(contado - calc.efectivo_esperado);
        let nota = datos.nota_diferencia.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if diferencia.abs() > EPS && nota.is_none() {
            return Err("La nota es obligatoria cuando hay diferencia de dinero".to_string());
        }

        db.execute(
            r#"INSERT INTO cortes (tipo, usuario_id, fecha_inicio, fecha_fin,
                   fondo_inicial, total_ventas_efectivo, total_ventas_tarjeta,
                   total_ventas_transferencia, total_ventas, num_transacciones,
                   total_descuentos, total_anulaciones, total_entradas_efectivo,
                   total_retiros_efectivo, efectivo_esperado, efectivo_contado,
                   diferencia, nota_diferencia, fondo_siguiente, created_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            rusqlite::params![
                datos.tipo, datos.usuario_id, calc.fecha_inicio, ahora,
                calc.fondo_inicial,
                calc.total_ventas_efectivo, calc.total_ventas_tarjeta,
                calc.total_ventas_transferencia, calc.total_ventas,
                calc.num_transacciones, calc.total_descuentos,
                calc.total_anulaciones, calc.total_entradas_efectivo,
                calc.total_retiros_efectivo, calc.efectivo_esperado,
                contado, diferencia, nota,
                fondo_siguiente, ahora
            ],
        ).map_err(|e| e.to_string())?;
        let corte_id = db.last_insert_rowid();

        // Asignar exactamente los movimientos que entraron en el cálculo
        // (mismo predicado, misma transacción).
        db.execute(
            &format!(
                "UPDATE movimientos_caja SET corte_id = ? \
                 WHERE caja = 'principal' AND corte_id IS NULL AND deleted_at IS NULL \
                   AND {mo} > ? AND {mo} <= ?",
                mo = MOMENTO_SQL
            ),
            rusqlite::params![corte_id, calc.fecha_inicio, ahora],
        ).map_err(|e| e.to_string())?;

        if let Some(denoms) = &datos.denominaciones {
            for d in denoms.iter().filter(|d| d.cantidad > 0) {
                db.execute(
                    "INSERT INTO corte_denominaciones (corte_id, denominacion, tipo, cantidad, subtotal) \
                     VALUES (?, ?, ?, ?, ?)",
                    rusqlite::params![
                        corte_id, d.denominacion, d.tipo, d.cantidad,
                        d.denominacion * d.cantidad as f64
                    ],
                ).map_err(|e| e.to_string())?;
            }
        }

        if datos.tipo == "DIA" {
            for v in &calc.vendedores {
                db.execute(
                    r#"INSERT INTO corte_vendedores
                       (corte_id, usuario_id, num_ventas, total_vendido, hora_inicio, hora_fin)
                       VALUES (?, ?, ?, ?, ?, ?)"#,
                    rusqlite::params![
                        corte_id, v.usuario_id, v.num_ventas,
                        v.total_vendido, v.hora_inicio, v.hora_fin
                    ],
                ).map_err(|e| e.to_string())?;
            }
        }

        // El retiro se inserta DESPUÉS de calcular y ya ligado a este corte:
        // no altera su esperado (se contó antes de sacar el dinero) y tampoco
        // entra al siguiente período (que arranca con fondo_siguiente, ya neto).
        if let Some((monto, concepto, autorizado)) = &retiro_validado {
            db.execute(
                "INSERT INTO movimientos_caja (tipo, usuario_id, monto, concepto, autorizado_por, corte_id, \
                                              fecha, registrado_at) \
                 VALUES ('RETIRO', ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    datos.usuario_id, monto, concepto, autorizado, corte_id, reloj, registrado_retiro
                ],
            ).map_err(|e| e.to_string())?;
        }

        let accion = if datos.tipo == "DIA" { "CORTE_DIA" } else { "CORTE_PARCIAL" };
        let desc = format!(
            "{} — Esperado: ${:.2} / Contado: ${:.2} / Diferencia: ${:.2} / Fondo que queda: ${:.2}",
            accion, calc.efectivo_esperado, contado, diferencia, fondo_siguiente
        );
        let _ = db.execute(
            r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id,
               descripcion_legible, origen)
               VALUES (?, ?, 'cortes', ?, ?, 'POS')"#,
            rusqlite::params![datos.usuario_id, accion, corte_id, desc],
        );

        Ok(CorteCreado {
            id: corte_id,
            tipo: datos.tipo.clone(),
            diferencia,
            efectivo_esperado: calc.efectivo_esperado,
            efectivo_contado: contado,
            fondo_siguiente,
            created_at: ahora.to_string(),
        })
    })();

    match resultado {
        Ok(c) => {
            db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            Ok(c)
        }
        Err(e) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// ─── Apertura: conteo de verificación ─────────────────────

#[derive(Serialize, Clone, Debug)]
pub struct EsperadoApertura {
    /// Lo que debería haber en caja ahora mismo.
    pub esperado: f64,
    /// Fondo que dejó el último corte.
    pub fondo_ultimo_corte: f64,
    /// Ventas en efectivo + entradas − retiros desde ese corte.
    pub flujo_desde_corte: f64,
    /// false si nunca ha habido un corte (primera apertura: no hay contra qué comparar).
    pub hay_referencia: bool,
    pub ultimo_corte_fecha: Option<String>,
}

pub(crate) fn calcular_esperado_apertura(db: &Connection, ahora: &str) -> Result<EsperadoApertura, String> {
    match ultimo_corte(db) {
        None => Ok(EsperadoApertura {
            esperado: 0.0,
            fondo_ultimo_corte: 0.0,
            flujo_desde_corte: 0.0,
            hay_referencia: false,
            ultimo_corte_fecha: None,
        }),
        Some(u) => {
            // Mismo piso que la vista previa del corte: con el reloj atrasado
            // las filas que quedaron en el piso también se esperan.
            let hasta = con_piso(ahora, piso_desde_inicio(&u.fin).as_deref());
            let f = flujo_efectivo(db, &u.fin, &hasta)?;
            let flujo = round2(f.ventas_efectivo + f.entradas - f.retiros);
            Ok(EsperadoApertura {
                esperado: round2(u.fondo_siguiente + flujo),
                fondo_ultimo_corte: u.fondo_siguiente,
                flujo_desde_corte: flujo,
                hay_referencia: true,
                ultimo_corte_fecha: Some(u.fin),
            })
        }
    }
}

#[derive(Deserialize)]
pub struct NuevaApertura {
    pub usuario_id: i64,
    pub fondo_declarado: f64,
    pub nota: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct AperturaCaja {
    pub id: i64,
    pub usuario_id: i64,
    pub usuario_nombre: String,
    pub fondo_declarado: f64,
    pub nota: Option<String>,
    pub fecha: String,
}

pub(crate) fn crear_apertura_core(
    db: &Connection,
    datos: NuevaApertura,
    ahora: &str,
) -> Result<AperturaCaja, String> {
    if datos.fondo_declarado < 0.0 {
        return Err("El fondo no puede ser negativo".to_string());
    }
    let declarado = round2(datos.fondo_declarado);

    let existe: i64 = db.query_row(
        "SELECT COUNT(*) FROM aperturas_caja \
         WHERE deleted_at IS NULL AND caja = 'principal' AND substr(fecha, 1, 10) = ?",
        rusqlite::params![&ahora[..10]],
        |r| r.get(0),
    ).unwrap_or(0);
    if existe > 0 {
        return Err("Ya existe una apertura de caja para hoy".to_string());
    }

    let esp = calcular_esperado_apertura(db, ahora)?;
    let diferencia = if esp.hay_referencia { round2(declarado - esp.esperado) } else { 0.0 };
    let nota = datos.nota.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);

    if diferencia.abs() > EPS && nota.is_none() {
        return Err(format!(
            "Contaste ${:.2} pero debería haber ${:.2} ({} ${:.2}). Escribe una nota explicando la diferencia.",
            declarado, esp.esperado,
            if diferencia < 0.0 { "faltan" } else { "sobran" },
            diferencia.abs()
        ));
    }

    db.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
    let resultado = (|| -> Result<i64, String> {
        db.execute(
            "INSERT INTO aperturas_caja (usuario_id, fondo_declarado, nota, fecha) VALUES (?, ?, ?, ?)",
            rusqlite::params![datos.usuario_id, declarado, nota, ahora],
        ).map_err(|e| {
            // idx_apertura_dia_unica: una apertura por día, de cualquier caja e
            // incluso eliminada. El sync nunca guarda aperturas de la caja web;
            // si alguna quedó en esta base, se explica en vez de mostrar el
            // error de SQLite.
            let msg = e.to_string();
            if msg.contains("UNIQUE") {
                "Ya hay otra apertura registrada para hoy en esta base (eliminada o de otra caja). \
                 Avisa al administrador.".to_string()
            } else {
                msg
            }
        })?;
        let id = db.last_insert_rowid();

        // La diferencia de la noche se registra como movimiento explícito en
        // el período en curso. Así el corte del día arranca cuadrado contra lo
        // que realmente había, y el faltante/sobrante queda documentado a
        // nombre de quien abrió.
        if diferencia.abs() > EPS {
            let (tipo, que) = if diferencia > 0.0 { ("ENTRADA", "sobraban") } else { ("RETIRO", "faltaban") };
            let concepto = format!(
                "Ajuste de apertura: {} ${:.2} respecto a lo esperado (${:.2}) — {}",
                que, diferencia.abs(), esp.esperado, nota.as_deref().unwrap_or("")
            );
            db.execute(
                "INSERT INTO movimientos_caja (tipo, usuario_id, monto, concepto, fecha, registrado_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    tipo, datos.usuario_id, diferencia.abs(), concepto, ahora,
                    registrado_at_para(db, ahora)
                ],
            ).map_err(|e| e.to_string())?;
        }

        let desc = if diferencia.abs() > EPS {
            format!(
                "Apertura de caja: contado ${:.2}, esperado ${:.2}, diferencia ${:.2}",
                declarado, esp.esperado, diferencia
            )
        } else {
            format!("Apertura de caja con fondo de ${:.2}", declarado)
        };
        let _ = db.execute(
            r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id,
               descripcion_legible, origen)
               VALUES (?, 'APERTURA_CAJA', 'aperturas_caja', ?, ?, 'POS')"#,
            rusqlite::params![datos.usuario_id, id, desc],
        );
        Ok(id)
    })();

    let id = match resultado {
        Ok(id) => { db.execute_batch("COMMIT").map_err(|e| e.to_string())?; id }
        Err(e) => { let _ = db.execute_batch("ROLLBACK"); return Err(e); }
    };

    let usuario_nombre: String = db.query_row(
        "SELECT nombre_completo FROM usuarios WHERE id = ?",
        rusqlite::params![datos.usuario_id],
        |row| row.get(0),
    ).unwrap_or_else(|_| "Desconocido".to_string());

    Ok(AperturaCaja {
        id,
        usuario_id: datos.usuario_id,
        usuario_nombre,
        fondo_declarado: declarado,
        nota,
        fecha: ahora.to_string(),
    })
}

/// Primera fecha (YYYY-MM-DD) anterior a `hoy` con ventas que todavía no
/// entran en ningún corte. None si todo lo de días anteriores ya se cortó.
pub(crate) fn primer_dia_pendiente(db: &Connection, hoy: &str) -> Option<String> {
    let (inicio, _) = inicio_y_fondo(db);
    db.query_row(
        &format!(
            "SELECT MIN(substr({mo}, 1, 10)) FROM ventas \
             WHERE caja = 'principal' AND deleted_at IS NULL AND anulada = 0 \
               AND {mo} > ? AND substr({mo}, 1, 10) < ?",
            mo = MOMENTO_SQL
        ),
        rusqlite::params![inicio, hoy],
        |r| r.get::<_, Option<String>>(0),
    ).ok().flatten()
}

/// Movimientos del período en curso (sin corte, posteriores al último corte),
/// más recientes primero.
pub(crate) fn movimientos_sin_corte(db: &Connection) -> Result<Vec<MovimientoCajaRs>, String> {
    let (inicio, _) = inicio_y_fondo(db);
    leer_movimientos(
        db,
        &format!(
            "m.caja = 'principal' AND m.corte_id IS NULL AND m.deleted_at IS NULL AND {mo} > ?1",
            mo = MOMENTO_M_SQL
        ),
        &[&inicio],
        "DESC",
    )
}

/// Cortes de la caja de la tienda, más recientes primero.
pub(crate) fn listar_cortes_core(db: &Connection, limite: i64) -> Result<Vec<CorteResumen>, String> {
    let mut stmt = db.prepare(
        r#"SELECT c.id, c.tipo, COALESCE(u.nombre_completo, 'Desconocido'), c.created_at,
                  c.fondo_inicial, c.total_ventas_efectivo, c.total_ventas_tarjeta,
                  c.total_ventas_transferencia, c.total_ventas, c.num_transacciones,
                  c.total_entradas_efectivo, c.total_retiros_efectivo,
                  c.efectivo_esperado, c.efectivo_contado, c.diferencia,
                  c.nota_diferencia, c.fondo_siguiente
           FROM cortes c
           LEFT JOIN usuarios u ON u.id = c.usuario_id
           WHERE c.deleted_at IS NULL AND c.caja = 'principal'
           ORDER BY c.created_at DESC, c.id DESC
           LIMIT ?"#,
    ).map_err(|e| e.to_string())?;

    let items = stmt.query_map(rusqlite::params![limite], |row| {
        Ok(CorteResumen {
            id: row.get(0)?,
            tipo: row.get(1)?,
            usuario_nombre: row.get(2)?,
            created_at: row.get(3)?,
            fondo_inicial: row.get(4)?,
            total_ventas_efectivo: row.get(5)?,
            total_ventas_tarjeta: row.get(6)?,
            total_ventas_transferencia: row.get(7)?,
            total_ventas: row.get(8)?,
            num_transacciones: row.get(9)?,
            total_entradas_efectivo: row.get(10)?,
            total_retiros_efectivo: row.get(11)?,
            efectivo_esperado: row.get(12)?,
            efectivo_contado: row.get(13)?,
            diferencia: row.get(14)?,
            nota_diferencia: row.get(15)?,
            fondo_siguiente: row.get(16)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(items)
}

/// Apertura de la caja de la tienda registrada en `dia` (YYYY-MM-DD), si existe.
pub(crate) fn apertura_del_dia(db: &Connection, dia: &str) -> Option<AperturaCaja> {
    db.query_row(
        r#"SELECT a.id, a.usuario_id, COALESCE(u.nombre_completo, 'Desconocido'),
                  a.fondo_declarado, a.nota, a.fecha
           FROM aperturas_caja a
           LEFT JOIN usuarios u ON u.id = a.usuario_id
           WHERE a.deleted_at IS NULL AND a.caja = 'principal' AND substr(a.fecha, 1, 10) = ?
           ORDER BY a.id DESC
           LIMIT 1"#,
        rusqlite::params![dia],
        |row| Ok(AperturaCaja {
            id: row.get(0)?,
            usuario_id: row.get(1)?,
            usuario_nombre: row.get(2)?,
            fondo_declarado: row.get(3)?,
            nota: row.get(4)?,
            fecha: row.get(5)?,
        }),
    ).ok()
}

// ─── Comandos ─────────────────────────────────────────────

/// Registrar entrada o retiro de efectivo (sin ser una venta)
#[tauri::command]
pub fn crear_movimiento_caja(
    datos: NuevoMovimiento,
    state: State<'_, AppState>,
) -> Result<MovimientoCajaRs, String> {
    let db = state.db.lock().unwrap();
    crear_movimiento_core(&db, datos, &ahora_local())
}

/// Núcleo de `crear_movimiento_caja` con el reloj explícito (para pruebas).
pub(crate) fn crear_movimiento_core(
    db: &Connection,
    datos: NuevoMovimiento,
    now: &str,
) -> Result<MovimientoCajaRs, String> {
    if !datos.monto.is_finite() || datos.monto <= 0.0 {
        return Err("El monto debe ser mayor a cero".to_string());
    }
    if datos.tipo != "ENTRADA" && datos.tipo != "RETIRO" {
        return Err("Tipo de movimiento inválido".to_string());
    }

    let monto = round2(datos.monto);

    let autorizado_por_validado = if datos.tipo == "RETIRO" {
        resolver_autorizacion_retiro(
            db, datos.usuario_id, monto, datos.pin_autorizacion.as_deref(), datos.autorizado_por,
        )?
    } else {
        datos.autorizado_por
    };

    let now = now.to_string();
    // Reloj atrasado (en o antes del último corte): entra al período abierto.
    let registrado_at = registrado_at_para(db, &now);

    db.execute(
        r#"INSERT INTO movimientos_caja (tipo, usuario_id, monto, concepto, autorizado_por, fecha,
                                         registrado_at)
           VALUES (?, ?, ?, ?, ?, ?, ?)"#,
        rusqlite::params![
            datos.tipo, datos.usuario_id, monto,
            datos.concepto, autorizado_por_validado, now, registrado_at
        ],
    ).map_err(|e| e.to_string())?;

    let id = db.last_insert_rowid();

    let usuario_nombre: String = db.query_row(
        "SELECT nombre_completo FROM usuarios WHERE id = ?",
        rusqlite::params![datos.usuario_id],
        |row| row.get(0),
    ).unwrap_or_else(|_| "Desconocido".to_string());

    let accion = if datos.tipo == "ENTRADA" { "ENTRADA_CAJA" } else { "RETIRO_CAJA" };
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id,
           descripcion_legible, origen)
           VALUES (?, ?, 'movimientos_caja', ?, ?, 'POS')"#,
        rusqlite::params![
            datos.usuario_id, accion, id,
            format!("{} de ${:.2} — {}", datos.tipo, monto, datos.concepto)
        ],
    );

    Ok(MovimientoCajaRs {
        id,
        tipo: datos.tipo,
        usuario_id: datos.usuario_id,
        usuario_nombre,
        monto,
        concepto: datos.concepto,
        autorizado_por: autorizado_por_validado,
        corte_id: None,
        fecha: now,
    })
}

/// Movimientos del período en curso (aún sin corte). Los movimientos viejos
/// que quedaron huérfanos por el modelo anterior no se listan aquí: ya no
/// pueden entrar en ningún corte y se revisan en la pestaña de Auditoría.
#[tauri::command]
pub fn listar_movimientos_sin_corte(
    state: State<'_, AppState>,
) -> Result<Vec<MovimientoCajaRs>, String> {
    let db = state.db.lock().unwrap();
    movimientos_sin_corte(&db)
}

/// Vista previa del corte. Los parámetros de fecha se aceptan por
/// compatibilidad con el frontend, pero el período lo define la cadena de
/// cortes: desde el último corte hasta este momento.
#[tauri::command]
pub fn calcular_datos_corte(
    fecha_inicio: String,
    fecha_fin: String,
    state: State<'_, AppState>,
) -> Result<DatosCorte, String> {
    let _ = (fecha_inicio, fecha_fin);
    let db = state.db.lock().unwrap();
    calcular_periodo(&db, &ahora_local())
}

/// Confirmar y guardar un corte (de turno o del día)
#[tauri::command]
pub fn crear_corte(
    datos: NuevoCorte,
    state: State<'_, AppState>,
) -> Result<CorteCreado, String> {
    let db = state.db.lock().unwrap();
    crear_corte_core(&db, datos, &ahora_local())
}

/// Listar cortes con resumen
#[tauri::command]
pub fn listar_cortes(
    limite: i64,
    state: State<'_, AppState>,
) -> Result<Vec<CorteResumen>, String> {
    let db = state.db.lock().unwrap();
    listar_cortes_core(&db, limite)
}

/// Obtener detalle completo de un corte (denominaciones + movimientos + vendedores)
#[tauri::command]
pub fn obtener_detalle_corte(
    id: i64,
    state: State<'_, AppState>,
) -> Result<CorteDetalle, String> {
    let db = state.db.lock().unwrap();

    let corte = db.query_row(
        r#"SELECT c.id, c.tipo, COALESCE(u.nombre_completo, 'Desconocido'), c.created_at,
                  c.fondo_inicial, c.total_ventas_efectivo, c.total_ventas_tarjeta,
                  c.total_ventas_transferencia, c.total_ventas, c.num_transacciones,
                  c.total_entradas_efectivo, c.total_retiros_efectivo,
                  c.efectivo_esperado, c.efectivo_contado, c.diferencia,
                  c.nota_diferencia, c.fondo_siguiente
           FROM cortes c
           LEFT JOIN usuarios u ON u.id = c.usuario_id
           WHERE c.id = ?"#,
        rusqlite::params![id],
        |row| Ok(CorteResumen {
            id: row.get(0)?,
            tipo: row.get(1)?,
            usuario_nombre: row.get(2)?,
            created_at: row.get(3)?,
            fondo_inicial: row.get(4)?,
            total_ventas_efectivo: row.get(5)?,
            total_ventas_tarjeta: row.get(6)?,
            total_ventas_transferencia: row.get(7)?,
            total_ventas: row.get(8)?,
            num_transacciones: row.get(9)?,
            total_entradas_efectivo: row.get(10)?,
            total_retiros_efectivo: row.get(11)?,
            efectivo_esperado: row.get(12)?,
            efectivo_contado: row.get(13)?,
            diferencia: row.get(14)?,
            nota_diferencia: row.get(15)?,
            fondo_siguiente: row.get(16)?,
        }),
    ).map_err(|_| "Corte no encontrado".to_string())?;

    let mut dstmt = db.prepare(
        "SELECT denominacion, tipo, cantidad, subtotal FROM corte_denominaciones WHERE corte_id = ? ORDER BY denominacion DESC",
    ).map_err(|e| e.to_string())?;

    let denominaciones: Vec<DenominacionDetalle> = dstmt.query_map(rusqlite::params![id], |row| {
        Ok(DenominacionDetalle {
            denominacion: row.get(0)?,
            tipo: row.get(1)?,
            cantidad: row.get(2)?,
            subtotal: row.get(3)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    let movimientos = leer_movimientos(&db, "m.corte_id = ?1", &[&id], "ASC")?;

    let mut vstmt = db.prepare(
        r#"SELECT cv.usuario_id, COALESCE(u.nombre_completo, 'Desconocido'), cv.num_ventas, cv.total_vendido,
                  cv.hora_inicio, cv.hora_fin
           FROM corte_vendedores cv
           LEFT JOIN usuarios u ON u.id = cv.usuario_id
           WHERE cv.corte_id = ?
           ORDER BY cv.total_vendido DESC"#,
    ).map_err(|e| e.to_string())?;

    let vendedores: Vec<VendedorResumenRs> = vstmt.query_map(rusqlite::params![id], |row| {
        Ok(VendedorResumenRs {
            usuario_id: row.get(0)?,
            usuario_nombre: row.get(1)?,
            num_ventas: row.get(2)?,
            total_vendido: row.get(3)?,
            hora_inicio: row.get(4)?,
            hora_fin: row.get(5)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(CorteDetalle { corte, denominaciones, movimientos, vendedores })
}

/// Registrar la apertura de caja del día. Compara lo contado contra lo
/// esperado y, si difiere, exige nota y registra un movimiento de ajuste.
#[tauri::command]
pub fn crear_apertura_caja(
    datos: NuevaApertura,
    state: State<'_, AppState>,
) -> Result<AperturaCaja, String> {
    let db = state.db.lock().unwrap();
    crear_apertura_core(&db, datos, &ahora_local())
}

/// Lo que debería haber en caja al abrir (para mostrarlo en el modal).
#[tauri::command]
pub fn obtener_esperado_apertura(
    state: State<'_, AppState>,
) -> Result<EsperadoApertura, String> {
    let db = state.db.lock().unwrap();
    calcular_esperado_apertura(&db, &ahora_local())
}

/// Obtener la apertura de caja de hoy (si existe)
#[tauri::command]
pub fn obtener_apertura_hoy(
    state: State<'_, AppState>,
) -> Result<Option<AperturaCaja>, String> {
    let db = state.db.lock().unwrap();
    let ahora = ahora_local();
    Ok(apertura_del_dia(&db, &ahora[..10]))
}

/// Fondo que dejó el último corte (cualquier tipo, el más reciente por
/// momento de conteo). Se mantiene por compatibilidad; la apertura usa
/// `obtener_esperado_apertura`, que además suma lo que pasó desde entonces.
#[tauri::command]
pub fn obtener_fondo_sugerido(
    state: State<'_, AppState>,
) -> Result<f64, String> {
    let db = state.db.lock().unwrap();
    Ok(ultimo_corte(&db).map(|u| u.fondo_siguiente).unwrap_or(2000.0))
}

/// Si hay ventas de días anteriores que todavía no entran en ningún corte,
/// devuelve la primera de esas fechas. El próximo corte las incluirá.
#[tauri::command]
pub fn verificar_corte_dia_pendiente(
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    let db = state.db.lock().unwrap();
    let ahora = ahora_local();
    Ok(primer_dia_pendiente(&db, &ahora[..10]))
}

/// Momento en que empieza el período abierto (fin del último corte).
#[tauri::command]
pub fn obtener_inicio_proximo_cierre(
    state: State<'_, AppState>,
) -> Result<String, String> {
    let db = state.db.lock().unwrap();
    Ok(inicio_y_fondo(&db).0)
}
