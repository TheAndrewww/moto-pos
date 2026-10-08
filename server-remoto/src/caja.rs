// caja.rs — Motor de caja del servidor: la cadena de cortes en Postgres.
//
// Es el puerto de src-tauri/src/commands/cortes.rs y auditoria_caja.rs (el
// modelo v0.1.39 del escritorio). Mismas reglas, mismos mensajes y la misma
// forma de las respuestas, para que el mismo React funcione igual en los dos.
//
// ─── DOS CAJAS ────────────────────────────────────────────
//
// Cada venta, movimiento, corte y apertura dice en `caja` a qué cajón
// pertenece (migración 010):
//   - 'principal' = el cajón físico de la tienda. Su cadena (cortes y
//     aperturas) tiene UN solo escritor: el POS de escritorio. Los equipos web
//     en modo espejo venden, registran entradas/retiros y devoluciones con ese
//     dinero (origen 'web', caja 'principal'); esas filas bajan al escritorio
//     y cuentan en SU corte. Aquí solo se lee: vista previa, historial y
//     auditoría "según lo sincronizado".
//   - 'web' = caja aparte de los equipos en modo individual (todos comparten
//     la misma). Su cadena la escribe SOLO este servidor, con el mismo
//     algoritmo que el escritorio.
// `origen` ('desktop' | 'web') solo dice quién creó la fila.
//
// ─── LA CADENA ────────────────────────────────────────────
//
//   - Cada corte cubre (FIN del corte anterior, momento de este conteo].
//   - fondo_inicial de un corte = fondo_siguiente del anterior.
//   - Toda venta y todo movimiento caen en exactamente UN corte.
//   - FIN = LEAST(fecha_fin, created_at) normalizados (los cortes viejos del
//     escritorio mienten de dos formas distintas sobre ese momento).
//   - MOMENTO = COALESCE(registrado_at, fecha): una venta espejo que llegó al
//     escritorio después de un corte que ya cubría su fecha entra al
//     siguiente. Las filas web de este servidor dejan registrado_at en NULL.
//   - La apertura no corta la cadena: es un conteo de verificación; si no
//     cuadra se registra un movimiento "Ajuste de apertura" con nota.
//   - Sin ningún corte, el fondo del período es el de la primera apertura.
//     En la caja 'web' solo cuentan las aperturas desde sync_meta
//     'caja_v2_desde' (ver `aperturas_validas_desde`).
//   - crear_corte vuelve a calcular dentro de la transacción y rechaza con
//     DATOS_CAMBIARON si el esperado cambió mientras se contaba.
//
// ─── SERIALIZACIÓN Y RELOJ (solo caja 'web') ──────────────
//
// El escritorio serializa todo con un Mutex y BEGIN IMMEDIATE. Aquí cada
// escritura a la caja 'web' (venta, movimiento, devolución, anulación, corte,
// apertura) toma primero `pg_advisory_xact_lock` de esa caja y estampa su
// fecha con `ahora_caja` = GREATEST(reloj real, FIN del último corte + 1 s).
// Así ninguna venta puede quedar con fecha dentro de un corte que no la
// contó (lo que pasaba con now(), que es el inicio de la transacción).
//
// Todas las funciones reciben `&mut PgConnection` (normalmente una
// transacción abierta) y, las que dependen de la hora, el `ahora` explícito:
// las pruebas recorren días completos sin depender del reloj.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::{postgres::PgRow, PgConnection, PgPool, Row};

use crate::auth::Claims;
use crate::error::ApiError;

// ─── Constantes ───────────────────────────────────────────

/// Tolerancia para comparar dinero (medio centavo).
pub(crate) const EPS: f64 = 0.005;

/// Monto de retiro a partir del cual se requiere PIN del dueño.
pub(crate) const RETIRO_LIMITE_SIN_PIN: f64 = 500.0;

/// Prefijo del error que el frontend reconoce para recargar el corte.
pub(crate) const ERR_DATOS_CAMBIARON: &str = "DATOS_CAMBIARON";

/// La cadena de la caja de la tienda solo la escribe el escritorio.
pub(crate) const MSG_PRINCIPAL_SOLO_ESCRITORIO: &str =
    "La caja de la tienda se corta y se abre en el POS de escritorio";

/// Inicio de la cadena cuando todavía no hay ningún corte.
pub(crate) const INICIO_SIN_CORTES: &str = "0000-00-00 00:00:00";

/// Momento real del conteo de un corte (ver encabezado).
pub(crate) const FIN_CORTE_SQL: &str =
    "LEAST(replace(substr(fecha_fin,1,19),'T',' '), replace(substr(created_at,1,19),'T',' '))";
const FIN_CORTE_C_SQL: &str =
    "LEAST(replace(substr(c.fecha_fin,1,19),'T',' '), replace(substr(c.created_at,1,19),'T',' '))";

/// Momento en que una venta o movimiento entra a la cadena.
pub(crate) const MOMENTO_SQL: &str = "COALESCE(registrado_at, fecha)";
const MOMENTO_V_SQL: &str = "COALESCE(v.registrado_at, v.fecha)";
const MOMENTO_M_SQL: &str = "COALESCE(m.registrado_at, m.fecha)";

/// Hora real de la tienda en este instante (clock_timestamp, no el inicio de
/// la transacción como now()).
pub(crate) const RELOJ_SQL: &str =
    "to_char(clock_timestamp() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS')";

/// updated_at de las filas (hora local, como todo el sync).
const NOW_TEXT: &str = "to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS')";

/// origen_device de los sync_cursor que escribe el POS web.
const WEB_ORIGIN: &str = "web-pos";

// ─── Caja ─────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caja {
    Principal,
    Web,
}

impl Caja {
    pub fn as_str(self) -> &'static str {
        match self {
            Caja::Principal => "principal",
            Caja::Web => "web",
        }
    }

    /// Llave de pg_advisory_xact_lock (forma de un bigint). No chocan con
    /// las de folios (7301100/7301101), códigos de producto (7301102, forma
    /// de dos enteros) ni la de las pruebas (7301199).
    fn llave_candado(self) -> i64 {
        match self {
            Caja::Principal => 7_301_001,
            Caja::Web => 7_301_002,
        }
    }
}

/// Valor del claim `device` en las sesiones web que entraron sin dispositivo.
/// Un token de sesión web NUNCA lleva `device` vacío: /sync/* solo acepta
/// tokens sin dispositivo (los de /auth/login del escritorio). No es un
/// dispositivo real: nunca se guarda en pos_devices y su caja es la de la
/// tienda (espejo).
pub(crate) const DISPOSITIVO_SIN_EQUIPO: &str = "web-sin-equipo";

/// ¿`uuid` identifica un dispositivo web real? (no vacío ni el centinela).
pub(crate) fn es_dispositivo_real(uuid: &str) -> bool {
    let u = uuid.trim();
    !u.is_empty() && u != DISPOSITIVO_SIN_EQUIPO
}

/// Dispositivo web real de la sesión, si lo hay.
pub fn dispositivo_de(claims: &Claims) -> Option<&str> {
    claims.device.as_deref().filter(|u| es_dispositivo_real(u))
}

/// Modo de caja del dispositivo de la sesión ('espejo' | 'individual').
/// Sin dispositivo en el token (o el centinela, o un dispositivo
/// desconocido) → 'espejo'.
pub async fn modo_de_dispositivo(pool: &PgPool, claims: &Claims) -> Result<String, ApiError> {
    let Some(uuid) = dispositivo_de(claims) else {
        return Ok("espejo".to_string());
    };
    let modo: Option<String> =
        sqlx::query_scalar("SELECT modo_caja FROM pos_devices WHERE device_uuid = $1")
            .bind(uuid)
            .fetch_optional(pool)
            .await?;
    Ok(modo.unwrap_or_else(|| "espejo".to_string()))
}

/// Caja con la que trabaja la sesión: 'web' solo si el dispositivo está en
/// modo individual; todo lo demás (espejo, tokens sin dispositivo o con el
/// centinela DISPOSITIVO_SIN_EQUIPO) es la caja de la tienda.
pub async fn caja_de(pool: &PgPool, claims: &Claims) -> Result<Caja, ApiError> {
    Ok(if modo_de_dispositivo(pool, claims).await? == "individual" {
        Caja::Web
    } else {
        Caja::Principal
    })
}

// ─── Estructuras (misma forma que el escritorio) ─────────

/// = MovimientoCajaRs del escritorio.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MovimientoCaja {
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

/// = VendedorResumenRs del escritorio.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct VendedorResumen {
    pub usuario_id: i64,
    pub usuario_nombre: String,
    pub num_ventas: i64,
    pub total_vendido: f64,
    pub hora_inicio: String,
    pub hora_fin: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
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
    pub movimientos: Vec<MovimientoCaja>,
    pub vendedores: Vec<VendedorResumen>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct DenominacionInput {
    pub denominacion: f64,
    pub tipo: String,
    pub cantidad: i64,
}

