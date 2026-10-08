// sync/fk.rs — Llaves foráneas (FK) de las tablas sincronizadas.
//
// Cada base numera sus filas por su cuenta: `usuario_id = 1` es "Dueño" en
// este equipo y "Andrew Web" en el servidor. La identidad real de una fila es
// su `uuid`, así que las FK viajan como uuids (`refs`) y cada lado las
// traduce a sus propios ids.
//
// El mapa (qué columna apunta a qué tabla) es el MISMO archivo que usa el
// servidor: se incluye tal cual para que los dos lados no se desalineen.

include!("../../../server-remoto/src/sync_fk_map.rs");

use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};

/// Tablas de dinero cuyo historial es SOLO del escritorio: nunca se importan
/// del servidor (la cadena de cortes/aperturas de la caja principal tiene un
/// único escritor).
pub const TABLAS_CAJA_NUNCA: &[&str] = &[
    "cortes", "corte_denominaciones", "corte_vendedores", "aperturas_caja",
];

/// Tablas transaccionales donde el escritorio es la autoridad de sus propias
/// filas: del servidor solo se aceptan las que hizo la web.
pub const TABLAS_TRANSACCIONALES: &[&str] = &[
    "ventas", "devoluciones", "recepciones", "ordenes_pedido",
    "presupuestos", "transferencias",
];

/// Catálogo: último-que-escribe-gana en los dos sentidos.
pub const TABLAS_CATALOGO: &[&str] = &[
    "sucursales", "categorias", "proveedores", "clientes", "usuarios", "productos",
];

/// ¿La tabla es una tabla sincronizada conocida? (protege los nombres de
/// tabla que llegan por la red antes de pegarlos en SQL).
pub fn es_tabla_sync(tabla: &str) -> bool {
    FK_MAP.iter().any(|(t, _)| *t == tabla)
}

/// ¿Es tabla hija de algún agregado? (venta_detalle, …)
pub fn es_tabla_hija(tabla: &str) -> bool {
    AGGREGATES.iter().any(|(_, hijos)| hijos.iter().any(|(h, _)| *h == tabla))
}

/// ¿La fila la hizo la web? Por formato de uuid; en audit_log por `origen`
/// (el servidor genera ahí también 32 hex por default).
pub fn es_fila_web(tabla: &str, uuid: &str, data: &Map<String, Value>) -> bool {
    if tabla == "audit_log" {
        return data.get("origen").and_then(|v| v.as_str()) == Some("WEB");
    }
    !es_uuid_escritorio(uuid)
}

/// Tabla destino de una celda FK, o None si la celda no se traduce (Paso,
/// Padre, o polimórfica hacia una tabla que no viaja).
pub fn tabla_destino(kind: RefKind, fila: &Map<String, Value>) -> Option<&'static str> {
    match kind {
        RefKind::Tabla(t) => Some(t),
        RefKind::Polimorfico(col) => fila
            .get(col)
            .and_then(|v| v.as_str())
            .and_then(tabla_de_auditoria),
        RefKind::Padre(_) | RefKind::Paso(_) => None,
    }
}

/// uuid de la fila `id` de `tabla` (incluye borradas). None si no existe o
/// no tiene uuid.
pub fn uuid_de_id(conn: &Connection, tabla: &str, id: i64) -> Option<String> {
    conn.query_row(
        &format!("SELECT uuid FROM {tabla} WHERE id = ?"),
        [id],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .ok()
    .flatten()
    .flatten()
}

/// id local de la fila con `uuid` en `tabla` (incluye borradas).
pub fn id_local_de_uuid(conn: &Connection, tabla: &str, uuid: &str) -> Option<i64> {
    conn.query_row(
        &format!("SELECT id FROM {tabla} WHERE uuid = ?"),
        [uuid],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Referencias de una fila local: `{columna: uuid | null}` para cada FK de
/// tipo Tabla/Polimórfico. Si la fila referenciada no existe aquí, la llave
/// se omite ("no sé" — el servidor usa su respaldo).
pub fn refs_de_fila(conn: &Connection, tabla: &str, fila: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (col, kind) in fks_de(tabla) {
        let Some(destino) = tabla_destino(*kind, fila) else { continue };
        let Some(valor) = fila.get(*col) else { continue };
        match valor {
            Value::Null => {
                out.insert(col.to_string(), Value::Null);
            }
            v => {
                let id = v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()));
                if let Some(id) = id {
                    if let Some(u) = uuid_de_id(conn, destino, id) {
                        out.insert(col.to_string(), Value::String(u));
                    }
                }
            }
        }
    }
    out
}

/// ¿La FK se traduce por uuid (Tabla / Polimórfico)? Padre y Paso no.
pub fn es_fk_traducible(kind: RefKind) -> bool {
    matches!(kind, RefKind::Tabla(_) | RefKind::Polimorfico(_))
}

/// Quita de `fila` las columnas FK que se traducen por uuid (Tabla /
/// Polimórfico) de `tabla`. Devuelve los nombres de las que quitó.
pub fn quitar_fks_traducibles(tabla: &str, fila: &mut Map<String, Value>) -> Vec<&'static str> {
    let mut quitadas = Vec::new();
    for (col, kind) in fks_de(tabla) {
        if es_fk_traducible(*kind) && fila.remove(*col).is_some() {
            quitadas.push(*col);
        }
    }
    quitadas
}

/// Normaliza una marca de tiempo para compararla: primeros 19 caracteres
/// con 'T' → ' ' ('2026-10-06T19:38:03.12-06' → '2026-10-06 19:38:03').
pub fn normalizar_marca(s: &str) -> String {
    s.chars().take(19).collect::<String>().replace('T', " ")
}
