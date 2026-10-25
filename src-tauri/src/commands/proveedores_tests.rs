// commands/proveedores_tests.rs — Pruebas del catálogo de proveedores.
//
// Mismo arranque que caja_tests (esquema → migraciones → semilla, en memoria).
// Cubren lo que puede ensuciar las compras del dueño:
//   - SUFCA queda dado de alta por la migración 019, una sola vez;
//   - no se puede repetir un proveedor cambiando mayúsculas o espacios;
//   - desactivar lo esconde de los menús sin borrar su historial.
//
// Correr con:  cargo test --lib proveedores_tests

use rusqlite::Connection;

use super::caja_tests::nueva_db;
use super::productos::{
    actualizar_proveedor_core, crear_proveedor_core, ActualizarProveedor,
};

fn nombres(db: &Connection) -> Vec<String> {
    let mut stmt = db.prepare("SELECT nombre FROM proveedores ORDER BY nombre").unwrap();
    let filas = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    filas.filter_map(|r| r.ok()).collect()
}

fn cuantos(db: &Connection, nombre: &str) -> i64 {
    db.query_row(
        "SELECT COUNT(*) FROM proveedores WHERE UPPER(TRIM(nombre)) = UPPER(TRIM(?))",
        [nombre], |r| r.get(0),
    ).unwrap()
}

fn alta(db: &Connection, nombre: &str) -> Result<i64, String> {
    crear_proveedor_core(db, nombre.to_string(), None, None, None, None).map(|p| p.id)
}

#[test]
fn migracion_019_deja_sufca_dado_de_alta() {
    let db = nueva_db();
    assert_eq!(nombres(&db), vec!["SUFCA".to_string()]);

    let notas: String = db.query_row(
        "SELECT notas FROM proveedores WHERE nombre = 'SUFCA'", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(notas, "Artículos de ferretería");

    // Activo y listo para usarse en una recepción
    let activo: i64 = db.query_row(
        "SELECT activo FROM proveedores WHERE nombre = 'SUFCA'", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(activo, 1);
}

#[test]
fn migracion_019_no_duplica_sufca_si_ya_existia() {
    // Simula la PC de la tienda, donde SUFCA pudo entrar por una importación
    // de catálogo antes de actualizar el POS.
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(crate::db::schema::SCHEMA_V1).unwrap();
    db.execute("INSERT INTO proveedores (nombre) VALUES ('Sufca')", []).unwrap();
    crate::db::migrations::aplicar_migraciones(&db).unwrap();

    assert_eq!(cuantos(&db, "SUFCA"), 1);
    assert_eq!(nombres(&db), vec!["Sufca".to_string()], "no debe renombrar el que ya existía");
}

#[test]
fn no_acepta_el_mismo_proveedor_dos_veces() {
    let db = nueva_db();
    alta(&db, "Alessia").unwrap();

    // Mismo negocio escrito distinto: debe rechazarse, igual que lo deduplica
    // el importador de catálogo (commands/importar.rs).
    for intento in ["Alessia", "ALESSIA", "alessia", "  Alessia  "] {
        let err = alta(&db, intento).expect_err(intento);
        assert!(err.contains("Ya existe un proveedor"), "{intento}: {err}");
    }
    assert_eq!(cuantos(&db, "Alessia"), 1);
}

#[test]
fn guarda_el_nombre_sin_espacios_sobrantes() {
    let db = nueva_db();
    let id = alta(&db, "  Rodamientos del Centro  ").unwrap();
    let nombre: String = db.query_row(
        "SELECT nombre FROM proveedores WHERE id = ?", [id], |r| r.get(0),
    ).unwrap();
    assert_eq!(nombre, "Rodamientos del Centro");
}

#[test]
fn exige_nombre() {
    let db = nueva_db();
    for vacio in ["", "   "] {
        let err = alta(&db, vacio).expect_err("debe exigir nombre");
        assert!(err.contains("obligatorio"), "{err}");
    }
}

#[test]
fn al_editar_puede_conservar_su_propio_nombre() {
    let db = nueva_db();
    let id = alta(&db, "Refacciones Bravo").unwrap();

    // Cambiar solo el teléfono no debe chocar consigo mismo
    actualizar_proveedor_core(&db, ActualizarProveedor {
        id,
        nombre: "Refacciones Bravo".into(),
        contacto: Some("Laura".into()),
        telefono: Some("462 100 0000".into()),
        email: None,
        notas: None,
    }).unwrap();

    let tel: String = db.query_row(
        "SELECT telefono FROM proveedores WHERE id = ?", [id], |r| r.get(0),
    ).unwrap();
    assert_eq!(tel, "462 100 0000");
}

#[test]
fn al_editar_no_puede_tomar_el_nombre_de_otro() {
    let db = nueva_db();
    let id = alta(&db, "Refacciones Bravo").unwrap();
    alta(&db, "Rodamientos del Centro").unwrap();

    let err = actualizar_proveedor_core(&db, ActualizarProveedor {
        id,
        nombre: "rodamientos del centro".into(),
        contacto: None, telefono: None, email: None, notas: None,
    }).expect_err("debe rechazar el nombre de otro proveedor");
    assert!(err.contains("Ya existe otro proveedor"), "{err}");
}

#[test]
fn desactivar_no_borra_al_proveedor_ni_sus_productos() {
    let db = nueva_db();
    let id = alta(&db, "Alessia").unwrap();
    db.execute("UPDATE productos SET proveedor_id = ? WHERE id = 1", [id]).unwrap();

    db.execute(
        "UPDATE proveedores SET activo = CASE activo WHEN 1 THEN 0 ELSE 1 END WHERE id = ?",
        [id],
    ).unwrap();

    let activo: i64 = db.query_row(
        "SELECT activo FROM proveedores WHERE id = ?", [id], |r| r.get(0),
    ).unwrap();
    assert_eq!(activo, 0);

    // El producto sigue apuntando a él: el historial de compras no se rompe
    let sigue: i64 = db.query_row(
        "SELECT COUNT(*) FROM productos WHERE proveedor_id = ?", [id], |r| r.get(0),
    ).unwrap();
    assert_eq!(sigue, 1);
}

#[test]
fn un_proveedor_nuevo_se_encola_para_el_sync() {
    // Los triggers de sync (migración 005) deben tomar el alta igual que si
    // hubiera entrado por importación, para que llegue al servidor y a la web.
    let db = nueva_db();
    let id = alta(&db, "Alessia").unwrap();

    let uuid: Option<String> = db.query_row(
        "SELECT uuid FROM proveedores WHERE id = ?", [id], |r| r.get(0),
    ).unwrap();
    assert!(uuid.is_some(), "el trigger de uuid debe llenarlo");

    let en_outbox: i64 = db.query_row(
        "SELECT COUNT(*) FROM sync_outbox WHERE tabla = 'proveedores' AND uuid = ?",
        [uuid.unwrap()], |r| r.get(0),
    ).unwrap();
    assert_eq!(en_outbox, 1);
}
