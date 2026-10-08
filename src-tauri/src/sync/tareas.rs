// sync/tareas.rs — Tareas que corren una sola vez tras actualizar a sync v2.
//
// Orden (cada paso requiere el anterior):
//   a) Subir el mapa id local → uuid de las tablas con FK (POST /sync/id-map,
//      en tandas de ≤ 5000) y pedir al servidor que repare las referencias de
//      las filas de este equipo (POST /sync/reparar).
//   b) Arreglo espejo: las filas hechas en la web que ya están aquí llegaron
//      con ids del servidor en sus FK. Se piden sus referencias por uuid
//      (POST /sync/refs) y se corrigen SOLO las columnas FK (nunca montos,
//      nunca corte_id, nunca se insertan filas), con la bandera puesta.
//   c) Re-descarga desde el cursor 0 hasta el cursor actual en modo "solo
//      insertar": llegan las filas web que se perdieron por los errores
//      viejos (ventas, una devolución, renglones de recepciones…).
// El estado vive en sync_state (migración 015). Un servidor sin estos
// endpoints (404) = "aún no se actualiza": se reintenta más tarde sin error.

use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Map, Value};

use super::{
    apply::{self, EspejoTabla, Modo},
    client::{RemoteClient, Respuesta},
    state,
};

/// Tablas cuyo mapa id → uuid se sube al servidor.
pub const TABLAS_MAPA: &[&str] = &[
    "usuarios", "clientes", "proveedores", "categorias", "sucursales", "productos",
    "ventas", "venta_detalle", "cortes", "movimientos_caja", "aperturas_caja",
    "devoluciones", "devolucion_detalle", "recepciones", "recepcion_detalle",
    "presupuestos", "presupuesto_detalle", "ordenes_pedido", "orden_pedido_detalle",
    "transferencias", "transferencia_detalle",
];

/// Máximo de pares por llamada a /sync/id-map.
pub const MAX_PARES: usize = 5000;
/// Máximo de uuids por llamada a /sync/refs.
pub const MAX_UUIDS_REFS: usize = 500;
/// Tandas de re-descarga por ciclo del worker.
const TANDAS_REPLAY_POR_CICLO: usize = 10;

