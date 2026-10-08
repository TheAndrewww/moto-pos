// sync/inbox.rs — Bandeja de cambios recibidos que no se pudieron aplicar.
//
// Antes, un cambio del servidor que fallaba solo se escribía en el log y el
// cursor avanzaba: se perdía para siempre. Ahora queda en `sync_inbox` con
// su payload y se reintenta:
//   - 'pendiente_local': cuando el cambio local de esa fila ya subió (o el
//     servidor lo rechazó por tener una versión más nueva: en ese caso el
//     push anota la fila aquí, sin payload, para pedir la versión del
//     servidor).
//   - 'error' / 'sin_fk': con espera creciente (5 min … 6 h).
// Antes de reintentar se pide la versión actual al servidor (GET /sync/fila);
// si el servidor no tiene ese endpoint, se usa el payload guardado. Si la
// llamada falla (sin red, tiempo de espera, error del servidor), una entrada
// 'pendiente_local' espera al siguiente ciclo (su payload puede ser más viejo
// que una venta de aquí que ya subió); las 'error'/'sin_fk' usan el
// guardado. Lo que lleva más de 30 días se descarta con una advertencia en
// el log.

use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{apply, client::{RemoteClient, Respuesta}};

/// Días que un cambio puede esperar en la bandeja antes de descartarse.
pub const DIAS_MAXIMOS: i64 = 30;
/// Cambios reintentados por ciclo.
const REINTENTOS_POR_CICLO: i64 = 50;

/// Fila de la bandeja para la UI (sin payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilaInbox {
    pub id: i64,
    pub tabla: String,
    pub uuid: String,
    pub motivo: String,
    pub error: Option<String>,
    pub intentos: i64,
    pub proximo_intento: Option<String>,
    pub created_at: String,
}

pub fn listar(conn: &Connection, limite: i64) -> rusqlite::Result<Vec<FilaInbox>> {
    let mut stmt = conn.prepare(
        "SELECT id, tabla, uuid, motivo, error, intentos, proximo_intento, created_at \
         FROM sync_inbox ORDER BY intentos DESC, id ASC LIMIT ?",
    )?;
    let v = stmt
        .query_map([limite], |r| {
            Ok(FilaInbox {
                id: r.get(0)?,
                tabla: r.get(1)?,
                uuid: r.get(2)?,
                motivo: r.get(3)?,
                error: r.get(4)?,
                intentos: r.get(5)?,
                proximo_intento: r.get(6)?,
                created_at: r.get(7)?,
            })
        })?
        .collect();
    v
}

pub fn contar(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM sync_inbox", [], |r| r.get(0))
}

#[derive(Debug, Clone, Serialize)]
pub struct ConteoMotivo {
    pub motivo: String,
    pub cantidad: i64,
}

pub fn contar_por_motivo(conn: &Connection) -> rusqlite::Result<Vec<ConteoMotivo>> {
    let mut stmt = conn.prepare(
        "SELECT motivo, COUNT(*) FROM sync_inbox GROUP BY motivo ORDER BY motivo",
    )?;
    let v = stmt
        .query_map([], |r| Ok(ConteoMotivo { motivo: r.get(0)?, cantidad: r.get(1)? }))?
        .collect();
    v
}

/// Descarta lo que lleva más de 30 días esperando. Devuelve cuántos.
pub fn purgar_vencidos(conn: &Connection, ahora: &str) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT tabla, uuid, motivo FROM sync_inbox \
         WHERE created_at < datetime(?, ?)",
    )?;
    let limite = format!("-{DIAS_MAXIMOS} days");
    let viejos: Vec<(String, String, String)> = stmt
        .query_map(rusqlite::params![ahora, limite], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (t, u, m) in &viejos {
        log::warn!(
            "sync inbox: se descarta {} {} ({}) tras {} días sin poder aplicarse",
            t, u, m, DIAS_MAXIMOS
        );
    }
    conn.execute(
        "DELETE FROM sync_inbox WHERE created_at < datetime(?, ?)",
        rusqlite::params![ahora, limite],
    )
}

/// El servidor rechazó un cambio local por tener una versión más nueva
/// ('remoto_mas_nuevo'). Si esa versión la escribió este mismo equipo (eco)
/// o ya pasó el cursor, el pull nunca la vuelve a traer y los dos lados se
/// quedan distintos para siempre. Se anota en la bandeja SIN payload como
/// 'pendiente_local': el renglón rechazado del outbox no la detiene, así que
/// el siguiente reintento pide la fila al servidor (GET /sync/fila) y la
/// aplica; al aplicarse, el renglón del outbox queda resuelto.
pub fn pedir_version_servidor(conn: &Connection, tabla: &str, uuid: &str, ahora: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sync_inbox (tabla, uuid, motivo, error, payload, intentos, proximo_intento, created_at) \
         VALUES (?1, ?2, ?3, ?4, NULL, 0, ?5, ?5) \
         ON CONFLICT(tabla, uuid) DO UPDATE SET \
            motivo = excluded.motivo, error = excluded.error, proximo_intento = excluded.proximo_intento",
        rusqlite::params![
            tabla,
            uuid,
            apply::MOTIVO_PENDIENTE_LOCAL,
            "remoto_mas_nuevo: se pide la versión del servidor",
            ahora
        ],
    )?;
    Ok(())
}

