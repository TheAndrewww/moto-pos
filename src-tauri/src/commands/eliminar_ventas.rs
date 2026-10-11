// commands/eliminar_ventas.rs — Eliminar una venta de PRUEBA (solo Daniela Bravo)
//
// Pedido del dueño (2026-10-10): se hacen pruebas con el inventario real y
// esas ventas se tienen que limpiar por completo, "como si nunca hubieran
// pasado", sin afectar el corte real:
//
//   - La venta desaparece: historial, reportes, estadísticas y web. Se borran
//     sus partidas, sus devoluciones (con sus partidas) y los retiros de caja
//     que se hicieron por ella (reembolso de devolución y reembolso de
//     anulación).
//   - El inventario regresa: lo vendido menos lo que ya regresaron sus
//     devoluciones. Si estaba anulada, la anulación ya lo regresó: no se toca.
//   - La caja queda como si no hubiera existido. Una venta del período
//     abierto sale sola del cálculo al borrarla. Si un corte cerrado ya la
//     contó, se le resta a ese corte exactamente lo que `calcular_periodo`
//     (cortes.rs) le sumó: totales por método, total, transacciones,
//     descuentos o anulaciones, efectivo esperado y la diferencia contra lo
//     contado; en un corte del día también su renglón de vendedor. Lo mismo
//     con cada retiro ligado a un corte cerrado. NUNCA se tocan fondo_inicial,
//     efectivo_contado, fondo_siguiente, notas ni denominaciones: la cadena
//     de cortes sigue continua. Se asume lo que pasa en una prueba: el dinero
//     de la venta (y de sus reembolsos) nunca entró ni salió del cajón.
//   - Bitácora: por pedido expreso del dueño (son ventas de PRUEBA y no deben
//     dejar rastro) se borran también sus renglones: los de la venta, los de
//     sus devoluciones y los de sus retiros (tabla_afectada + registro_id).
//     No se agrega ninguno. El resto de la bitácora no se toca.
//   - Sync: los renglones se borran con la bandera de supresión puesta (el
//     outbox no encola DELETE) y se anotan en `sync_purgas_pendientes`; el
//     worker los manda a POST /sync/purgar hasta que el servidor confirma.
//     Los cambios de existencia, cortes y presupuesto suben por el push normal.
//   - Folios: si era el último folio de venta (o de devolución), la secuencia
//     baja uno y la siguiente venta reutiliza el número (sin huecos).
//
// Solo usuarios con `usuarios.puede_eliminar_ventas = 1` (migración 017: solo
// Daniela Bravo; otros usuarios con rol dueño NO) y con SU propio PIN.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use tauri::State;

use super::auth::AppState;
use super::cortes::{round2, FIN_CORTE_SQL};

/// Tolerancia para comparar dinero y cantidades (medio centavo).
const EPS: f64 = 0.005;

/// Nota del outbox para los renglones pendientes de filas eliminadas.
const NOTA_OUTBOX: &str = "eliminada aquí (venta de prueba); se purga en el servidor";

#[derive(Serialize, Clone, Debug)]
pub struct VentaEliminada {
    pub venta_id: i64,
    pub folio: String,
    pub total: f64,
    pub metodo_pago: String,
    pub anulada: bool,
    /// Piezas que regresaron al inventario (0 si estaba anulada).
    pub piezas_regresadas: f64,
    /// Corte cerrado que había contado la venta (None = período abierto).
    pub corte_ajustado_id: Option<i64>,
    /// Todos los cortes ajustados (el de la venta y los de sus retiros).
    pub cortes_ajustados: Vec<i64>,
    pub devoluciones_eliminadas: usize,
    pub movimientos_eliminados: usize,
    /// La secuencia de folios bajó: la siguiente venta usa este folio.
    pub folio_reutilizable: bool,
}