#[derive(Debug, Default, Clone, Serialize)]
pub struct MapaTabla {
    pub tabla: String,
    pub enviados: i64,
    pub recibidos: i64,
    pub desconocidos: i64,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ReporteMapa {
    pub tablas: Vec<MapaTabla>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ReporteEspejo {
    pub tablas: Vec<EspejoTabla>,
    pub filas: i64,
    pub celdas_cambiadas: i64,
    pub sin_resolver: i64,
    /// Tablas que el servidor no pudo resolver (se pueden reintentar con el
    /// botón "Reparar referencias"; no bloquean a las demás).
    pub errores: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ReporteReparacion {
    pub mapa: ReporteMapa,
    /// Reporte tal cual lo devuelve el servidor.
    pub servidor: Value,
    pub espejo: ReporteEspejo,
}

/// Error de una tarea: el servidor no la soporta aún, la rechazó (su
/// verificación falló; no se confirmó nada) o falló.
#[derive(Debug)]
pub enum ErrorTarea {
    NoSoportado,
    Rechazado(Value),
    Fallo(String),
}

impl From<String> for ErrorTarea {
    fn from(s: String) -> Self { ErrorTarea::Fallo(s) }
}

impl ErrorTarea {
    pub fn mensaje(&self) -> String {
        match self {
            ErrorTarea::NoSoportado => {
                "El servidor todavía no tiene esta función (falta actualizarlo). \
                 Se reintentará automáticamente.".to_string()
            }
            ErrorTarea::Rechazado(r) => format!(
                "El servidor no confirmó la reparación porque su verificación falló \
                 (no se cambió nada). Reporte: {r}"
            ),
            ErrorTarea::Fallo(e) => e.clone(),
        }
    }
}

fn existe_tabla(conn: &Connection, tabla: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
        [tabla],
        |_| Ok(()),
    ).is_ok()
}

/// Pares (id local, uuid) de una tabla, en orden de id.
pub fn pares_de_tabla(conn: &Connection, tabla: &str) -> rusqlite::Result<Vec<(i64, String)>> {
    if !existe_tabla(conn, tabla) {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT id, uuid FROM {tabla} WHERE uuid IS NOT NULL AND uuid <> '' ORDER BY id"
    ))?;
    let v = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect();
    v
}

fn contar_desconocidos(v: &Value) -> i64 {
    match v {
        Value::Array(a) => a.len() as i64,
        Value::Number(n) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// a) Sube el mapa id → uuid completo.
pub async fn subir_mapa_ids(
    db: &Arc<Mutex<Connection>>,
    client: &RemoteClient,
    device_uuid: &str,
) -> Result<ReporteMapa, ErrorTarea> {
    let mut rep = ReporteMapa::default();
    for tabla in TABLAS_MAPA {
        let pares = {
            let conn = db.lock().map_err(|e| e.to_string())?;
            pares_de_tabla(&conn, tabla).map_err(|e| e.to_string())?
        };
        let mut mt = MapaTabla { tabla: tabla.to_string(), ..Default::default() };
        for tanda in pares.chunks(MAX_PARES) {
            match client.subir_mapa(device_uuid, tabla, tanda).await? {
                Respuesta::NoSoportado | Respuesta::Rechazado(_) => return Err(ErrorTarea::NoSoportado),
                Respuesta::Ok(r) => {
                    mt.enviados += tanda.len() as i64;
                    mt.recibidos += r.recibidos;
                    mt.desconocidos += contar_desconocidos(&r.desconocidos);
                }
            }
        }
        rep.tablas.push(mt);
    }
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        state::marcar_mapa_subido(&conn).map_err(|e| e.to_string())?;
    }
    log::info!(
        "sync: mapa de ids subido ({} pares)",
        rep.tablas.iter().map(|t| t.enviados).sum::<i64>()
    );
    Ok(rep)
}

/// Pide al servidor la reparación de referencias de este equipo.
pub async fn reparar_servidor(client: &RemoteClient, device_uuid: &str) -> Result<Value, ErrorTarea> {
    match client.reparar(device_uuid).await? {
        Respuesta::NoSoportado => Err(ErrorTarea::NoSoportado),
        Respuesta::Rechazado(r) => Err(ErrorTarea::Rechazado(r)),
        Respuesta::Ok(v) => Ok(v),
    }
}

/// `acepta_web_desde := MIN(acepta_web_desde, caja_v2_desde)` si el reporte
/// del servidor lo trae (ver state::ajustar_acepta_web_desde).
fn ajustar_caja_v2_desde(db: &Arc<Mutex<Connection>>, reporte: &Value) {
    let Some(m) = reporte.get("caja_v2_desde").and_then(|v| v.as_str()) else { return };
    if let Ok(conn) = db.lock() {
        state::ajustar_desde_respuesta(&conn, Some(m));
    }
}

/// Las respuestas de /sync/refs pueden venir directas o envueltas en `refs`.
fn mapa_refs(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(mut m) => match m.remove("refs") {
            Some(Value::Object(r)) => r,
            Some(otro) => {
                m.insert("refs".into(), otro);
                m
            }
            None => m,
        },
        _ => Map::new(),
    }
}

/// b) Arreglo espejo de las filas web locales.
pub async fn arreglo_espejo(
    db: &Arc<Mutex<Connection>>,
    client: &RemoteClient,
) -> Result<ReporteEspejo, ErrorTarea> {
    let mut rep = ReporteEspejo::default();
    for tabla in apply::TABLAS_ESPEJO {
        let uuids = {
            let conn = db.lock().map_err(|e| e.to_string())?;
            apply::uuids_web_locales(&conn, tabla).map_err(|e| e.to_string())?
        };
        let mut total = EspejoTabla { tabla: tabla.to_string(), ..Default::default() };
        for tanda in uuids.chunks(MAX_UUIDS_REFS) {
            let refs = match client.refs(tabla, tanda).await {
                Ok(Respuesta::NoSoportado) | Ok(Respuesta::Rechazado(_)) => return Err(ErrorTarea::NoSoportado),
                Ok(Respuesta::Ok(v)) => mapa_refs(v),
                Err(e) => {
                    log::warn!("sync: arreglo espejo de {}: {}", tabla, e);
                    rep.errores.push(format!("{tabla}: {e}"));
                    break;
                }
            };
            let mut conn = db.lock().map_err(|e| e.to_string())?;
            let r = apply::aplicar_refs_espejo(&mut conn, tabla, &refs).map_err(|e| e.to_string())?;
            total.filas += r.filas;
            total.celdas_cambiadas += r.celdas_cambiadas;
            total.sin_resolver += r.sin_resolver;
        }
        rep.filas += total.filas;
        rep.celdas_cambiadas += total.celdas_cambiadas;
        rep.sin_resolver += total.sin_resolver;
        rep.tablas.push(total);
    }
    log::info!(
        "sync: arreglo espejo — {} filas web revisadas, {} celdas corregidas, {} sin resolver",
        rep.filas, rep.celdas_cambiadas, rep.sin_resolver
    );
    Ok(rep)
}

/// a + reparación del servidor + b. Marca la reparación como hecha y
/// programa la re-descarga (c). `forzar_mapa` = volver a subir el mapa
/// aunque ya se haya subido (botón manual); el worker no lo repite.
pub async fn reparar_referencias(
    db: &Arc<Mutex<Connection>>,
    client: &RemoteClient,
    device_uuid: &str,
    forzar_mapa: bool,
) -> Result<ReporteReparacion, ErrorTarea> {
    let ya_subido = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        state::leer_tareas(&conn).map_err(|e| e.to_string())?.mapa_ids_subido_at.is_some()
    };
    let mapa = if forzar_mapa || !ya_subido {
        subir_mapa_ids(db, client, device_uuid).await?
    } else {
        ReporteMapa::default()
    };
    let servidor = match reparar_servidor(client, device_uuid).await {
        Ok(v) => v,
        Err(ErrorTarea::Rechazado(r)) => {
            ajustar_caja_v2_desde(db, &r);
            return Err(ErrorTarea::Rechazado(r));
        }
        Err(e) => return Err(e),
    };
    // Antes de programar la re-descarga: con el límite en la hora en que se
    // actualizó el servidor, las ventas web de la ventana del despliegue que
    // ya quedaron cubiertas por un corte se re-acomodan y cuentan.
    ajustar_caja_v2_desde(db, &servidor);
    let espejo = arreglo_espejo(db, client).await?;
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        state::marcar_referencias_reparadas(&conn).map_err(|e| e.to_string())?;
    }
    Ok(ReporteReparacion { mapa, servidor, espejo })
}