/// Retiro al momento de un corte de turno: se registra DENTRO de la
/// transacción del corte, ya ligado a él (no descuadra el corte que lo
/// origina ni entra al siguiente período).
#[derive(Deserialize, Clone, Debug)]
pub struct RetiroCorte {
    pub monto: f64,
    #[serde(default)]
    pub concepto: String,
    #[serde(default)]
    pub pin_autorizacion: Option<String>,
}

/// Lo único que se usa de los datos que vio el cajero: el esperado contra el
/// que contó. Todo lo demás se recalcula aquí.
#[derive(Deserialize, Clone, Debug)]
pub struct DatosCorteCliente {
    pub efectivo_esperado: f64,
}

/// Lo que manda el frontend en `crear_corte`. Deserialización tolerante:
/// usuario_id, fecha_inicio y fecha_fin se aceptan y se ignoran (quién corta
/// es la sesión; el período lo define la cadena).
#[derive(Deserialize, Clone, Debug)]
pub struct NuevoCorte {
    pub tipo: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub usuario_id: Option<i64>,
    #[serde(default)]
    #[allow(dead_code)]
    pub fecha_inicio: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub fecha_fin: Option<String>,
    pub datos: DatosCorteCliente,
    pub efectivo_contado: f64,
    #[serde(default)]
    pub nota_diferencia: Option<String>,
    #[serde(default)]
    pub fondo_siguiente: f64,
    #[serde(default)]
    pub denominaciones: Option<Vec<DenominacionInput>>,
    #[serde(default)]
    pub retiro: Option<RetiroCorte>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct CorteCreado {
    pub id: i64,
    pub tipo: String,
    pub diferencia: f64,
    pub efectivo_esperado: f64,
    pub efectivo_contado: f64,
    pub fondo_siguiente: f64,
    pub created_at: String,
}

#[derive(Serialize, Clone, Debug, Default)]
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

#[derive(Serialize, Clone, Debug, Default)]
pub struct DenominacionDetalle {
    pub denominacion: f64,
    pub tipo: String,
    pub cantidad: i64,
    pub subtotal: f64,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct CorteDetalle {
    pub corte: CorteResumen,
    pub denominaciones: Vec<DenominacionDetalle>,
    pub movimientos: Vec<MovimientoCaja>,
    pub vendedores: Vec<VendedorResumen>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct EsperadoApertura {
    /// Lo que debería haber en caja ahora mismo.
    pub esperado: f64,
    /// Fondo que dejó el último corte.
    pub fondo_ultimo_corte: f64,
    /// Ventas en efectivo + entradas − retiros desde ese corte.
    pub flujo_desde_corte: f64,
    /// false si nunca ha habido un corte (primera apertura).
    pub hay_referencia: bool,
    pub ultimo_corte_fecha: Option<String>,
}

/// usuario_id se acepta y se ignora: quien abre es la sesión.
#[derive(Deserialize, Clone, Debug)]
pub struct NuevaApertura {
    #[serde(default)]
    #[allow(dead_code)]
    pub usuario_id: Option<i64>,
    pub fondo_declarado: f64,
    #[serde(default)]
    pub nota: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct AperturaCaja {
    pub id: i64,
    pub usuario_id: i64,
    pub usuario_nombre: String,
    pub fondo_declarado: f64,
    pub nota: Option<String>,
    pub fecha: String,
}

// ─── Utilidades ───────────────────────────────────────────

/// Redondea a centavos (sumas de flotantes no deben dejar diferencias
/// fantasma que exijan nota).
pub(crate) fn round2(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

fn mal(msg: impl Into<String>) -> ApiError {
    ApiError::BadRequest(msg.into())
}

/// Hora de la tienda en este instante.
pub async fn reloj(conn: &mut PgConnection) -> Result<String, ApiError> {
    Ok(sqlx::query_scalar(&format!("SELECT {RELOJ_SQL}"))
        .fetch_one(&mut *conn)
        .await?)
}

/// Serializa las escrituras de una caja hasta el fin de la transacción.
pub async fn bloquear(conn: &mut PgConnection, caja: Caja) -> Result<(), ApiError> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(caja.llave_candado())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Un segundo después de `fin` ('YYYY-MM-DD HH:MM:SS'), si se puede leer.
fn segundo_siguiente(fin: &str) -> Option<String> {
    NaiveDateTime::parse_from_str(fin, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|t| (t + chrono::Duration::seconds(1)).format("%Y-%m-%d %H:%M:%S").to_string())
}

/// Momento para una escritura en la caja: el reloj real, pero nunca antes de
/// un segundo después del último corte. Para escrituras se llama DESPUÉS de
/// `bloquear`; así una venta del mismo segundo de un corte cae en el período
/// siguiente y no en uno ya cerrado.
pub async fn ahora_caja(conn: &mut PgConnection, caja: Caja) -> Result<String, ApiError> {
    let r = reloj(&mut *conn).await?;
    let minimo = ultimo_corte(&mut *conn, caja)
        .await?
        .and_then(|u| segundo_siguiente(&u.fin));
    Ok(match minimo {
        Some(m) if m > r => m,
        _ => r,
    })
}

/// Fila de sync_cursor para que el escritorio se entere del cambio.
pub(crate) async fn registrar_cursor(
    conn: &mut PgConnection,
    tabla: &str,
    uuid: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) VALUES ($1, $2, 1, $3)",
    )
    .bind(tabla)
    .bind(uuid)
    .bind(WEB_ORIGIN)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Renglón de bitácora (origen 'WEB'). Dentro de una transacción de Postgres
/// un error ya la abortó, así que se propaga.
pub(crate) async fn bitacora(
    conn: &mut PgConnection,
    usuario_id: i64,
    accion: &str,
    tabla: &str,
    registro_id: Option<i64>,
    descripcion: &str,
    fecha: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, \
                                descripcion_legible, origen, fecha) \
         VALUES ($1, $2, $3, $4, $5, 'WEB', $6)",
    )
    .bind(usuario_id)
    .bind(accion)
    .bind(tabla)
    .bind(registro_id)
    .bind(descripcion)
    .bind(fecha)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Inserta un movimiento de caja hecho en la web (con su sync_cursor).
/// Devuelve (id, uuid).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insertar_movimiento(
    conn: &mut PgConnection,
    caja: Caja,
    tipo: &str,
    usuario_id: i64,
    monto: f64,
    concepto: &str,
    autorizado_por: Option<i64>,
    corte_id: Option<i64>,
    fecha: &str,
) -> Result<(i64, String), ApiError> {
    let uuid = uuid::Uuid::now_v7().to_string();
    let id: i64 = sqlx::query_scalar(&format!(
        "INSERT INTO movimientos_caja \
           (uuid, sucursal_id, tipo, usuario_id, monto, concepto, autorizado_por, \
            corte_id, fecha, origen, caja, updated_at) \
         VALUES ($1, 1, $2, $3, $4, $5, $6, $7, $8, 'web', $9, {NOW_TEXT}) \
         RETURNING id"
    ))
    .bind(&uuid)
    .bind(tipo)
    .bind(usuario_id)
    .bind(monto)
    .bind(concepto)
    .bind(autorizado_por)
    .bind(corte_id)
    .bind(fecha)
    .bind(caja.as_str())
    .fetch_one(&mut *conn)
    .await?;
    registrar_cursor(&mut *conn, "movimientos_caja", &uuid).await?;
    Ok((id, uuid))
}

async fn nombre_usuario(conn: &mut PgConnection, id: i64) -> Result<String, ApiError> {
    let n: Option<String> = sqlx::query_scalar("SELECT nombre_completo FROM usuarios WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(n.unwrap_or_else(|| "Desconocido".to_string()))
}

// ─── Cadena: último corte, inicio y flujo ─────────────────

/// Último eslabón de la cadena: momento en que terminó y fondo que dejó.
#[derive(Clone, Debug)]
pub struct UltimoCorte {
    pub fin: String,
    pub fondo_siguiente: f64,
}

pub async fn ultimo_corte(conn: &mut PgConnection, caja: Caja) -> Result<Option<UltimoCorte>, ApiError> {
    let fila = sqlx::query(&format!(
        "SELECT {FIN_CORTE_SQL} AS fin, fondo_siguiente::float8 AS fondo FROM cortes \
         WHERE caja = $1 AND deleted_at IS NULL \
         ORDER BY 1 DESC, id DESC LIMIT 1"
    ))
    .bind(caja.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    Ok(fila.map(|r| UltimoCorte { fin: r.get("fin"), fondo_siguiente: r.get("fondo") }))
}

/// Desde cuándo cuentan las aperturas de la caja: para la caja 'web', el
/// momento en que este servidor empezó a separar el dinero por caja
/// (sync_meta 'caja_v2_desde', migración 011). Las aperturas web anteriores
/// son las del modal diario del modelo viejo (casi todas en $0) que la
/// migración 010 pasó a esta caja: no definen el fondo de su primer corte ni
/// cuentan como "la apertura de hoy". La caja de la tienda no se filtra
/// (None), igual que si el servidor no tuviera la marca.
pub async fn aperturas_validas_desde(conn: &mut PgConnection, caja: Caja) -> Result<Option<String>, ApiError> {
    if caja != Caja::Web {
        return Ok(None);
    }
    let valor: Option<String> =
        sqlx::query_scalar("SELECT valor FROM sync_meta WHERE clave = 'caja_v2_desde'")
            .fetch_optional(&mut *conn)
            .await?;
    // Misma normalización que FIN_CORTE_SQL: 19 caracteres, sin 'T'.
    Ok(valor
        .map(|v| v.trim().chars().take(19).collect::<String>().replace('T', " "))
        .filter(|v| !v.is_empty()))
}

/// Condición SQL "la apertura (alias `a`) cuenta": su fecha normalizada no es
/// anterior a `$n` (NULL = sin filtro). Ver `aperturas_validas_desde`.
fn apertura_valida_sql(n: usize) -> String {
    format!("(${n}::text IS NULL OR replace(substr(a.fecha, 1, 19), 'T', ' ') >= ${n}::text)")
}

/// Inicio (exclusivo) del período abierto y fondo con el que arrancó. Sin
/// cortes, el período empieza en el origen y el fondo es lo declarado en la
/// primera apertura de esa caja (en la caja 'web', la primera desde
/// 'caja_v2_desde': las del modelo viejo no cuentan).
pub async fn inicio_y_fondo(conn: &mut PgConnection, caja: Caja) -> Result<(String, f64), ApiError> {
    if let Some(u) = ultimo_corte(&mut *conn, caja).await? {
        return Ok((u.fin, u.fondo_siguiente));
    }
    let desde = aperturas_validas_desde(&mut *conn, caja).await?;
    let fondo: Option<f64> = sqlx::query_scalar(&format!(
        "SELECT a.fondo_declarado::float8 FROM aperturas_caja a \
         WHERE a.deleted_at IS NULL AND a.caja = $1 AND {} \
         ORDER BY a.fecha ASC, a.id ASC LIMIT 1",
        apertura_valida_sql(2)
    ))
    .bind(caja.as_str())
    .bind(&desde)
    .fetch_optional(&mut *conn)
    .await?;
    Ok((INICIO_SIN_CORTES.to_string(), fondo.unwrap_or(0.0)))
}

/// Efectivo que entró y salió de la caja en (inicio, fin].
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FlujoEfectivo {
    pub ventas_efectivo: f64,
    pub entradas: f64,
    pub retiros: f64,
}

pub(crate) async fn flujo_efectivo(
    conn: &mut PgConnection,
    caja: Caja,
    inicio: &str,
    fin: &str,
) -> Result<FlujoEfectivo, ApiError> {
    // Se suma en numeric (exacto) y se convierte una sola vez.
    let r = sqlx::query(&format!(
        "SELECT \
           (SELECT COALESCE(SUM(total), 0)::float8 FROM ventas \
             WHERE caja = $1 AND deleted_at IS NULL AND anulada = 0 \
               AND metodo_pago = 'efectivo' AND {mo} > $2 AND {mo} <= $3) AS ventas, \
           COALESCE(SUM(monto) FILTER (WHERE tipo = 'ENTRADA'), 0)::float8 AS entradas, \
           COALESCE(SUM(monto) FILTER (WHERE tipo = 'RETIRO'), 0)::float8 AS retiros \
         FROM movimientos_caja \
         WHERE caja = $1 AND corte_id IS NULL AND deleted_at IS NULL \
           AND {mo} > $2 AND {mo} <= $3",
        mo = MOMENTO_SQL
    ))
    .bind(caja.as_str())
    .bind(inicio)
    .bind(fin)
    .fetch_one(&mut *conn)
    .await?;
    Ok(FlujoEfectivo {
        ventas_efectivo: r.get("ventas"),
        entradas: r.get("entradas"),
        retiros: r.get("retiros"),
    })
}

/// ¿La caja tiene dinero del período abierto sin cortar (ventas en efectivo,
/// entradas o retiros)?
pub async fn hay_efectivo_sin_cortar(conn: &mut PgConnection, caja: Caja) -> Result<bool, ApiError> {
    let (inicio, _) = inicio_y_fondo(&mut *conn, caja).await?;
    let ahora = ahora_caja(&mut *conn, caja).await?;
    let f = flujo_efectivo(&mut *conn, caja, &inicio, &ahora).await?;
    Ok(f.ventas_efectivo.abs() > EPS || f.entradas.abs() > EPS || f.retiros.abs() > EPS)
}

// ─── Autorización de retiros ──────────────────────────────

/// Valida la autorización de un retiro y devuelve quién lo autorizó.
///
/// Retiros mayores a RETIRO_LIMITE_SIN_PIN requieren el PIN de un dueño
/// (roles.es_admin = 1, activo, no eliminado), resuelto aquí. Si quien retira
/// es admin, se autoriza a sí mismo. Debajo del límite no hay autorización
/// (el id que mande el navegador no se guarda).
pub async fn resolver_autorizacion_retiro(
    conn: &mut PgConnection,
    usuario_id: i64,
    monto: f64,
    pin: Option<&str>,
) -> Result<Option<i64>, ApiError> {
    if monto <= RETIRO_LIMITE_SIN_PIN {
        return Ok(None);
    }

    let es_admin: Option<i32> = sqlx::query_scalar(
        "SELECT COALESCE(r.es_admin, 0) FROM usuarios u JOIN roles r ON r.id = u.rol_id WHERE u.id = $1",
    )
    .bind(usuario_id)
    .fetch_optional(&mut *conn)
    .await?;
    if es_admin == Some(1) {
        return Ok(Some(usuario_id));
    }

    let pin = pin.unwrap_or("").trim();
    if pin.is_empty() {
        return Err(mal(format!(
            "Retiros mayores a ${:.0} requieren PIN del dueño",
            RETIRO_LIMITE_SIN_PIN
        )));
    }

    let duenos = sqlx::query(
        "SELECT u.id, u.pin FROM usuarios u JOIN roles r ON r.id = u.rol_id \
         WHERE r.es_admin = 1 AND u.activo = 1 AND u.deleted_at IS NULL",
    )
    .fetch_all(&mut *conn)
    .await?;
    for r in &duenos {
        let hash: String = r.get("pin");
        if bcrypt::verify(pin, &hash).unwrap_or(false) {
            return Ok(Some(r.get::<i64, _>("id")));
        }
    }
    Err(mal("PIN del dueño incorrecto"))
}

// ─── Movimientos ──────────────────────────────────────────

const SELECT_MOVIMIENTOS: &str =
    "SELECT m.id, m.tipo, m.usuario_id, COALESCE(u.nombre_completo, 'Desconocido') AS usuario_nombre, \
            m.monto::float8 AS monto, m.concepto, m.autorizado_por, m.corte_id, m.fecha \
     FROM movimientos_caja m \
     LEFT JOIN usuarios u ON u.id = m.usuario_id";

fn movimiento_de_fila(r: &PgRow) -> MovimientoCaja {
    MovimientoCaja {
        id: r.get("id"),
        tipo: r.get("tipo"),
        usuario_id: r.get("usuario_id"),
        usuario_nombre: r.get("usuario_nombre"),
        monto: r.get("monto"),
        concepto: r.get("concepto"),
        autorizado_por: r.get("autorizado_por"),
        corte_id: r.get("corte_id"),
        fecha: r.get("fecha"),
    }
}

/// Movimientos sin corte de la caja con MOMENTO en (inicio, fin], en orden.
async fn movimientos_del_periodo(
    conn: &mut PgConnection,
    caja: Caja,
    inicio: &str,
    fin: &str,
) -> Result<Vec<MovimientoCaja>, ApiError> {
    let filas = sqlx::query(&format!(
        "{SELECT_MOVIMIENTOS} \
         WHERE m.caja = $1 AND m.corte_id IS NULL AND m.deleted_at IS NULL \
           AND {mo} > $2 AND {mo} <= $3 \
         ORDER BY {mo} ASC, m.id ASC",
        mo = MOMENTO_M_SQL
    ))
    .bind(caja.as_str())
    .bind(inicio)
    .bind(fin)
    .fetch_all(&mut *conn)
    .await?;
    Ok(filas.iter().map(movimiento_de_fila).collect())
}

/// Movimientos del período en curso (sin corte, posteriores al último
/// corte), más recientes primero. Los huérfanos viejos del modelo anterior
/// no se listan: ya no pueden entrar en ningún corte (ver la auditoría).
pub async fn movimientos_sin_corte(conn: &mut PgConnection, caja: Caja) -> Result<Vec<MovimientoCaja>, ApiError> {
    let (inicio, _) = inicio_y_fondo(&mut *conn, caja).await?;
    let filas = sqlx::query(&format!(
        "{SELECT_MOVIMIENTOS} \
         WHERE m.caja = $1 AND m.corte_id IS NULL AND m.deleted_at IS NULL AND {mo} > $2 \
         ORDER BY {mo} DESC, m.id DESC",
        mo = MOMENTO_M_SQL
    ))
    .bind(caja.as_str())
    .bind(&inicio)
    .fetch_all(&mut *conn)
    .await?;
    Ok(filas.iter().map(movimiento_de_fila).collect())
}

// ─── Núcleo: cálculo del período abierto ──────────────────

/// Calcula todo lo que un corte hecho en `ahora` debe cubrir. Es la ÚNICA
/// fuente del esperado: la usan la vista previa y la confirmación.
pub async fn calcular_periodo(conn: &mut PgConnection, caja: Caja, ahora: &str) -> Result<DatosCorte, ApiError> {
    let (inicio, fondo_inicial) = inicio_y_fondo(&mut *conn, caja).await?;
    let fin = ahora.to_string();

    let flujo = flujo_efectivo(&mut *conn, caja, &inicio, &fin).await?;

    let t = sqlx::query(&format!(
        "SELECT \
           COALESCE(SUM(total) FILTER (WHERE metodo_pago = 'tarjeta'), 0)::float8 AS tarjeta, \
           COALESCE(SUM(total) FILTER (WHERE metodo_pago = 'transferencia'), 0)::float8 AS transferencia, \
           COUNT(*)::bigint AS n, \
           COALESCE(SUM(total), 0)::float8 AS total, \
           COALESCE(SUM(descuento), 0)::float8 AS descuentos \
         FROM ventas \
         WHERE caja = $1 AND deleted_at IS NULL AND anulada = 0 AND {mo} > $2 AND {mo} <= $3",
        mo = MOMENTO_SQL
    ))
    .bind(caja.as_str())
    .bind(&inicio)
    .bind(&fin)
    .fetch_one(&mut *conn)
    .await?;

    let total_anulaciones: f64 = sqlx::query_scalar(&format!(
        "SELECT COALESCE(SUM(total), 0)::float8 FROM ventas \
         WHERE caja = $1 AND deleted_at IS NULL AND anulada = 1 AND {mo} > $2 AND {mo} <= $3",
        mo = MOMENTO_SQL
    ))
    .bind(caja.as_str())
    .bind(&inicio)
    .bind(&fin)
    .fetch_one(&mut *conn)
    .await?;

    let movimientos = movimientos_del_periodo(&mut *conn, caja, &inicio, &fin).await?;

    let efectivo_esperado =
        round2(fondo_inicial + flujo.ventas_efectivo + flujo.entradas - flujo.retiros);

    // Informativo: cortes de turno de hoy (día de `ahora`) y lo retirado en
    // ellos (contado − fondo que se dejó).
    let dia = fin.get(..10).unwrap_or(&fin).to_string();
    let p = sqlx::query(&format!(
        "SELECT COUNT(*)::bigint AS n, \
                COALESCE(SUM(efectivo_contado - fondo_siguiente), 0)::float8 AS retirado \
         FROM cortes \
         WHERE tipo = 'PARCIAL' AND deleted_at IS NULL AND caja = $1 \
           AND substr({FIN_CORTE_SQL}, 1, 10) = $2"
    ))
    .bind(caja.as_str())
    .bind(&dia)
    .fetch_one(&mut *conn)
    .await?;

    // hora_inicio/hora_fin muestran la fecha real de la venta; el rango usa MOMENTO.
    let vend = sqlx::query(&format!(
        "SELECT v.usuario_id, COALESCE(u.nombre_completo, 'Desconocido') AS usuario_nombre, \
                COUNT(*)::bigint AS n, COALESCE(SUM(v.total), 0)::float8 AS total, \
                MIN(v.fecha) AS hora_inicio, MAX(v.fecha) AS hora_fin \
         FROM ventas v \
         LEFT JOIN usuarios u ON u.id = v.usuario_id \
         WHERE v.caja = $1 AND v.deleted_at IS NULL AND v.anulada = 0 \
           AND {mo} > $2 AND {mo} <= $3 \
         GROUP BY v.usuario_id, u.nombre_completo \
         ORDER BY 4 DESC, 1 ASC",
        mo = MOMENTO_V_SQL
    ))
    .bind(caja.as_str())
    .bind(&inicio)
    .bind(&fin)
    .fetch_all(&mut *conn)
    .await?;
    let vendedores = vend
        .iter()
        .map(|r| VendedorResumen {
            usuario_id: r.get("usuario_id"),
            usuario_nombre: r.get("usuario_nombre"),
            num_ventas: r.get("n"),
            total_vendido: r.get("total"),
            hora_inicio: r.get("hora_inicio"),
            hora_fin: r.get("hora_fin"),
        })
        .collect();

    Ok(DatosCorte {
        fecha_inicio: inicio,
        fecha_fin: fin,
        fondo_inicial,
        total_ventas_efectivo: flujo.ventas_efectivo,
        total_ventas_tarjeta: t.get("tarjeta"),
        total_ventas_transferencia: t.get("transferencia"),
        total_ventas: t.get("total"),
        num_transacciones: t.get("n"),
        total_descuentos: t.get("descuentos"),
        total_anulaciones,
        total_entradas_efectivo: flujo.entradas,
        total_retiros_efectivo: flujo.retiros,
        efectivo_esperado,
        cortes_parciales_hoy: p.get("n"),
        total_retirado_parciales: p.get("retirado"),
        movimientos,
        vendedores,
    })
}

// ─── Crear corte ──────────────────────────────────────────

/// Guarda un corte de la caja 'web'. Debe llamarse dentro de una transacción
/// que ya tomó `bloquear(caja)`, con `ahora` = `ahora_caja`. Recalcula todo
/// en `ahora` y rechaza si el esperado ya no coincide con lo que vio el
/// cajero.
pub async fn crear_corte_core(
    conn: &mut PgConnection,
    caja: Caja,
    usuario_id: i64,
    datos: NuevoCorte,
    ahora: &str,
) -> Result<CorteCreado, ApiError> {
    if caja != Caja::Web {
        return Err(mal(MSG_PRINCIPAL_SOLO_ESCRITORIO));
    }
    if datos.tipo != "PARCIAL" && datos.tipo != "DIA" {
        return Err(mal("Tipo de corte inválido"));
    }
    if !datos.efectivo_contado.is_finite() || datos.efectivo_contado < 0.0 {
        return Err(mal("El efectivo contado no puede ser negativo"));
    }
    if datos.efectivo_contado > crate::venta_calc::MAX_IMPORTE {
        return Err(mal("El efectivo contado es demasiado grande"));
    }
    let contado = round2(datos.efectivo_contado);
    let dia = ahora.get(..10).unwrap_or(ahora);

    // Un solo cierre del día por fecha calendario (del momento del conteo).
    if datos.tipo == "DIA" {
        let existe: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*)::bigint FROM cortes WHERE tipo = 'DIA' AND deleted_at IS NULL \
             AND caja = $1 AND substr({FIN_CORTE_SQL}, 1, 10) = $2"
        ))
        .bind(caja.as_str())
        .bind(dia)
        .fetch_one(&mut *conn)
        .await?;
        if existe > 0 {
            return Err(mal(format!(
                "Ya se hizo el cierre de caja del {}. Las ventas posteriores se incluirán \
                 en el próximo corte.",
                dia
            )));
        }
    }

