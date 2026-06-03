// sync/outbox.rs — Lectura y gestión de la cola de cambios salientes.

use rusqlite::{Connection, Result as SqlResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CambioPendiente {
    pub id: i64,
    pub tabla: String,
    pub uuid: String,
    pub operacion: String,   // 'UPDATE' | 'DELETE'
    pub created_at: String,
    pub intentos: i64,
}

/// Lee hasta `limite` entradas pendientes (synced_at IS NULL).
///
/// ORDER BY `intentos ASC, id ASC` (no solo `id ASC`) para evitar el
/// bug de "una fila mala bloquea la cola entera":
///
/// Si una fila vieja falla cada vez que se intenta empujar (foreign key
/// inválido, conflicto LWW persistente, malformación), el worker la
/// seguía trayendo en cada ciclo porque tenía el id más bajo. Las nuevas
/// filas (insertadas después) NUNCA llegaban a tocarse aunque estuvieran
/// perfectamente bien. Esto causa que el sync "se detenga" desde la
/// fecha en que ocurrió el error persistente.
///
/// Con `intentos ASC` primero: las filas con `intentos=0` (recién
/// encoladas, nunca probadas) tienen prioridad. Las falladas siguen
/// reintentándose pero solo después de drenar las nuevas. Ninguna fila
/// se pierde y la cola avanza.
pub fn pendientes(conn: &Connection, limite: i64) -> SqlResult<Vec<CambioPendiente>> {
    let mut stmt = conn.prepare(
        "SELECT id, tabla, uuid, operacion, created_at, intentos \
         FROM sync_outbox \
         WHERE synced_at IS NULL \
         ORDER BY intentos ASC, id ASC \
         LIMIT ?",
    )?;
    let rows = stmt.query_map([limite], |r| {
        Ok(CambioPendiente {
            id: r.get(0)?,
            tabla: r.get(1)?,
            uuid: r.get(2)?,
            operacion: r.get(3)?,
            created_at: r.get(4)?,
            intentos: r.get(5)?,
        })
    })?;
    rows.collect()
}

/// Marca un lote como sincronizado exitosamente.
pub fn marcar_sincronizados(conn: &Connection, ids: &[i64]) -> SqlResult<usize> {
    if ids.is_empty() { return Ok(0); }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "UPDATE sync_outbox SET synced_at = datetime('now') WHERE id IN ({})",
        placeholders
    );
    let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
    conn.execute(&sql, rusqlite::params_from_iter(params.iter()))
}

/// Marca una entrada con error, incrementa intentos. Se reintenta en el siguiente ciclo.
pub fn marcar_error(conn: &Connection, id: i64, error: &str) -> SqlResult<()> {
    conn.execute(
        "UPDATE sync_outbox SET intentos = intentos + 1, ultimo_error = ? WHERE id = ?",
        rusqlite::params![error, id],
    )?;
    Ok(())
}

/// Limpia entradas sincronizadas de más de N días.
pub fn limpiar_antiguos(conn: &Connection, dias: i64) -> SqlResult<usize> {
    conn.execute(
        &format!(
            "DELETE FROM sync_outbox WHERE synced_at IS NOT NULL \
             AND synced_at < datetime('now', '-{} days')",
            dias
        ),
        [],
    )
}

/// Cuenta de filas pendientes (útil para UI).
pub fn contar_pendientes(conn: &Connection) -> SqlResult<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM sync_outbox WHERE synced_at IS NULL",
        [],
        |r| r.get(0),
    )
}