/// Un cambio de la bandeja listo para reintentar.
#[derive(Debug, Clone)]
pub struct Candidato {
    pub tabla: String,
    pub uuid: String,
    pub motivo: String,
    /// Payload guardado al recibirlo (None en las entradas que solo piden la
    /// versión del servidor).
    pub payload: Option<String>,
}

/// Cambios listos para reintentar.
pub fn candidatos(conn: &Connection, ahora: &str, limite: i64) -> rusqlite::Result<Vec<Candidato>> {
    let mut stmt = conn.prepare(
        "SELECT i.tabla, i.uuid, i.motivo, i.payload FROM sync_inbox i \
         WHERE (i.motivo = 'pendiente_local' AND NOT EXISTS ( \
                  SELECT 1 FROM sync_outbox o \
                   WHERE o.tabla = i.tabla AND o.uuid = i.uuid AND o.synced_at IS NULL \
                     AND COALESCE(o.ultimo_error, '') NOT LIKE '%remoto_mas_nuevo%')) \
            OR (i.motivo <> 'pendiente_local' \
                AND (i.proximo_intento IS NULL OR i.proximo_intento <= ?)) \
         ORDER BY i.id LIMIT ?",
    )?;
    let v = stmt
        .query_map(rusqlite::params![ahora, limite], |r| {
            Ok(Candidato { tabla: r.get(0)?, uuid: r.get(1)?, motivo: r.get(2)?, payload: r.get(3)? })
        })?
        .collect();
    v
}

/// Reintenta lo que toca de la bandeja. Devuelve cuántos se aplicaron.
pub async fn reintentar(db: &Arc<Mutex<Connection>>, client: &RemoteClient) -> Result<usize, String> {
    let ahora = apply::ahora_local();
    let lista = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        purgar_vencidos(&conn, &ahora).map_err(|e| e.to_string())?;
        candidatos(&conn, &ahora, REINTENTOS_POR_CICLO).map_err(|e| e.to_string())?
    };
    if lista.is_empty() {
        return Ok(0);
    }

    let mut cambios: Vec<Value> = Vec::with_capacity(lista.len());
    let mut sin_payload: Vec<(String, String)> = Vec::new();
    let mut usar_guardado = false; // el servidor no tiene /sync/fila
    for Candidato { tabla, uuid, motivo, payload } in lista {
        let mut cambio: Option<Value> = None;
        // Solo un "no existe" del servidor permite descartar una entrada sin
        // payload; sin red o con un servidor sin /sync/fila se espera.
        let mut servidor_no_la_tiene = false;
        // /sync/fila falló (sin red, tiempo de espera, error del servidor).
        let mut sin_respuesta = false;
        if !usar_guardado {
            match client.fila(&tabla, &uuid).await {
                Ok(Respuesta::Ok(Some(c))) => cambio = Some(c),
                Ok(Respuesta::Ok(None)) => servidor_no_la_tiene = true,
                Ok(Respuesta::NoSoportado) | Ok(Respuesta::Rechazado(_)) => usar_guardado = true,
                Err(e) => {
                    sin_respuesta = true;
                    log::debug!("sync inbox: /sync/fila {} {}: {}", tabla, uuid, e);
                }
            }
        }
        // 'pendiente_local': el payload guardado es la versión del servidor
        // de ANTES de que subiera el cambio local (p. ej. una venta de este
        // equipo ya aplicada allá). Si /sync/fila no contestó, aplicarlo
        // podría deshacer aquí esa venta (con una marca igual o más nueva que
        // la local, gana) y el siguiente push se la quitaría también al
        // servidor: se espera al siguiente ciclo. Solo un servidor SIN
        // /sync/fila usa el guardado (no hay de otra).
        if sin_respuesta && motivo == apply::MOTIVO_PENDIENTE_LOCAL {
            continue;
        }
        if cambio.is_none() {
            cambio = payload.as_deref().and_then(|p| serde_json::from_str::<Value>(p).ok());
        }
        match cambio {
            Some(c) => cambios.push(c),
            None if servidor_no_la_tiene => sin_payload.push((tabla, uuid)),
            None => {}
        }
    }

    let mut conn = db.lock().map_err(|e| e.to_string())?;
    for (t, u) in &sin_payload {
        log::warn!("sync inbox: {} {}: el servidor ya no tiene la fila y no hay payload guardado; se descarta", t, u);
        let _ = conn.execute(
            "DELETE FROM sync_inbox WHERE tabla = ? AND uuid = ?",
            rusqlite::params![t, u],
        );
    }
    if cambios.is_empty() {
        return Ok(0);
    }
    let r = apply::aplicar_lote(&mut conn, &cambios).map_err(|e| e.to_string())?;
    if r.aplicados + r.omitidos > 0 {
        log::info!(
            "sync inbox: {} aplicados, {} resueltos sin cambios, {} siguen pendientes",
            r.aplicados, r.omitidos, r.aparcados
        );
    }
    Ok(r.aplicados)
}