    let calc = calcular_periodo(&mut *conn, caja, ahora).await?;

    // El cajero contó contra un esperado concreto; si entró una venta o un
    // movimiento mientras contaba, ese esperado ya es otro.
    let esperado_cliente = datos.datos.efectivo_esperado;
    if !esperado_cliente.is_finite() || (calc.efectivo_esperado - round2(esperado_cliente)).abs() > EPS {
        return Err(mal(format!(
            "{}: El efectivo esperado cambió de ${:.2} a ${:.2} porque se registraron \
             ventas o movimientos mientras hacías el corte. Revisa el nuevo esperado y \
             vuelve a confirmar.",
            ERR_DATOS_CAMBIARON, esperado_cliente, calc.efectivo_esperado
        )));
    }

    // Retiro al momento del corte (solo de turno) y fondo que queda.
    let (fondo_siguiente, retiro_validado) = match &datos.retiro {
        Some(r) if r.monto.is_finite() && r.monto > EPS => {
            if datos.tipo != "PARCIAL" {
                return Err(mal("El retiro solo aplica en cortes de turno"));
            }
            let monto = round2(r.monto);
            if monto > contado + EPS {
                return Err(mal("No puedes retirar más efectivo del que hay contado en caja"));
            }
            if r.concepto.trim().is_empty() {
                return Err(mal("El concepto del retiro es obligatorio"));
            }
            let autorizado = resolver_autorizacion_retiro(
                &mut *conn,
                usuario_id,
                monto,
                r.pin_autorizacion.as_deref(),
            )
            .await?;
            (round2(contado - monto), Some((monto, r.concepto.trim().to_string(), autorizado)))
        }
        _ => {
            if !datos.fondo_siguiente.is_finite() {
                return Err(mal("El fondo que se deja no es válido"));
            }
            let f = round2(datos.fondo_siguiente);
            if f < 0.0 {
                return Err(mal("El fondo que se deja no puede ser negativo"));
            }
            if f > contado + EPS {
                return Err(mal(format!(
                    "No puedes dejar ${:.2} de fondo si solo contaste ${:.2}",
                    f, contado
                )));
            }
            (f, None)
        }
    };

