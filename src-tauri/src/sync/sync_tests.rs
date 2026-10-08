// sync/sync_tests.rs — Pruebas del sync v2 del lado escritorio.
//
// Cada prueba levanta una base SQLite en memoria con el MISMO esquema,
// migraciones y datos semilla que usa el POS (SCHEMA_V1 → migraciones →
// SEED_DATA) y simula lo que manda el servidor.
//
// Escenario base: aquí 1 = Dueño, 2 = Nancy. En el servidor 1 = 'Andrew Web'
// (solo web), 2 = Dueño, 3 = Nancy: los ids no coinciden y solo el uuid
// identifica a cada quien.
//
// Correr con:  cargo test --lib sync_tests
// La prueba sobre la copia de la BD de desarrollo necesita
// POS_DB_COPIA_DEV=/ruta/a/una/copia.db (si no está, se salta).

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::db::migrations::aplicar_migraciones;
use crate::db::schema::{SCHEMA_V1, SEED_DATA};

use super::apply::{self, aplicar_lote_con, Modo};
use super::{fk, inbox, outbox, payload};

// ─── Utilidades ───────────────────────────────────────────

const AHORA: &str = "2026-10-06 12:00:00";
const DESDE: &str = "2026-10-01 00:00:00";

fn hex(n: u64) -> String {
    format!("{:032x}", n)
}

fn web(n: u64) -> String {
    format!("019dd079-a8fb-7bd0-88ab-{:012x}", n)
}

const U_DUENO: &str = "45b6808b00000000000000000000aaaa";
const U_NANCY: &str = "4758fab900000000000000000000bbbb";
const P_BALATA: &str = "0000000000000000000000000000c001";

fn nueva_db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(SCHEMA_V1).unwrap();
    aplicar_migraciones(&db).unwrap();
    db.execute_batch(SEED_DATA).unwrap();
    db.execute(
        "INSERT INTO usuarios (id, nombre_completo, nombre_usuario, pin, password_hash, rol_id, activo, uuid) \
         VALUES (1, 'Dueño', 'admin', 'x', 'x', 1, 1, ?1), (2, 'Nancy', 'nancy', 'x', 'x', 2, 1, ?2)",
        [U_DUENO, U_NANCY],
    ).unwrap();
    db.execute(
        "INSERT INTO productos (id, codigo, nombre, precio_venta, stock_actual, uuid, updated_at) \
         VALUES (1, 'P1', 'Balata', 100, 1000, ?1, '2026-10-01 08:00:00')",
        [P_BALATA],
    ).unwrap();
    db.execute("UPDATE sync_state SET acepta_web_desde = ?1 WHERE id = 1", [DESDE]).unwrap();
    limpiar_outbox(&db);
    db
}

/// Marca todo el outbox como subido (para que la guardia de "cambio local
/// pendiente" no interfiera con la preparación de la prueba).
fn limpiar_outbox(db: &Connection) {
    db.execute("UPDATE sync_outbox SET synced_at = datetime('now') WHERE synced_at IS NULL", []).unwrap();
}

fn aplicar(db: &mut Connection, cambios: &[Value]) -> apply::ResultadoLote {
    aplicar_lote_con(db, cambios, Modo::Normal, AHORA).unwrap()
}

fn cambio(tabla: &str, data: Value) -> Value {
    json!({ "__tabla": tabla, "__operacion": "UPSERT", "__data": data })
}

fn uuid_de(db: &Connection, tabla: &str, id: i64) -> String {
    db.query_row(&format!("SELECT uuid FROM {tabla} WHERE id = ?"), [id], |r| r.get(0)).unwrap()
}

