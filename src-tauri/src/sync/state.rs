// sync/state.rs — Singleton sync_state: URL del remoto, JWT, cursor de pull.
//
// Sync v2 (migración 015) agrega columnas de control para tareas que corren
// una sola vez: subida del mapa id → uuid, reparación de referencias y
// re-descarga ("replay") de filas web que nunca llegaron.

use rusqlite::{Connection, Result as SqlResult, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    pub device_uuid: String,
    pub sucursal_id: i64,
    pub remote_url: Option<String>,
    pub remote_token: Option<String>,
    pub last_push_at: Option<String>,
    pub last_pull_cursor: Option<String>,
    pub last_pull_at: Option<String>,
    pub activo: bool,
}

pub fn leer(conn: &Connection) -> SqlResult<Option<SyncConfig>> {
    conn.query_row(
        "SELECT device_uuid, sucursal_id, remote_url, remote_token, \
         last_push_at, last_pull_cursor, last_pull_at, activo \
         FROM sync_state WHERE id = 1",
        [],
        |r| Ok(SyncConfig {
            device_uuid: r.get(0)?,
            sucursal_id: r.get(1)?,
            remote_url: r.get(2)?,
            remote_token: r.get(3)?,
            last_push_at: r.get(4)?,
            last_pull_cursor: r.get(5)?,
            last_pull_at: r.get(6)?,
            activo: r.get::<_, i64>(7)? != 0,
        }),
    ).optional()
}

pub fn guardar_credenciales(
    conn: &Connection,
    remote_url: &str,
    remote_token: &str,
    sucursal_id: i64,
) -> SqlResult<()> {
    conn.execute(
        "UPDATE sync_state SET remote_url = ?, remote_token = ?, sucursal_id = ?, \
         activo = 1, updated_at = datetime('now','localtime') WHERE id = 1",
        rusqlite::params![remote_url, remote_token, sucursal_id],
    )?;
    Ok(())
}

pub fn desactivar(conn: &Connection) -> SqlResult<()> {
    conn.execute(
        "UPDATE sync_state SET activo = 0, updated_at = datetime('now','localtime') WHERE id = 1",
        [],
    )?;
    Ok(())
}

pub fn actualizar_push_at(conn: &Connection) -> SqlResult<()> {
    conn.execute(
        "UPDATE sync_state SET last_push_at = datetime('now') WHERE id = 1",
        [],
    )?;
    Ok(())
}

pub fn actualizar_pull(conn: &Connection, cursor: &str) -> SqlResult<()> {
    conn.execute(
        "UPDATE sync_state SET last_pull_cursor = ?, last_pull_at = datetime('now') WHERE id = 1",
        rusqlite::params![cursor],
    )?;
    Ok(())
}

// ─── Tareas únicas del sync v2 ────────────────────────────

/// Estado de las tareas que corren una sola vez (columnas de migración 015).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TareasSync {
    /// Las ventas/movimientos web con fecha anterior nunca se re-acomodan.
    pub acepta_web_desde: Option<String>,
    /// Cuándo se subió completo el mapa id → uuid.
    pub mapa_ids_subido_at: Option<String>,
    /// Cuándo terminó la reparación (servidor + espejo local).
    pub referencias_reparadas_at: Option<String>,
    /// Cursor de la re-descarga de filas web faltantes (solo inserta).
    pub replay_cursor: Option<String>,
    /// Hasta qué cursor llega la re-descarga.
    pub replay_hasta: Option<String>,
}

impl TareasSync {
    /// ¿La re-descarga está programada y aún no termina?
    pub fn replay_pendiente(&self) -> bool {
        match (&self.replay_cursor, &self.replay_hasta) {
            (Some(c), Some(h)) => cursor_num(c) < cursor_num(h),
            _ => false,
        }
    }
}

/// Cursor como número (los cursores del servidor son ids de sync_cursor).
pub fn cursor_num(c: &str) -> i64 {
    c.trim().parse::<i64>().unwrap_or(0)
}