    let diferencia = round2(contado - calc.efectivo_esperado);
    let nota = datos
        .nota_diferencia
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    if diferencia.abs() > EPS && nota.is_none() {
        return Err(mal("La nota es obligatoria cuando hay diferencia de dinero"));
    }

    let corte_uuid = uuid::Uuid::now_v7().to_string();
    let corte_id: i64 = sqlx::query_scalar(&format!(
        "INSERT INTO cortes \
           (uuid, sucursal_id, tipo, usuario_id, fecha_inicio, fecha_fin, \
            fondo_inicial, total_ventas_efectivo, total_ventas_tarjeta, \
            total_ventas_transferencia, total_ventas, num_transacciones, \
            total_descuentos, total_anulaciones, total_entradas_efectivo, \
            total_retiros_efectivo, efectivo_esperado, efectivo_contado, \
            diferencia, nota_diferencia, fondo_siguiente, origen, caja, \
            created_at, updated_at) \
         VALUES ($1, 1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, \
                 $16, $17, $18, $19, $20, 'web', $21, $5, {NOW_TEXT}) \
         RETURNING id"
    ))
    .bind(&corte_uuid)
    .bind(&datos.tipo)
    .bind(usuario_id)
    .bind(&calc.fecha_inicio)
    .bind(ahora)
    .bind(calc.fondo_inicial)
    .bind(calc.total_ventas_efectivo)
    .bind(calc.total_ventas_tarjeta)
    .bind(calc.total_ventas_transferencia)
    .bind(calc.total_ventas)
    .bind(calc.num_transacciones)
    .bind(calc.total_descuentos)
    .bind(calc.total_anulaciones)
    .bind(calc.total_entradas_efectivo)
    .bind(calc.total_retiros_efectivo)
    .bind(calc.efectivo_esperado)
    .bind(contado)
    .bind(diferencia)
    .bind(&nota)
    .bind(fondo_siguiente)
    .bind(caja.as_str())
    .fetch_one(&mut *conn)
    .await?;