fn i64_de(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn opt_txt(db: &Connection, sql: &str) -> Option<String> {
    db.query_row(sql, [], |r| r.get::<_, Option<String>>(0)).optional().unwrap().flatten()
}

fn bandera(db: &Connection) -> i64 {
    i64_de(db, "SELECT COUNT(*) FROM sync_suppress_flag")
}

fn inbox_motivo(db: &Connection, tabla: &str, uuid: &str) -> Option<String> {
    db.query_row(
        "SELECT motivo FROM sync_inbox WHERE tabla = ? AND uuid = ?",
        [tabla, uuid],
        |r| r.get(0),
    ).optional().unwrap()
}

/// Venta web (como la manda el servidor): ids del servidor en las FK.
fn venta_web(uuid: &str, folio: &str, fecha: &str, total: f64, caja: Option<&str>) -> Map<String, Value> {
    let mut v = json!({
        "id": 999, "folio": folio, "usuario_id": 2, "cliente_id": null,
        "subtotal": total, "descuento": 0, "total": total, "metodo_pago": "efectivo",
        "monto_recibido": total, "cambio": 0, "anulada": false, "anulada_por": null,
        "motivo_anulacion": null, "fecha": fecha, "synced_at": null, "sucursal_id": 1,
        "uuid": uuid, "updated_at": fecha, "deleted_at": null, "origen": "web",
        "registrado_at": null, "columna_solo_servidor": "x",
    }).as_object().unwrap().clone();
    if let Some(c) = caja {
        v.insert("caja".into(), json!(c));
    }
    v
}

fn detalle_web(uuid: &str, venta_server_id: i64, producto_server_id: i64, total: f64) -> Value {
    json!({
        "id": 5000, "venta_id": venta_server_id, "producto_id": producto_server_id,
        "cantidad": 1, "precio_original": total, "descuento_porcentaje": 0,
        "descuento_monto": 0, "precio_final": total, "subtotal": total,
        "autorizado_por": null, "uuid": uuid,
    })
}

/// Cambio de venta web con hijos y refs por uuid (servidor v2).
fn cambio_venta_web(uuid: &str, folio: &str, fecha: &str, total: f64) -> Value {
    let det = web(9000 + total as u64);
    json!({
        "__tabla": "ventas", "__operacion": "UPSERT",
        "__data": Value::Object(venta_web(uuid, folio, fecha, total, Some("principal"))),
        "__children": { "venta_detalle": [detalle_web(&det, 999, 77, total)] },
        "__refs": {
            uuid: { "usuario_id": U_DUENO, "cliente_id": null, "anulada_por": null },
            det: { "producto_id": P_BALATA, "autorizado_por": null },
        },
    })
}

fn corte_principal(db: &Connection, fin: &str) {
    db.execute(
        "INSERT INTO cortes (tipo, usuario_id, fecha_inicio, fecha_fin, created_at, fondo_siguiente) \
         VALUES ('DIA', 1, '2026-10-05 08:00:00', ?1, ?1, 0)",
        [fin],
    ).unwrap();
    limpiar_outbox(db);
}

// ─── Mapa de FK ───────────────────────────────────────────

/// Toda columna *_id / autorizado_por / anulada_por de cada tabla
/// sincronizada (y cada hijo) del esquema del escritorio está en el mapa.
#[test]
fn fk_map_cubre_esquema_escritorio() {
    let db = nueva_db();
    let mut faltan = Vec::new();
    let mut tablas: Vec<&str> = fk::FK_MAP.iter().map(|(t, _)| *t).collect();
    for (_, hijos) in fk::AGGREGATES {
        for (h, _) in *hijos {
            tablas.push(h);
        }
    }
    for t in tablas {
        let mut stmt = db.prepare(&format!("PRAGMA table_info({t})")).unwrap();
        let cols: Vec<String> = stmt.query_map([], |r| r.get(1)).unwrap().map(|c| c.unwrap()).collect();
        assert!(!cols.is_empty(), "la tabla {t} no existe en el escritorio");
        for c in cols {
            let es_fk = (c.ends_with("_id") && c != "id") || c == "autorizado_por" || c == "anulada_por";
            if es_fk && !fk::fks_de(t).iter().any(|(x, _)| *x == c) {
                faltan.push(format!("{t}.{c}"));
            }
        }
    }
    assert!(faltan.is_empty(), "columnas FK fuera del mapa: {faltan:?}");
}

// ─── Payload (push) ───────────────────────────────────────

#[test]
fn payload_incluye_refs_por_uuid() {
    let db = nueva_db();
    db.execute("INSERT INTO clientes (id, nombre, uuid) VALUES (7, 'Taller', ?1)", [hex(70)]).unwrap();
    db.execute(
        "INSERT INTO ventas (id, folio, usuario_id, cliente_id, total, fecha, uuid) \
         VALUES (10, 'V-000010', 2, 7, 100, '2026-10-06 10:00:00', ?1)",
        [hex(10)],
    ).unwrap();
    db.execute(
        "INSERT INTO venta_detalle (venta_id, producto_id, cantidad, precio_final, subtotal, autorizado_por, uuid) \
         VALUES (10, 1, 1, 100, 100, 1, ?1)",
        [hex(11)],
    ).unwrap();
    // Renglón con un producto que ya no existe aquí (dato viejo).
    db.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    db.execute(
        "INSERT INTO venta_detalle (venta_id, producto_id, cantidad, precio_final, subtotal, uuid) \
         VALUES (10, 4242, 1, 5, 5, ?1)",
        [hex(12)],
    ).unwrap();
    db.execute_batch("PRAGMA foreign_keys = ON").unwrap();

    let p = payload::construir_payload(&db, "ventas", &hex(10)).unwrap().unwrap();
    // La fila viaja completa, con su id y valores crudos.
    assert_eq!(p["id"], json!(10));
    assert_eq!(p["usuario_id"], json!(2));
    let refs = p["__refs"].as_object().unwrap();
    let r = refs[&hex(10)].as_object().unwrap();
    assert_eq!(r["usuario_id"], json!(U_NANCY));
    assert_eq!(r["cliente_id"], json!(hex(70)));
    assert_eq!(r["anulada_por"], Value::Null, "NULL viaja como null");
    assert!(!r.contains_key("sucursal_id"), "Paso nunca viaja en refs");
    let h1 = refs[&hex(11)].as_object().unwrap();
    assert_eq!(h1["producto_id"], json!(P_BALATA));
    assert_eq!(h1["autorizado_por"], json!(U_DUENO));
    assert!(!h1.contains_key("venta_id"), "Padre nunca viaja en refs");
    let h2 = refs[&hex(12)].as_object().unwrap();
    assert!(!h2.contains_key("producto_id"), "referencia faltante → llave omitida");
    assert_eq!(p["__children"]["venta_detalle"].as_array().unwrap().len(), 2);

    // audit_log: registro_id según tabla_afectada.
    db.execute(
        "INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, uuid) \
         VALUES (1, 'VENTA', 'ventas', 10, 'Venta', ?1), (1, 'LOGIN', 'sesiones', 3, 'Login', ?2)",
        [hex(20), hex(21)],
    ).unwrap();
    let a = payload::construir_payload(&db, "audit_log", &hex(20)).unwrap().unwrap();
    assert_eq!(a["__refs"][hex(20)]["registro_id"], json!(hex(10)));
    assert_eq!(a["__refs"][hex(20)]["usuario_id"], json!(U_DUENO));
    let b = payload::construir_payload(&db, "audit_log", &hex(21)).unwrap().unwrap();
    assert!(b["__refs"][hex(21)].get("registro_id").is_none(), "sesiones se copia tal cual");
}

#[test]
fn push_body_lleva_proto_2_y_refs() {
    use super::client::{CambioOut, PushBody, PROTO};
    let body = PushBody {
        device_uuid: "d".into(),
        sucursal_id: 1,
        proto: PROTO,
        cambios: vec![
            CambioOut { tabla: "ventas".into(), operacion: "UPDATE".into(), data: json!({"uuid": "u"}),
                        children: None, refs: Some(json!({"u": {"usuario_id": U_DUENO}})), base: None },
            CambioOut { tabla: "categorias".into(), operacion: "UPDATE".into(), data: json!({"uuid": "c"}),
                        children: None, refs: None, base: None },
            CambioOut { tabla: "productos".into(), operacion: "UPDATE".into(), data: json!({"uuid": "p"}),
                        children: None, refs: None, base: Some(json!({"uuid": "p", "stock_actual": 3})) },
        ],
    };
    let v = serde_json::to_value(&body).unwrap();
    assert_eq!(v["proto"], json!(2));
    assert_eq!(v["cambios"][0]["refs"]["u"]["usuario_id"], json!(U_DUENO));
    assert!(v["cambios"][1].get("refs").is_none());
    assert!(v["cambios"][0].get("base").is_none(), "sin base no viaja la llave");
    assert_eq!(v["cambios"][2]["base"]["stock_actual"], json!(3));
}

// ─── Traducción de FK al aplicar ──────────────────────────

#[test]
fn apply_traduce_fk_por_uuid_con_numeracion_distinta() {
    let mut db = nueva_db();
    let v = web(1);
    let r = aplicar(&mut db, &[cambio_venta_web(&v, "V-20261006-0001", "2026-10-06 10:00:00", 50.0)]);
    assert_eq!(r.aplicados, 1, "{r:?}");
    // Servidor: usuario 2 = Dueño → aquí es 1. Producto 77 del servidor = local 1.
    let (vid, usuario, caja, origen): (i64, i64, String, String) = db.query_row(
        "SELECT id, usuario_id, caja, origen FROM ventas WHERE uuid = ?", [&v],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).unwrap();
    assert_eq!(usuario, 1);
    assert_eq!((caja.as_str(), origen.as_str()), ("principal", "web"));
    let (venta_id, producto_id): (i64, i64) = db.query_row(
        "SELECT venta_id, producto_id FROM venta_detalle", [], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(venta_id, vid, "el hijo cuelga del id LOCAL del padre");
    assert_eq!(producto_id, 1);
    // Aplicar no encola nada (bandera puesta dentro de la tx).
    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 0);
    assert_eq!(bandera(&db), 0);

    // Bitácora web: usuario 2 del servidor (Dueño) → 1; registro_id → venta local.
    let a = web(30);
    aplicar(&mut db, &[json!({
        "__tabla": "audit_log", "__operacion": "UPSERT",
        "__data": { "id": 6000, "usuario_id": 2, "accion": "VENTA", "tabla_afectada": "ventas",
                    "registro_id": 999, "descripcion_legible": null, "origen": "WEB",
                    "fecha": "2026-10-06 10:00:00", "uuid": a, "updated_at": "2026-10-06 10:00:00" },
        "__refs": { a.clone(): { "usuario_id": U_DUENO, "registro_id": v } },
    })]);
    let (u, reg, desc): (i64, i64, String) = db.query_row(
        "SELECT usuario_id, registro_id, descripcion_legible FROM audit_log WHERE uuid = ?", [&a],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!((u, reg), (1, vid));
    assert_eq!(desc, "", "NOT NULL TEXT sin default: NULL → ''");
}

#[test]
fn usuario_solo_web_nunca_baja() {
    let mut db = nueva_db();
    let r = aplicar(&mut db, &[cambio("usuarios", json!({
        "id": 1, "nombre_completo": "Andrew Web", "nombre_usuario": "andrew", "pin": "x",
        "password_hash": "x", "rol_id": 1, "activo": 1, "uuid": "web-admin-2bc80cfd",
        "updated_at": "2026-10-06 10:00:00",
    }))]);
    assert_eq!(r.omitidos, 1);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM usuarios WHERE nombre_usuario = 'andrew'"), 0);
}

#[test]
fn fila_existente_sin_ref_conserva_fk_local() {
    let mut db = nueva_db();
    db.execute("UPDATE productos SET categoria_id = 3 WHERE id = 1", []).unwrap();
    db.execute("UPDATE productos SET updated_at = '2026-10-01 08:00:00' WHERE id = 1", []).unwrap();
    limpiar_outbox(&db);
    let base = json!({
        "id": 4321, "codigo": "P1", "nombre": "Balata delantera", "categoria_id": 7,
        "precio_venta": 120, "stock_actual": 1000, "uuid": P_BALATA, "activo": 1,
        "updated_at": "2026-10-02 09:00:00",
    });
    // Sin __refs (servidor viejo o celda sin traducir): categoría local intacta.
    aplicar(&mut db, &[cambio("productos", base.clone())]);
    let (nombre, cat): (String, i64) = db.query_row(
        "SELECT nombre, categoria_id FROM productos WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!((nombre.as_str(), cat), ("Balata delantera", 3));

    // Con __refs pero sin la llave categoria_id: también se conserva.
    let mut b2 = base.clone();
    b2["updated_at"] = json!("2026-10-03 09:00:00");
    b2["nombre"] = json!("Balata 2");
    aplicar(&mut db, &[json!({ "__tabla": "productos", "__operacion": "UPSERT", "__data": b2,
                               "__refs": { P_BALATA: { "proveedor_id": null } } })]);
    assert_eq!(i64_de(&db, "SELECT categoria_id FROM productos WHERE id = 1"), 3);

    // Con la llave: se traduce por uuid (categoría local 5).
    let cat5 = uuid_de(&db, "categorias", 5);
    let mut b3 = base.clone();
    b3["updated_at"] = json!("2026-10-04 09:00:00");
    aplicar(&mut db, &[json!({ "__tabla": "productos", "__operacion": "UPSERT", "__data": b3,
                               "__refs": { P_BALATA: { "categoria_id": cat5 } } })]);
    assert_eq!(i64_de(&db, "SELECT categoria_id FROM productos WHERE id = 1"), 5);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM productos"), 1, "actualiza, no inserta");
}

#[test]
fn lww_normalizado_y_columnas_desconocidas() {
    let mut db = nueva_db();
    // Marca del servidor con 'T' y fracción: más vieja que la local → no aplica.
    let r = aplicar(&mut db, &[cambio("productos", json!({
        "codigo": "P1", "nombre": "Viejo", "uuid": P_BALATA, "updated_at": "2026-09-30T23:59:59.5-06:00",
        "foo": 1, "origen": "web",
    }))]);
    assert_eq!(r.omitidos, 1);
    // Empate → gana el servidor (él también aplica en empate; si no, un
    // cambio web del mismo segundo quedaba solo allá).
    let r = aplicar(&mut db, &[cambio("productos", json!({
        "codigo": "P1", "nombre": "Empate", "uuid": P_BALATA, "updated_at": "2026-10-01T08:00:00",
    }))]);
    assert_eq!(r.aplicados, 1, "{r:?}");
    assert_eq!(opt_txt(&db, "SELECT nombre FROM productos WHERE id = 1").unwrap(), "Empate");
    // Local estrictamente más nuevo → se queda lo local.
    db.execute("UPDATE productos SET nombre = 'Local', updated_at = '2026-10-01 09:00:00' WHERE id = 1", []).unwrap();
    limpiar_outbox(&db);
    let r = aplicar(&mut db, &[cambio("productos", json!({
        "codigo": "P1", "nombre": "Viejo 2", "uuid": P_BALATA, "updated_at": "2026-10-01 08:59:59",
    }))]);
    assert_eq!(r.omitidos, 1);
    assert_eq!(opt_txt(&db, "SELECT nombre FROM productos WHERE id = 1").unwrap(), "Local");
    // Más nueva, con columnas que no existen aquí → aplica ignorándolas.
    let r = aplicar(&mut db, &[cambio("productos", json!({
        "codigo": "P1", "nombre": "Nuevo", "uuid": P_BALATA, "updated_at": "2026-10-02T08:00:00.123",
        "foo": 1, "origen": "web", "precio_mayoreo": 3,
    }))]);
    assert_eq!(r.aplicados, 1, "{r:?}");
    assert_eq!(opt_txt(&db, "SELECT nombre FROM productos WHERE id = 1").unwrap(), "Nuevo");
    assert_eq!(opt_txt(&db, "SELECT updated_at FROM productos WHERE id = 1").unwrap(), "2026-10-02 08:00:00");
}

#[test]
fn fila_web_nueva_sin_refs_queda_en_bandeja_sin_fk() {
    let mut db = nueva_db();
    let v = web(2);
    let mut c = cambio("ventas", Value::Object(venta_web(&v, "V-W2", "2026-10-06 10:00:00", 10.0, Some("principal"))));
    c["__children"] = json!({ "venta_detalle": [] });
    let r = aplicar(&mut db, &[c]);
    assert_eq!(r.aparcados, 1);
    assert_eq!(inbox_motivo(&db, "ventas", &v).as_deref(), Some("sin_fk"));
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM ventas"), 0);
}

// ─── Savepoint por cambio ─────────────────────────────────

#[test]
fn hijo_que_falla_revierte_el_cambio_completo_y_sigue_el_lote() {
    let mut db = nueva_db();
    // Venta del escritorio a la que la web le hace una devolución.
    db.execute(
        "INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid) VALUES (1, 'V-000001', 1, 100, '2026-10-06 09:00:00', ?1)",
        [hex(1)],
    ).unwrap();
    db.execute(
        "INSERT INTO venta_detalle (id, venta_id, producto_id, cantidad, precio_final, subtotal, uuid) VALUES (1, 1, 1, 1, 100, 100, ?1)",
        [hex(2)],
    ).unwrap();
    limpiar_outbox(&db);

    let d = web(40);
    let (h1, h2) = (web(41), web(42));
    let devol = json!({
        "__tabla": "devoluciones", "__operacion": "UPSERT",
        "__data": { "id": 77, "folio": "D-W1", "venta_id": 555, "usuario_id": 2, "autorizado_por": null,
                    "motivo": "defecto", "total_devuelto": 100, "movimiento_caja_id": null,
                    "fecha": "2026-10-06 10:00:00", "uuid": d, "updated_at": "2026-10-06 10:00:00" },
        "__children": { "devolucion_detalle": [
            { "id": 1, "devolucion_id": 77, "venta_detalle_id": 9, "producto_id": 77, "cantidad": 1,
              "precio_unitario": 100, "subtotal": 100, "uuid": h1 },
            // Sin cantidad (NOT NULL sin default) → el INSERT truena.
            { "id": 2, "devolucion_id": 77, "venta_detalle_id": 9, "producto_id": 77, "cantidad": null,
              "precio_unitario": 100, "subtotal": 100, "uuid": h2 },
        ]},
        "__refs": {
            d.clone(): { "venta_id": hex(1), "usuario_id": U_DUENO, "autorizado_por": null, "movimiento_caja_id": null },
            h1.clone(): { "venta_detalle_id": hex(2), "producto_id": P_BALATA },
            h2.clone(): { "venta_detalle_id": hex(2), "producto_id": P_BALATA },
        },
    });
    let prod = cambio("productos", json!({
        "codigo": "P1", "nombre": "Sigue aplicando", "uuid": P_BALATA, "updated_at": "2026-10-06 11:00:00",
    }));
    let r = aplicar(&mut db, &[devol, prod]);
    assert_eq!((r.aplicados, r.aparcados), (1, 1), "{r:?}");
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM devoluciones"), 0, "ni el padre");
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM devolucion_detalle"), 0, "ni los hermanos");
    assert_eq!(inbox_motivo(&db, "devoluciones", &d).as_deref(), Some("error"));
    let payload: String = db.query_row("SELECT payload FROM sync_inbox WHERE uuid = ?", [&d], |r| r.get(0)).unwrap();
    assert!(payload.contains("D-W1"), "se guarda el payload para reintentar");
    assert_eq!(opt_txt(&db, "SELECT nombre FROM productos WHERE id = 1").unwrap(), "Sigue aplicando");
    assert_eq!(bandera(&db), 0);
}

// ─── Bandera de supresión ─────────────────────────────────

fn insertar_categoria_encola(db: &Connection, nombre: &str) -> bool {
    db.execute("INSERT INTO categorias (nombre) VALUES (?)", [nombre]).unwrap();
    let uuid: String = db.query_row("SELECT uuid FROM categorias WHERE nombre = ?", [nombre], |r| r.get(0)).unwrap();
    i64_de(db, &format!(
        "SELECT COUNT(*) FROM sync_outbox WHERE tabla = 'categorias' AND uuid = '{uuid}' AND synced_at IS NULL"
    )) == 1
}

#[test]
fn bandera_nunca_se_queda_puesta() {
    // a) Lote con un cambio que falla: la bandera se va con el commit.
    let mut db = nueva_db();
    let mut malo = cambio("ventas", Value::Object(venta_web(&web(3), "V-W3", "2026-10-06 10:00:00", 1.0, Some("principal"))));
    malo["__refs"] = json!({ web(3): { "usuario_id": U_DUENO } });
    malo["__children"] = json!({ "venta_detalle": [ { "uuid": null } ] });
    let r = aplicar(&mut db, &[malo.clone()]);
    assert_eq!(r.aparcados, 1);
    assert_eq!(bandera(&db), 0);
    assert!(insertar_categoria_encola(&db, "Cat A"), "los triggers del outbox siguen vivos");

    // b) Error de todo el lote (no hay dónde aparcar): rollback → sin bandera.
    db.execute_batch("DROP TABLE sync_inbox").unwrap();
    assert!(aplicar_lote_con(&mut db, &[malo], Modo::Normal, AHORA).is_err());
    assert_eq!(bandera(&db), 0);
    assert!(insertar_categoria_encola(&db, "Cat B"));

    // c) Bandera vieja pegada (versión anterior): el lote la limpia.
    let mut db = nueva_db();
    db.execute("INSERT INTO sync_suppress_flag (id) VALUES (1)", []).unwrap();
    aplicar(&mut db, &[]);
    assert_eq!(bandera(&db), 0);
}

#[test]
fn bandera_se_limpia_al_iniciar_la_conexion() {
    let dir = std::env::temp_dir().join(format!("pos_sync_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ruta = dir.join("pos_database.db");
    let _ = std::fs::remove_file(&ruta);
    {
        let conn = crate::db::connection::init_database(&ruta).unwrap();
        conn.execute("INSERT INTO sync_suppress_flag (id) VALUES (1)", []).unwrap();
    } // "se cierra a medias" con la bandera puesta
    let conn = crate::db::connection::init_database(&ruta).unwrap();
    assert_eq!(bandera(&conn), 0);
    assert!(insertar_categoria_encola(&conn, "Cat C"));
    drop(conn);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─── Política de caja ─────────────────────────────────────

#[test]
fn cortes_y_aperturas_del_servidor_nunca_se_guardan() {
    let mut db = nueva_db();
    let mut cambios = Vec::new();
    for (i, (origen, caja)) in [("web", "principal"), ("desktop", "principal"), ("web", "web")].iter().enumerate() {
        let n = 100 + i as u64;
        cambios.push(json!({
            "__tabla": "cortes", "__operacion": "UPSERT",
            "__data": { "id": n, "tipo": "DIA", "usuario_id": 2, "fecha_inicio": "2026-10-06 08:00:00",
                        "fecha_fin": "2026-10-06 20:00:00", "created_at": "2026-10-06 20:00:00",
                        "origen": origen, "caja": caja, "uuid": web(n), "updated_at": "2026-10-06 20:00:00" },
            "__children": { "corte_denominaciones": [], "corte_vendedores": [] },
            "__refs": { web(n): { "usuario_id": U_DUENO } },
        }));
        cambios.push(cambio("aperturas_caja", json!({
            "id": n, "usuario_id": 2, "fondo_declarado": 0, "fecha": "2026-10-06 08:10:00",
            "origen": origen, "caja": caja, "uuid": web(n + 50), "updated_at": "2026-10-06 08:10:00",
        })));
    }
    let r = aplicar(&mut db, &cambios);
    assert_eq!(r.omitidos, 6, "{r:?}");
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM cortes"), 0);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM aperturas_caja"), 0);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_inbox"), 0);
    // La apertura del día del escritorio sigue siendo posible.
    db.execute(
        "INSERT INTO aperturas_caja (usuario_id, fondo_declarado, fecha) VALUES (1, 500, '2026-10-06 08:00:00')",
        [],
    ).unwrap();
}

fn mov_web(uuid: &str, origen: &str, caja: Option<&str>, monto: f64, upd: &str) -> Value {
    let mut d = json!({
        "id": 406, "tipo": "RETIRO", "usuario_id": 2, "monto": monto, "concepto": "Devolución D-W",
        "autorizado_por": null, "corte_id": 33, "fecha": "2026-10-06 10:00:00", "sucursal_id": 1,
        "origen": origen, "uuid": uuid, "updated_at": upd, "registrado_at": null,
    });
    if let Some(c) = caja { d["caja"] = json!(c); }
    json!({ "__tabla": "movimientos_caja", "__operacion": "UPSERT", "__data": d,
            "__refs": { uuid: { "usuario_id": U_DUENO, "autorizado_por": null, "corte_id": null } } })
}

#[test]
fn movimientos_web_principal_solo_se_insertan_una_vez() {
    let mut db = nueva_db();
    let m = web(60);
    let r = aplicar(&mut db, &[mov_web(&m, "web", Some("principal"), 30.0, "2026-10-06 10:00:00")]);
    assert_eq!(r.aplicados, 1);
    let (monto, corte, usuario): (f64, Option<i64>, i64) = db.query_row(
        "SELECT monto, corte_id, usuario_id FROM movimientos_caja WHERE uuid = ?", [&m],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!((monto, corte, usuario), (30.0, None, 1), "corte_id del servidor no se copia");
    // Una actualización posterior del servidor no lo toca.
    let r = aplicar(&mut db, &[mov_web(&m, "web", Some("principal"), 999.0, "2026-10-06 11:00:00")]);
    assert_eq!(r.omitidos, 1);
    assert_eq!(i64_de(&db, &format!("SELECT CAST(monto AS INTEGER) FROM movimientos_caja WHERE uuid = '{m}'")), 30);
    // Caja web, origen desktop o servidor sin caja: nunca se guardan.
    let r = aplicar(&mut db, &[
        mov_web(&web(61), "web", Some("web"), 5.0, "2026-10-06 10:00:00"),
        mov_web(&web(62), "desktop", Some("principal"), 5.0, "2026-10-06 10:00:00"),
        mov_web(&web(63), "web", None, 5.0, "2026-10-06 10:00:00"),
    ]);
    assert_eq!(r.omitidos, 3);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM movimientos_caja"), 1);

    // Movimiento del escritorio: un cambio del servidor que le pone corte no aplica.
    db.execute(
        "INSERT INTO movimientos_caja (tipo, usuario_id, monto, concepto, fecha, uuid) VALUES ('RETIRO', 1, 10, 'x', '2026-10-06 09:00:00', ?1)",
        [hex(64)],
    ).unwrap();
    limpiar_outbox(&db);
    let mut c = mov_web(&hex(64), "desktop", Some("principal"), 10.0, "2026-10-07 09:00:00");
    c["__data"]["corte_id"] = json!(1);
    aplicar(&mut db, &[c]);
    assert_eq!(opt_txt(&db, &format!("SELECT corte_id FROM movimientos_caja WHERE uuid = '{}'", hex(64))), None);
}

#[test]
fn ventas_del_escritorio_y_ventas_sin_caja_se_ignoran() {
    let mut db = nueva_db();
    db.execute(
        "INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid, updated_at) \
         VALUES (1, 'V-000001', 1, 100, '2026-10-06 09:00:00', ?1, '2026-10-06 09:00:00')",
        [hex(1)],
    ).unwrap();
    limpiar_outbox(&db);
    // La web "anula" una venta del escritorio: no aplica (aquí manda el escritorio).
    let mut d = venta_web(&hex(1), "V-000001", "2026-10-06 09:00:00", 100.0, Some("principal"));
    d.insert("anulada".into(), json!(true));
    d.insert("updated_at".into(), json!("2026-10-07 09:00:00"));
    d.insert("origen".into(), json!("desktop"));
    let r = aplicar(&mut db, &[cambio("ventas", Value::Object(d))]);
    assert_eq!(r.omitidos, 1);
    assert_eq!(i64_de(&db, "SELECT anulada FROM ventas WHERE id = 1"), 0);

    // Servidor sin migración de caja: venta web sin `caja` → se ignora.
    let v = web(5);
    let mut c = cambio("ventas", Value::Object(venta_web(&v, "V-W5", "2026-10-06 10:00:00", 8.0, None)));
    c["__refs"] = json!({ v.clone(): { "usuario_id": U_DUENO } });
    let r = aplicar(&mut db, &[c]);
    assert_eq!(r.omitidos, 1);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM ventas"), 1);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_inbox"), 0);

    // Venta de la caja web: se guarda (historial) con su caja.
    let w = web(6);
    let mut c = cambio("ventas", Value::Object(venta_web(&w, "V-W6", "2026-10-06 10:00:00", 8.0, Some("web"))));
    c["__refs"] = json!({ w.clone(): { "usuario_id": U_DUENO } });
    aplicar(&mut db, &[c]);
    assert_eq!(opt_txt(&db, &format!("SELECT caja FROM ventas WHERE uuid = '{w}'")).as_deref(), Some("web"));
}

#[test]
fn venta_web_principal_en_periodo_abierto_cuenta_en_el_corte() {
    let mut db = nueva_db();
    corte_principal(&db, "2026-10-05 18:00:00");
    let antes = crate::commands::cortes::calcular_periodo(&db, AHORA).unwrap();
    let v = web(7);
    aplicar(&mut db, &[cambio_venta_web(&v, "V-W7", "2026-10-06 10:00:00", 50.0)]);
    assert_eq!(opt_txt(&db, &format!("SELECT registrado_at FROM ventas WHERE uuid = '{v}'")), None);
    let despues = crate::commands::cortes::calcular_periodo(&db, AHORA).unwrap();
    assert_eq!(despues.total_ventas_efectivo - antes.total_ventas_efectivo, 50.0);
    assert_eq!(despues.efectivo_esperado - antes.efectivo_esperado, 50.0);
}

#[test]
fn venta_web_tardia_se_reacomoda_y_cuenta_en_el_siguiente_periodo() {
    let mut db = nueva_db();
    corte_principal(&db, "2026-10-05 18:00:00");
    // Hecha a las 17:59:50 en la web, llega después del corte de las 18:00.
    let v = web(8);
    aplicar(&mut db, &[cambio_venta_web(&v, "V-W8", "2026-10-05 17:59:50", 40.0)]);
    assert_eq!(
        opt_txt(&db, &format!("SELECT registrado_at FROM ventas WHERE uuid = '{v}'")).as_deref(),
        Some(AHORA),
        "registrado_at = max(ahora, fin del corte + 1 s)"
    );
    // No se encola ni mueve updated_at.
    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 0);
    assert_eq!(opt_txt(&db, &format!("SELECT updated_at FROM ventas WHERE uuid = '{v}'")).as_deref(), Some("2026-10-05 17:59:50"));
    let p = crate::commands::cortes::calcular_periodo(&db, AHORA).unwrap();
    assert_eq!(p.total_ventas_efectivo, 40.0, "cuenta en el período abierto");

    // Con el reloj de la PC atrasado: nunca antes del fin del corte + 1 s.
    let v2 = web(9);
    aplicar_lote_con(&mut db, &[cambio_venta_web(&v2, "V-W9", "2026-10-05 17:00:00", 1.0)], Modo::Normal, "2026-10-05 17:30:00").unwrap();
    assert_eq!(
        opt_txt(&db, &format!("SELECT registrado_at FROM ventas WHERE uuid = '{v2}'")).as_deref(),
        Some("2026-10-05 18:00:01")
    );

    // Historia anterior a acepta_web_desde: nunca se re-acomoda.
    let v3 = web(10);
    aplicar(&mut db, &[cambio_venta_web(&v3, "V-W10", "2026-09-20 10:00:00", 3.0)]);
    assert_eq!(opt_txt(&db, &format!("SELECT registrado_at FROM ventas WHERE uuid = '{v3}'")), None);

    // Movimiento web tardío: mismo trato.
    let m = web(11);
    let mut c = mov_web(&m, "web", Some("principal"), 15.0, "2026-10-05 17:00:00");
    c["__data"]["fecha"] = json!("2026-10-05 17:00:00");
    aplicar(&mut db, &[c]);
    assert_eq!(opt_txt(&db, &format!("SELECT registrado_at FROM movimientos_caja WHERE uuid = '{m}'")).as_deref(), Some(AHORA));
}

#[test]
fn hijos_se_cuelgan_del_padre_local_aunque_el_id_del_servidor_choque() {
    let mut db = nueva_db();
    // Venta local id 1 (escritorio) con su renglón.
    db.execute(
        "INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid) VALUES (1, 'V-000001', 1, 100, '2026-10-06 09:00:00', ?1)",
        [hex(1)],
    ).unwrap();
    db.execute(
        "INSERT INTO venta_detalle (venta_id, producto_id, cantidad, precio_final, subtotal, uuid) VALUES (1, 1, 1, 100, 100, ?1)",
        [hex(2)],
    ).unwrap();
    limpiar_outbox(&db);
    // Venta web cuyo id EN EL SERVIDOR es 1 y sus hijos dicen venta_id = 1.
    let v = web(12);
    let (h1, h2) = (web(13), web(14));
    let mut data = venta_web(&v, "V-W12", "2026-10-06 10:00:00", 30.0, Some("principal"));
    data.insert("id".into(), json!(1));
    let c = json!({
        "__tabla": "ventas", "__operacion": "UPSERT", "__data": data,
        "__children": { "venta_detalle": [detalle_web(&h1, 1, 77, 10.0), detalle_web(&h2, 1, 77, 20.0)] },
        "__refs": { v.clone(): { "usuario_id": U_DUENO }, h1.clone(): { "producto_id": P_BALATA }, h2.clone(): { "producto_id": P_BALATA } },
    });
    aplicar(&mut db, &[c.clone()]);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM venta_detalle WHERE venta_id = 1"), 1, "la venta local 1 queda igual");
    let vid: i64 = db.query_row("SELECT id FROM ventas WHERE uuid = ?", [&v], |r| r.get(0)).unwrap();
    assert_ne!(vid, 1);
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM venta_detalle WHERE venta_id = {vid}")), 2);
    let ids_antes: Vec<i64> = db.prepare("SELECT id FROM venta_detalle ORDER BY id").unwrap()
        .query_map([], |r| r.get(0)).unwrap().map(|x| x.unwrap()).collect();

    // La web quita un renglón: se borra el que ya no viene; el otro conserva su id.
    let mut c2 = c.clone();
    c2["__data"]["updated_at"] = json!("2026-10-06 11:00:00");
    c2["__children"]["venta_detalle"] = json!([detalle_web(&h1, 1, 77, 10.0)]);
    aplicar(&mut db, &[c2]);
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM venta_detalle WHERE venta_id = {vid}")), 1);
    let id_h1: i64 = db.query_row("SELECT id FROM venta_detalle WHERE uuid = ?", [&h1], |r| r.get(0)).unwrap();
    assert!(ids_antes.contains(&id_h1), "actualizar no cambia el id del renglón");
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM venta_detalle WHERE venta_id = 1"), 1);
}

// ─── Borrado lógico ───────────────────────────────────────

#[test]
fn borrado_del_servidor_es_logico() {
    let mut db = nueva_db();
    // Producto con ventas: un DELETE físico rompería la FK.
    db.execute(
        "INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid) VALUES (1, 'V-000001', 1, 100, '2026-10-06 09:00:00', ?1)",
        [hex(1)],
    ).unwrap();
    db.execute(
        "INSERT INTO venta_detalle (venta_id, producto_id, cantidad, precio_final, subtotal, uuid) VALUES (1, 1, 1, 100, 100, ?1)",
        [hex(2)],
    ).unwrap();
    limpiar_outbox(&db);
    // proto 2: UPSERT con deleted_at.
    aplicar(&mut db, &[cambio("productos", json!({
        "codigo": "P1", "nombre": "Balata", "uuid": P_BALATA, "activo": 1,
        "updated_at": "2026-10-06 10:00:00", "deleted_at": "2026-10-06 10:00:00",
    }))]);
    let (del, activo): (Option<String>, i64) = db.query_row(
        "SELECT deleted_at, activo FROM productos WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!((del.as_deref(), activo), (Some("2026-10-06 10:00:00"), 0));

    // Legado: __operacion DELETE con la fila completa.
    db.execute("INSERT INTO clientes (nombre, uuid, updated_at) VALUES ('Taller', ?1, '2026-10-01 00:00:00')", [hex(80)]).unwrap();
    limpiar_outbox(&db);
    aplicar(&mut db, &[json!({ "__tabla": "clientes", "__operacion": "DELETE",
        "__data": { "nombre": "Taller", "uuid": hex(80), "updated_at": "2026-10-06 10:00:00", "deleted_at": null } })]);
    let (del, activo): (Option<String>, i64) = db.query_row(
        "SELECT deleted_at, activo FROM clientes WHERE uuid = ?", [hex(80)], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert!(del.is_some());
    assert_eq!(activo, 0);

    // Fila que ya no existe en el servidor (solo uuid): borrado lógico.
    db.execute("INSERT INTO proveedores (nombre, uuid) VALUES ('Prov', ?1)", [hex(81)]).unwrap();
    limpiar_outbox(&db);
    aplicar(&mut db, &[json!({ "__tabla": "proveedores", "__operacion": "DELETE", "__data": { "uuid": hex(81) } })]);
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM proveedores WHERE uuid = '{}' AND deleted_at IS NOT NULL AND activo = 0", hex(81))), 1);

    // Nada se borra físicamente.
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM productos"), 1);
    // SPEC3 D4: producto hecho en la web, borrado allá y nunca visto aquí: se
    // crea YA borrado (sus ventas web lo referencian).
    let r = aplicar(&mut db, &[cambio("productos", json!({
        "codigo": "WEB-9", "nombre": "x", "uuid": web(90), "activo": 1, "updated_at": "2026-10-06 10:00:00",
        "deleted_at": "2026-10-06 10:00:00",
    }))]);
    assert_eq!((r.aplicados, r.aparcados), (1, 0), "{r:?}");
    let (del, activo): (Option<String>, i64) = db.query_row(
        "SELECT deleted_at, activo FROM productos WHERE uuid = ?", [web(90)], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!((del.as_deref(), activo), (Some("2026-10-06 10:00:00"), 0));
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM productos"), 2);
    // Uno hecho en un escritorio, borrado y nunca visto aquí: no se crea
    // (chocaría con códigos vivos de este equipo).
    let r = aplicar(&mut db, &[cambio("productos", json!({
        "codigo": "ESC-9", "nombre": "x", "uuid": hex(91), "activo": 1, "updated_at": "2026-10-06 10:00:00",
        "deleted_at": "2026-10-06 10:00:00",
    }))]);
    assert_eq!(r.omitidos, 1);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM productos"), 2);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_inbox"), 0);
    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 0);
}

// ─── Cambio local pendiente ───────────────────────────────

#[test]
fn cambio_local_sin_subir_no_se_pisa_y_se_reintenta() {
    let mut db = nueva_db();
    db.execute("UPDATE productos SET nombre = 'Editado aquí', updated_at = '2026-10-06 09:00:00' WHERE id = 1", []).unwrap();
    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 1);
    let c = cambio("productos", json!({
        "codigo": "P1", "nombre": "Editado en web", "uuid": P_BALATA, "updated_at": "2026-10-06 09:30:00",
    }));
    let r = aplicar(&mut db, &[c]);
    assert_eq!(r.aparcados, 1);
    assert_eq!(opt_txt(&db, "SELECT nombre FROM productos WHERE id = 1").unwrap(), "Editado aquí");
    assert_eq!(inbox_motivo(&db, "productos", P_BALATA).as_deref(), Some("pendiente_local"));
    // Mientras el outbox no sube, no es candidato.
    assert!(inbox::candidatos(&db, AHORA, 10).unwrap().is_empty());

    // El push sube el cambio local → se reintenta con el payload guardado.
    limpiar_outbox(&db);
    let cand = inbox::candidatos(&db, AHORA, 10).unwrap();
    assert_eq!(cand.len(), 1);
    assert_eq!(cand[0].motivo, apply::MOTIVO_PENDIENTE_LOCAL);
    let guardado: Value = serde_json::from_str(cand[0].payload.as_deref().unwrap()).unwrap();
    let r = aplicar(&mut db, &[guardado]);
    assert_eq!(r.aplicados, 1);
    assert_eq!(opt_txt(&db, "SELECT nombre FROM productos WHERE id = 1").unwrap(), "Editado en web");
    assert_eq!(inbox_motivo(&db, "productos", P_BALATA), None, "éxito → sale de la bandeja");
}

#[test]
fn bandeja_espera_creciente_y_caducidad() {
    let mut db = nueva_db();
    let v = web(15);
    let mut c = cambio("ventas", Value::Object(venta_web(&v, "V-W15", "2026-10-06 10:00:00", 1.0, Some("principal"))));
    c["__children"] = json!({ "venta_detalle": [] });
    aplicar(&mut db, &[c.clone()]); // sin refs → sin_fk
    let prox: String = db.query_row("SELECT proximo_intento FROM sync_inbox WHERE uuid = ?", [&v], |r| r.get(0)).unwrap();
    assert_eq!(prox, "2026-10-06 12:05:00");
    assert!(inbox::candidatos(&db, AHORA, 10).unwrap().is_empty());
    assert_eq!(inbox::candidatos(&db, "2026-10-06 12:05:00", 10).unwrap().len(), 1);
    aplicar(&mut db, &[c]); // segundo intento falla igual → 10 min
    let (n, prox): (i64, String) = db.query_row(
        "SELECT intentos, proximo_intento FROM sync_inbox WHERE uuid = ?", [&v], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!((n, prox.as_str()), (2, "2026-10-06 12:10:00"));
    assert_eq!(apply::espera_reintento(30).num_minutes(), 360);
    // 30 días después se descarta.
    assert_eq!(inbox::purgar_vencidos(&db, "2026-11-04 12:00:00").unwrap(), 0);
    assert_eq!(inbox::purgar_vencidos(&db, "2026-11-06 12:00:01").unwrap(), 1);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_inbox"), 0);
}

#[test]
fn errores_de_la_ui_incluyen_la_bandeja_sin_payload() {
    let mut db = nueva_db();
    let v = web(16);
    let mut c = cambio("ventas", Value::Object(venta_web(&v, "V-W16", "2026-10-06 10:00:00", 1.0, Some("principal"))));
    c["__children"] = json!({ "venta_detalle": [] });
    aplicar(&mut db, &[c]);
    db.execute("UPDATE productos SET nombre = 'x' WHERE id = 1", []).unwrap();
    db.execute("UPDATE sync_outbox SET intentos = 3, ultimo_error = 'push 500' WHERE synced_at IS NULL", []).unwrap();
    let lista = outbox::errores_con_bandeja(&db, 50).unwrap();
    assert_eq!(lista.len(), 2);
    assert_eq!(lista[0].fuente, "outbox");
    let b = lista.iter().find(|f| f.fuente == "inbox").unwrap();
    assert_eq!((b.tabla.as_str(), b.uuid.as_str(), b.motivo.as_deref()), ("ventas", v.as_str(), Some("sin_fk")));
    let j = serde_json::to_string(&lista).unwrap();
    assert!(!j.contains("V-W16"), "nunca se manda el payload a la UI");

    let d = crate::commands::sync_remoto::diagnostico(&db).unwrap();
    assert_eq!(d.bandera_supresion, 0);
    assert_eq!(d.bandeja.iter().map(|m| m.cantidad).sum::<i64>(), 1);
    assert!(d.tablas.iter().any(|t| t.tabla == "ventas" && t.columnas.iter().any(|c| c.starts_with("caja "))));
}

// ─── Re-descarga (solo insertar) ──────────────────────────

#[test]
fn replay_solo_inserta_y_recuelga_hijos_web() {
    let mut db = nueva_db();
    // Producto existente: aunque el servidor lo tenga más nuevo, replay no lo toca.
    let r = aplicar_lote_con(&mut db, &[cambio("productos", json!({
        "codigo": "P1", "nombre": "No debe cambiar", "uuid": P_BALATA, "updated_at": "2026-10-06 11:00:00",
    }))], Modo::Replay, AHORA).unwrap();
    assert_eq!(r.omitidos, 1);
    assert_eq!(opt_txt(&db, "SELECT nombre FROM productos WHERE id = 1").unwrap(), "Balata");

    // Venta web que faltaba: se inserta.
    let v = web(17);
    aplicar_lote_con(&mut db, &[cambio_venta_web(&v, "V-W17", "2026-09-10 10:00:00", 3.0)], Modo::Replay, AHORA).unwrap();
    let vid: i64 = db.query_row("SELECT id FROM ventas WHERE uuid = ?", [&v], |r| r.get(0)).unwrap();
    assert_eq!(opt_txt(&db, &format!("SELECT registrado_at FROM ventas WHERE id = {vid}")), None, "historia vieja no se re-acomoda");

    // Venta web vieja que llegó sin renglones y un renglón colgado de otra venta.
    db.execute(
        "INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid, origen) VALUES (200, 'V-20260427-0001', 2, 3, '2026-04-27 10:00:00', ?1, 'web')",
        [web(18)],
    ).unwrap();
    let h = web(19);
    db.execute(
        "INSERT INTO venta_detalle (venta_id, producto_id, cantidad, precio_final, subtotal, uuid) VALUES (?1, 1, 1, 3, 3, ?2)",
        rusqlite::params![vid, h],
    ).unwrap();
    limpiar_outbox(&db);
    let mut c = cambio_venta_web(&web(18), "V-20260427-0001", "2026-04-27 10:00:00", 3.0);
    c["__children"] = json!({ "venta_detalle": [detalle_web(&h, 205, 77, 3.0)] });
    c["__refs"] = json!({ web(18): { "usuario_id": U_DUENO }, h.clone(): { "producto_id": P_BALATA } });
    c["__data"]["usuario_id"] = json!(3);
    aplicar_lote_con(&mut db, &[c], Modo::Replay, AHORA).unwrap();
    assert_eq!(i64_de(&db, &format!("SELECT venta_id FROM venta_detalle WHERE uuid = '{h}'")), 200);
    assert_eq!(i64_de(&db, "SELECT usuario_id FROM ventas WHERE id = 200"), 2, "el padre existente no se actualiza");
}

// ─── Arreglo espejo ───────────────────────────────────────

#[test]
fn arreglo_espejo_solo_cambia_columnas_fk() {
    let mut db = nueva_db();
    let v = web(20);
    db.execute(
        "INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid, origen, updated_at) \
         VALUES (414, 'V-20260430-0001', 2, 8, '2026-04-30 10:00:00', ?1, 'web', '2026-04-30 10:00:00')",
        [&v],
    ).unwrap();
    let a = web(21);
    db.execute(
        "INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen, uuid) \
         VALUES (2, 'VENTA', 'ventas', 999, 'Venta web', 'WEB', ?1)",
        [&a],
    ).unwrap();
    let m = web(22);
    db.execute(
        "INSERT INTO movimientos_caja (tipo, usuario_id, monto, concepto, fecha, uuid, origen) VALUES ('RETIRO', 2, 3, 'x', '2026-04-27 10:00:00', ?1, 'web')",
        [&m],
    ).unwrap();
    // Venta del escritorio: nunca se toca aunque venga en la respuesta.
    db.execute("INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid) VALUES (1, 'V-000001', 2, 1, '2026-10-06 09:00:00', ?1)", [hex(1)]).unwrap();
    limpiar_outbox(&db);
    assert_eq!(apply::uuids_web_locales(&db, "ventas").unwrap(), vec![v.clone()]);
    assert_eq!(apply::uuids_web_locales(&db, "audit_log").unwrap(), vec![a.clone()]);

    let refs = json!({ v.clone(): { "usuario_id": U_DUENO, "cliente_id": null }, hex(1): { "usuario_id": U_DUENO } });
    let r = apply::aplicar_refs_espejo(&mut db, "ventas", refs.as_object().unwrap()).unwrap();
    assert_eq!((r.filas, r.celdas_cambiadas), (1, 1));
    assert_eq!(i64_de(&db, "SELECT usuario_id FROM ventas WHERE id = 414"), 1);
    assert_eq!(i64_de(&db, "SELECT CAST(total AS INTEGER) FROM ventas WHERE id = 414"), 8);
    assert_eq!(opt_txt(&db, "SELECT updated_at FROM ventas WHERE id = 414").as_deref(), Some("2026-04-30 10:00:00"));
    assert_eq!(i64_de(&db, "SELECT usuario_id FROM ventas WHERE id = 1"), 2, "fila del escritorio intacta");

    let refs = json!({ a.clone(): { "usuario_id": U_DUENO, "registro_id": v } });
    apply::aplicar_refs_espejo(&mut db, "audit_log", refs.as_object().unwrap()).unwrap();
    assert_eq!(
        db.query_row("SELECT usuario_id, registro_id FROM audit_log WHERE uuid = ?", [&a], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).unwrap(),
        (1, 414)
    );

    let refs = json!({ m.clone(): { "usuario_id": U_DUENO, "corte_id": hex(999) } });
    let r = apply::aplicar_refs_espejo(&mut db, "movimientos_caja", refs.as_object().unwrap()).unwrap();
    assert_eq!(r.celdas_cambiadas, 1, "corte_id nunca se toca");
    assert_eq!(opt_txt(&db, &format!("SELECT corte_id FROM movimientos_caja WHERE uuid = '{m}'")), None);

    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 0, "no encola nada");
    assert_eq!(bandera(&db), 0);
    // Repetir no cambia nada.
    let refs = json!({ v.clone(): { "usuario_id": U_DUENO } });
    assert_eq!(apply::aplicar_refs_espejo(&mut db, "ventas", refs.as_object().unwrap()).unwrap().celdas_cambiadas, 0);
}

// ─── Ronda 2 (SPEC2 R3) ───────────────────────────────────

const WEB_ADMIN: &str = "web-admin-2bc80cfd-fe3a-45e9-8178-83edbc372aa7";

/// R3.1: venta y movimiento web de la caja principal cobrados con un login
/// solo web ('Andrew Web', uuid 'web-admin-*', nunca baja aquí): quedan a
/// nombre del primer administrador activo y cuentan en el corte.
#[test]
fn usuario_web_admin_queda_a_nombre_del_administrador_local() {
    let mut db = nueva_db();
    corte_principal(&db, "2026-10-05 18:00:00");
    let antes = crate::commands::cortes::calcular_periodo(&db, AHORA).unwrap();
    let v = web(1100);
    let mut c = cambio_venta_web(&v, "V-WA1", "2026-10-06 10:00:00", 70.0);
    c["__refs"][&v]["usuario_id"] = json!(WEB_ADMIN);
    let m = web(1101);
    let mut cm = mov_web(&m, "web", Some("principal"), 20.0, "2026-10-06 10:05:00");
    cm["__refs"][&m]["usuario_id"] = json!(WEB_ADMIN);
    let r = aplicar(&mut db, &[c, cm]);
    assert_eq!((r.aplicados, r.aparcados), (2, 0), "{r:?}");
    assert_eq!(i64_de(&db, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{v}'")), 1, "Dueño local");
    assert_eq!(i64_de(&db, &format!("SELECT usuario_id FROM movimientos_caja WHERE uuid = '{m}'")), 1);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_inbox"), 0);
    let despues = crate::commands::cortes::calcular_periodo(&db, AHORA).unwrap();
    assert_eq!(despues.total_ventas_efectivo - antes.total_ventas_efectivo, 70.0, "la venta cuenta");

    // Un usuario que falta y NO es web-admin sigue esperando en la bandeja.
    let v2 = web(1102);
    let mut c2 = cambio_venta_web(&v2, "V-WA2", "2026-10-06 10:10:00", 5.0);
    c2["__refs"][&v2]["usuario_id"] = json!(web(4242));
    aplicar(&mut db, &[c2]);
    assert_eq!(inbox_motivo(&db, "ventas", &v2).as_deref(), Some("sin_fk"));

    // Sin ningún administrador activo aquí: también espera (no inventa usuario).
    db.execute("UPDATE usuarios SET activo = 0 WHERE id = 1", []).unwrap();
    limpiar_outbox(&db);
    let v3 = web(1103);
    let mut c3 = cambio_venta_web(&v3, "V-WA3", "2026-10-06 10:20:00", 5.0);
    c3["__refs"][&v3]["usuario_id"] = json!(WEB_ADMIN);
    aplicar(&mut db, &[c3]);
    assert_eq!(inbox_motivo(&db, "ventas", &v3).as_deref(), Some("sin_fk"));
}

/// R3.2: producto web nuevo cuyo código ya es de otro producto local
/// (borrados incluidos): se guarda como '<código>-W<8 del uuid>' y su venta
/// web resuelve el producto. Después, una versión con el código original
/// conserva el código local; un código libre (el servidor lo renombró) sí
/// se aplica.
#[test]
fn producto_web_con_codigo_ocupado_se_renombra_y_su_venta_cuenta() {
    let mut db = nueva_db();
    db.execute(
        "INSERT INTO productos (codigo, nombre, uuid, deleted_at, activo) VALUES ('VIEJO', 'Borrado', ?1, '2026-01-01 00:00:00', 0)",
        [hex(300)],
    ).unwrap();
    limpiar_outbox(&db);
    let (pw, pw2) = (web(1200), web(1201));
    let r = aplicar(&mut db, &[
        cambio("productos", json!({ "codigo": "P1", "nombre": "Balata web", "precio_venta": 90, "stock_actual": 5,
                                    "uuid": pw, "activo": 1, "updated_at": "2026-10-06 09:00:00" })),
        cambio("productos", json!({ "codigo": "VIEJO", "nombre": "Otro web", "uuid": pw2, "activo": 1,
                                    "updated_at": "2026-10-06 09:00:00" })),
    ]);
    assert_eq!((r.aplicados, r.aparcados), (2, 0), "{r:?}");
    let codigo = |db: &Connection, u: &str| opt_txt(db, &format!("SELECT codigo FROM productos WHERE uuid = '{u}'")).unwrap();
    assert_eq!(codigo(&db, &pw), "P1-W019dd079");
    assert_eq!(codigo(&db, &pw2), "VIEJO-W019dd079");
    assert_eq!(codigo(&db, P_BALATA), "P1", "el producto local no cambia");
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_inbox"), 0);

    // Venta web de ese producto: el renglón apunta al producto local.
    let v = web(1202);
    let mut c = cambio_venta_web(&v, "V-COD", "2026-10-06 10:00:00", 90.0);
    let det = web(9090);
    c["__refs"][&det]["producto_id"] = json!(pw);
    let r = aplicar(&mut db, &[c]);
    assert_eq!(r.aplicados, 1, "{r:?}");
    let pid: i64 = db.query_row("SELECT id FROM productos WHERE uuid = ?", [&pw], |r| r.get(0)).unwrap();
    assert_eq!(i64_de(&db, &format!("SELECT producto_id FROM venta_detalle WHERE uuid = '{det}'")), pid);

    // Versión posterior con el código original: se queda el código local.
    aplicar(&mut db, &[cambio("productos", json!({
        "codigo": "P1", "nombre": "Balata web 2", "precio_venta": 95, "uuid": pw, "updated_at": "2026-10-06 11:00:00",
    }))]);
    assert_eq!(codigo(&db, &pw), "P1-W019dd079");
    assert_eq!(i64_de(&db, &format!("SELECT CAST(precio_venta AS INTEGER) FROM productos WHERE uuid = '{pw}'")), 95);
    // El servidor lo renombró (migración 011): el código libre sí se aplica.
    aplicar(&mut db, &[cambio("productos", json!({
        "codigo": "P1-W11922", "nombre": "Balata web 2", "uuid": pw, "updated_at": "2026-10-06 11:30:00",
    }))]);
    assert_eq!(codigo(&db, &pw), "P1-W11922");
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_inbox"), 0);
    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 0);
}

/// R3.3: una venta web re-acomodada (registrado_at local) que después cambia
/// en el servidor (p. ej. se anula) conserva su registrado_at aunque el
/// servidor mande NULL: si no, volvería a la fecha ya cubierta por el corte.
#[test]
fn registrado_at_local_no_se_borra_con_el_null_del_servidor() {
    let mut db = nueva_db();
    corte_principal(&db, "2026-10-05 18:00:00");
    let v = web(1300);
    aplicar(&mut db, &[cambio_venta_web(&v, "V-RA", "2026-10-05 17:59:50", 40.0)]);
    let reg = |db: &Connection| opt_txt(db, &format!("SELECT registrado_at FROM ventas WHERE uuid = '{v}'"));
    assert_eq!(reg(&db).as_deref(), Some(AHORA));

    let mut c = cambio_venta_web(&v, "V-RA", "2026-10-05 17:59:50", 40.0);
    c["__data"]["updated_at"] = json!("2026-10-06 12:30:00");
    c["__data"]["anulada"] = json!(true);
    c["__data"]["motivo_anulacion"] = json!("cliente se arrepintió");
    assert_eq!(c["__data"]["registrado_at"], Value::Null);
    let r = aplicar(&mut db, &[c]);
    assert_eq!(r.aplicados, 1, "{r:?}");
    assert_eq!(i64_de(&db, &format!("SELECT anulada FROM ventas WHERE uuid = '{v}'")), 1, "el resto sí se aplica");
    assert_eq!(reg(&db).as_deref(), Some(AHORA), "registrado_at local intacto");
    let p = crate::commands::cortes::calcular_periodo(&db, "2026-10-06 13:00:00").unwrap();
    assert_eq!(p.total_anulaciones, 40.0, "la anulación cae en el período abierto");

    // Un registrado_at que sí trae valor se aplica normalmente.
    let mut c = cambio_venta_web(&v, "V-RA", "2026-10-05 17:59:50", 40.0);
    c["__data"]["updated_at"] = json!("2026-10-06 12:40:00");
    c["__data"]["anulada"] = json!(true);
    c["__data"]["registrado_at"] = json!("2026-10-06 12:35:00");
    aplicar(&mut db, &[c]);
    assert_eq!(reg(&db).as_deref(), Some("2026-10-06 12:35:00"));
}

/// R3.4: acepta_web_desde := MIN(acepta_web_desde, caja_v2_desde).
#[test]
fn caja_v2_desde_solo_baja_acepta_web_desde() {
    let db = nueva_db();
    let desde = |db: &Connection| opt_txt(db, "SELECT acepta_web_desde FROM sync_state WHERE id = 1");
    db.execute("UPDATE sync_state SET acepta_web_desde = '2026-10-08 10:00:00' WHERE id = 1", []).unwrap();
    assert!(super::state::ajustar_acepta_web_desde(&db, "2026-10-07T21:15:00.123").unwrap());
    assert_eq!(desde(&db).as_deref(), Some("2026-10-07 21:15:00"));
    assert!(!super::state::ajustar_acepta_web_desde(&db, "2026-10-09 00:00:00").unwrap(), "nunca sube");
    assert!(!super::state::ajustar_acepta_web_desde(&db, "no es fecha").unwrap());
    assert_eq!(desde(&db).as_deref(), Some("2026-10-07 21:15:00"));
    super::state::ajustar_desde_respuesta(&db, None);
    super::state::ajustar_desde_respuesta(&db, Some(""));
    assert_eq!(desde(&db).as_deref(), Some("2026-10-07 21:15:00"));
    db.execute("UPDATE sync_state SET acepta_web_desde = NULL WHERE id = 1", []).unwrap();
    super::state::ajustar_desde_respuesta(&db, Some("2026-10-07 21:15:00"));
    assert_eq!(desde(&db).as_deref(), Some("2026-10-07 21:15:00"));
}

/// R3.5: la re-descarga (solo insertar) no borra lo que espera en la
/// bandeja y no pisa los hijos de un agregado web con un cambio local sin
/// subir; sin cambio local re-cuelga los hijos pero deja pendiente la
/// entrada de la bandeja del padre.
#[test]
fn replay_respeta_la_bandeja_y_los_cambios_locales() {
    let mut db = nueva_db();
    // a) Edición web de un cliente aparcada por un cambio local sin subir.
    db.execute("INSERT INTO clientes (nombre, uuid, updated_at) VALUES ('Taller', ?1, '2026-10-01 00:00:00')", [hex(400)]).unwrap();
    limpiar_outbox(&db);
    db.execute("UPDATE clientes SET nombre = 'Taller local', updated_at = '2026-10-06 09:00:00' WHERE uuid = ?1", [hex(400)]).unwrap();
    aplicar(&mut db, &[cambio("clientes", json!({ "nombre": "Taller web", "uuid": hex(400), "updated_at": "2026-10-06 11:00:00" }))]);
    assert_eq!(inbox_motivo(&db, "clientes", &hex(400)).as_deref(), Some("pendiente_local"));
    // La re-descarga pasa por un cursor viejo de la misma fila.
    let r = aplicar_lote_con(&mut db, &[cambio("clientes", json!({
        "nombre": "Taller", "uuid": hex(400), "updated_at": "2026-10-01 00:00:00",
    }))], Modo::Replay, AHORA).unwrap();
    assert_eq!(r.omitidos, 1);
    assert_eq!(inbox_motivo(&db, "clientes", &hex(400)).as_deref(), Some("pendiente_local"), "sigue en la bandeja");
    let payload: String = db.query_row("SELECT payload FROM sync_inbox WHERE uuid = ?", [hex(400)], |r| r.get(0)).unwrap();
    assert!(payload.contains("Taller web"), "con la edición web, no la versión vieja");

    // b) Venta web ya aquí con 2 renglones y un cambio local sin subir.
    let v = web(1400);
    let (h1, h2) = (web(1401), web(1402));
    let base_venta = |hijos: Vec<Value>| -> Value {
        let mut refs = json!({ v.clone(): { "usuario_id": U_DUENO } });
        for h in &hijos {
            refs[h["uuid"].as_str().unwrap()] = json!({ "producto_id": P_BALATA });
        }
        json!({
            "__tabla": "ventas", "__operacion": "UPSERT",
            "__data": Value::Object(venta_web(&v, "V-RP", "2026-10-06 10:00:00", 30.0, Some("principal"))),
            "__children": { "venta_detalle": hijos }, "__refs": refs,
        })
    };
    aplicar(&mut db, &[base_venta(vec![detalle_web(&h1, 1, 77, 10.0), detalle_web(&h2, 1, 77, 20.0)])]);
    let vid: i64 = db.query_row("SELECT id FROM ventas WHERE uuid = ?", [&v], |r| r.get(0)).unwrap();
    db.execute("UPDATE ventas SET anulada = 1, updated_at = '2026-10-06 11:00:00' WHERE id = ?1", [vid]).unwrap();
    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 2, "cliente + venta sin subir");
    let r = aplicar_lote_con(&mut db, &[base_venta(vec![detalle_web(&h1, 1, 77, 10.0)])], Modo::Replay, AHORA).unwrap();
    assert_eq!(r.aparcados, 1, "{r:?}");
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM venta_detalle WHERE venta_id = {vid}")), 2, "renglones intactos");
    assert_eq!(inbox_motivo(&db, "ventas", &v).as_deref(), Some("pendiente_local"));

    // c) Sin cambio local: re-cuelga los hijos y NO borra la entrada del padre.
    limpiar_outbox(&db);
    db.execute("UPDATE sync_inbox SET motivo = 'error' WHERE uuid = ?1", [&v]).unwrap();
    let r = aplicar_lote_con(&mut db, &[base_venta(vec![detalle_web(&h1, 1, 77, 10.0)])], Modo::Replay, AHORA).unwrap();
    assert_eq!(r.aplicados, 1, "{r:?}");
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM venta_detalle WHERE venta_id = {vid}")), 1);
    assert_eq!(inbox_motivo(&db, "ventas", &v).as_deref(), Some("error"), "el padre sigue pendiente en la bandeja");
    assert_eq!(i64_de(&db, &format!("SELECT anulada FROM ventas WHERE id = {vid}")), 1, "el padre no se toca");
    assert_eq!(bandera(&db), 0);
}

/// R3.8: al aplicar un producto del servidor, la fila local resultante
/// (ids LOCALES, updated_at normalizado) queda como base de la fusión.
#[test]
fn base_de_productos_se_guarda_al_aplicar() {
    let mut db = nueva_db();
    assert!(super::base::leer(&db, "productos", P_BALATA).unwrap().is_none());
    let cat5 = uuid_de(&db, "categorias", 5);
    let r = aplicar(&mut db, &[json!({
        "__tabla": "productos", "__operacion": "UPSERT",
        "__data": { "id": 4321, "codigo": "P1", "nombre": "Balata", "categoria_id": 77, "precio_venta": 120,
                    "stock_actual": 990, "uuid": P_BALATA, "activo": 1, "updated_at": "2026-10-02T09:00:00.5" },
        "__refs": { P_BALATA: { "categoria_id": cat5 } },
    })]);
    assert_eq!(r.aplicados, 1);
    let b = super::base::leer(&db, "productos", P_BALATA).unwrap().expect("base guardada");
    assert_eq!(b["precio_venta"], json!(120.0));
    assert_eq!(b["stock_actual"], json!(990.0));
    assert_eq!(b["categoria_id"], json!(5), "id LOCAL, igual que `data` del push");
    assert_eq!(b["updated_at"], json!("2026-10-02 09:00:00"));
    assert!(b.as_object().unwrap().keys().all(|k| !k.starts_with("__")));
    assert_eq!(
        opt_txt(&db, &format!("SELECT updated_at FROM sync_base WHERE tabla = 'productos' AND uuid = '{P_BALATA}'")).as_deref(),
        Some("2026-10-02 09:00:00")
    );
    // Un cambio omitido (más viejo) no toca la base.
    aplicar(&mut db, &[cambio("productos", json!({
        "codigo": "P1", "nombre": "Viejo", "precio_venta": 1, "uuid": P_BALATA, "updated_at": "2026-10-01 00:00:00",
    }))]);
    assert_eq!(super::base::leer(&db, "productos", P_BALATA).unwrap().unwrap()["precio_venta"], json!(120.0));
    // Solo productos.
    aplicar(&mut db, &[cambio("categorias", json!({ "nombre": "Web X", "uuid": web(1500), "updated_at": "2026-10-06 10:00:00" }))]);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_base"), 1);
    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 0);
}