/// c) Re-descarga en modo "solo insertar". Avanza hasta `replay_hasta`.
pub async fn correr_replay(
    db: &Arc<Mutex<Connection>>,
    client: &RemoteClient,
    cfg: &state::SyncConfig,
) -> Result<(), String> {
    for _ in 0..TANDAS_REPLAY_POR_CICLO {
        let t = {
            let conn = db.lock().map_err(|e| e.to_string())?;
            state::leer_tareas(&conn).map_err(|e| e.to_string())?
        };
        if !t.replay_pendiente() {
            return Ok(());
        }
        let cursor = t.replay_cursor.clone().unwrap_or_else(|| "0".into());
        let hasta_txt = t.replay_hasta.clone().unwrap_or_else(|| "0".into());
        let hasta = state::cursor_num(&hasta_txt);

        let resp = client.pull(&cursor, cfg.sucursal_id, &cfg.device_uuid).await?;
        // Lo posterior a `hasta` lo trae el pull normal.
        let cambios: Vec<Value> = resp
            .cambios
            .into_iter()
            .filter(|c| {
                c.get("__cursor")
                    .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                    .map(|n| n <= hasta)
                    .unwrap_or(true)
            })
            .collect();

        let siguiente = state::cursor_num(&resp.next_cursor);
        let terminado = !resp.hay_mas || siguiente >= hasta || siguiente <= state::cursor_num(&cursor);
        let nuevo_cursor = if terminado { hasta_txt.clone() } else { resp.next_cursor.clone() };

        let mut conn = db.lock().map_err(|e| e.to_string())?;
        state::ajustar_desde_respuesta(&conn, resp.caja_v2_desde.as_deref());
        if !cambios.is_empty() {
            let r = apply::aplicar_lote_con(&mut conn, &cambios, Modo::Replay, &apply::ahora_local())
                .map_err(|e| e.to_string())?;
            if r.aplicados > 0 || r.aparcados > 0 {
                log::info!(
                    "sync replay: {} insertados, {} ya existían/omitidos, {} a la bandeja (cursor {})",
                    r.aplicados, r.omitidos, r.aparcados, nuevo_cursor
                );
            }
        }
        state::actualizar_replay_cursor(&conn, &nuevo_cursor).map_err(|e| e.to_string())?;
        if terminado {
            log::info!("sync replay: terminado (cursor {})", hasta_txt);
            return Ok(());
        }
    }
    Ok(())
}