    // Asignar exactamente los movimientos que entraron en el cálculo (mismo
    // predicado, misma transacción).
    let asignados: Vec<String> = sqlx::query_scalar(&format!(
        "UPDATE movimientos_caja SET corte_id = $1, updated_at = {NOW_TEXT} \
         WHERE caja = $2 AND corte_id IS NULL AND deleted_at IS NULL \
           AND {mo} > $3 AND {mo} <= $4 \
         RETURNING uuid",
        mo = MOMENTO_SQL
    ))
    .bind(corte_id)
    .bind(caja.as_str())
    .bind(&calc.fecha_inicio)
    .bind(ahora)
    .fetch_all(&mut *conn)
    .await?;
    for u in &asignados {
        registrar_cursor(&mut *conn, "movimientos_caja", u).await?;
    }

    if let Some(denoms) = &datos.denominaciones {
        for d in denoms.iter().filter(|d| d.cantidad > 0) {
            if !d.denominacion.is_finite() || d.denominacion <= 0.0 || d.cantidad > 1_000_000 {
                return Err(mal("Denominación inválida"));
            }
            sqlx::query(&format!(
                "INSERT INTO corte_denominaciones \
                   (uuid, corte_id, denominacion, tipo, cantidad, subtotal, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, {NOW_TEXT})"
            ))
            .bind(uuid::Uuid::now_v7().to_string())
            .bind(corte_id)
            .bind(d.denominacion)
            .bind(&d.tipo)
            .bind(d.cantidad)
            .bind(round2(d.denominacion * d.cantidad as f64))
            .execute(&mut *conn)
            .await?;
        }
    }

    if datos.tipo == "DIA" {
        for v in &calc.vendedores {
            sqlx::query(&format!(
                "INSERT INTO corte_vendedores \
                   (uuid, corte_id, usuario_id, num_ventas, total_vendido, hora_inicio, hora_fin, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, {NOW_TEXT})"
            ))
            .bind(uuid::Uuid::now_v7().to_string())
            .bind(corte_id)
            .bind(v.usuario_id)
            .bind(v.num_ventas)
            .bind(v.total_vendido)
            .bind(&v.hora_inicio)
            .bind(&v.hora_fin)
            .execute(&mut *conn)
            .await?;
        }
    }

    // El retiro se inserta DESPUÉS de calcular y ya ligado a este corte: no
    // altera su esperado (se contó antes de sacar el dinero) y tampoco entra
    // al siguiente período (que arranca con fondo_siguiente, ya neto).
    if let Some((monto, concepto, autorizado)) = &retiro_validado {
        insertar_movimiento(
            &mut *conn, caja, "RETIRO", usuario_id, *monto, concepto, *autorizado,
            Some(corte_id), ahora,
        )
        .await?;
    }

    let accion = if datos.tipo == "DIA" { "CORTE_DIA" } else { "CORTE_PARCIAL" };
    let desc = format!(
        "{} — Esperado: ${:.2} / Contado: ${:.2} / Diferencia: ${:.2} / Fondo que queda: ${:.2}",
        accion, calc.efectivo_esperado, contado, diferencia, fondo_siguiente
    );
    bitacora(&mut *conn, usuario_id, accion, "cortes", Some(corte_id), &desc, ahora).await?;
    registrar_cursor(&mut *conn, "cortes", &corte_uuid).await?;