/// R3.7 (unidad): el outbox solo se marca subido / con error si el renglón
/// no cambió (created_at igual) desde que se leyó.
#[test]
fn outbox_marca_solo_si_no_cambio_el_renglon() {
    let db = nueva_db();
    db.execute("UPDATE productos SET nombre = 'x' WHERE id = 1", []).unwrap();
    let p = outbox::pendientes(&db, 10).unwrap().pop().unwrap();
    // Otro cambio "durante la llamada" (el trigger mueve created_at).
    db.execute("UPDATE sync_outbox SET created_at = '2999-01-01 00:00:00' WHERE id = ?1", [p.id]).unwrap();
    assert!(!outbox::marcar_error_si(&db, p.id, &p.created_at, "remoto_mas_nuevo").unwrap());
    assert!(!outbox::marcar_sincronizado_si(&db, p.id, &p.created_at).unwrap());
    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 1);
    assert_eq!(opt_txt(&db, &format!("SELECT ultimo_error FROM sync_outbox WHERE id = {}", p.id)), None);
    assert!(outbox::marcar_error_si(&db, p.id, "2999-01-01 00:00:00", "push 500").unwrap());
    assert!(outbox::marcar_sincronizado_si(&db, p.id, "2999-01-01 00:00:00").unwrap());
    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 0);
}

