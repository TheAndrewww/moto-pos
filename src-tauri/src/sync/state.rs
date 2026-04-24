// sync/state.rs — Singleton sync_state: URL del remoto, JWT, cursor de pull.

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
         activo = 1, updated_at = datetime('now') WHERE id = 1",
        rusqlite::params![remote_url, remote_token, sucursal_id],
    )?;
    Ok(())
}

pub fn desactivar(conn: &Connection) -> SqlResult<()> {
    conn.execute(
        "UPDATE sync_state SET activo = 0, updated_at = datetime('now') WHERE id = 1",
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