    Ok(CorteCreado {
        id: corte_id,
        tipo: datos.tipo.clone(),
        diferencia,
        efectivo_esperado: calc.efectivo_esperado,
        efectivo_contado: contado,
        fondo_siguiente,
        created_at: ahora.to_string(),
    })
}

// ─── Apertura: conteo de verificación ─────────────────────

pub async fn calcular_esperado_apertura(
    conn: &mut PgConnection,
    caja: Caja,
    ahora: &str,
) -> Result<EsperadoApertura, ApiError> {
    match ultimo_corte(&mut *conn, caja).await? {
        None => Ok(EsperadoApertura {
            esperado: 0.0,
            fondo_ultimo_corte: 0.0,
            flujo_desde_corte: 0.0,
            hay_referencia: false,
            ultimo_corte_fecha: None,
        }),
        Some(u) => {
            let f = flujo_efectivo(&mut *conn, caja, &u.fin, ahora).await?;
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

/// Registra la apertura del día de la caja 'web'. Dentro de una transacción
/// con `bloquear(caja)` tomado y `ahora` = `ahora_caja`. Si lo contado no
/// coincide con lo esperado exige nota y registra un movimiento de ajuste
/// que entra en el período en curso.
pub async fn crear_apertura_core(
    conn: &mut PgConnection,
    caja: Caja,
    usuario_id: i64,
    datos: NuevaApertura,
    ahora: &str,
) -> Result<AperturaCaja, ApiError> {
    if caja != Caja::Web {
        return Err(mal(MSG_PRINCIPAL_SOLO_ESCRITORIO));
    }
    if !datos.fondo_declarado.is_finite() || datos.fondo_declarado < 0.0 {
        return Err(mal("El fondo no puede ser negativo"));
    }
    if datos.fondo_declarado > crate::venta_calc::MAX_IMPORTE {
        return Err(mal("El fondo es demasiado grande"));
    }
    let declarado = round2(datos.fondo_declarado);
    let dia = ahora.get(..10).unwrap_or(ahora);

    // Una apertura del modelo viejo (anterior a 'caja_v2_desde') no cuenta
    // como la de hoy: si no, el día del cambio no se podría declarar el fondo.
    let desde = aperturas_validas_desde(&mut *conn, caja).await?;
    let existe: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*)::bigint FROM aperturas_caja a \
         WHERE a.deleted_at IS NULL AND a.caja = $1 AND substr(a.fecha, 1, 10) = $2 AND {}",
        apertura_valida_sql(3)
    ))
    .bind(caja.as_str())
    .bind(dia)
    .bind(&desde)
    .fetch_one(&mut *conn)
    .await?;
    if existe > 0 {
        return Err(mal("Ya existe una apertura de caja para hoy"));
    }

    let esp = calcular_esperado_apertura(&mut *conn, caja, ahora).await?;
    let diferencia = if esp.hay_referencia { round2(declarado - esp.esperado) } else { 0.0 };
    let nota = datos
        .nota
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    if diferencia.abs() > EPS && nota.is_none() {
        return Err(mal(format!(
            "Contaste ${:.2} pero debería haber ${:.2} ({} ${:.2}). Escribe una nota explicando la diferencia.",
            declarado,
            esp.esperado,
            if diferencia < 0.0 { "faltan" } else { "sobran" },
            diferencia.abs()
        )));
    }

    let apertura_uuid = uuid::Uuid::now_v7().to_string();
    let id: i64 = sqlx::query_scalar(&format!(
        "INSERT INTO aperturas_caja \
           (uuid, sucursal_id, usuario_id, fondo_declarado, nota, fecha, origen, caja, updated_at) \
         VALUES ($1, 1, $2, $3, $4, $5, 'web', $6, {NOW_TEXT}) RETURNING id"
    ))
    .bind(&apertura_uuid)
    .bind(usuario_id)
    .bind(declarado)
    .bind(&nota)
    .bind(ahora)
    .bind(caja.as_str())
    .fetch_one(&mut *conn)
    .await?;
    registrar_cursor(&mut *conn, "aperturas_caja", &apertura_uuid).await?;

    // La diferencia de la noche queda como movimiento explícito del período
    // en curso, a nombre de quien abrió.
    if diferencia.abs() > EPS {
        let (tipo, que) = if diferencia > 0.0 { ("ENTRADA", "sobraban") } else { ("RETIRO", "faltaban") };
        let concepto = format!(
            "Ajuste de apertura: {} ${:.2} respecto a lo esperado (${:.2}) — {}",
            que,
            diferencia.abs(),
            esp.esperado,
            nota.as_deref().unwrap_or("")
        );
        insertar_movimiento(
            &mut *conn, caja, tipo, usuario_id, diferencia.abs(), &concepto, None, None, ahora,
        )
        .await?;
    }

    let desc = if diferencia.abs() > EPS {
        format!(
            "Apertura de caja: contado ${:.2}, esperado ${:.2}, diferencia ${:.2}",
            declarado, esp.esperado, diferencia
        )
    } else {
        format!("Apertura de caja con fondo de ${:.2}", declarado)
    };
    bitacora(&mut *conn, usuario_id, "APERTURA_CAJA", "aperturas_caja", Some(id), &desc, ahora).await?;

    let usuario_nombre = nombre_usuario(&mut *conn, usuario_id).await?;
    Ok(AperturaCaja {
        id,
        usuario_id,
        usuario_nombre,
        fondo_declarado: declarado,
        nota,
        fecha: ahora.to_string(),
    })
}

/// Apertura de la caja registrada en `dia` (YYYY-MM-DD), si existe. En la
/// caja 'web' no cuentan las del modelo viejo (anteriores a 'caja_v2_desde').
pub async fn apertura_del_dia(
    conn: &mut PgConnection,
    caja: Caja,
    dia: &str,
) -> Result<Option<AperturaCaja>, ApiError> {
    let desde = aperturas_validas_desde(&mut *conn, caja).await?;
    let r = sqlx::query(&format!(
        "SELECT a.id, a.usuario_id, COALESCE(u.nombre_completo, 'Desconocido') AS usuario_nombre, \
                a.fondo_declarado::float8 AS fondo, a.nota, a.fecha \
         FROM aperturas_caja a \
         LEFT JOIN usuarios u ON u.id = a.usuario_id \
         WHERE a.deleted_at IS NULL AND a.caja = $1 AND substr(a.fecha, 1, 10) = $2 AND {} \
         ORDER BY a.id DESC LIMIT 1",
        apertura_valida_sql(3)
    ))
    .bind(caja.as_str())
    .bind(dia)
    .bind(&desde)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(r.map(|r| AperturaCaja {
        id: r.get("id"),
        usuario_id: r.get("usuario_id"),
        usuario_nombre: r.get("usuario_nombre"),
        fondo_declarado: r.get("fondo"),
        nota: r.get("nota"),
        fecha: r.get("fecha"),
    }))
}

/// Primera fecha (YYYY-MM-DD) anterior a `hoy` con ventas que todavía no
/// entran en ningún corte. None si todo lo de días anteriores ya se cortó.
pub async fn primer_dia_pendiente(
    conn: &mut PgConnection,
    caja: Caja,
    hoy: &str,
) -> Result<Option<String>, ApiError> {
    let (inicio, _) = inicio_y_fondo(&mut *conn, caja).await?;
    let dia: Option<String> = sqlx::query_scalar(&format!(
        "SELECT MIN(substr({mo}, 1, 10)) FROM ventas \
         WHERE caja = $1 AND deleted_at IS NULL AND anulada = 0 \
           AND {mo} > $2 AND substr({mo}, 1, 10) < $3",
        mo = MOMENTO_SQL
    ))
    .bind(caja.as_str())
    .bind(&inicio)
    .bind(hoy)
    .fetch_one(&mut *conn)
    .await?;
    Ok(dia)
}

// ─── Historial de cortes ──────────────────────────────────

const SELECT_CORTE_RESUMEN: &str =
    "SELECT c.id, c.tipo, COALESCE(u.nombre_completo, 'Desconocido') AS usuario_nombre, c.created_at, \
            c.fondo_inicial::float8 AS fondo_inicial, \
            c.total_ventas_efectivo::float8 AS total_ventas_efectivo, \
            c.total_ventas_tarjeta::float8 AS total_ventas_tarjeta, \
            c.total_ventas_transferencia::float8 AS total_ventas_transferencia, \
            c.total_ventas::float8 AS total_ventas, \
            c.num_transacciones::bigint AS num_transacciones, \
            c.total_entradas_efectivo::float8 AS total_entradas_efectivo, \
            c.total_retiros_efectivo::float8 AS total_retiros_efectivo, \
            c.efectivo_esperado::float8 AS efectivo_esperado, \
            c.efectivo_contado::float8 AS efectivo_contado, \
            c.diferencia::float8 AS diferencia, c.nota_diferencia, \
            c.fondo_siguiente::float8 AS fondo_siguiente \
     FROM cortes c \
     LEFT JOIN usuarios u ON u.id = c.usuario_id";

fn corte_de_fila(r: &PgRow) -> CorteResumen {
    CorteResumen {
        id: r.get("id"),
        tipo: r.get("tipo"),
        usuario_nombre: r.get("usuario_nombre"),
        created_at: r.get("created_at"),
        fondo_inicial: r.get("fondo_inicial"),
        total_ventas_efectivo: r.get("total_ventas_efectivo"),
        total_ventas_tarjeta: r.get("total_ventas_tarjeta"),
        total_ventas_transferencia: r.get("total_ventas_transferencia"),
        total_ventas: r.get("total_ventas"),
        num_transacciones: r.get("num_transacciones"),
        total_entradas_efectivo: r.get("total_entradas_efectivo"),
        total_retiros_efectivo: r.get("total_retiros_efectivo"),
        efectivo_esperado: r.get("efectivo_esperado"),
        efectivo_contado: r.get("efectivo_contado"),
        diferencia: r.get("diferencia"),
        nota_diferencia: r.get("nota_diferencia"),
        fondo_siguiente: r.get("fondo_siguiente"),
    }
}

/// Cortes de la caja, más recientes primero.
pub async fn listar_cortes(conn: &mut PgConnection, caja: Caja, limite: i64) -> Result<Vec<CorteResumen>, ApiError> {
    let filas = sqlx::query(&format!(
        "{SELECT_CORTE_RESUMEN} \
         WHERE c.deleted_at IS NULL AND c.caja = $1 \
         ORDER BY c.created_at DESC, c.id DESC \
         LIMIT $2"
    ))
    .bind(caja.as_str())
    .bind(limite.clamp(1, 1000))
    .fetch_all(&mut *conn)
    .await?;
    Ok(filas.iter().map(corte_de_fila).collect())
}

/// Detalle completo de un corte (denominaciones + movimientos + vendedores).
pub async fn detalle_corte(conn: &mut PgConnection, id: i64) -> Result<CorteDetalle, ApiError> {
    let corte = sqlx::query(&format!("{SELECT_CORTE_RESUMEN} WHERE c.id = $1"))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| corte_de_fila(&r))
        .ok_or_else(|| mal("Corte no encontrado"))?;