// ─── Ronda 3 (SPEC3 D) ────────────────────────────────────

/// D4: producto creado en la web, vendido en espejo y BORRADO en la web antes
/// de que este equipo jale (p. ej. sin internet): llegan juntos; el producto
/// se crea ya borrado y la venta liga su renglón y cuenta. Lo que no debe
/// crearse sigue sin crearse: transaccionales borradas, DELETE de una fila que
/// no existe aquí, y una fila borrada cuyo nombre único ya es de otra viva.
#[test]
fn producto_web_borrado_se_crea_borrado_y_su_venta_cuenta() {
    let mut db = nueva_db();
    corte_principal(&db, "2026-10-05 18:00:00");
    let antes = crate::commands::cortes::calcular_periodo(&db, AHORA).unwrap();
    let (pw, v) = (web(1400), web(1401));
    let mut cv = cambio_venta_web(&v, "V-BORR", "2026-10-06 09:00:00", 55.0);
    let det = web(9055);
    cv["__refs"][&det]["producto_id"] = json!(pw);
    // El producto web choca además con el código de uno de aquí ('P1').
    let cp = cambio("productos", json!({
        "codigo": "P1", "nombre": "Producto web borrado", "precio_venta": 55, "stock_actual": 1,
        "uuid": pw, "activo": 1, "updated_at": "2026-10-06 09:30:00", "deleted_at": "2026-10-06 09:30:00",
    }));
    // La venta viene primero en el lote: el orden por tabla pone antes al producto.
    let r = aplicar(&mut db, &[cv, cp]);
    assert_eq!((r.aplicados, r.aparcados), (2, 0), "{r:?}");
    let (pid, codigo, del, activo): (i64, String, Option<String>, i64) = db.query_row(
        "SELECT id, codigo, deleted_at, activo FROM productos WHERE uuid = ?", [&pw],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).unwrap();
    assert_eq!((codigo.as_str(), del.as_deref(), activo), ("P1-W019dd079", Some("2026-10-06 09:30:00"), 0));
    assert_eq!(i64_de(&db, &format!("SELECT producto_id FROM venta_detalle WHERE uuid = '{det}'")), pid);
    let despues = crate::commands::cortes::calcular_periodo(&db, AHORA).unwrap();
    assert_eq!(despues.total_ventas_efectivo - antes.total_ventas_efectivo, 55.0, "el dinero de la venta cuenta");
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_inbox"), 0);
    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 0, "nada de lo recibido se re-encola");
    assert_eq!(bandera(&db), 0);

    // Venta web borrada que nunca llegó: transaccional, no se crea.
    let vb = web(1402);
    let mut c = cambio_venta_web(&vb, "V-BORR2", "2026-10-06 09:10:00", 7.0);
    c["__data"]["deleted_at"] = json!("2026-10-06 09:20:00");
    // DELETE de una categoría web que nunca llegó: no se crea.
    let cat_del = json!({ "__tabla": "categorias", "__operacion": "DELETE",
        "__data": { "nombre": "Web borrada", "uuid": web(1403), "updated_at": "2026-10-06 09:00:00" } });
    // Categoría web borrada con el nombre de una viva de aquí ('Frenos'): no
    // se crea y no se atora en la bandeja.
    let cat_choca = cambio("categorias", json!({ "nombre": "Frenos", "uuid": web(1404),
        "updated_at": "2026-10-06 09:00:00", "deleted_at": "2026-10-06 09:05:00" }));
    // Cliente web borrado: sí se crea borrado.
    let cli = cambio("clientes", json!({ "nombre": "Cliente web borrado", "uuid": web(1405), "activo": 1,
        "updated_at": "2026-10-06 09:00:00", "deleted_at": "2026-10-06 09:05:00" }));
    let r = aplicar(&mut db, &[c, cat_del, cat_choca, cli]);
    assert_eq!((r.aplicados, r.omitidos, r.aparcados), (1, 3, 0), "{r:?}");
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM ventas WHERE uuid = '{vb}'")), 0);
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM categorias WHERE uuid IN ('{}', '{}')", web(1403), web(1404))), 0);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM categorias WHERE nombre = 'Frenos' AND deleted_at IS NULL"), 1);
    assert_eq!(
        i64_de(&db, &format!("SELECT COUNT(*) FROM clientes WHERE uuid = '{}' AND deleted_at IS NOT NULL AND activo = 0", web(1405))),
        1
    );
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_inbox"), 0);
    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 0);
}

/// D5: re-descarga en una PC nueva de un producto hecho en OTRO escritorio
/// cuyo código ya es de otro producto aquí (el servidor tiene pares viejos
/// con el mismo código): se guarda como '<código>-D<8 del uuid>' en vez de
/// quedarse en la bandeja con 'UNIQUE constraint failed'. Una versión
/// posterior con el código original conserva el código local.
#[test]
fn producto_del_escritorio_con_codigo_repetido_se_guarda_con_d() {
    let mut db = nueva_db();
    let otro = "abcdef0123456789abcdef0123456789";
    let r = aplicar_lote_con(&mut db, &[cambio("productos", json!({
        "codigo": "P1", "nombre": "Balata repetida", "precio_venta": 80, "stock_actual": 3,
        "uuid": otro, "activo": 1, "updated_at": "2026-09-01 10:00:00",
    }))], Modo::Replay, AHORA).unwrap();
    assert_eq!((r.aplicados, r.aparcados), (1, 0), "{r:?}");
    let codigo = |db: &Connection, u: &str| opt_txt(db, &format!("SELECT codigo FROM productos WHERE uuid = '{u}'")).unwrap();
    assert_eq!(codigo(&db, otro), "P1-Dabcdef01");
    assert_eq!(codigo(&db, P_BALATA), "P1", "el producto local no cambia");
    // Otro más en el pull normal (venta web de él incluida): mismo trato.
    let otro2 = "1234567890abcdef1234567890abcdef";
    let r = aplicar(&mut db, &[cambio("productos", json!({
        "codigo": "P1", "nombre": "Balata repetida 2", "uuid": otro2, "activo": 1, "updated_at": "2026-10-06 09:00:00",
    }))]);
    assert_eq!(r.aplicados, 1, "{r:?}");
    assert_eq!(codigo(&db, otro2), "P1-D12345678");
    // Versión posterior con el código original: se queda el código local.
    aplicar(&mut db, &[cambio("productos", json!({
        "codigo": "P1", "nombre": "Balata repetida editada", "uuid": otro, "updated_at": "2026-10-06 10:00:00",
    }))]);
    assert_eq!(codigo(&db, otro), "P1-Dabcdef01");
    assert_eq!(opt_txt(&db, &format!("SELECT nombre FROM productos WHERE uuid = '{otro}'")).as_deref(), Some("Balata repetida editada"));
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_inbox"), 0);
    assert_eq!(outbox::contar_pendientes(&db).unwrap(), 0);
}