/// Eliminar una venta de prueba (solo usuarios con el permiso individual).
#[tauri::command]
pub fn eliminar_venta(
    venta_id: i64,
    usuario_id: i64,
    pin: String,
    state: State<'_, AppState>,
) -> Result<VentaEliminada, String> {
    let db = state.db.lock().unwrap();
    eliminar_venta_core(&db, venta_id, usuario_id, &pin, &super::cortes::ahora_local())
}

/// Núcleo de `eliminar_venta` con el reloj explícito (para pruebas). `now`
/// es hora local 'YYYY-MM-DD HH:MM:SS' (updated_at de lo que se ajusta).
pub(crate) fn eliminar_venta_core(
    db: &Connection,
    venta_id: i64,
    usuario_id: i64,
    pin: &str,
    now: &str,
) -> Result<VentaEliminada, String> {
    // Antes del BEGIN: bcrypt es lento y una negativa no abre transacción.
    autorizar(db, usuario_id, pin)?;
    eliminar_venta_autorizada(db, venta_id, now)
}

/// Resultado de aplicar una eliminación pedida desde la web.
#[derive(Debug)]
pub(crate) enum ResultadoSolicitud {
    /// Eliminada (o ya no existía porque se eliminó antes aquí).
    Aplicada(Option<VentaEliminada>),
    /// Todavía no se puede (la venta aún no baja a este equipo): se reintenta.
    Pendiente(String),
    /// No se aplicará; el motivo se muestra en la web.
    Rechazada(String),
}

/// Aplica una "eliminar venta de prueba" pedida desde la web. El servidor ya
/// verificó el PIN; aquí se vuelve a exigir que quien la pidió tenga el
/// permiso individual en ESTE equipo.
pub(crate) fn aplicar_solicitud_web(
    db: &Connection,
    venta_uuid: &str,
    solicitado_por_uuid: &str,
    now: &str,
) -> ResultadoSolicitud {
    let autorizado: Option<i64> = db
        .query_row(
            "SELECT id FROM usuarios WHERE uuid = ? AND activo = 1 AND deleted_at IS NULL \
               AND puede_eliminar_ventas = 1",
            [solicitado_por_uuid],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None);
    if autorizado.is_none() {
        return ResultadoSolicitud::Rechazada(
            "Quien la pidió no tiene permiso para eliminar ventas en el POS de escritorio.".into(),
        );
    }
    let venta_id: Option<i64> = db
        .query_row("SELECT id FROM ventas WHERE uuid = ?", [venta_uuid], |r| r.get(0))
        .optional()
        .unwrap_or(None);
    let Some(venta_id) = venta_id else {
        let ya_eliminada: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sync_purgas_pendientes WHERE tabla = 'ventas' AND uuid = ?)",
                [venta_uuid],
                |r| r.get(0),
            )
            .unwrap_or(false);
        return if ya_eliminada {
            ResultadoSolicitud::Aplicada(None)
        } else {
            ResultadoSolicitud::Pendiente("la venta todavía no llega a este equipo".into())
        };
    };
    match eliminar_venta_autorizada(db, venta_id, now) {
        Ok(r) => ResultadoSolicitud::Aplicada(Some(r)),
        Err(e) => ResultadoSolicitud::Rechazada(e),
    }
}