    let denominaciones = sqlx::query(
        "SELECT denominacion::float8 AS denominacion, tipo, cantidad::bigint AS cantidad, \
                subtotal::float8 AS subtotal \
         FROM corte_denominaciones WHERE corte_id = $1 AND deleted_at IS NULL \
         ORDER BY denominacion DESC, id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| DenominacionDetalle {
        denominacion: r.get("denominacion"),
        tipo: r.get("tipo"),
        cantidad: r.get("cantidad"),
        subtotal: r.get("subtotal"),
    })
    .collect();

    let movimientos = sqlx::query(&format!(
        "{SELECT_MOVIMIENTOS} WHERE m.corte_id = $1 AND m.deleted_at IS NULL \
         ORDER BY {mo} ASC, m.id ASC",
        mo = MOMENTO_M_SQL
    ))
    .bind(id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(movimiento_de_fila)
    .collect();

    let vendedores = sqlx::query(
        "SELECT cv.usuario_id, COALESCE(u.nombre_completo, 'Desconocido') AS usuario_nombre, \
                cv.num_ventas::bigint AS num_ventas, cv.total_vendido::float8 AS total_vendido, \
                cv.hora_inicio, cv.hora_fin \
         FROM corte_vendedores cv \
         LEFT JOIN usuarios u ON u.id = cv.usuario_id \
         WHERE cv.corte_id = $1 AND cv.deleted_at IS NULL \
         ORDER BY cv.total_vendido DESC, cv.id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| VendedorResumen {
        usuario_id: r.get("usuario_id"),
        usuario_nombre: r.get("usuario_nombre"),
        num_ventas: r.get("num_ventas"),
        total_vendido: r.get("total_vendido"),
        hora_inicio: r.get("hora_inicio"),
        hora_fin: r.get("hora_fin"),
    })
    .collect();

    Ok(CorteDetalle { corte, denominaciones, movimientos, vendedores })
}

// ─── Auditoría (puerto de auditoria_caja.rs) ──────────────
//
// Diagnóstico de solo lectura de los descuadres históricos de una caja:
//   1. ventas sin corte (anteriores al último corte, ya no recuperables);
//   2. movimientos asignados a un corte cuyo rango no contiene su momento;
//   3. movimientos huérfanos (sin corte, anteriores al período abierto);
//   4. cadena rota (fondo inicial ≠ fondo que dejó el corte anterior);
//   5. cortes cuya aritmética no da su esperado;
//   6. días con ventas sin cierre del día (informativo).

