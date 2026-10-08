// sync/base.rs — Base de la fusión de 3 vías de productos (tabla sync_base).
//
// La "base" de una fila es la versión en la que este equipo y el servidor
// coincidieron la última vez: lo último que el servidor ACEPTÓ en un push o
// lo último que se APLICÓ desde el servidor. Viaja en el push como `base`;
// con ella el servidor sabe qué campos cambió el escritorio desde entonces y
// aplica la existencia como diferencia (stock = servidor + (local − base)),
// así una venta web y una venta del escritorio del mismo producto cuentan las
// dos, y un precio editado en la web no se pierde porque el escritorio vendió
// ese producto antes de bajar el cambio.
//
// Formato: la fila local completa (mismas columnas e ids LOCALES que `data`
// del push), con `updated_at` normalizado. Solo productos.

use rusqlite::{Connection, OptionalExtension, Result as SqlResult};
use serde_json::Value;

use super::fk;

/// Tablas que mandan `base` en el push.
pub const TABLAS_CON_BASE: &[&str] = &["productos"];

pub fn usa_base(tabla: &str) -> bool {
    TABLAS_CON_BASE.contains(&tabla)
}

/// Quita las llaves internas del payload (`__children`, `__refs`) y normaliza
/// `updated_at`.
fn limpiar(datos: &Value) -> Value {
    let mut v = datos.clone();
    if let Some(o) = v.as_object_mut() {
        o.retain(|k, _| !k.starts_with("__"));
        if let Some(u) = o.get("updated_at").and_then(|x| x.as_str()).map(fk::normalizar_marca) {
            o.insert("updated_at".into(), Value::String(u));
        }
    }
    v
}

/// Base guardada de una fila (None si nunca se guardó o el JSON está roto).
pub fn leer(conn: &Connection, tabla: &str, uuid: &str) -> SqlResult<Option<Value>> {
    let txt: Option<String> = conn
        .query_row(
            "SELECT datos FROM sync_base WHERE tabla = ? AND uuid = ?",
            rusqlite::params![tabla, uuid],
            |r| r.get(0),
        )
        .optional()?;
    Ok(txt.and_then(|t| serde_json::from_str::<Value>(&t).ok()).filter(|v| v.is_object()))
}

/// Guarda (o reemplaza) la base de una fila.
pub fn guardar(conn: &Connection, tabla: &str, uuid: &str, datos: &Value) -> SqlResult<()> {
    if !usa_base(tabla) {
        return Ok(());
    }
    let limpio = limpiar(datos);
    let upd = limpio.get("updated_at").and_then(|x| x.as_str()).map(|s| s.to_string());
    conn.execute(
        "INSERT INTO sync_base (tabla, uuid, datos, updated_at) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(tabla, uuid) DO UPDATE SET datos = excluded.datos, updated_at = excluded.updated_at",
        rusqlite::params![tabla, uuid, limpio.to_string(), upd],
    )?;
    Ok(())
}

/// Guarda como base la fila local tal como quedó (después de aplicar una
/// versión del servidor).
pub fn guardar_fila_local(conn: &Connection, tabla: &str, uuid: &str) -> SqlResult<()> {
    if !usa_base(tabla) {
        return Ok(());
    }
    if let Some(v) = super::payload::fila_a_json(conn, tabla, uuid)? {
        guardar(conn, tabla, uuid, &v)?;
    }
    Ok(())
}
