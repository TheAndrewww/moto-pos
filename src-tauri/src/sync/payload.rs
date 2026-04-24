// sync/payload.rs — Serialización genérica de filas SQLite a JSON.
//
// El sync worker NO necesita conocer el schema de cada tabla: convierte la fila
// completa a un objeto JSON con los nombres de columnas. El servidor remoto
// tiene columnas equivalentes y usa estos objetos directamente.

use rusqlite::{Connection, Result as SqlResult, types::ValueRef};
use serde_json::{Map, Value};

/// Tablas que sincronizan como "agregado": la fila padre lleva sus hijos.
/// `(tabla_padre, &[(tabla_hijo, columna_fk)])`.
pub const AGGREGATES: &[(&str, &[(&str, &str)])] = &[
    ("ventas",         &[("venta_detalle",        "venta_id")]),
    ("presupuestos",   &[("presupuesto_detalle",  "presupuesto_id")]),
    ("ordenes_pedido", &[("orden_pedido_detalle", "orden_id")]),
    ("recepciones",    &[("recepcion_detalle",    "recepcion_id")]),
    ("cortes",         &[("corte_denominaciones", "corte_id"),
                         ("corte_vendedores",     "corte_id")]),
    ("devoluciones",   &[("devolucion_detalle",   "devolucion_id")]),
    ("transferencias", &[("transferencia_detalle","transferencia_id")]),
];

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
    let sql = format!("SELECT * FROM {} WHERE {} = ?", tabla_hijo, fk_columna);
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

/// Construye el payload completo para una fila (incluyendo hijos si es agregado).
pub fn construir_payload(conn: &Connection, tabla: &str, uuid: &str) -> SqlResult<Option<Value>> {
    let Some(fila) = fila_a_json(conn, tabla, uuid)? else { return Ok(None); };

    // Si es agregado, agregar array "children" con sus hijos.
    if let Some((_, hijos_defs)) = AGGREGATES.iter().find(|(p, _)| *p == tabla) {
        let parent_id = fila.get("id")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        let mut children = Map::new();
        for (tabla_hijo, fk) in *hijos_defs {
            let hijos = hijos_a_json(conn, tabla_hijo, fk, parent_id)?;
            children.insert(tabla_hijo.to_string(), Value::Array(hijos));
        }

        if let Value::Object(mut obj) = fila {
            obj.insert("__children".to_string(), Value::Object(children));
            return Ok(Some(Value::Object(obj)));
        }
    }

    Ok(Some(fila))
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