pub fn leer_tareas(conn: &Connection) -> SqlResult<TareasSync> {
    Ok(conn.query_row(
        "SELECT acepta_web_desde, mapa_ids_subido_at, referencias_reparadas_at, \
                replay_cursor, replay_hasta \
         FROM sync_state WHERE id = 1",
        [],
        |r| Ok(TareasSync {
            acepta_web_desde: r.get(0)?,
            mapa_ids_subido_at: r.get(1)?,
            referencias_reparadas_at: r.get(2)?,
            replay_cursor: r.get(3)?,
            replay_hasta: r.get(4)?,
        }),
    ).optional()?.unwrap_or_default())
}

pub fn marcar_mapa_subido(conn: &Connection) -> SqlResult<()> {
    conn.execute(
        "UPDATE sync_state SET mapa_ids_subido_at = datetime('now','localtime') WHERE id = 1",
        [],
    )?;
    Ok(())
}

/// Marca la reparación como hecha y programa la re-descarga (desde el
/// cursor 0 hasta el cursor actual) si nunca se programó.
pub fn marcar_referencias_reparadas(conn: &Connection) -> SqlResult<()> {
    conn.execute(
        "UPDATE sync_state SET referencias_reparadas_at = datetime('now','localtime'), \
                replay_cursor = COALESCE(replay_cursor, '0'), \
                replay_hasta = COALESCE(replay_hasta, COALESCE(last_pull_cursor, '0')) \
          WHERE id = 1",
        [],
    )?;
    Ok(())
}

/// `acepta_web_desde := MIN(acepta_web_desde, caja_v2_desde)`.
///
/// `caja_v2_desde` es la hora (local) en que se actualizó el SERVIDOR; viene
/// en la respuesta del pull y en el reporte de /sync/reparar. Entre esa hora y
/// la actualización de este equipo, el pull viejo no traía las ventas y
/// movimientos web de la caja principal: llegan después con la re-descarga.
/// Bajando el límite a esa hora, si un corte ya cubrió su fecha se
/// re-acomodan al período abierto (cuentan) en vez de perderse. Nunca sube
/// el límite. Devuelve true si lo cambió.
pub fn ajustar_acepta_web_desde(conn: &Connection, caja_v2_desde: &str) -> SqlResult<bool> {
    let marca = super::fk::normalizar_marca(caja_v2_desde.trim());
    if chrono::NaiveDateTime::parse_from_str(&marca, "%Y-%m-%d %H:%M:%S").is_err() {
        log::warn!("sync: caja_v2_desde con formato inesperado ({caja_v2_desde}); se ignora");
        return Ok(false);
    }
    let n = conn.execute(
        "UPDATE sync_state SET acepta_web_desde = ?1 \
          WHERE id = 1 \
            AND (acepta_web_desde IS NULL \
                 OR replace(substr(acepta_web_desde, 1, 19), 'T', ' ') > ?1)",
        rusqlite::params![marca],
    )?;
    if n > 0 {
        log::info!("sync: acepta_web_desde = {marca} (hora en que se actualizó el servidor)");
    }
    Ok(n > 0)
}

/// Aplica `caja_v2_desde` si la respuesta (pull o reporte) lo trae.
pub fn ajustar_desde_respuesta(conn: &Connection, caja_v2_desde: Option<&str>) {
    if let Some(m) = caja_v2_desde.filter(|s| !s.trim().is_empty()) {
        if let Err(e) = ajustar_acepta_web_desde(conn, m) {
            log::warn!("sync: no se pudo ajustar acepta_web_desde: {e}");
        }
    }
}

pub fn actualizar_replay_cursor(conn: &Connection, cursor: &str) -> SqlResult<()> {
    conn.execute(
        "UPDATE sync_state SET replay_cursor = ? WHERE id = 1",
        rusqlite::params![cursor],
    )?;
    Ok(())
}
