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

/// Marca UNA entrada como sincronizada solo si no cambió desde que se leyó
/// para el push (`created_at` igual). Si la fila se volvió a modificar
/// durante la llamada HTTP, el trigger movió `created_at` y el renglón sigue
/// pendiente (el cambio nuevo se sube en el siguiente ciclo). Devuelve true
/// si la marcó.
pub fn marcar_sincronizado_si(conn: &Connection, id: i64, created_at: &str) -> SqlResult<bool> {
    let n = conn.execute(
        "UPDATE sync_outbox SET synced_at = datetime('now') \
          WHERE id = ? AND created_at = ? AND synced_at IS NULL",
        rusqlite::params![id, created_at],
    )?;
    Ok(n > 0)
}

/// Marca una entrada con error e incrementa intentos (se reintenta en el
/// siguiente ciclo), solo si el renglón no cambió desde que se leyó: un
/// cambio nuevo durante la llamada no hereda el error del anterior (con
/// 'remoto_mas_nuevo' heredado, la guardia del pull lo dejaría pisar).
/// Devuelve true si lo marcó.
pub fn marcar_error_si(conn: &Connection, id: i64, created_at: &str, error: &str) -> SqlResult<bool> {
    let n = conn.execute(
        "UPDATE sync_outbox SET intentos = intentos + 1, ultimo_error = ? \
          WHERE id = ? AND created_at = ? AND synced_at IS NULL",
        rusqlite::params![error, id, created_at],
    )?;
    Ok(n > 0)
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxErrorFila {
    pub id: i64,
    pub tabla: String,
    pub uuid: String,
    pub operacion: String,
    pub intentos: i64,
    pub ultimo_error: Option<String>,
    pub created_at: String,
    /// 'outbox' (cambio local que no sube) | 'inbox' (cambio recibido que
    /// no se pudo aplicar). Los ids de cada fuente son independientes.
    #[serde(default = "fuente_outbox")]
    pub fuente: String,
    /// Solo inbox: 'error' | 'pendiente_local' | 'sin_fk'.
    #[serde(default)]
    pub motivo: Option<String>,
}

fn fuente_outbox() -> String { "outbox".to_string() }

/// Lee las filas pendientes que han fallado al menos una vez (intentos > 0
/// y ultimo_error IS NOT NULL). Ordenadas por intentos DESC para mostrar
/// primero las que más han fallado.
pub fn errores(conn: &Connection, limite: i64) -> SqlResult<Vec<OutboxErrorFila>> {
    let mut stmt = conn.prepare(
        "SELECT id, tabla, uuid, operacion, intentos, ultimo_error, created_at \
         FROM sync_outbox \
         WHERE synced_at IS NULL AND intentos > 0 \
         ORDER BY intentos DESC, id ASC \
         LIMIT ?",
    )?;
    let rows = stmt.query_map([limite], |r| {
        Ok(OutboxErrorFila {
            id: r.get(0)?,
            tabla: r.get(1)?,
            uuid: r.get(2)?,
            operacion: r.get(3)?,
            intentos: r.get(4)?,
            ultimo_error: r.get(5)?,
            created_at: r.get(6)?,
            fuente: fuente_outbox(),
            motivo: None,
        })
    })?;
    rows.collect()
}

/// Errores del outbox seguidos de los cambios recibidos que esperan en la
/// bandeja de entrada (sync_inbox, sin payload). Lo usa la página de
/// Sincronización.
pub fn errores_con_bandeja(conn: &Connection, limite: i64) -> SqlResult<Vec<OutboxErrorFila>> {
    let mut lista = errores(conn, limite)?;
    for f in super::inbox::listar(conn, limite)? {
        lista.push(OutboxErrorFila {
            id: f.id,
            tabla: f.tabla,
            uuid: f.uuid,
            operacion: "RECIBIR".to_string(),
            intentos: f.intentos,
            ultimo_error: f.error,
            created_at: f.created_at,
            fuente: "inbox".to_string(),
            motivo: Some(f.motivo),
        });
    }
    Ok(lista)
}

/// Resetea intentos y borra ultimo_error de TODAS las filas pendientes
/// fallidas. Las próximas pasadas del worker las reintentarán como si fueran
/// nuevas. Devuelve cuántas filas se resetearon.
pub fn reintentar_fallidos(conn: &Connection) -> SqlResult<usize> {
    conn.execute(
        "UPDATE sync_outbox \
            SET intentos = 0, ultimo_error = NULL, created_at = datetime('now') \
          WHERE synced_at IS NULL AND intentos > 0",
        [],
    )
}

/// Marca filas específicas como sincronizadas SIN haber sido enviadas.
/// Úsalo para descartar filas problemáticas que bloquean la cola y cuya
/// pérdida es aceptable (ej. movimientos efímeros). Operación destructiva
/// — solo debería invocarse desde UI con confirmación.
pub fn descartar(conn: &Connection, ids: &[i64]) -> SqlResult<usize> {
    if ids.is_empty() { return Ok(0); }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "UPDATE sync_outbox SET synced_at = datetime('now') \
          WHERE id IN ({})",
        placeholders
    );
    let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
    conn.execute(&sql, rusqlite::params_from_iter(params.iter()))
}