#[derive(Serialize, Clone, Debug, Default)]
pub struct VentaSinCorte {
    pub id: i64,
    pub folio: String,
    pub fecha: String,
    pub total: f64,
    pub metodo_pago: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct MovimientoDesalineado {
    pub id: i64,
    pub tipo: String,
    pub monto: f64,
    pub concepto: String,
    pub fecha: String,
    pub corte_id: i64,
    pub corte_tipo: String,
    pub corte_inicio: String,
    pub corte_fin: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct MovimientoHuerfano {
    pub id: i64,
    pub tipo: String,
    pub monto: f64,
    pub concepto: String,
    pub fecha: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct EslabonRoto {
    pub corte_id: i64,
    pub tipo: String,
    pub fecha: String,
    pub fondo_anterior: f64,
    pub fondo_inicial: f64,
    pub diferencia: f64,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct CorteInconsistente {
    pub id: i64,
    pub tipo: String,
    pub created_at: String,
    pub esperado_guardado: f64,
    pub esperado_recalculado: f64,
    pub descuadre: f64,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct DiaSinCierre {
    pub dia: String,
    pub num_ventas: i64,
    pub total_ventas: f64,
    pub total_efectivo: f64,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct ResumenAuditoria {
    pub ventas_sin_corte_count: i64,
    pub ventas_sin_corte_efectivo: f64,
    pub movimientos_desalineados_count: i64,
    pub movimientos_desalineados_monto: f64,
    pub movimientos_huerfanos_count: i64,
    pub movimientos_huerfanos_monto: f64,
    pub eslabones_rotos_count: i64,
    pub eslabones_rotos_monto: f64,
    pub cortes_inconsistentes_count: i64,
    pub dias_sin_cierre_count: i64,
    /// Suma con signo del efectivo que el modelo anterior dejó fuera de la
    /// conciliación. Positivo = la caja debería tener más; negativo = menos.
    pub impacto_total_estimado: f64,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct AuditoriaCaja {
    pub resumen: ResumenAuditoria,
    pub ventas_sin_corte: Vec<VentaSinCorte>,
    pub movimientos_desalineados: Vec<MovimientoDesalineado>,
    pub movimientos_huerfanos: Vec<MovimientoHuerfano>,
    pub cadena_rota: Vec<EslabonRoto>,
    pub cortes_inconsistentes: Vec<CorteInconsistente>,
    pub dias_sin_cierre: Vec<DiaSinCierre>,
}

pub async fn auditar(conn: &mut PgConnection, caja: Caja, dias: i64) -> Result<AuditoriaCaja, ApiError> {
    let dias = dias.clamp(1, 3650) as i32;
    let fechas = sqlx::query(&format!(
        "SELECT to_char((clock_timestamp() AT TIME ZONE 'America/Mexico_City')::date - $1::int, \
                        'YYYY-MM-DD') AS desde, \
                substr({RELOJ_SQL}, 1, 10) AS hoy"
    ))
    .bind(dias)
    .fetch_one(&mut *conn)
    .await?;
    let desde: String = fechas.get("desde");
    let hoy: String = fechas.get("hoy");

    // Lo posterior al último corte es el período abierto: el próximo corte lo
    // incluirá, no es una fuga.
    let (inicio_abierto, _) = inicio_y_fondo(&mut *conn, caja).await?;
    let c = caja.as_str();

    // ── 1. Ventas fuera de todo corte (y ya no recuperables) ──
    let ventas_sin_corte: Vec<VentaSinCorte> = sqlx::query(&format!(
        "SELECT v.id, v.folio, v.fecha, v.total::float8 AS total, v.metodo_pago \
         FROM ventas v \
         WHERE v.anulada = 0 AND v.caja = $1 AND v.deleted_at IS NULL \
           AND substr({mo}, 1, 10) >= $2 AND {mo} <= $3 \
           AND NOT EXISTS ( \
               SELECT 1 FROM cortes c \
               WHERE c.deleted_at IS NULL AND c.caja = $1 \
                 AND {mo} > c.fecha_inicio AND {mo} <= c.fecha_fin) \
         ORDER BY {mo} ASC, v.id ASC",
        mo = MOMENTO_V_SQL
    ))
    .bind(c)
    .bind(&desde)
    .bind(&inicio_abierto)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| VentaSinCorte {
        id: r.get("id"),
        folio: r.get("folio"),
        fecha: r.get("fecha"),
        total: r.get("total"),
        metodo_pago: r.get("metodo_pago"),
    })
    .collect();
    let ventas_sin_corte_efectivo: f64 = ventas_sin_corte
        .iter()
        .filter(|v| v.metodo_pago == "efectivo")
        .map(|v| v.total)
        .sum();

    // ── 2. Movimientos asignados fuera del rango de su corte ──
    // Se excluye el retiro propio de un corte de turno del modelo anterior
    // (ya reflejado en el fondo que dejó ese corte).
    let movimientos_desalineados: Vec<MovimientoDesalineado> = sqlx::query(&format!(
        "SELECT m.id, m.tipo, m.monto::float8 AS monto, m.concepto, m.fecha, \
                c.id AS corte_id, c.tipo AS corte_tipo, c.fecha_inicio, c.fecha_fin \
         FROM movimientos_caja m \
         JOIN cortes c ON c.id = m.corte_id \
         WHERE substr({mo}, 1, 10) >= $2 \
           AND m.caja = $1 AND c.caja = $1 \
           AND m.deleted_at IS NULL AND c.deleted_at IS NULL \
           AND NOT ({mo} > c.fecha_inicio AND {mo} <= c.fecha_fin) \
           AND NOT (c.tipo = 'PARCIAL' AND m.tipo = 'RETIRO' \
                    AND ABS(m.monto - (c.efectivo_contado - c.fondo_siguiente)) < 0.005) \
         ORDER BY {mo} ASC, m.id ASC",
        mo = MOMENTO_M_SQL
    ))
    .bind(c)
    .bind(&desde)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| MovimientoDesalineado {
        id: r.get("id"),
        tipo: r.get("tipo"),
        monto: r.get("monto"),
        concepto: r.get("concepto"),
        fecha: r.get("fecha"),
        corte_id: r.get("corte_id"),
        corte_tipo: r.get("corte_tipo"),
        corte_inicio: r.get("fecha_inicio"),
        corte_fin: r.get("fecha_fin"),
    })
    .collect();
    // Con signo: un retiro aplicado de más hizo que se esperara menos.
    let movimientos_desalineados_monto: f64 = movimientos_desalineados
        .iter()
        .map(|m| if m.tipo == "RETIRO" { -m.monto } else { m.monto })
        .sum();

    // ── 3. Movimientos sin corte anteriores al período abierto ──
    let movimientos_huerfanos: Vec<MovimientoHuerfano> = sqlx::query(&format!(
        "SELECT id, tipo, monto::float8 AS monto, concepto, fecha FROM movimientos_caja \
         WHERE caja = $1 AND corte_id IS NULL AND deleted_at IS NULL \
           AND substr({mo}, 1, 10) >= $2 AND {mo} <= $3 \
         ORDER BY {mo} ASC, id ASC",
        mo = MOMENTO_SQL
    ))
    .bind(c)
    .bind(&desde)
    .bind(&inicio_abierto)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| MovimientoHuerfano {
        id: r.get("id"),
        tipo: r.get("tipo"),
        monto: r.get("monto"),
        concepto: r.get("concepto"),
        fecha: r.get("fecha"),
    })
    .collect();
    // Un retiro que ningún corte contó: la caja tiene menos de lo esperado.
    let movimientos_huerfanos_monto: f64 = movimientos_huerfanos
        .iter()
        .map(|m| if m.tipo == "RETIRO" { -m.monto } else { m.monto })
        .sum();

    // ── 4. Continuidad de la cadena de cortes ──
    let cadena: Vec<(i64, String, String, f64, f64)> = sqlx::query(&format!(
        "SELECT id, tipo, {FIN_CORTE_SQL} AS fin, fondo_inicial::float8 AS fi, \
                fondo_siguiente::float8 AS fs \
         FROM cortes WHERE deleted_at IS NULL AND caja = $1 \
         ORDER BY 3 ASC, id ASC"
    ))
    .bind(c)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| (r.get("id"), r.get("tipo"), r.get("fin"), r.get("fi"), r.get("fs")))
    .collect();
    let mut cadena_rota: Vec<EslabonRoto> = cadena
        .windows(2)
        .filter_map(|w| {
            let (_, _, _, _, fondo_anterior) = &w[0];
            let (id, tipo, fecha, fondo_inicial, _) = &w[1];
            let diferencia = fondo_inicial - fondo_anterior;
            (diferencia.abs() > 0.005 && fecha.as_str() >= desde.as_str()).then(|| EslabonRoto {
                corte_id: *id,
                tipo: tipo.clone(),
                fecha: fecha.clone(),
                fondo_anterior: *fondo_anterior,
                fondo_inicial: *fondo_inicial,
                diferencia,
            })
        })
        .collect();
    cadena_rota.reverse(); // más recientes primero
    let eslabones_rotos_monto: f64 = cadena_rota.iter().map(|e| e.diferencia).sum();

    // ── 5. Aritmética interna de cada corte ──
    let cortes_inconsistentes: Vec<CorteInconsistente> = sqlx::query(
        "SELECT id, tipo, created_at, efectivo_esperado::float8 AS guardado, \
                (fondo_inicial + total_ventas_efectivo + total_entradas_efectivo \
                 - total_retiros_efectivo)::float8 AS recalculado \
         FROM cortes \
         WHERE deleted_at IS NULL AND caja = $1 AND substr(created_at, 1, 10) >= $2 \
           AND ABS((fondo_inicial + total_ventas_efectivo \
                    + total_entradas_efectivo - total_retiros_efectivo) \
                   - efectivo_esperado) > 0.01 \
         ORDER BY created_at DESC, id DESC",
    )
    .bind(c)
    .bind(&desde)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| {
        let guardado: f64 = r.get("guardado");
        let recalculado: f64 = r.get("recalculado");
        CorteInconsistente {
            id: r.get("id"),
            tipo: r.get("tipo"),
            created_at: r.get("created_at"),
            esperado_guardado: guardado,
            esperado_recalculado: recalculado,
            descuadre: recalculado - guardado,
        }
    })
    .collect();

    // ── 6. Días con ventas sin cierre del día (informativo) ──
    let dias_sin_cierre: Vec<DiaSinCierre> = sqlx::query(&format!(
        "SELECT substr({mo}, 1, 10) AS dia, COUNT(*)::bigint AS n, \
                COALESCE(SUM(v.total), 0)::float8 AS total, \
                COALESCE(SUM(v.total) FILTER (WHERE v.metodo_pago = 'efectivo'), 0)::float8 AS efectivo \
         FROM ventas v \
         WHERE v.anulada = 0 AND v.caja = $1 AND v.deleted_at IS NULL \
           AND substr({mo}, 1, 10) >= $2 \
           AND substr({mo}, 1, 10) < $3 \
           AND NOT EXISTS ( \
               SELECT 1 FROM cortes c \
               WHERE c.tipo = 'DIA' AND c.deleted_at IS NULL AND c.caja = $1 \
                 AND substr({f}, 1, 10) = substr({mo}, 1, 10)) \
         GROUP BY 1 \
         ORDER BY 1 DESC",
        mo = MOMENTO_V_SQL,
        f = FIN_CORTE_C_SQL
    ))
    .bind(c)
    .bind(&desde)
    .bind(&hoy)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| DiaSinCierre {
        dia: r.get("dia"),
        num_ventas: r.get("n"),
        total_ventas: r.get("total"),
        total_efectivo: r.get("efectivo"),
    })
    .collect();

    let resumen = ResumenAuditoria {
        ventas_sin_corte_count: ventas_sin_corte.len() as i64,
        ventas_sin_corte_efectivo,
        movimientos_desalineados_count: movimientos_desalineados.len() as i64,
        movimientos_desalineados_monto,
        movimientos_huerfanos_count: movimientos_huerfanos.len() as i64,
        movimientos_huerfanos_monto,
        eslabones_rotos_count: cadena_rota.len() as i64,
        eslabones_rotos_monto,
        cortes_inconsistentes_count: cortes_inconsistentes.len() as i64,
        dias_sin_cierre_count: dias_sin_cierre.len() as i64,
        impacto_total_estimado: ventas_sin_corte_efectivo
            + movimientos_desalineados_monto
            + movimientos_huerfanos_monto
            + eslabones_rotos_monto,
    };

    Ok(AuditoriaCaja {
        resumen,
        ventas_sin_corte,
        movimientos_desalineados,
        movimientos_huerfanos,
        cadena_rota,
        cortes_inconsistentes,
        dias_sin_cierre,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segundo_siguiente_cruza_dia_y_rechaza_basura() {
        assert_eq!(segundo_siguiente("2026-10-01 23:59:59").as_deref(), Some("2026-10-02 00:00:00"));
        assert_eq!(segundo_siguiente("2026-10-01 10:00:00").as_deref(), Some("2026-10-01 10:00:01"));
        assert_eq!(segundo_siguiente("0000-00-00 00:00:00"), None);
        assert_eq!(segundo_siguiente("basura"), None);
    }

    #[test]
    fn el_centinela_no_es_un_dispositivo() {
        assert!(!es_dispositivo_real(DISPOSITIVO_SIN_EQUIPO));
        assert!(!es_dispositivo_real(""));
        assert!(!es_dispositivo_real("   "));
        assert!(es_dispositivo_real("0b7c1f2e-6a1d-4c55-9b0e-2f6d8a1c3e44"));
        let claims = |device: Option<&str>| Claims {
            sub: 1,
            email: "x".into(),
            role: "admin".into(),
            sucursal: 1,
            device: device.map(String::from),
            exp: 0,
        };
        assert_eq!(dispositivo_de(&claims(None)), None);
        assert_eq!(dispositivo_de(&claims(Some(DISPOSITIVO_SIN_EQUIPO))), None);
        assert_eq!(dispositivo_de(&claims(Some("equipo-1"))), Some("equipo-1"));
    }

    #[test]
    fn caja_texto_y_llaves() {
        assert_eq!(Caja::Web.as_str(), "web");
        assert_eq!(Caja::Principal.as_str(), "principal");
        assert_ne!(Caja::Web.llave_candado(), Caja::Principal.llave_candado());
        for k in [7_301_100, 7_301_101, 7_301_199] {
            assert_ne!(Caja::Web.llave_candado(), k);
            assert_ne!(Caja::Principal.llave_candado(), k);
        }
    }

    #[test]
    fn nuevo_corte_acepta_lo_que_manda_el_frontend() {
        // Lo que manda CortesCaja.tsx (datos completos, usuario_id, fechas).
        let v = serde_json::json!({
            "tipo": "PARCIAL", "usuario_id": 3,
            "fecha_inicio": "2026-10-01 00:00:00", "fecha_fin": "2026-10-01 10:00:00",
            "datos": { "efectivo_esperado": 120.5, "fondo_inicial": 100, "movimientos": [] },
            "efectivo_contado": 120.5, "nota_diferencia": null, "fondo_siguiente": 120.5,
            "denominaciones": [{ "denominacion": 100, "tipo": "BILLETE", "cantidad": 1 }],
            "retiro": { "monto": 20, "concepto": "turno", "pin_autorizacion": null }
        });
        let n: NuevoCorte = serde_json::from_value(v).unwrap();
        assert_eq!(n.datos.efectivo_esperado, 120.5);
        assert_eq!(n.retiro.as_ref().map(|r| r.monto), Some(20.0));
        // Mínimo indispensable (cliente viejo sin retiro ni denominaciones).
        let m: NuevoCorte = serde_json::from_value(serde_json::json!({
            "tipo": "DIA", "datos": { "efectivo_esperado": 0 }, "efectivo_contado": 0
        }))
        .unwrap();
        assert!(m.retiro.is_none() && m.denominaciones.is_none());
    }
}