/// La eliminación completa, ya autorizada, en UNA transacción.
fn eliminar_venta_autorizada(db: &Connection, venta_id: i64, now: &str) -> Result<VentaEliminada, String> {
    db.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
    match eliminar_en_tx(db, venta_id, now) {
        Ok(r) => {
            db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            Ok(r)
        }
        Err(e) => {
            // La bandera de supresión se puso dentro de la transacción: el
            // rollback también la quita.
            let _ = db.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Quien elimina: activo, no borrado, con el permiso individual y con SU PIN
/// (no basta el PIN de cualquier dueño). Devuelve su nombre.
fn autorizar(db: &Connection, usuario_id: i64, pin: &str) -> Result<String, String> {
    let pin = pin.trim();
    if pin.len() != 4 || !pin.chars().all(|c| c.is_ascii_digit()) {
        return Err("Escribe tu PIN de 4 dígitos".to_string());
    }
    let fila: Option<(String, String, i64, i64, Option<String>)> = db
        .query_row(
            "SELECT nombre_completo, pin, activo, COALESCE(puede_eliminar_ventas, 0), deleted_at \
             FROM usuarios WHERE id = ?",
            [usuario_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((nombre, hash, activo, puede, borrado)) = fila else {
        return Err("Usuario no encontrado".to_string());
    };
    if activo != 1 || borrado.is_some() {
        return Err("Tu usuario está desactivado: no puede eliminar ventas".to_string());
    }
    if puede != 1 {
        return Err("Tu usuario no tiene permiso para eliminar ventas de prueba".to_string());
    }
    if !bcrypt::verify(pin, &hash).unwrap_or(false) {
        return Err("PIN incorrecto".to_string());
    }
    Ok(nombre)
}

struct Venta {
    id: i64,
    uuid: Option<String>,
    folio: String,
    usuario_id: i64,
    total: f64,
    descuento: f64,
    metodo_pago: String,
    anulada: bool,
    fecha: String,
    /// COALESCE(registrado_at, fecha): su lugar en la cadena de cortes.
    momento: String,
    caja: String,
    deleted_at: Option<String>,
    updated_at: Option<String>,
}

struct Devolucion {
    uuid: Option<String>,
    folio: String,
}

struct Movimiento {
    id: i64,
    uuid: Option<String>,
    tipo: String,
    monto: f64,
    corte_id: Option<i64>,
}

/// Corte cerrado de la caja de la tienda que contó la venta.
struct CorteContado {
    id: i64,
    tipo: String,
    fin: String,
}

/// Lo que hay que restarle a un corte cerrado.
#[derive(Default, Clone, Copy)]
struct Ajuste {
    ventas_efectivo: f64,
    ventas_tarjeta: f64,
    ventas_transferencia: f64,
    total_ventas: f64,
    num_transacciones: i64,
    total_descuentos: f64,
    total_anulaciones: f64,
    entradas: f64,
    retiros: f64,
}

fn eliminar_en_tx(db: &Connection, venta_id: i64, now: &str) -> Result<VentaEliminada, String> {
    let err = |e: rusqlite::Error| e.to_string();

    // ── 1. La venta y todo lo que es suyo ─────────────────
    let v: Venta = db
        .query_row(
            "SELECT id, uuid, folio, usuario_id, total, descuento, metodo_pago, anulada, fecha, \
                    COALESCE(registrado_at, fecha), caja, deleted_at, updated_at \
             FROM ventas WHERE id = ?",
            [venta_id],
            |r| {
                Ok(Venta {
                    id: r.get(0)?,
                    uuid: r.get(1)?,
                    folio: r.get(2)?,
                    usuario_id: r.get(3)?,
                    total: r.get(4)?,
                    descuento: r.get(5)?,
                    metodo_pago: r.get(6)?,
                    anulada: r.get::<_, i64>(7)? != 0,
                    fecha: r.get(8)?,
                    momento: r.get(9)?,
                    caja: r.get(10)?,
                    deleted_at: r.get(11)?,
                    updated_at: r.get(12)?,
                })
            },
        )
        .optional()
        .map_err(err)?
        .ok_or_else(|| "Venta no encontrada".to_string())?;

    // Las ventas de la caja web solo se guardan aquí como historial: su
    // dinero y su cadena de cortes viven en el servidor.
    if v.caja != "principal" {
        return Err(format!(
            "La venta {} es de la caja web: su dinero y sus cortes viven en el servidor, \
             no se puede eliminar desde el escritorio.",
            v.folio
        ));
    }

    let devoluciones: Vec<Devolucion> = {
        let mut st = db
            .prepare("SELECT uuid, folio FROM devoluciones WHERE venta_id = ? ORDER BY id")
            .map_err(err)?;
        let filas = st
            .query_map([v.id], |r| Ok(Devolucion { uuid: r.get(0)?, folio: r.get(1)? }))
            .map_err(err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(err)?;
        filas
    };

    // Retiros hechos POR esta venta: el reembolso de cada devolución y el
    // reembolso de la anulación (anular_venta_core, venta ya cortada).
    let mut ids_mov: Vec<i64> = {
        let mut st = db
            .prepare(
                "SELECT movimiento_caja_id FROM devoluciones \
                 WHERE venta_id = ? AND movimiento_caja_id IS NOT NULL",
            )
            .map_err(err)?;
        let filas = st
            .query_map([v.id], |r| r.get::<_, i64>(0))
            .map_err(err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(err)?;
        filas
    };
    // El concepto lo escribe anular_venta_core: "Reembolso por anulación de
    // venta V-000123 (ya incluida en un corte) — motivo".
    let mut reembolsos_anulacion: Vec<i64> = Vec::new();
    if v.anulada {
        let prefijo = format!("Reembolso por anulación de venta {} ", v.folio);
        let mut st = db
            .prepare(
                "SELECT id FROM movimientos_caja \
                 WHERE tipo = 'RETIRO' AND substr(concepto, 1, length(?1)) = ?1 ORDER BY id",
            )
            .map_err(err)?;
        reembolsos_anulacion = st
            .query_map([&prefijo], |r| r.get::<_, i64>(0))
            .map_err(err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(err)?;
        ids_mov.extend(reembolsos_anulacion.iter().copied());
    }
    let hay_reembolso_anulacion = !reembolsos_anulacion.is_empty();
    ids_mov.sort_unstable();
    ids_mov.dedup();
    let mut movimientos: Vec<Movimiento> = Vec::with_capacity(ids_mov.len());
    for id in &ids_mov {
        let m = db
            .query_row(
                "SELECT id, uuid, tipo, monto, corte_id FROM movimientos_caja WHERE id = ?",
                [id],
                |r| {
                    Ok(Movimiento {
                        id: r.get(0)?,
                        uuid: r.get(1)?,
                        tipo: r.get(2)?,
                        monto: r.get(3)?,
                        corte_id: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(err)?;
        if let Some(m) = m {
            movimientos.push(m);
        }
    }

    // ── 2. Inventario: lo vendido menos lo ya devuelto ────
    // Una venta anulada ya regresó todo al anular (y no admite devoluciones).
    let mut regresar: BTreeMap<i64, f64> = BTreeMap::new();
    if !v.anulada {
        let mut st = db
            .prepare("SELECT producto_id, SUM(cantidad) FROM venta_detalle WHERE venta_id = ? GROUP BY producto_id")
            .map_err(err)?;
        let vendidas = st
            .query_map([v.id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))
            .map_err(err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(err)?;
        for (p, c) in vendidas {
            *regresar.entry(p).or_default() += c;
        }
        let mut st = db
            .prepare(
                "SELECT dd.producto_id, SUM(dd.cantidad) FROM devolucion_detalle dd \
                 JOIN devoluciones d ON d.id = dd.devolucion_id \
                 WHERE d.venta_id = ? GROUP BY dd.producto_id",
            )
            .map_err(err)?;
        let devueltas = st
            .query_map([v.id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))
            .map_err(err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(err)?;
        for (p, c) in devueltas {
            *regresar.entry(p).or_default() -= c;
        }
    }
    regresar.retain(|_, c| *c > 0.0001);
    let piezas_regresadas: f64 = regresar.values().sum();

    // ── 3. Caja: qué corte cerrado contó la venta y cómo ──
    let mut ajustes: BTreeMap<i64, Ajuste> = BTreeMap::new();
    // (corte DIA, vendedor) cuyo renglón de corte_vendedores hay que bajar.
    let mut vendedor_dia: Option<(i64, String)> = None;
    let corte_venta = if v.deleted_at.is_none() { corte_que_conto(db, &v.momento)? } else { None };
    if let Some(c) = &corte_venta {
        let viva = contada_viva(db, &v, c, hay_reembolso_anulacion)?;
        let a = ajustes.entry(c.id).or_default();
        if viva {
            match v.metodo_pago.as_str() {
                "efectivo" => a.ventas_efectivo += v.total,
                "tarjeta" => a.ventas_tarjeta += v.total,
                "transferencia" => a.ventas_transferencia += v.total,
                _ => {}
            }
            a.total_ventas += v.total;
            a.num_transacciones += 1;
            a.total_descuentos += v.descuento;
            if c.tipo == "DIA" {
                vendedor_dia = Some((c.id, c.fin.clone()));
            }
        } else {
            a.total_anulaciones += v.total;
        }
    }
    // Retiros ya ligados a un corte cerrado: ese corte los restó.
    for m in &movimientos {
        let Some(cid) = m.corte_id else { continue };
        let a = ajustes.entry(cid).or_default();
        if m.tipo == "ENTRADA" {
            a.entradas += m.monto;
        } else {
            a.retiros += m.monto;
        }
    }

    // ── 4. Presupuesto convertido en esta venta: vuelve a pendiente ──
    // Antes de borrar la venta (FK presupuestos.venta_id) y sin la bandera:
    // el cambio sube por el push normal.
    db.execute(
            "UPDATE presupuestos SET venta_id = NULL, \
                    estado = CASE WHEN estado = 'convertido' THEN 'pendiente' ELSE estado END \
              WHERE venta_id = ?",
            [v.id],
        )
        .map_err(err)?;

    // Renglones de bitácora de la venta, de sus devoluciones y de sus retiros
    // (antes de borrar las devoluciones: se buscan por su id).
    let bitacora: Vec<(i64, Option<String>)> = {
        let movs = ids_mov.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let mut sql = String::from(
            "SELECT id, uuid FROM audit_log \
              WHERE (tabla_afectada = 'ventas' AND registro_id = ?1) \
                 OR (tabla_afectada = 'devoluciones' \
                     AND registro_id IN (SELECT id FROM devoluciones WHERE venta_id = ?1))",
        );
        if !movs.is_empty() {
            sql.push_str(&format!(" OR (tabla_afectada = 'movimientos_caja' AND registro_id IN ({movs}))"));
        }
        sql.push_str(" ORDER BY id");
        let mut st = db.prepare(&sql).map_err(err)?;
        let filas = st
            .query_map([v.id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))
            .map_err(err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(err)?;
        filas
    };

    // ── 5. Borrar con la bandera de supresión (el outbox no encola DELETE) ──
    db.execute("INSERT OR IGNORE INTO sync_suppress_flag (id) VALUES (1)", []).map_err(err)?;
    for (id, _) in &bitacora {
        db.execute("DELETE FROM audit_log WHERE id = ?", [id]).map_err(err)?;
    }
    db.execute(
        "DELETE FROM devolucion_detalle WHERE devolucion_id IN (SELECT id FROM devoluciones WHERE venta_id = ?)",
        [v.id],
    )
    .map_err(err)?;
    db.execute("DELETE FROM devoluciones WHERE venta_id = ?", [v.id]).map_err(err)?;
    for m in &movimientos {
        db.execute("DELETE FROM movimientos_caja WHERE id = ?", [m.id]).map_err(err)?;
    }
    db.execute("DELETE FROM venta_detalle WHERE venta_id = ?", [v.id]).map_err(err)?;
    db.execute("DELETE FROM ventas WHERE id = ?", [v.id]).map_err(err)?;

    // Lo que falta borrar en el servidor, y renglones del outbox de esas
    // filas que ya no tienen nada que subir (p. ej. venta hecha sin internet).
    let mut purgar: Vec<(&str, &str)> = Vec::new();
    if let Some(u) = v.uuid.as_deref() {
        purgar.push(("ventas", u));
    }
    for d in &devoluciones {
        if let Some(u) = d.uuid.as_deref() {
            purgar.push(("devoluciones", u));
        }
    }
    for m in &movimientos {
        if let Some(u) = m.uuid.as_deref() {
            purgar.push(("movimientos_caja", u));
        }
    }
    for (_, u) in &bitacora {
        if let Some(u) = u.as_deref() {
            purgar.push(("audit_log", u));
        }
    }
    for (tabla, uuid) in &purgar {
        db.execute(
            "INSERT OR IGNORE INTO sync_purgas_pendientes (tabla, uuid, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![tabla, uuid, now],
        )
        .map_err(err)?;
        db.execute(
            "UPDATE sync_outbox SET synced_at = datetime('now'), ultimo_error = ?3 \
              WHERE tabla = ?1 AND uuid = ?2 AND synced_at IS NULL",
            rusqlite::params![tabla, uuid, NOTA_OUTBOX],
        )
        .map_err(err)?;
    }
    // Se quita ANTES de los ajustes: existencias y cortes sí deben subir.
    db.execute("DELETE FROM sync_suppress_flag", []).map_err(err)?;

    // ── 6. Inventario ─────────────────────────────────────
    for (producto_id, cantidad) in &regresar {
        db.execute(
            "UPDATE productos SET stock_actual = stock_actual + ?, updated_at = ? WHERE id = ?",
            rusqlite::params![cantidad, now, producto_id],
        )
        .map_err(err)?;
    }

    // ── 7. Cortes cerrados ────────────────────────────────
    let mut cortes_ajustados: Vec<i64> = Vec::new();
    for (corte_id, a) in &ajustes {
        if ajustar_corte(db, *corte_id, a, now)? {
            cortes_ajustados.push(*corte_id);
        }
    }
    if let Some((corte_id, fin)) = &vendedor_dia {
        ajustar_vendedor(db, *corte_id, fin, &v)?;
    }

    // ── 8. Folios: si era el último, el número se reutiliza ──
    let folio_reutilizable = match numero_folio(&v.folio, "V-") {
        Some(n) => {
            db.execute(
                "UPDATE folio_secuencia SET ultimo_valor = ultimo_valor - 1 WHERE id = 1 AND ultimo_valor = ?",
                [n],
            )
            .map_err(err)?
                > 0
        }
        None => false,
    };
    // Devoluciones de la más nueva a la más vieja: si eran las últimas
    // seguidas, la secuencia baja por cada una.
    let mut nums_dev: Vec<i64> = devoluciones.iter().filter_map(|d| numero_folio(&d.folio, "D-")).collect();
    nums_dev.sort_unstable_by(|a, b| b.cmp(a));
    for n in nums_dev {
        db.execute(
            "UPDATE devolucion_folio_secuencia SET ultimo_valor = ultimo_valor - 1 \
              WHERE id = 1 AND ultimo_valor = ?",
            [n],
        )
        .map_err(err)?;
    }

    Ok(VentaEliminada {
        venta_id: v.id,
        folio: v.folio,
        total: v.total,
        metodo_pago: v.metodo_pago,
        anulada: v.anulada,
        piezas_regresadas,
        corte_ajustado_id: corte_venta.map(|c| c.id),
        cortes_ajustados,
        devoluciones_eliminadas: devoluciones.len(),
        movimientos_eliminados: movimientos.len(),
        folio_reutilizable,
    })
}

/// Corte de la caja de la tienda que contó una venta con este `momento`:
/// el primero de la cadena con FIN >= momento (el anterior terminó antes), y
/// solo si el momento cae después de su inicio, igual que el intervalo
/// (inicio, fin] de `calcular_periodo`. None = período abierto (o una venta
/// que ningún corte contó): borrarla basta.
fn corte_que_conto(db: &Connection, momento: &str) -> Result<Option<CorteContado>, String> {
    let fila: Option<(i64, String, String, String)> = db
        .query_row(
            &format!(
                "SELECT id, tipo, replace(substr(fecha_inicio, 1, 19), 'T', ' '), {f} FROM cortes \
                 WHERE deleted_at IS NULL AND caja = 'principal' AND {f} >= ?1 \
                 ORDER BY {f} ASC, id ASC LIMIT 1",
                f = FIN_CORTE_SQL
            ),
            [momento],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(fila.and_then(|(id, tipo, inicio, fin)| {
        (momento > inicio.as_str()).then_some(CorteContado { id, tipo, fin })
    }))
}

/// ¿El corte la contó como venta viva (totales, efectivo) o como anulada
/// (solo total_anulaciones)? Una venta sin anular, viva. Anulada:
///   - en efectivo con total > 0: anular_venta_core registra el reembolso
///     solo si la venta YA estaba cortada → con reembolso, el corte la contó
///     viva; sin reembolso, se anuló antes del corte y la contó anulada;
///   - si no: por la hora de la anulación (updated_at) contra el fin del
///     corte, verificada contra lo que el corte guardó (un corte que no tiene
///     ese monto en anulaciones no la pudo contar anulada, y al revés).
fn contada_viva(
    db: &Connection,
    v: &Venta,
    c: &CorteContado,
    hay_reembolso_anulacion: bool,
) -> Result<bool, String> {
    if !v.anulada {
        return Ok(true);
    }
    if v.metodo_pago == "efectivo" && v.total > EPS {
        return Ok(hay_reembolso_anulacion);
    }
    let anulada_despues = v
        .updated_at
        .as_deref()
        .map(super::cortes::normalizar_marca)
        .map(|u| !u.is_empty() && u > super::cortes::normalizar_marca(&c.fin))
        .unwrap_or(false);
    let (anulaciones, en_su_metodo): (f64, f64) = db
        .query_row(
            "SELECT total_anulaciones, \
                    CASE ?2 WHEN 'efectivo' THEN total_ventas_efectivo \
                            WHEN 'tarjeta' THEN total_ventas_tarjeta \
                            WHEN 'transferencia' THEN total_ventas_transferencia \
                            ELSE total_ventas END \
             FROM cortes WHERE id = ?1",
            rusqlite::params![c.id, v.metodo_pago],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    let cabe_anulada = anulaciones + EPS >= v.total;
    let cabe_viva = en_su_metodo + EPS >= v.total;
    Ok(match (cabe_viva, cabe_anulada) {
        (true, false) => true,
        (false, true) => false,
        _ => anulada_despues,
    })
}

/// Resta el ajuste a un corte cerrado con las MISMAS fórmulas de
/// crear_corte_core: esperado = fondo + ventas en efectivo + entradas −
/// retiros, diferencia = contado − esperado, todo a centavos. Nunca toca
/// fondo_inicial, efectivo_contado, fondo_siguiente, notas ni
/// denominaciones. Devuelve false si el corte no existe.
fn ajustar_corte(db: &Connection, corte_id: i64, a: &Ajuste, now: &str) -> Result<bool, String> {
    type Fila = (f64, f64, f64, f64, i64, f64, f64, f64, f64, f64, f64);
    let fila: Option<Fila> = db
        .query_row(
            "SELECT total_ventas_efectivo, total_ventas_tarjeta, total_ventas_transferencia, \
                    total_ventas, num_transacciones, total_descuentos, total_anulaciones, \
                    total_entradas_efectivo, total_retiros_efectivo, efectivo_esperado, efectivo_contado \
             FROM cortes WHERE id = ?",
            [corte_id],
            |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                    r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((ef, ta, tr, tot, num, desc, anul, ent, ret, esperado, contado)) = fila else {
        return Ok(false);
    };
    let nuevo_esperado = round2(esperado - a.ventas_efectivo - a.entradas + a.retiros);
    db.execute(
        "UPDATE cortes SET total_ventas_efectivo = ?, total_ventas_tarjeta = ?, \
                total_ventas_transferencia = ?, total_ventas = ?, num_transacciones = ?, \
                total_descuentos = ?, total_anulaciones = ?, total_entradas_efectivo = ?, \
                total_retiros_efectivo = ?, efectivo_esperado = ?, diferencia = ?, updated_at = ? \
          WHERE id = ?",
        rusqlite::params![
            round2(ef - a.ventas_efectivo),
            round2(ta - a.ventas_tarjeta),
            round2(tr - a.ventas_transferencia),
            round2(tot - a.total_ventas),
            (num - a.num_transacciones).max(0),
            round2(desc - a.total_descuentos),
            round2(anul - a.total_anulaciones),
            round2(ent - a.entradas),
            round2(ret - a.retiros),
            nuevo_esperado,
            round2(contado - nuevo_esperado),
            now,
            corte_id
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(true)
}

/// Corte del día: baja el renglón del vendedor como lo habría calculado el
/// corte sin esta venta (una venta menos, su total menos). Si era su única
/// venta, el renglón desaparece (el corte no lo habría creado). Si era su
/// primera o última venta, la hora se recalcula con las que quedan en el
/// período del corte.
fn ajustar_vendedor(db: &Connection, corte_id: i64, fin: &str, v: &Venta) -> Result<(), String> {
    let err = |e: rusqlite::Error| e.to_string();
    let fila: Option<(i64, i64, f64, String, String)> = db
        .query_row(
            "SELECT id, num_ventas, total_vendido, hora_inicio, hora_fin FROM corte_vendedores \
             WHERE corte_id = ? AND usuario_id = ? ORDER BY id LIMIT 1",
            [corte_id, v.usuario_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(err)?;
    let Some((id, num, total, mut hora_inicio, mut hora_fin)) = fila else { return Ok(()) };
    if num <= 1 {
        db.execute("DELETE FROM corte_vendedores WHERE id = ?", [id]).map_err(err)?;
        return Ok(());
    }
    if hora_inicio == v.fecha || hora_fin == v.fecha {
        let inicio: String = db
            .query_row(
                "SELECT fecha_inicio FROM cortes WHERE id = ?",
                [corte_id],
                |r| r.get(0),
            )
            .map_err(err)?;
        let (min, max): (Option<String>, Option<String>) = db
            .query_row(
                "SELECT MIN(fecha), MAX(fecha) FROM ventas \
                 WHERE caja = 'principal' AND deleted_at IS NULL AND anulada = 0 AND usuario_id = ?1 \
                   AND COALESCE(registrado_at, fecha) > ?2 AND COALESCE(registrado_at, fecha) <= ?3",
                rusqlite::params![v.usuario_id, super::cortes::normalizar_marca(&inicio), fin],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(err)?;
        if let (Some(a), Some(b)) = (min, max) {
            hora_inicio = a;
            hora_fin = b;
        }
    }
    db.execute(
        "UPDATE corte_vendedores SET num_ventas = ?, total_vendido = ?, hora_inicio = ?, hora_fin = ? \
          WHERE id = ?",
        rusqlite::params![num - 1, round2(total - v.total), hora_inicio, hora_fin, id],
    )
    .map_err(err)?;
    Ok(())
}

/// Número de un folio 'V-000123' / 'D-000045'. None si no tiene ese formato.
fn numero_folio(folio: &str, prefijo: &str) -> Option<i64> {
    folio.strip_prefix(prefijo)?.parse::<i64>().ok()
}

