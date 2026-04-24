// sync/apply.rs — Aplicar cambios recibidos del remoto a la BD local.
//
// Usa `sqlite_temp_master` + tabla TEMP `sync_suppress` para que los triggers
// de outbox NO generen entradas (evita loop push → pull → push...).
//
// Política LWW: si la fila local existe y su updated_at >= updated_at del
// cambio entrante, descarta el cambio (local es más nuevo).

use rusqlite::{Connection, Result as SqlResult, OptionalExtension};
use serde_json::Value;

use super::payload::AGGREGATES;

/// Aplica un lote de cambios. El caller debe pasar &mut Connection para poder abrir una tx.
pub fn aplicar_lote(conn: &mut Connection, cambios: &[Value]) -> SqlResult<usize> {
    // Insertar flag para que los triggers de outbox se salten estos cambios.
    conn.execute("INSERT OR IGNORE INTO sync_suppress_flag (id) VALUES (1)", [])?;

    let tx = conn.transaction()?;
    let mut aplicados = 0usize;
    for cambio in cambios {
        match aplicar_uno(&tx, cambio) {
            Ok(true) => aplicados += 1,
            Ok(false) => { /* descartado por LWW */ }
            Err(e) => {
                log::warn!("sync apply error: {} — payload: {}", e, cambio);
            }
        }
    }
    tx.commit()?;

    // Quitar flag para reactivar triggers.
    let _ = conn.execute("DELETE FROM sync_suppress_flag", []);
    Ok(aplicados)
}

fn aplicar_uno(tx: &rusqlite::Transaction, cambio: &Value) -> SqlResult<bool> {
    let obj = cambio.as_object().ok_or_else(|| rusqlite::Error::InvalidQuery)?;
    let tabla = obj.get("__tabla").and_then(|v| v.as_str())
        .ok_or(rusqlite::Error::InvalidQuery)?;
    let operacion = obj.get("__operacion").and_then(|v| v.as_str()).unwrap_or("UPDATE");
    let data = obj.get("__data").and_then(|v| v.as_object())
        .ok_or(rusqlite::Error::InvalidQuery)?;

    let uuid = data.get("uuid").and_then(|v| v.as_str())
        .ok_or(rusqlite::Error::InvalidQuery)?;
    let entrante_updated_at = data.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");

    // LWW: comparar updated_at
    let local_updated: Option<String> = tx.query_row(
        &format!("SELECT updated_at FROM {} WHERE uuid = ? LIMIT 1", tabla),
        [uuid],
        |r| r.get::<_, Option<String>>(0),
    ).optional()?.flatten();

    if let Some(local) = local_updated.as_ref() {
        if local.as_str() >= entrante_updated_at {
            return Ok(false);
        }
    }

    // Aplicar upsert genérico
    match operacion {
        "DELETE" => {
            tx.execute(
                &format!("DELETE FROM {} WHERE uuid = ?", tabla),
                [uuid],
            )?;
        }
        _ => {
            upsert_generico(tx, tabla, data)?;

            // Si es agregado, aplicar hijos
            if let Some((_, hijos_defs)) = AGGREGATES.iter().find(|(p, _)| *p == tabla) {
                if let Some(children) = obj.get("__children").and_then(|v| v.as_object()) {
                    let parent_id: Option<i64> = tx.query_row(
                        &format!("SELECT id FROM {} WHERE uuid = ?", tabla),
                        [uuid],
                        |r| r.get(0),
                    ).optional()?;
                    if let Some(pid) = parent_id {
                        for (tabla_hijo, fk) in *hijos_defs {
                            if let Some(arr) = children.get(*tabla_hijo).and_then(|v| v.as_array()) {
                                // Reemplazar todos los hijos por los que llegan
                                tx.execute(
                                    &format!("DELETE FROM {} WHERE {} = ?", tabla_hijo, fk),
                                    [pid],
                                )?;
                                for hijo in arr {
                                    if let Some(hijo_obj) = hijo.as_object() {
                                        upsert_generico(tx, tabla_hijo, hijo_obj)?;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(true)
}

fn upsert_generico(
    tx: &rusqlite::Transaction,
    tabla: &str,
    data: &serde_json::Map<String, Value>,
) -> SqlResult<()> {
    // No intentamos setear `id` (autoincrement local); lo ignoramos si viene.
    let cols: Vec<&String> = data.keys().filter(|k| k.as_str() != "id").collect();
    if cols.is_empty() { return Ok(()); }

    let col_list = cols.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(",");
    let placeholders = cols.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let updates = cols.iter()
        .filter(|c| c.as_str() != "uuid")
        .map(|c| format!("{}=excluded.{}", c, c))
        .collect::<Vec<_>>().join(",");

    let sql = format!(
        "INSERT INTO {tabla} ({col_list}) VALUES ({placeholders}) \
         ON CONFLICT(uuid) DO UPDATE SET {updates}"
    );

    let params: Vec<rusqlite::types::Value> = cols.iter()
        .map(|c| json_a_sqlite(&data[c.as_str()]))
        .collect();
    let refs: Vec<&dyn rusqlite::ToSql> = params.iter()
        .map(|p| p as &dyn rusqlite::ToSql)
        .collect();
    tx.execute(&sql, rusqlite::params_from_iter(refs.iter()))?;
    Ok(())
}

fn json_a_sqlite(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as SqlV;
    match v {
        Value::Null => SqlV::Null,
        Value::Bool(b) => SqlV::Integer(if *b { 1 } else { 0 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() { SqlV::Integer(i) }
            else if let Some(f) = n.as_f64() { SqlV::Real(f) }
            else { SqlV::Null }
        }
        Value::String(s) => SqlV::Text(s.clone()),
        Value::Array(_) | Value::Object(_) => SqlV::Text(v.to_string()),
    }
}