/// PC nueva (BD vacía): abre el 5 de octubre a las 9:00 con 1000, vende 200
/// en efectivo y hace su corte del DÍA a las 18:00 dejando 1000 de fondo.
fn pc_nueva_con_su_primer_corte(db: &Connection) {
    use crate::commands::cortes::{calcular_periodo, crear_apertura_core, crear_corte_core, NuevaApertura, NuevoCorte};
    use crate::commands::ventas::{crear_venta_core, ItemVenta, NuevaVenta};
    crear_apertura_core(db, NuevaApertura { usuario_id: 1, fondo_declarado: 1000.0, nota: None }, "2026-10-05 09:00:00").unwrap();
    crear_venta_core(db, NuevaVenta {
        usuario_id: 1, cliente_id: None, subtotal: 200.0, descuento: 0.0, total: 200.0,
        metodo_pago: "efectivo".into(), monto_recibido: 200.0, cambio: 0.0,
        items: vec![ItemVenta {
            producto_id: 1, cantidad: 2.0, precio_original: 100.0, descuento_porcentaje: 0.0,
            descuento_monto: 0.0, precio_final: 100.0, subtotal: 200.0, autorizado_por: None,
        }],
        presupuesto_origen_id: None,
    }, "2026-10-05 12:00:00").unwrap();
    let datos = calcular_periodo(db, "2026-10-05 18:00:00").unwrap();
    assert_eq!(datos.efectivo_esperado, 1200.0);
    let c = crear_corte_core(db, NuevoCorte {
        tipo: "DIA".into(), usuario_id: 1,
        fecha_inicio: datos.fecha_inicio.clone(), fecha_fin: datos.fecha_fin.clone(), datos,
        efectivo_contado: 1200.0, nota_diferencia: None, fondo_siguiente: 1000.0,
        denominaciones: None, retiro: None,
    }, "2026-10-05 18:00:00").unwrap();
    assert_eq!(c.diferencia, 0.0);
    limpiar_outbox(db);
}

fn esperado_a_las(db: &Connection, ahora: &str) -> f64 {
    crate::commands::cortes::calcular_periodo(db, ahora).unwrap().efectivo_esperado
}

/// D3 (revisión rev2_d): en una PC nueva, la bandeja reintenta DESPUÉS de su
/// primer corte una venta web espejo vieja (2 de octubre, antes de que este
/// cajón existiera). No se re-acomoda al período abierto: el esperado sigue
/// siendo el fondo (1000), no 1500.
#[test]
fn pc_nueva_venta_web_vieja_tras_el_primer_corte_no_se_reacomoda() {
    let mut db = nueva_db();
    // acepta_web_desde ya bajó a la hora en que se actualizó el servidor.
    db.execute("UPDATE sync_state SET acepta_web_desde = '2026-09-01 00:00:00' WHERE id = 1", []).unwrap();
    pc_nueva_con_su_primer_corte(&db);
    let v = web(1600);
    let r = aplicar_lote_con(&mut db, &[cambio_venta_web(&v, "V-VIEJA", "2026-10-02 10:00:00", 500.0)],
        Modo::Normal, "2026-10-05 19:00:00").unwrap();
    assert_eq!(r.aplicados, 1, "{r:?}");
    assert_eq!(opt_txt(&db, &format!("SELECT registrado_at FROM ventas WHERE uuid = '{v}'")), None);
    assert_eq!(esperado_a_las(&db, "2026-10-05 20:00:00"), 1000.0);
}

/// D3 (revisión rev2_d2): la PC trabajó sin sync y se conecta después de su
/// primer corte. El primer pull baja acepta_web_desde de la hora de
/// instalación a `caja_v2_desde` y trae las ventas web de la caja principal
/// de antes de que esta PC existiera: ninguna se re-acomoda (esperado 1000,
/// no 1750).
#[test]
fn pc_nueva_conectada_despues_del_primer_corte_no_reacomoda_la_historia_web() {
    let mut db = nueva_db();
    db.execute("UPDATE sync_state SET acepta_web_desde = '2026-10-05 08:00:00' WHERE id = 1", []).unwrap();
    pc_nueva_con_su_primer_corte(&db);
    super::state::ajustar_desde_respuesta(&db, Some("2026-09-01 00:00:00"));
    assert_eq!(opt_txt(&db, "SELECT acepta_web_desde FROM sync_state").as_deref(), Some("2026-09-01 00:00:00"));
    let (v1, v2) = (web(1610), web(1611));
    let r = aplicar_lote_con(&mut db, &[
        cambio_venta_web(&v1, "V-HIST1", "2026-09-15 10:00:00", 500.0),
        cambio_venta_web(&v2, "V-HIST2", "2026-10-03 10:00:00", 250.0),
    ], Modo::Normal, "2026-10-05 19:00:00").unwrap();
    assert_eq!(r.aplicados, 2, "{r:?}");
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM ventas WHERE origen = 'web' AND registrado_at IS NOT NULL"), 0);
    assert_eq!(esperado_a_las(&db, "2026-10-05 20:00:00"), 1000.0);
}

/// D3 (positivo): en esa misma PC nueva, una venta web espejo hecha DESPUÉS
/// de su primera apertura (ese dinero sí entró a este cajón) que llega
/// después del corte sí se re-acomoda y cuenta en el período abierto.
#[test]
fn pc_nueva_venta_web_posterior_a_su_apertura_si_se_reacomoda() {
    let mut db = nueva_db();
    db.execute("UPDATE sync_state SET acepta_web_desde = '2026-09-01 00:00:00' WHERE id = 1", []).unwrap();
    pc_nueva_con_su_primer_corte(&db);
    let v = web(1620);
    aplicar_lote_con(&mut db, &[cambio_venta_web(&v, "V-NUEVA", "2026-10-05 10:30:00", 80.0)],
        Modo::Normal, "2026-10-05 19:00:00").unwrap();
    assert_eq!(
        opt_txt(&db, &format!("SELECT registrado_at FROM ventas WHERE uuid = '{v}'")).as_deref(),
        Some("2026-10-05 19:00:00")
    );
    assert_eq!(esperado_a_las(&db, "2026-10-05 20:00:00"), 1080.0);
    // Y un movimiento web de la caja principal del mismo momento: igual.
    let m = web(1621);
    let mut c = mov_web(&m, "web", Some("principal"), 30.0, "2026-10-05 10:45:00");
    c["__data"]["fecha"] = json!("2026-10-05 10:45:00");
    aplicar_lote_con(&mut db, &[c], Modo::Normal, "2026-10-05 19:05:00").unwrap();
    assert_eq!(
        opt_txt(&db, &format!("SELECT registrado_at FROM movimientos_caja WHERE uuid = '{m}'")).as_deref(),
        Some("2026-10-05 19:05:00")
    );
}

// ─── Migración 015 ────────────────────────────────────────

/// "Baja" una BD ya migrada a v14: quita columnas/tablas de la 015.
fn bajar_a_v14(db: &Connection) {
    db.execute_batch(
        "DROP INDEX idx_ventas_caja_fecha; DROP INDEX idx_ventas_momento; \
         DROP INDEX idx_movimientos_caja_caja; DROP INDEX idx_movimientos_momento; \
         DROP TABLE sync_inbox; DROP TABLE sync_base;",
    ).unwrap();
    for t in ["ventas", "cortes", "movimientos_caja", "aperturas_caja"] {
        db.execute_batch(&format!("ALTER TABLE {t} DROP COLUMN origen; ALTER TABLE {t} DROP COLUMN caja;")).unwrap();
    }
    for t in ["ventas", "movimientos_caja"] {
        db.execute_batch(&format!("ALTER TABLE {t} DROP COLUMN registrado_at;")).unwrap();
    }
    for c in ["acepta_web_desde", "mapa_ids_subido_at", "referencias_reparadas_at", "replay_cursor", "replay_hasta"] {
        db.execute_batch(&format!("ALTER TABLE sync_state DROP COLUMN {c};")).unwrap();
    }
    db.execute_batch("PRAGMA user_version = 14").unwrap();
}

/// Simula una BD v14 con datos: se quitan las columnas/tablas de la 015 y se
/// baja user_version; luego se migra como al actualizar el POS.
#[test]
fn migracion_015_sobre_bd_con_datos() {
    let db = nueva_db();
    db.execute(
        "INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid, updated_at) \
         VALUES (1, 'V-000001', 1, 100, '2026-10-06 09:00:00', ?1, '2026-10-06 09:00:00'), \
                (2, 'V-20260427-0001', 2, 3, '2026-04-27 10:00:00', ?2, '2026-04-27 10:00:00')",
        [hex(1), web(1)],
    ).unwrap();
    db.execute(
        "INSERT INTO movimientos_caja (tipo, usuario_id, monto, concepto, fecha, uuid) VALUES ('RETIRO', 1, 3, 'x', '2026-04-27 10:00:00', ?1)",
        [web(2)],
    ).unwrap();
    db.execute("INSERT INTO aperturas_caja (usuario_id, fondo_declarado, fecha, uuid) VALUES (1, 0, '2026-04-25 08:10:00', ?1)", [web(3)]).unwrap();
    db.execute(
        "INSERT INTO cortes (tipo, usuario_id, fecha_inicio, fecha_fin, created_at) VALUES ('DIA', 1, '2026-10-05 08:00:00', '2026-10-05 18:00:00', '2026-10-05 18:00:00')",
        [],
    ).unwrap();
    // Todas las marcas en el pasado (los defaults datetime('now') son UTC y en
    // México quedarían "en el futuro")…
    for t in ["productos", "proveedores", "clientes", "usuarios", "categorias", "sucursales",
              "stock_sucursal", "ventas", "presupuestos", "ordenes_pedido", "recepciones", "cortes",
              "devoluciones", "transferencias", "movimientos_caja", "aperturas_caja", "audit_log"] {
        db.execute(&format!("UPDATE {t} SET updated_at = '2026-10-01 00:00:00'"), []).unwrap();
    }
    // …salvo un producto con marca en el futuro (reloj mal / UTC).
    db.execute("UPDATE productos SET updated_at = '2099-01-01 00:00:00' WHERE id = 1", []).unwrap();

    // "Bajar" a v14.
    bajar_a_v14(&db);
    limpiar_outbox(&db);
    db.execute("INSERT INTO sync_suppress_flag (id) VALUES (1)", []).unwrap(); // pegada de antes
    let outbox_antes = i64_de(&db, "SELECT COUNT(*) FROM sync_outbox");

    aplicar_migraciones(&db).unwrap();
    db.execute_batch(SEED_DATA).unwrap();

    assert_eq!(i64_de(&db, "PRAGMA user_version"), 15);
    assert_eq!(bandera(&db), 0);
    let fila = |uuid: &str| -> (String, String) {
        db.query_row("SELECT origen, caja FROM ventas WHERE uuid = ?", [uuid], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
    };
    assert_eq!(fila(&hex(1)), ("desktop".into(), "principal".into()));
    assert_eq!(fila(&web(1)), ("web".into(), "principal".into()));
    assert_eq!(
        opt_txt(&db, &format!("SELECT updated_at FROM ventas WHERE uuid = '{}'", web(1))).as_deref(),
        Some("2026-10-01 00:00:00"),
        "marcar origen='web' no mueve updated_at"
    );
    assert_eq!(opt_txt(&db, &format!("SELECT origen FROM movimientos_caja WHERE uuid = '{}'", web(2))).as_deref(), Some("web"));
    assert_eq!(opt_txt(&db, &format!("SELECT caja FROM aperturas_caja WHERE uuid = '{}'", web(3))).as_deref(), Some("principal"));
    assert_eq!(opt_txt(&db, "SELECT origen || '/' || caja FROM cortes").as_deref(), Some("desktop/principal"));
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM ventas WHERE registrado_at IS NOT NULL"), 0);
    assert!(opt_txt(&db, "SELECT acepta_web_desde FROM sync_state").is_some());
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_inbox"), 0);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'trg_stock_sucursal_outbox%'"), 0);
    // La marca en el futuro se convierte a hora local SIN re-encolar la fila
    // (re-encolarla con "ahora" pisaría ediciones web hechas en esas horas).
    let nuevos: Vec<String> = db.prepare("SELECT tabla FROM sync_outbox WHERE synced_at IS NULL ORDER BY id")
        .unwrap().query_map([], |r| r.get(0)).unwrap().map(|x| x.unwrap()).collect();
    assert!(nuevos.is_empty(), "outbox antes: {outbox_antes}, nuevos: {nuevos:?}");
    assert!(opt_txt(&db, "SELECT updated_at FROM productos WHERE id = 1").unwrap() < "2099".to_string());
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_base"), 0);
    // Repetir las migraciones no hace nada.
    aplicar_migraciones(&db).unwrap();
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_outbox WHERE synced_at IS NULL"), 0);
}

