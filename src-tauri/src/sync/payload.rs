// sync/payload.rs — Serialización genérica de filas SQLite a JSON.
//
// El sync worker NO necesita conocer el schema de cada tabla: convierte la fila
// completa a un objeto JSON con los nombres de columnas. El servidor remoto
// tiene columnas equivalentes y usa estos objetos directamente.
//
// Sync v2: además de la fila se manda `__refs` = {uuid_fila: {columna_fk:
// uuid_referenciado | null}} para el padre y cada hijo. Los ids enteros
// solo significan algo en este equipo; el servidor traduce por uuid.

use rusqlite::{Connection, Result as SqlResult, types::ValueRef};
use serde_json::{Map, Value};

use super::fk;

/// Serializa una fila de cualquier tabla a JSON usando metadata del prepare.
pub fn fila_a_json(conn: &Connection, tabla: &str, uuid: &str) -> SqlResult<Option<Value>> {
    let sql = format!("SELECT * FROM {} WHERE uuid = ? LIMIT 1", tabla);
    let mut stmt = conn.prepare(&sql)?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([uuid])?;
    let Some(row) = rows.next()? else { return Ok(None); };

    let mut obj = Map::new();
    for (i, name) in col_names.iter().enumerate() {
        obj.insert(name.clone(), valor_sqlite_a_json(row.get_ref(i)?));
    }
    Ok(Some(Value::Object(obj)))
}

/// Serializa todas las filas hijas de un agregado (por fk_column = parent_id).
pub fn hijos_a_json(
    conn: &Connection,
    tabla_hijo: &str,
    fk_columna: &str,
    parent_id: i64,
) -> SqlResult<Vec<Value>> {
    let sql = format!("SELECT * FROM {} WHERE {} = ? ORDER BY id", tabla_hijo, fk_columna);
    let mut stmt = conn.prepare(&sql)?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([parent_id])?;
    let mut lista = Vec::new();
    while let Some(row) = rows.next()? {
        let mut obj = Map::new();
        for (i, name) in col_names.iter().enumerate() {
            obj.insert(name.clone(), valor_sqlite_a_json(row.get_ref(i)?));
        }
        lista.push(Value::Object(obj));
    }
    Ok(lista)
}

/// Construye el payload completo para una fila (incluyendo hijos si es
/// agregado) con sus referencias por uuid en `__refs`.
pub fn construir_payload(conn: &Connection, tabla: &str, uuid: &str) -> SqlResult<Option<Value>> {
    let Some(Value::Object(mut obj)) = fila_a_json(conn, tabla, uuid)? else { return Ok(None); };

    let mut refs = Map::new();
    refs.insert(uuid.to_string(), Value::Object(fk::refs_de_fila(conn, tabla, &obj)));

    // Si es agregado, agregar "__children" con sus hijos.
    let hijos_defs = fk::hijos_de(tabla);
    if !hijos_defs.is_empty() {
        let parent_id = obj.get("id").and_then(|v| v.as_i64()).unwrap_or(0);

        let mut children = Map::new();
        for (tabla_hijo, columna_fk) in hijos_defs {
            let hijos = hijos_a_json(conn, tabla_hijo, columna_fk, parent_id)?;
            for h in &hijos {
                if let Some(ho) = h.as_object() {
                    if let Some(hu) = ho.get("uuid").and_then(|v| v.as_str()) {
                        refs.insert(hu.to_string(), Value::Object(fk::refs_de_fila(conn, tabla_hijo, ho)));
                    }
                }
            }
            children.insert(tabla_hijo.to_string(), Value::Array(hijos));
        }
        obj.insert("__children".to_string(), Value::Object(children));
    }

    obj.insert("__refs".to_string(), Value::Object(refs));
    Ok(Some(Value::Object(obj)))
}

fn valor_sqlite_a_json(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Number(i.into()),
        ValueRef::Real(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => {
            use base64::Engine;
            Value::String(base64::engine::general_purpose::STANDARD.encode(b))
        }
    }
}