/// Actualización real sobre una COPIA de la BD de desarrollo (nunca la
/// original). Se salta si POS_DB_COPIA_DEV no apunta a una copia.
#[test]
fn migracion_015_sobre_copia_de_bd_dev() {
    let Ok(origen) = std::env::var("POS_DB_COPIA_DEV") else {
        eprintln!("POS_DB_COPIA_DEV no definida: se salta la prueba con la copia de la BD de desarrollo");
        return;
    };
    assert!(!origen.contains("Application Support"), "usar una COPIA, nunca la BD original");
    let dir = std::env::temp_dir().join(format!("pos_sync_copia_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ruta = dir.join("pos_database.db");
    std::fs::copy(&origen, &ruta).unwrap();

    let (version_antes, ventas_antes, productos_antes): (i64, i64, i64) = {
        let c = Connection::open(&ruta).unwrap();
        (
            i64_de(&c, "PRAGMA user_version"),
            i64_de(&c, "SELECT COUNT(*) FROM ventas"),
            i64_de(&c, "SELECT COUNT(*) FROM productos"),
        )
    };
    let db = crate::db::connection::init_database(&ruta).expect("la BD de desarrollo debe abrir y migrar");
    eprintln!("copia dev: user_version {version_antes} → {}", i64_de(&db, "PRAGMA user_version"));
    assert_eq!(i64_de(&db, "PRAGMA user_version"), 15);
    for t in ["ventas", "cortes", "movimientos_caja", "aperturas_caja"] {
        assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM pragma_table_info('{t}') WHERE name IN ('origen', 'caja')")), 2, "{t}");
        assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM {t} WHERE caja <> 'principal' OR origen NOT IN ('desktop', 'web')")), 0, "{t}");
    }
    for t in ["ventas", "movimientos_caja"] {
        assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM pragma_table_info('{t}') WHERE name = 'registrado_at'")), 1);
    }
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM ventas"), ventas_antes);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM productos"), productos_antes);
    assert_eq!(bandera(&db), 0);
    assert!(opt_txt(&db, "SELECT acepta_web_desde FROM sync_state").is_some());
    // El diagnóstico corre sobre el esquema real.
    let d = crate::commands::sync_remoto::diagnostico(&db).unwrap();
    assert!(d.tablas.iter().all(|t| !t.columnas.is_empty()), "todas las tablas sync existen");
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─── Contra un servidor simulado (HTTP real en 127.0.0.1) ─

mod con_servidor_simulado {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::{
        extract::{Query, State},
        http::StatusCode,
        routing::{get, post},
        Json, Router,
    };

    use crate::sync::client::{RemoteClient, Respuesta};
    use crate::sync::{state, tareas};

    /// Acción que corre DENTRO del handler de /sync/push (simula un cambio
    /// local mientras viaja la llamada).
    type Durante = Arc<dyn Fn() + Send + Sync>;

    #[derive(Clone, Default)]
    struct Mock {
        llamadas: Arc<Mutex<Vec<(String, Value)>>>,
        refs: Value,
        paginas: Arc<HashMap<String, Value>>,
        fila: Value,
        /// uuid → motivo de rechazo en /sync/push.
        rechazar: Arc<Mutex<HashMap<String, String>>>,
        durante_push: Option<Durante>,
        /// Respuesta de /sync/reparar (Null → la de siempre).
        reparar: Value,
        /// uuid → marca con la que "el servidor" guardó la fila (`marcas`
        /// de la respuesta del push, solo aceptados). Vacío = servidor viejo.
        marcas: Arc<Mutex<HashMap<String, String>>>,
        /// Milisegundos que tardan /sync/push y /sync/pull en contestar.
        espera_ms: u64,
        /// Llamadas push/pull en vuelo ahora y el máximo visto.
        en_vuelo: Arc<AtomicUsize>,
        max_en_vuelo: Arc<AtomicUsize>,
    }

    impl Mock {
        fn anotar(&self, ruta: &str, v: Value) {
            self.llamadas.lock().unwrap().push((ruta.to_string(), v));
        }
        fn de(&self, ruta: &str) -> Vec<Value> {
            self.llamadas.lock().unwrap().iter().filter(|(r, _)| r == ruta).map(|(_, v)| v.clone()).collect()
        }
        /// Marca el inicio de una llamada lenta; devuelve cuántas van.
        async fn entrar(&self) {
            let n = self.en_vuelo.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_en_vuelo.fetch_max(n, Ordering::SeqCst);
            if self.espera_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.espera_ms)).await;
            }
        }
        fn salir(&self) {
            self.en_vuelo.fetch_sub(1, Ordering::SeqCst);
        }
    }

    async fn id_map(State(m): State<Mock>, Json(b): Json<Value>) -> Json<Value> {
        let n = b["pares"].as_array().map(|a| a.len()).unwrap_or(0);
        m.anotar("id-map", b);
        Json(json!({ "recibidos": n, "remapeados": 0, "invalidos": 0, "desconocidos": ["no-existe"] }))
    }
    async fn reparar(State(m): State<Mock>, Json(b): Json<Value>) -> Json<Value> {
        m.anotar("reparar", b);
        if m.reparar.is_null() {
            Json(json!({ "lote": "L1", "celdas": 3 }))
        } else {
            Json(m.reparar.clone())
        }
    }
    async fn refs(State(m): State<Mock>, Json(b): Json<Value>) -> Json<Value> {
        m.anotar("refs", b);
        Json(m.refs.clone())
    }
    async fn pull(State(m): State<Mock>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
        m.entrar().await;
        m.anotar("pull", json!(q));
        let c = q.get("cursor").cloned().unwrap_or_default();
        m.salir();
        Json(m.paginas.get(&c).cloned().unwrap_or(json!({ "cambios": [], "next_cursor": c, "hay_mas": false })))
    }
    async fn fila(State(m): State<Mock>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
        m.anotar("fila", json!(q));
        Json(m.fila.clone())
    }

    async fn push(State(m): State<Mock>, Json(b): Json<Value>) -> Json<Value> {
        m.entrar().await;
        if let Some(f) = &m.durante_push {
            f();
        }
        let rechazar = m.rechazar.lock().unwrap().clone();
        let marcas_cfg = m.marcas.lock().unwrap().clone();
        let mut aceptados = Vec::new();
        let mut rechazados = Vec::new();
        let mut marcas = Map::new();
        for c in b["cambios"].as_array().unwrap() {
            let u = c["data"]["uuid"].as_str().unwrap_or_default().to_string();
            match rechazar.get(&u) {
                Some(motivo) => rechazados.push(json!({ "uuid": u, "motivo": motivo })),
                None => {
                    if let Some(mk) = marcas_cfg.get(&u) {
                        marcas.insert(u.clone(), json!(mk));
                    }
                    aceptados.push(json!(u));
                }
            }
        }
        m.anotar("push", b);
        m.salir();
        let mut r = json!({ "aceptados": aceptados, "rechazados": rechazados });
        if !marcas_cfg.is_empty() {
            r["marcas"] = Value::Object(marcas);
        }
        Json(r)
    }

    async fn servir(app: Router) -> String {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn servidor_v2(m: Mock) -> String {
        servir(
            Router::new()
                .route("/sync/push", post(push))
                .route("/sync/id-map", post(id_map))
                .route("/sync/reparar", post(reparar))
                .route("/sync/refs", post(refs))
                .route("/sync/pull", get(pull))
                .route("/sync/fila", get(fila))
                .with_state(m),
        )
        .await
    }

    /// Servidor sin actualizar: la SPA contesta index.html a lo desconocido;
    /// POST a rutas de archivos da 405 y otras 404.
    async fn servidor_viejo() -> String {
        servir(
            Router::new()
                .route("/sync/id-map", post(|| async { StatusCode::METHOD_NOT_ALLOWED }))
                .route("/sync/refs", post(|| async { StatusCode::NOT_FOUND }))
                .route("/sync/reparar", post(|| async { StatusCode::METHOD_NOT_ALLOWED }))
                .fallback(|| async { axum::response::Html("<!doctype html><html><body>SPA</body></html>") }),
        )
        .await
    }

    fn cfg(device: &str) -> state::SyncConfig {
        state::SyncConfig {
            device_uuid: device.into(),
            sucursal_id: 1,
            remote_url: None,
            remote_token: None,
            last_push_at: None,
            last_pull_cursor: None,
            last_pull_at: None,
            activo: true,
        }
    }

    fn con_cursor(mut c: Value, cursor: i64) -> Value {
        c["__cursor"] = json!(cursor);
        c
    }

    #[tokio::test]
    async fn mapa_reparacion_espejo_y_replay() {
        let db = nueva_db();
        // 12 001 productos: el mapa se sube en tandas de ≤ 5000.
        db.execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 2 UNION ALL SELECT i + 1 FROM n WHERE i < 12001) \
             INSERT INTO productos (id, codigo, nombre, uuid) \
             SELECT i, 'C' || i, 'p', lower(hex(randomblob(16))) FROM n",
        ).unwrap();
        let v = web(500);
        db.execute(
            "INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid, origen) VALUES (414, 'V-W', 2, 8, '2026-04-30 10:00:00', ?1, 'web')",
            [&v],
        ).unwrap();
        db.execute("UPDATE sync_state SET last_pull_cursor = '10' WHERE id = 1", []).unwrap();
        limpiar_outbox(&db);
        let db = Arc::new(Mutex::new(db));

        let (a, b, c) = (web(501), web(502), web(503));
        let mut paginas = HashMap::new();
        paginas.insert("0".to_string(), json!({
            "cambios": [con_cursor(cambio_venta_web(&a, "V-A", "2026-09-01 10:00:00", 1.0), 5)],
            "next_cursor": "8", "hay_mas": true,
        }));
        paginas.insert("8".to_string(), json!({
            "cambios": [
                con_cursor(cambio_venta_web(&b, "V-B", "2026-09-02 10:00:00", 2.0), 9),
                con_cursor(cambio_venta_web(&c, "V-C", "2026-09-03 10:00:00", 3.0), 12),
            ],
            "next_cursor": "12", "hay_mas": false,
        }));
        let m = Mock {
            refs: json!({ v.clone(): { "usuario_id": U_DUENO, "cliente_id": null } }),
            paginas: Arc::new(paginas),
            ..Default::default()
        };
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();

        let rep = tareas::reparar_referencias(&db, &client, "dev-1", false).await
            .map_err(|e| e.mensaje()).unwrap();

        // a) mapa en tandas ≤ 5000, todas las tablas de la lista.
        let ids = m.de("id-map");
        assert!(ids.iter().all(|b| b["pares"].as_array().unwrap().len() <= tareas::MAX_PARES));
        assert!(ids.iter().all(|b| b["device_uuid"] == json!("dev-1")));
        let prod: Vec<usize> = ids.iter().filter(|b| b["tabla"] == json!("productos"))
            .map(|b| b["pares"].as_array().unwrap().len()).collect();
        assert_eq!(prod, vec![5000, 5000, 2001]);
        let mt = rep.mapa.tablas.iter().find(|t| t.tabla == "productos").unwrap();
        assert_eq!((mt.enviados, mt.recibidos, mt.desconocidos), (12001, 12001, 3));
        let tablas: Vec<&str> = rep.mapa.tablas.iter().map(|t| t.tabla.as_str()).collect();
        assert_eq!(tablas, tareas::TABLAS_MAPA.to_vec());
        assert!(!tablas.contains(&"stock_sucursal"));
        let par0 = &ids.iter().find(|b| b["tabla"] == json!("usuarios")).unwrap()["pares"][0];
        assert_eq!(par0, &json!([1, U_DUENO]));
        // Reparación del servidor para este equipo.
        assert_eq!(m.de("reparar"), vec![json!({ "device_uuid": "dev-1" })]);
        assert_eq!(rep.servidor["lote"], json!("L1"));
        // b) espejo: la venta web local queda con el Dueño local.
        assert_eq!(rep.espejo.celdas_cambiadas, 1, "{:?}", rep.espejo);
        assert!(m.de("refs").iter().any(|b| b["tabla"] == json!("ventas") && b["uuids"] == json!([v])));
        {
            let conn = db.lock().unwrap();
            assert_eq!(i64_de(&conn, "SELECT usuario_id FROM ventas WHERE id = 414"), 1);
            let t = state::leer_tareas(&conn).unwrap();
            assert!(t.mapa_ids_subido_at.is_some() && t.referencias_reparadas_at.is_some());
            assert_eq!((t.replay_cursor.as_deref(), t.replay_hasta.as_deref()), (Some("0"), Some("10")));
            assert!(t.replay_pendiente());
        }
        // Otra vez (worker): no vuelve a subir el mapa.
        tareas::reparar_referencias(&db, &client, "dev-1", false).await.map_err(|e| e.mensaje()).unwrap();
        assert_eq!(m.de("id-map").len(), ids.len());

        // c) re-descarga: solo hasta el cursor que había al reparar.
        tareas::correr_replay(&db, &client, &cfg("dev-1")).await.unwrap();
        let pulls = m.de("pull");
        assert_eq!(pulls.len(), 2);
        assert_eq!(pulls[0]["device_uuid"], json!("dev-1"));
        assert_eq!(pulls[0]["proto"], json!("2"));
        let conn = db.lock().unwrap();
        for (u, esperado) in [(&a, 1), (&b, 1), (&c, 0)] {
            assert_eq!(i64_de(&conn, &format!("SELECT COUNT(*) FROM ventas WHERE uuid = '{u}'")), esperado, "{u}");
        }
        let t = state::leer_tareas(&conn).unwrap();
        assert_eq!(t.replay_cursor.as_deref(), Some("10"));
        assert!(!t.replay_pendiente());
        assert_eq!(
            opt_txt(&conn, "SELECT last_pull_cursor FROM sync_state").as_deref(),
            Some("10"),
            "la re-descarga no mueve el cursor normal"
        );
    }

    #[tokio::test]
    async fn bandeja_reintenta_con_la_fila_del_servidor() {
        let mut db = nueva_db();
        let v = web(600);
        let mut sin_refs = cambio("ventas", Value::Object(venta_web(&v, "V-600", "2026-10-06 10:00:00", 5.0, Some("principal"))));
        sin_refs["__children"] = json!({ "venta_detalle": [] });
        aplicar(&mut db, &[sin_refs]);
        assert_eq!(inbox_motivo(&db, "ventas", &v).as_deref(), Some("sin_fk"));
        db.execute(
            "UPDATE sync_inbox SET proximo_intento = '2000-01-01 00:00:00', created_at = datetime('now','localtime')",
            [],
        ).unwrap();
        let db = Arc::new(Mutex::new(db));
        let m = Mock { fila: cambio_venta_web(&v, "V-600", "2026-10-06 10:00:00", 5.0), ..Default::default() };
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();

        let n = crate::sync::inbox::reintentar(&db, &client).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(m.de("fila"), vec![json!({ "tabla": "ventas", "uuid": v })]);
        let conn = db.lock().unwrap();
        assert_eq!(i64_de(&conn, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{v}'")), 1);
        assert_eq!(i64_de(&conn, "SELECT COUNT(*) FROM sync_inbox"), 0);
    }

    #[tokio::test]
    async fn servidor_sin_actualizar_no_es_error() {
        let mut db = nueva_db();
        let v = web(700);
        db.execute(
            "INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid, origen) VALUES (414, 'V-W', 2, 8, '2026-04-30 10:00:00', ?1, 'web')",
            [&v],
        ).unwrap();
        // Un cambio que esperaba a que subiera el cambio local (ya subió).
        db.execute("UPDATE productos SET nombre = 'Local', updated_at = '2026-10-06 09:00:00' WHERE id = 1", []).unwrap();
        aplicar(&mut db, &[cambio("productos", json!({
            "codigo": "P1", "nombre": "Web", "uuid": P_BALATA, "updated_at": "2026-10-06 09:30:00",
        }))]);
        db.execute("UPDATE sync_inbox SET created_at = datetime('now','localtime')", []).unwrap();
        limpiar_outbox(&db);
        let db = Arc::new(Mutex::new(db));
        let url = servidor_viejo().await;
        let client = RemoteClient::new(&url, "token").unwrap();

        assert!(matches!(client.subir_mapa("d", "usuarios", &[(1, "u".into())]).await, Ok(Respuesta::NoSoportado)));
        assert!(matches!(client.refs("ventas", &[v.clone()]).await, Ok(Respuesta::NoSoportado)));
        assert!(matches!(client.fila("ventas", &v).await, Ok(Respuesta::NoSoportado)), "index.html no es JSON");
        assert!(matches!(
            tareas::reparar_referencias(&db, &client, "d", false).await,
            Err(tareas::ErrorTarea::NoSoportado)
        ));
        {
            let conn = db.lock().unwrap();
            let t = state::leer_tareas(&conn).unwrap();
            assert!(t.mapa_ids_subido_at.is_none() && t.referencias_reparadas_at.is_none());
            assert_eq!(i64_de(&conn, "SELECT usuario_id FROM ventas WHERE id = 414"), 2, "nada se tocó");
        }
        // Sin /sync/fila la bandeja usa el payload guardado.
        assert_eq!(crate::sync::inbox::reintentar(&db, &client).await.unwrap(), 1);
        let conn = db.lock().unwrap();
        assert_eq!(opt_txt(&conn, "SELECT nombre FROM productos WHERE id = 1").as_deref(), Some("Web"));
    }

    #[tokio::test]
    async fn push_con_refs_y_pull_mientras_haya_mas() {
        let db = nueva_db();
        db.execute(
            "INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid) VALUES (1, 'V-000001', 1, 100, '2026-10-06 09:00:00', ?1)",
            [hex(1)],
        ).unwrap();
        db.execute(
            "INSERT INTO venta_detalle (venta_id, producto_id, cantidad, precio_final, subtotal, uuid) VALUES (1, 1, 1, 100, 100, ?1)",
            [hex(2)],
        ).unwrap();
        // Fila vieja de stock_sucursal en la cola: ya no se envía.
        db.execute("INSERT INTO sync_outbox (tabla, uuid, operacion) VALUES ('stock_sucursal', 'x', 'UPDATE')", []).unwrap();
        let db = Arc::new(Mutex::new(db));

        // 3 páginas: el pull sigue mientras `hay_mas`.
        let mut paginas = HashMap::new();
        for (i, (cur, sig, mas)) in [("", "3", true), ("3", "6", true), ("6", "9", false)].iter().enumerate() {
            paginas.insert(cur.to_string(), json!({
                "cambios": [cambio("categorias", json!({ "nombre": format!("Web {i}"), "uuid": web(800 + i as u64),
                                                       "updated_at": "2026-10-06 10:00:00" }))],
                "next_cursor": sig, "hay_mas": mas,
            }));
        }
        let m = Mock { paginas: Arc::new(paginas), ..Default::default() };
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();
        let c = cfg("dev-9");

        crate::sync::worker::push(&db, &client, &c).await.unwrap();
        let pushes = m.de("push");
        assert_eq!(pushes.len(), 1);
        let body = &pushes[0];
        assert_eq!(body["proto"], json!(2));
        assert_eq!(body["device_uuid"], json!("dev-9"));
        let venta = body["cambios"].as_array().unwrap().iter().find(|x| x["tabla"] == json!("ventas")).unwrap();
        assert_eq!(venta["refs"][hex(1)]["usuario_id"], json!(U_DUENO));
        assert_eq!(venta["refs"][hex(2)]["producto_id"], json!(P_BALATA));
        assert!(venta["data"].get("__refs").is_none() && venta["data"].get("__children").is_none());
        assert_eq!(venta["children"]["venta_detalle"].as_array().unwrap().len(), 1);
        assert!(body["cambios"].as_array().unwrap().iter().all(|x| x["tabla"] != json!("stock_sucursal")));
        {
            let conn = db.lock().unwrap();
            assert_eq!(outbox::contar_pendientes(&conn).unwrap(), 0, "aceptados + stock_sucursal descartada");
        }

        crate::sync::worker::pull(&db, &client, &c).await.unwrap();
        let pulls = m.de("pull");
        assert_eq!(pulls.len(), 3);
        assert!(pulls.iter().all(|q| q["device_uuid"] == json!("dev-9") && q["proto"] == json!("2")));
        let conn = db.lock().unwrap();
        assert_eq!(i64_de(&conn, "SELECT COUNT(*) FROM categorias WHERE nombre LIKE 'Web %'"), 3);
        assert_eq!(opt_txt(&conn, "SELECT last_pull_cursor FROM sync_state").as_deref(), Some("9"));
        assert_eq!(outbox::contar_pendientes(&conn).unwrap(), 0, "lo recibido no se re-encola");
    }

    // ─── Ronda 2 (SPEC2 R3) ───────────────────────────────

    fn cambio_de<'a>(body: &'a Value, uuid: &str) -> &'a Value {
        body["cambios"].as_array().unwrap().iter().find(|c| c["data"]["uuid"] == json!(uuid))
            .unwrap_or_else(|| panic!("{uuid} no viajó en el push"))
    }

    fn hijo_de<'a>(c: &'a Value, tabla: &str, uuid: &str) -> &'a Value {
        c["children"][tabla].as_array().unwrap().iter().find(|h| h["uuid"] == json!(uuid)).unwrap()
    }

    /// R3.6: hasta que termina la reparación de referencias, las FK
    /// (Tabla/Polimórfico) de las filas hechas en la web no viajan ni en
    /// `data`, ni en `refs`, ni en `base`: guardan ids crudos del servidor.
    /// Las filas del escritorio y los hijos hechos aquí sí llevan sus FK.
    #[tokio::test]
    async fn push_quita_fks_de_filas_web_hasta_reparar() {
        let db = nueva_db();
        db.execute("INSERT INTO proveedores (id, nombre, uuid) VALUES (1, 'Prov', ?1)", [hex(500)]).unwrap();
        // Producto web bajado por la versión vieja: categoria_id 2 = 'Accesorios'
        // en el SERVIDOR (aquí 2 es otra categoría).
        let pw = web(2000);
        db.execute(
            "INSERT INTO productos (id, codigo, nombre, categoria_id, proveedor_id, stock_actual, uuid, updated_at) \
             VALUES (2, 'W1', 'Web', 2, 1, 5, ?1, '2026-10-06 09:00:00')",
            [&pw],
        ).unwrap();
        let (vw, hw, hd) = (web(2001), web(2002), hex(2003));
        db.execute(
            "INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid, origen) VALUES (50, 'V-W50', 2, 30, '2026-10-06 10:00:00', ?1, 'web')",
            [&vw],
        ).unwrap();
        db.execute(
            "INSERT INTO venta_detalle (venta_id, producto_id, cantidad, precio_final, subtotal, autorizado_por, uuid) \
             VALUES (50, 1, 1, 10, 10, 2, ?1), (50, 2, 1, 20, 20, NULL, ?2)",
            [&hw, &hd],
        ).unwrap();
        db.execute("UPDATE productos SET categoria_id = 3 WHERE id = 1", []).unwrap();
        let db = Arc::new(Mutex::new(db));
        let m = Mock::default();
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();

        crate::sync::worker::push(&db, &client, &cfg("dev-1")).await.unwrap();
        let body = m.de("push").pop().unwrap();
        let cp = cambio_de(&body, &pw);
        for col in ["categoria_id", "proveedor_id"] {
            assert!(cp["data"].get(col).is_none(), "producto web: {col} no viaja");
            assert!(cp["refs"][&pw].get(col).is_none(), "producto web: {col} no viaja en refs");
        }
        assert_eq!(cp["data"]["stock_actual"], json!(5.0), "lo demás sí viaja");
        assert!(cp.get("base").is_none(), "aún no hay base");
        let cv = cambio_de(&body, &vw);
        for col in ["usuario_id", "cliente_id", "anulada_por"] {
            assert!(cv["data"].get(col).is_none(), "venta web: {col}");
            assert!(cv["refs"][&vw].get(col).is_none(), "venta web refs: {col}");
        }
        assert_eq!(cv["data"]["sucursal_id"], json!(1), "Paso sí viaja");
        let (h_web, h_esc) = (hijo_de(cv, "venta_detalle", &hw), hijo_de(cv, "venta_detalle", &hd));
        assert!(h_web.get("producto_id").is_none() && h_web.get("autorizado_por").is_none());
        assert!(cv["refs"][&hw].get("producto_id").is_none());
        assert_eq!(h_esc["producto_id"], json!(2), "renglón hecho aquí conserva su FK");
        assert_eq!(cv["refs"][&hd]["producto_id"], json!(pw));
        let cb = cambio_de(&body, P_BALATA);
        assert_eq!(cb["data"]["categoria_id"], json!(3), "fila del escritorio: sus FK sí viajan");
        assert_eq!(cb["refs"][P_BALATA]["categoria_id"], json!(uuid_de(&db.lock().unwrap(), "categorias", 3)));
        {
            let conn = db.lock().unwrap();
            assert_eq!(outbox::contar_pendientes(&conn).unwrap(), 0);
            // La base guarda la fila local completa (lo que el servidor aceptó).
            assert_eq!(super::super::base::leer(&conn, "productos", &pw).unwrap().unwrap()["categoria_id"], json!(2));
            // Reparación hecha: a partir de aquí las FK viajan.
            conn.execute("UPDATE sync_state SET referencias_reparadas_at = '2026-10-06 12:00:00' WHERE id = 1", []).unwrap();
            conn.execute("UPDATE productos SET precio_venta = 9 WHERE id = 2", []).unwrap();
            conn.execute("UPDATE ventas SET anulada = 1, anulada_por = 1 WHERE id = 50", []).unwrap();
        }
        crate::sync::worker::push(&db, &client, &cfg("dev-1")).await.unwrap();
        let body = m.de("push").pop().unwrap();
        let cp = cambio_de(&body, &pw);
        assert_eq!(cp["data"]["categoria_id"], json!(2));
        assert!(cp["refs"][&pw].get("categoria_id").is_some());
        assert_eq!(cp["base"]["categoria_id"], json!(2), "la base viaja completa");
        let cv = cambio_de(&body, &vw);
        assert_eq!(cv["data"]["usuario_id"], json!(2));
        assert_eq!(cv["refs"][&vw]["anulada_por"], json!(U_DUENO));
        assert_eq!(hijo_de(cv, "venta_detalle", &hw)["producto_id"], json!(1));
    }

    /// R3.8: el push de productos lleva su `base` (lo último en que este
    /// equipo y el servidor coincidieron); al aceptarse, lo enviado pasa a
    /// ser la base. Un rechazo no la mueve. Otras tablas nunca llevan base.
    #[tokio::test]
    async fn push_guarda_base_y_la_manda() {
        let db = nueva_db();
        db.execute("UPDATE productos SET stock_actual = 999 WHERE id = 1", []).unwrap();
        db.execute("INSERT INTO clientes (nombre, uuid) VALUES ('Taller', ?1)", [hex(550)]).unwrap();
        let db = Arc::new(Mutex::new(db));
        let m = Mock::default();
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();
        let base_stock = |db: &Arc<Mutex<Connection>>| {
            super::super::base::leer(&db.lock().unwrap(), "productos", P_BALATA).unwrap().map(|b| b["stock_actual"].clone())
        };

        crate::sync::worker::push(&db, &client, &cfg("dev-1")).await.unwrap();
        let body = m.de("push").pop().unwrap();
        assert!(cambio_de(&body, P_BALATA).get("base").is_none());
        assert!(cambio_de(&body, &hex(550)).get("base").is_none());
        assert_eq!(base_stock(&db), Some(json!(999.0)));

        db.lock().unwrap().execute("UPDATE productos SET stock_actual = 998 WHERE id = 1", []).unwrap();
        crate::sync::worker::push(&db, &client, &cfg("dev-1")).await.unwrap();
        let body = m.de("push").pop().unwrap();
        let c = cambio_de(&body, P_BALATA);
        assert_eq!(c["base"]["stock_actual"], json!(999.0));
        assert_eq!(c["data"]["stock_actual"], json!(998.0));
        assert_eq!(c["base"]["uuid"], json!(P_BALATA));
        assert!(c["base"].get("__refs").is_none());
        assert_eq!(base_stock(&db), Some(json!(998.0)));
        {
            // Servidor sin `marcas` (viejo): la marca local no se toca y la
            // base lleva la marca local, como antes.
            let conn = db.lock().unwrap();
            let local = opt_txt(&conn, "SELECT updated_at FROM productos WHERE id = 1").unwrap();
            assert_eq!(c["data"]["updated_at"], json!(local));
            assert_eq!(opt_txt(&conn, "SELECT updated_at FROM sync_base WHERE uuid = '0000000000000000000000000000c001'"), Some(local));
        }

        // Rechazado: la base no cambia; productos no pide la versión del
        // servidor (el servidor fusiona; la existencia local no se pierde).
        m.rechazar.lock().unwrap().insert(P_BALATA.to_string(), "remoto_mas_nuevo".into());
        db.lock().unwrap().execute("UPDATE productos SET stock_actual = 997 WHERE id = 1", []).unwrap();
        crate::sync::worker::push(&db, &client, &cfg("dev-1")).await.unwrap();
        assert_eq!(cambio_de(&m.de("push").pop().unwrap(), P_BALATA)["base"]["stock_actual"], json!(998.0));
        assert_eq!(base_stock(&db), Some(json!(998.0)));
        let conn = db.lock().unwrap();
        assert_eq!(outbox::contar_pendientes(&conn).unwrap(), 1);
        assert_eq!(i64_de(&conn, "SELECT COUNT(*) FROM sync_inbox"), 0);
    }

    /// R3.7: si la fila cambia mientras viaja el push, el servidor acepta la
    /// versión anterior pero el renglón del outbox sigue pendiente y el
    /// cambio nuevo sube en el siguiente ciclo (aunque caiga en el mismo
    /// segundo y el trigger no mueva created_at).
    #[tokio::test]
    async fn push_no_marca_subido_lo_que_cambio_durante_la_llamada() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let db = nueva_db();
        db.execute("INSERT INTO clientes (nombre, uuid) VALUES ('Taller', ?1)", [hex(600)]).unwrap();
        db.execute("UPDATE productos SET nombre = 'Antes' WHERE id = 1", []).unwrap();
        let db = Arc::new(Mutex::new(db));
        let una_vez = Arc::new(AtomicBool::new(true));
        let (dbc, flag) = (db.clone(), una_vez.clone());
        let m = Mock {
            durante_push: Some(Arc::new(move || {
                if flag.swap(false, Ordering::SeqCst) {
                    dbc.lock().unwrap()
                        .execute("UPDATE productos SET nombre = 'Cambió en el camino' WHERE id = 1", [])
                        .unwrap();
                }
            })),
            ..Default::default()
        };
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();

        crate::sync::worker::push(&db, &client, &cfg("dev-1")).await.unwrap();
        assert_eq!(cambio_de(&m.de("push").pop().unwrap(), P_BALATA)["data"]["nombre"], json!("Antes"));
        {
            let conn = db.lock().unwrap();
            let pend: Vec<String> = conn.prepare("SELECT tabla FROM sync_outbox WHERE synced_at IS NULL").unwrap()
                .query_map([], |r| r.get(0)).unwrap().map(|x| x.unwrap()).collect();
            assert_eq!(pend, vec!["productos".to_string()], "el cliente subió; el producto cambió en el camino");
            assert_eq!(opt_txt(&conn, "SELECT ultimo_error FROM sync_outbox WHERE synced_at IS NULL"), None);
            // La base es lo que el servidor aceptó, no lo que cambió después.
            assert_eq!(super::super::base::leer(&conn, "productos", P_BALATA).unwrap().unwrap()["nombre"], json!("Antes"));
        }
        crate::sync::worker::push(&db, &client, &cfg("dev-1")).await.unwrap();
        assert_eq!(cambio_de(&m.de("push").pop().unwrap(), P_BALATA)["data"]["nombre"], json!("Cambió en el camino"));
        assert_eq!(outbox::contar_pendientes(&db.lock().unwrap()).unwrap(), 0);
    }

    /// R3.10: rechazo 'remoto_mas_nuevo' (no productos) → la bandeja pide la
    /// versión del servidor, se aplica y el renglón del outbox queda resuelto.
    #[tokio::test]
    async fn rechazo_remoto_mas_nuevo_trae_la_version_del_servidor() {
        let db = nueva_db();
        db.execute(
            "INSERT INTO clientes (nombre, uuid, updated_at) VALUES ('Local viejo', ?1, '2026-10-06 09:00:00')",
            [hex(700)],
        ).unwrap();
        db.execute("UPDATE productos SET nombre = 'Prod local' WHERE id = 1", []).unwrap();
        // Venta del escritorio: aquí manda el escritorio; nunca se aplicaría la
        // del servidor, así que tampoco se pide.
        db.execute(
            "INSERT INTO ventas (id, folio, usuario_id, total, fecha, uuid) VALUES (1, 'V-000001', 1, 100, '2026-10-06 09:00:00', ?1)",
            [hex(701)],
        ).unwrap();
        let db = Arc::new(Mutex::new(db));
        let m = Mock {
            fila: cambio("clientes", json!({ "id": 9, "nombre": "Web nuevo", "uuid": hex(700),
                                             "updated_at": "2026-10-06 10:00:00" })),
            ..Default::default()
        };
        for u in [hex(700), P_BALATA.to_string(), hex(701)] {
            m.rechazar.lock().unwrap().insert(u, "remoto_mas_nuevo".into());
        }
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();

        crate::sync::worker::push(&db, &client, &cfg("dev-1")).await.unwrap();
        {
            let conn = db.lock().unwrap();
            assert_eq!(inbox_motivo(&conn, "clientes", &hex(700)).as_deref(), Some("pendiente_local"));
            assert_eq!(opt_txt(&conn, &format!("SELECT payload FROM sync_inbox WHERE uuid = '{}'", hex(700))), None, "sin payload");
            assert_eq!(inbox_motivo(&conn, "productos", P_BALATA), None, "productos no");
            assert_eq!(inbox_motivo(&conn, "ventas", &hex(701)), None, "venta del escritorio no");
            assert_eq!(
                opt_txt(&conn, "SELECT ultimo_error FROM sync_outbox WHERE tabla = 'clientes' AND synced_at IS NULL").as_deref(),
                Some("remoto_mas_nuevo")
            );
        }
        let n = crate::sync::inbox::reintentar(&db, &client).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(m.de("fila"), vec![json!({ "tabla": "clientes", "uuid": hex(700) })]);
        let conn = db.lock().unwrap();
        assert_eq!(opt_txt(&conn, &format!("SELECT nombre FROM clientes WHERE uuid = '{}'", hex(700))).as_deref(), Some("Web nuevo"));
        assert_eq!(i64_de(&conn, "SELECT COUNT(*) FROM sync_inbox"), 0);
        let (sync, err): (Option<String>, Option<String>) = conn.query_row(
            "SELECT synced_at, ultimo_error FROM sync_outbox WHERE tabla = 'clientes' ORDER BY id DESC LIMIT 1", [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert!(sync.is_some(), "el renglón rechazado queda resuelto");
        assert_eq!(err.as_deref(), Some(apply::RESUELTO_SERVIDOR));
        assert_eq!(outbox::contar_pendientes(&conn).unwrap(), 2, "solo el producto y la venta siguen pendientes");
        assert_eq!(bandera(&conn), 0);
    }

    /// Una entrada de la bandeja SIN payload (la de un rechazo, o la que deja
    /// la migración 015) solo se descarta si el servidor dice que la fila no
    /// existe; sin red se queda para el siguiente ciclo.
    #[tokio::test]
    async fn bandeja_sin_payload_no_se_pierde_sin_red() {
        let db = nueva_db();
        crate::sync::inbox::pedir_version_servidor(&db, "clientes", &hex(710), &apply::ahora_local()).unwrap();
        let db = Arc::new(Mutex::new(db));
        let sin_red = RemoteClient::new("http://127.0.0.1:9", "token").unwrap();
        assert_eq!(crate::sync::inbox::reintentar(&db, &sin_red).await.unwrap(), 0);
        assert_eq!(inbox_motivo(&db.lock().unwrap(), "clientes", &hex(710)).as_deref(), Some("pendiente_local"));
        // El servidor responde que no la tiene: se descarta.
        let m = Mock::default();
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();
        assert_eq!(crate::sync::inbox::reintentar(&db, &client).await.unwrap(), 0);
        assert_eq!(m.de("fila").len(), 1);
        assert_eq!(i64_de(&db.lock().unwrap(), "SELECT COUNT(*) FROM sync_inbox"), 0);
    }

    /// R3.4: `caja_v2_desde` (hora en que se actualizó el servidor) baja
    /// acepta_web_desde ANTES de aplicar: la venta web de la ventana del
    /// despliegue que llega con la re-descarga después de un corte que ya
    /// cubrió su fecha se re-acomoda y cuenta. También desde el pull normal y
    /// desde el reporte de /sync/reparar.
    #[tokio::test]
    async fn caja_v2_desde_reacomoda_ventas_de_la_ventana() {
        let db = nueva_db();
        corte_principal(&db, "2026-10-06 10:30:00");
        // El escritorio se actualizó a las 11:00; el servidor a las 09:00.
        db.execute(
            "UPDATE sync_state SET acepta_web_desde = '2026-10-06 11:00:00', replay_cursor = '0', replay_hasta = '5', \
                    referencias_reparadas_at = '2026-10-06 11:00:00', last_pull_cursor = '5' WHERE id = 1",
            [],
        ).unwrap();
        let db = Arc::new(Mutex::new(db));
        let (v, v_vieja, v_pull) = (web(2100), web(2101), web(2102));
        let mut paginas = HashMap::new();
        paginas.insert("0".to_string(), json!({
            "cambios": [
                con_cursor(cambio_venta_web(&v, "V-VEN", "2026-10-06 10:00:00", 25.0), 3),
                con_cursor(cambio_venta_web(&v_vieja, "V-VIE", "2026-10-06 08:00:00", 7.0), 4),
            ],
            "next_cursor": "5", "hay_mas": false, "caja_v2_desde": "2026-10-06 09:00:00",
        }));
        paginas.insert("5".to_string(), json!({
            "cambios": [con_cursor(cambio_venta_web(&v_pull, "V-PUL", "2026-10-06 08:45:00", 3.0), 6)],
            "next_cursor": "6", "hay_mas": false, "caja_v2_desde": "2026-10-06 08:30:00",
        }));
        let m = Mock {
            paginas: Arc::new(paginas),
            reparar: json!({ "lote": "L2", "caja_v2_desde": "2026-10-06 08:00:00" }),
            ..Default::default()
        };
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();
        let reg = |db: &Arc<Mutex<Connection>>, u: &str| {
            opt_txt(&db.lock().unwrap(), &format!("SELECT registrado_at FROM ventas WHERE uuid = '{u}'"))
        };
        let desde = |db: &Arc<Mutex<Connection>>| opt_txt(&db.lock().unwrap(), "SELECT acepta_web_desde FROM sync_state");

        tareas::correr_replay(&db, &client, &cfg("dev-1")).await.unwrap();
        assert_eq!(desde(&db).as_deref(), Some("2026-10-06 09:00:00"));
        let r = reg(&db, &v).expect("la venta de la ventana se re-acomoda");
        assert!(r.as_str() > "2026-10-06 10:30:00", "{r}");
        assert_eq!(reg(&db, &v_vieja), None, "antes del despliegue: nunca se re-acomoda");
        {
            let conn = db.lock().unwrap();
            let p = crate::commands::cortes::calcular_periodo(&conn, &apply::ahora_local()).unwrap();
            assert_eq!(p.total_ventas_efectivo, 25.0, "cuenta en el período abierto");
        }

        // Pull normal: el ajuste ocurre ANTES de aplicar el lote.
        crate::sync::worker::pull(&db, &client, &cfg("dev-1")).await.unwrap();
        assert_eq!(desde(&db).as_deref(), Some("2026-10-06 08:30:00"));
        assert!(reg(&db, &v_pull).is_some());

        // Reporte de /sync/reparar.
        tareas::reparar_referencias(&db, &client, "dev-1", false).await.map_err(|e| e.mensaje()).unwrap();
        assert_eq!(desde(&db).as_deref(), Some("2026-10-06 08:00:00"));
    }

    /// Migración 015 con el sync activo: la fila de catálogo con marca futura
    /// (UTC de la versión anterior) se convierte a hora local SIN re-encolarse
    /// y queda en la bandeja sin payload; el primer ciclo pide su versión al
    /// servidor y aplica la edición web que la versión vieja se saltó. Los
    /// rechazos 'remoto_mas_nuevo' vuelven a intentarse.
    #[tokio::test]
    async fn migracion_015_con_sync_activo_pide_la_version_del_servidor() {
        let db = nueva_db();
        let fmt = "%Y-%m-%d %H:%M:%S";
        let futuro = (chrono::Local::now() + chrono::Duration::hours(3)).format(fmt).to_string();
        db.execute("UPDATE sync_state SET activo = 1 WHERE id = 1", []).unwrap();
        // Todo en el pasado (los defaults datetime('now') son UTC) salvo P1.
        for t in ["productos", "proveedores", "clientes", "usuarios", "categorias", "sucursales"] {
            db.execute(&format!("UPDATE {t} SET updated_at = '2026-10-01 00:00:00'"), []).unwrap();
        }
        db.execute("UPDATE productos SET updated_at = ?1 WHERE id = 1", [&futuro]).unwrap();
        limpiar_outbox(&db);
        db.execute("INSERT INTO clientes (nombre, uuid, updated_at) VALUES ('C', ?1, '2026-10-01 00:00:00')", [hex(800)]).unwrap();
        db.execute("UPDATE sync_outbox SET intentos = 7, ultimo_error = 'remoto_mas_nuevo' WHERE tabla = 'clientes'", []).unwrap();
        bajar_a_v14(&db);
        aplicar_migraciones(&db).unwrap();
        db.execute_batch(SEED_DATA).unwrap();

        let (motivo, payload): (String, Option<String>) = db.query_row(
            "SELECT motivo, payload FROM sync_inbox WHERE tabla = 'productos' AND uuid = ?1", [P_BALATA],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!((motivo.as_str(), payload), ("error", None));
        let local_upd = opt_txt(&db, "SELECT updated_at FROM productos WHERE id = 1").unwrap();
        assert!(local_upd <= chrono::Local::now().format(fmt).to_string(), "{local_upd}");
        assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_outbox WHERE tabla = 'productos' AND synced_at IS NULL"), 0);
        assert_eq!(
            db.query_row("SELECT intentos, ultimo_error FROM sync_outbox WHERE tabla = 'clientes' AND synced_at IS NULL", [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))).unwrap(),
            (0, None)
        );

        let web_upd = (chrono::Local::now() + chrono::Duration::hours(1)).format(fmt).to_string();
        let m = Mock {
            fila: cambio("productos", json!({ "id": 17, "codigo": "P1", "nombre": "Editado en la web", "precio_venta": 150,
                                              "stock_actual": 1000, "uuid": P_BALATA, "activo": 1, "updated_at": web_upd })),
            ..Default::default()
        };
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();
        let db = Arc::new(Mutex::new(db));
        assert_eq!(crate::sync::inbox::reintentar(&db, &client).await.unwrap(), 1);
        let conn = db.lock().unwrap();
        assert_eq!(opt_txt(&conn, "SELECT nombre FROM productos WHERE id = 1").as_deref(), Some("Editado en la web"));
        assert_eq!(i64_de(&conn, "SELECT COUNT(*) FROM sync_inbox"), 0);
        assert_eq!(super::super::base::leer(&conn, "productos", P_BALATA).unwrap().unwrap()["nombre"], json!("Editado en la web"));
        assert_eq!(i64_de(&conn, "SELECT COUNT(*) FROM sync_outbox WHERE tabla = 'productos' AND synced_at IS NULL"), 0);
    }


    // ─── Ronda 3 (SPEC3 D) ───────────────────────────────

    /// Configura el sync de la BD contra `url` (sin tareas únicas pendientes).
    fn configurar(db: &Connection, url: &str) {
        state::guardar_credenciales(db, url, "token", 1).unwrap();
        db.execute("UPDATE sync_state SET referencias_reparadas_at = '2026-10-06 12:00:00' WHERE id = 1", []).unwrap();
    }

    /// D1: reloj de esta PC 10 min adelantado. El servidor guarda la fila con
    /// SU marca y la devuelve en `marcas`: la fila local y la base de productos
    /// la toman (sin re-encolar ni mover updated_at otra vez) y una edición web
    /// posterior (más nueva que la marca del servidor pero "más vieja" que el
    /// reloj adelantado) sí baja. Una fila sin marca en la respuesta se queda
    /// con la suya.
    #[tokio::test]
    async fn push_adopta_la_marca_del_servidor_y_la_edicion_web_posterior_baja() {
        let db = nueva_db();
        let futuro = "2026-10-06 12:10:00";
        db.execute("UPDATE productos SET nombre = 'Adelantado', updated_at = ?1 WHERE id = 1", [futuro]).unwrap();
        for u in [hex(560), hex(561)] {
            db.execute("INSERT INTO clientes (nombre, uuid, updated_at) VALUES ('Taller', ?1, ?2)", [&u, futuro]).unwrap();
        }
        let db = Arc::new(Mutex::new(db));
        let mut paginas = HashMap::new();
        paginas.insert("".to_string(), json!({
            "cambios": [cambio("productos", json!({ "codigo": "P1", "nombre": "Editado en la web", "precio_venta": 130,
                                                    "uuid": P_BALATA, "updated_at": "2026-10-06 12:05:00" }))],
            "next_cursor": "5", "hay_mas": false,
        }));
        let m = Mock { paginas: Arc::new(paginas), ..Default::default() };
        {
            let mut mk = m.marcas.lock().unwrap();
            mk.insert(P_BALATA.to_string(), "2026-10-06T12:00:00.123-06".to_string());
            mk.insert(hex(560), "2026-10-06 12:00:01".to_string());
            mk.insert(hex(999), "no es fecha".to_string());
        }
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();

        crate::sync::worker::push(&db, &client, &cfg("dev-1")).await.unwrap();
        {
            let conn = db.lock().unwrap();
            let upd = |sql: &str| opt_txt(&conn, sql).unwrap();
            assert_eq!(upd("SELECT updated_at FROM productos WHERE id = 1"), "2026-10-06 12:00:00");
            assert_eq!(upd(&format!("SELECT updated_at FROM clientes WHERE uuid = '{}'", hex(560))), "2026-10-06 12:00:01");
            assert_eq!(upd(&format!("SELECT updated_at FROM clientes WHERE uuid = '{}'", hex(561))), futuro, "sin marca: la suya");
            assert_eq!(outbox::contar_pendientes(&conn).unwrap(), 0, "la marca del servidor no re-encola");
            assert_eq!(bandera(&conn), 0);
            let b = super::super::base::leer(&conn, "productos", P_BALATA).unwrap().unwrap();
            assert_eq!((b["nombre"].clone(), b["updated_at"].clone()), (json!("Adelantado"), json!("2026-10-06 12:00:00")));
            assert_eq!(upd("SELECT updated_at FROM sync_base WHERE tabla = 'productos'"), "2026-10-06 12:00:00");
        }
        // Lo que viajó sí llevaba la marca adelantada.
        assert_eq!(cambio_de(&m.de("push").pop().unwrap(), P_BALATA)["data"]["updated_at"], json!(futuro));

        crate::sync::worker::pull(&db, &client, &cfg("dev-1")).await.unwrap();
        let conn = db.lock().unwrap();
        assert_eq!(
            opt_txt(&conn, "SELECT nombre || ' / ' || CAST(precio_venta AS INTEGER) FROM productos WHERE id = 1").as_deref(),
            Some("Editado en la web / 130"),
            "la edición web posterior a la marca del servidor se aplica"
        );
        assert_eq!(outbox::contar_pendientes(&conn).unwrap(), 0);
        assert_eq!(i64_de(&conn, "SELECT COUNT(*) FROM sync_inbox"), 0);
    }

    /// D1: si la fila cambió mientras viajaba (aunque su updated_at siga
    /// siendo el mismo), su marca local NO se cambia por la del servidor (el
    /// cambio nuevo sube en el siguiente ciclo); la base sí queda con lo que
    /// el servidor aceptó y la marca con que lo guardó.
    #[tokio::test]
    async fn marca_del_servidor_no_toca_una_fila_que_cambio_en_el_camino() {
        use std::sync::atomic::AtomicBool;
        let db = nueva_db();
        let futuro = "2026-10-06 12:10:00";
        db.execute("UPDATE productos SET nombre = 'Antes', updated_at = ?1 WHERE id = 1", [futuro]).unwrap();
        let db = Arc::new(Mutex::new(db));
        let una_vez = Arc::new(AtomicBool::new(true));
        let (dbc, flag) = (db.clone(), una_vez.clone());
        let m = Mock {
            durante_push: Some(Arc::new(move || {
                if flag.swap(false, Ordering::SeqCst) {
                    // Mismo segundo: el contenido cambia, la marca no.
                    let c = dbc.lock().unwrap();
                    c.execute("INSERT OR IGNORE INTO sync_suppress_flag (id) VALUES (1)", []).unwrap();
                    c.execute("UPDATE productos SET nombre = 'En el camino' WHERE id = 1", []).unwrap();
                    c.execute("DELETE FROM sync_suppress_flag", []).unwrap();
                }
            })),
            ..Default::default()
        };
        m.marcas.lock().unwrap().insert(P_BALATA.to_string(), "2026-10-06 12:00:00".to_string());
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();

        crate::sync::worker::push(&db, &client, &cfg("dev-1")).await.unwrap();
        let conn = db.lock().unwrap();
        assert_eq!(opt_txt(&conn, "SELECT updated_at FROM productos WHERE id = 1").as_deref(), Some(futuro));
        assert_eq!(opt_txt(&conn, "SELECT nombre FROM productos WHERE id = 1").as_deref(), Some("En el camino"));
        assert_eq!(outbox::contar_pendientes(&conn).unwrap(), 1, "el cambio nuevo sigue pendiente");
        let b = super::super::base::leer(&conn, "productos", P_BALATA).unwrap().unwrap();
        assert_eq!((b["nombre"].clone(), b["updated_at"].clone()), (json!("Antes"), json!("2026-10-06 12:00:00")));
        assert_eq!(bandera(&conn), 0);
    }

    /// D2: un ciclo forzado nunca corre junto con otro: mientras uno está
    /// empujando, otro que no puede esperar devuelve "Ya hay una
    /// sincronización en curso…"; uno que sí espera corre DESPUÉS. El
    /// servidor nunca ve dos llamadas a la vez y los renglones del outbox se
    /// empujan una sola vez.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ciclos_de_sync_nunca_corren_juntos() {
        use crate::sync::worker::{forzar_ciclo_con_espera, MSG_EN_CURSO};
        use std::time::Duration;
        let m = Mock { espera_ms: 600, ..Default::default() };
        let url = servidor_v2(m.clone()).await;
        let db = nueva_db();
        configurar(&db, &url);
        db.execute("UPDATE productos SET nombre = 'Vendido aquí' WHERE id = 1", []).unwrap();
        let db = Arc::new(Mutex::new(db));

        let a = tokio::spawn(forzar_ciclo_con_espera(db.clone(), Duration::from_secs(20)));
        // Espera a que el primer ciclo esté dentro del push.
        for _ in 0..250 {
            if m.en_vuelo.load(Ordering::SeqCst) > 0 { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(m.en_vuelo.load(Ordering::SeqCst), 1, "el primer ciclo está empujando");
        let b = forzar_ciclo_con_espera(db.clone(), Duration::from_millis(100)).await;
        assert_eq!(b, Err(MSG_EN_CURSO.to_string()));
        let c = forzar_ciclo_con_espera(db.clone(), Duration::from_secs(20)).await;
        assert_eq!(c, Ok(()));
        assert_eq!(a.await.unwrap(), Ok(()));

        assert_eq!(m.max_en_vuelo.load(Ordering::SeqCst), 1, "nunca dos llamadas a la vez");
        assert_eq!(m.de("push").len(), 1, "el renglón se empujó una sola vez");
        assert_eq!(m.de("pull").len(), 2, "cada ciclo jaló una vez");
        assert_eq!(outbox::contar_pendientes(&db.lock().unwrap()).unwrap(), 0);
    }

    // ─── Ronda 4 (SPEC4 D) ───────────────────────────────

    /// D1: productos que llegaron del servidor con el código 'P1' (ya es de la
    /// Balata de aquí) se guardan como 'P1-W<8 del uuid>' (web) o
    /// 'P1-D<8 del uuid>' (otro escritorio). Al venderlos aquí suben con el
    /// código del SERVIDOR ('P1') en `data` y en `base`: el servidor no ve un
    /// cambio de código ni una diferencia con lo suyo (antes: fusión 'merge'
    /// en cada venta, para siempre, y un push normal le escribía el sufijo).
    /// Aquí se quedan con su sufijo. Un código editado aquí sí sube como
    /// cambio; un sufijo que no es '-W'/'-D' + los 8 del uuid de ESA fila (p.
    /// ej. el '-W<id>' de la migración 011) viaja tal cual.
    #[tokio::test]
    async fn push_manda_el_codigo_del_servidor_sin_el_sufijo_local() {
        let mut db = nueva_db();
        let pw = web(3000);
        let pd = "abcdef0123456789abcdef0123456789".to_string();
        // Otro prefijo: dos uuids web con los mismos 8 primeros chocarían
        // también con el código renombrado.
        let pz = "019ee111-a8fb-7bd0-88ab-000000003001".to_string();
        let p11 = web(3002);
        let pq = web(3003);
        let prod = |codigo: &str, nombre: &str, u: &str| {
            cambio("productos", json!({ "codigo": codigo, "nombre": nombre, "precio_venta": 50, "stock_actual": 10,
                                        "uuid": u, "activo": 1, "updated_at": "2026-10-06 09:00:00" }))
        };
        let r = aplicar(&mut db, &[
            prod("P1", "Web", &pw),
            prod("P1", "Otro escritorio", &pd),
            prod("P1", "Web, código editado aquí", &pz),
            prod("P1-W11922", "Renombrado por la migración 011", &p11),
            prod("Q-W12345678", "Sufijo de otro uuid", &pq),
        ]);
        assert_eq!((r.aplicados, r.aparcados), (5, 0), "{r:?}");
        let codigo = |db: &Connection, u: &str| opt_txt(db, &format!("SELECT codigo FROM productos WHERE uuid = '{u}'")).unwrap();
        assert_eq!(codigo(&db, &pw), "P1-W019dd079");
        assert_eq!(codigo(&db, &pd), "P1-Dabcdef01");
        assert_eq!(codigo(&db, &pz), "P1-W019ee111");
        assert_eq!(codigo(&db, &p11), "P1-W11922");
        assert_eq!(codigo(&db, &pq), "Q-W12345678");
        // Ventas de aquí (y una edición de código en uno de ellos).
        for u in [&pw, &pd, &p11, &pq] {
            db.execute("UPDATE productos SET stock_actual = stock_actual - 1 WHERE uuid = ?1", [u]).unwrap();
        }
        db.execute("UPDATE productos SET codigo = 'Z9', stock_actual = 9 WHERE uuid = ?1", [&pz]).unwrap();
        let db = Arc::new(Mutex::new(db));
        let m = Mock::default();
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();

        crate::sync::worker::push(&db, &client, &cfg("dev-1")).await.unwrap();
        let body = m.de("push").pop().unwrap();
        let viaja = |u: &str| {
            let c = cambio_de(&body, u);
            (c["data"]["codigo"].clone(), c["base"]["codigo"].clone(), c["data"]["stock_actual"].clone(), c["base"]["stock_actual"].clone())
        };
        for u in [&pw, &pd] {
            assert_eq!(viaja(u), (json!("P1"), json!("P1"), json!(9.0), json!(10.0)), "{u}: código del servidor en data y base");
        }
        assert_eq!(viaja(&pz).0, json!("Z9"), "código editado aquí: sube como cambio");
        assert_eq!(viaja(&pz).1, json!("P1"), "…sobre la base con el código del servidor");
        assert_eq!(viaja(&p11).0, json!("P1-W11922"), "'-W<id>' del servidor viaja tal cual");
        assert_eq!(viaja(&p11).1, json!("P1-W11922"));
        assert_eq!(viaja(&pq).0, json!("Q-W12345678"), "sufijo de otro uuid viaja tal cual");
        assert_eq!(viaja(&pq).1, json!("Q-W12345678"));
        {
            let conn = db.lock().unwrap();
            assert_eq!(outbox::contar_pendientes(&conn).unwrap(), 0, "aceptados: el renglón queda subido");
            // Aquí sigue el sufijo (fila y base guardada = fila local).
            assert_eq!(codigo(&conn, &pw), "P1-W019dd079");
            assert_eq!(codigo(&conn, &pd), "P1-Dabcdef01");
            assert_eq!(super::super::base::leer(&conn, "productos", &pw).unwrap().unwrap()["codigo"], json!("P1-W019dd079"));
            assert_eq!(super::super::base::leer(&conn, "productos", &pz).unwrap().unwrap()["codigo"], json!("Z9"));
            assert_eq!(bandera(&conn), 0);
            // Otra venta del producto web.
            conn.execute("UPDATE productos SET stock_actual = stock_actual - 1 WHERE uuid = ?1", [&pw]).unwrap();
        }
        crate::sync::worker::push(&db, &client, &cfg("dev-1")).await.unwrap();
        let body = m.de("push").pop().unwrap();
        let c = cambio_de(&body, &pw);
        assert_eq!(
            (c["data"]["codigo"].clone(), c["base"]["codigo"].clone(), c["data"]["stock_actual"].clone(), c["base"]["stock_actual"].clone()),
            (json!("P1"), json!("P1"), json!(8.0), json!(9.0)),
            "la venta siguiente tampoco lleva cambio de código"
        );
        assert_eq!(body["cambios"].as_array().unwrap().len(), 1);
        assert_eq!(outbox::contar_pendientes(&db.lock().unwrap()).unwrap(), 0);
    }

    /// Una entrada 'pendiente_local' de la bandeja de productos: la versión
    /// web (precio 120, existencia 1000) llegó mientras la venta de aquí (999)
    /// no subía. La venta ya subió.
    fn bandeja_con_version_web_vieja() -> Connection {
        let mut db = nueva_db();
        db.execute("UPDATE productos SET stock_actual = 999, updated_at = '2026-10-06 10:00:00' WHERE id = 1", []).unwrap();
        let r = aplicar(&mut db, &[cambio("productos", json!({
            "codigo": "P1", "nombre": "Balata", "precio_venta": 120, "stock_actual": 1000,
            "uuid": P_BALATA, "activo": 1, "updated_at": "2026-10-06 10:05:00",
        }))]);
        assert_eq!(r.aparcados, 1, "{r:?}");
        assert_eq!(inbox_motivo(&db, "productos", P_BALATA).as_deref(), Some("pendiente_local"));
        limpiar_outbox(&db);
        db.execute("UPDATE sync_inbox SET created_at = datetime('now','localtime')", []).unwrap();
        db
    }

    /// D2: si GET /sync/fila no contesta (sin red o error del servidor), una
    /// entrada 'pendiente_local' NO usa su payload guardado: es la versión
    /// del servidor de antes de la venta que ya subió, y aplicarla deshacía
    /// aquí la venta (y el siguiente push se la quitaba al servidor). Espera
    /// al siguiente ciclo y se aplica con la versión ACTUAL del servidor. Las
    /// entradas 'error' sí siguen usando su payload si no hay respuesta, y
    /// un servidor sin /sync/fila sigue usando el guardado
    /// (servidor_sin_actualizar_no_es_error).
    #[tokio::test]
    async fn bandeja_pendiente_local_sin_respuesta_no_usa_el_payload_viejo() {
        let db = bandeja_con_version_web_vieja();
        // Control: una entrada 'error' con payload, lista para reintentar.
        let cli = hex(3100);
        let pay = cambio("clientes", json!({ "nombre": "Cliente web", "uuid": cli, "updated_at": "2026-10-06 09:00:00" }));
        db.execute(
            "INSERT INTO sync_inbox (tabla, uuid, motivo, error, payload, intentos, proximo_intento) \
             VALUES ('clientes', ?1, 'error', 'x', ?2, 1, '2000-01-01 00:00:00')",
            rusqlite::params![cli, pay.to_string()],
        ).unwrap();
        let db = Arc::new(Mutex::new(db));
        let estado = |db: &Arc<Mutex<Connection>>| {
            let c = db.lock().unwrap();
            (
                opt_txt(&c, "SELECT CAST(stock_actual AS INTEGER) || ' / ' || CAST(precio_venta AS INTEGER) FROM productos WHERE id = 1").unwrap(),
                inbox_motivo(&c, "productos", P_BALATA),
            )
        };

        // Sin red.
        let sin_red = RemoteClient::new("http://127.0.0.1:9", "token").unwrap();
        assert_eq!(crate::sync::inbox::reintentar(&db, &sin_red).await.unwrap(), 1, "solo la entrada 'error' (payload guardado)");
        assert_eq!(estado(&db), ("999 / 100".to_string(), Some("pendiente_local".to_string())), "la venta de aquí no se deshace");
        assert_eq!(
            opt_txt(&db.lock().unwrap(), &format!("SELECT nombre FROM clientes WHERE uuid = '{cli}'")).as_deref(),
            Some("Cliente web"),
            "'error' sin respuesta: usa su payload, como antes"
        );

        // Error del servidor (500).
        let url_500 = servir(Router::new().route("/sync/fila", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))).await;
        let c500 = RemoteClient::new(&url_500, "token").unwrap();
        assert_eq!(crate::sync::inbox::reintentar(&db, &c500).await.unwrap(), 0);
        assert_eq!(estado(&db), ("999 / 100".to_string(), Some("pendiente_local".to_string())));

        // El servidor contesta: su versión actual (la venta ya aplicada allá).
        let m = Mock {
            fila: cambio("productos", json!({ "id": 17, "codigo": "P1", "nombre": "Balata", "precio_venta": 120,
                                              "stock_actual": 999, "uuid": P_BALATA, "activo": 1,
                                              "updated_at": "2026-10-06 10:06:00" })),
            ..Default::default()
        };
        let url = servidor_v2(m.clone()).await;
        let client = RemoteClient::new(&url, "token").unwrap();
        assert_eq!(crate::sync::inbox::reintentar(&db, &client).await.unwrap(), 1);
        assert_eq!(m.de("fila"), vec![json!({ "tabla": "productos", "uuid": P_BALATA })]);
        assert_eq!(estado(&db), ("999 / 120".to_string(), None), "precio web y la venta de aquí");
        let conn = db.lock().unwrap();
        assert_eq!(i64_de(&conn, "SELECT COUNT(*) FROM sync_inbox"), 0);
        assert_eq!(outbox::contar_pendientes(&conn).unwrap(), 0);
        assert_eq!(bandera(&conn), 0);
    }
}
