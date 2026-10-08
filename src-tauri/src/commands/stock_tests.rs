// commands/stock_tests.rs — Pruebas de inventario y del reloj de updated_at.
//
// Mismo arranque que caja_tests (esquema → migraciones → semilla, en memoria).
// Cubren la regla de stock que el dueño decidió mantener:
//   - una venta frena el stock en 0 (nunca negativo);
//   - anular o devolver suma la cantidad completa (la pieza regresa al anaquel);
//   - ajustes, altas e importaciones no aceptan stock negativo;
// y el reloj único del sync: updated_at siempre en hora local.
//
// Correr con:  cargo test --lib stock_tests

use rusqlite::Connection;

use super::caja_tests::{contar, nueva_db, vender, DUENO};
use super::devoluciones::{crear_devolucion_core, ItemDevolucion, NuevaDevolucion};
use super::importar::importar_catalogo_texto;
use super::productos::{ajustar_stock_core, crear_producto_core, NuevoProducto};
use super::ventas::anular_venta_core;

fn stock(db: &Connection, producto_id: i64) -> f64 {
    db.query_row("SELECT stock_actual FROM productos WHERE id = ?", [producto_id], |r| r.get(0)).unwrap()
}

fn poner_stock(db: &Connection, producto_id: i64, valor: f64) {
    db.execute("UPDATE productos SET stock_actual = ? WHERE id = ?", rusqlite::params![valor, producto_id]).unwrap();
}

fn texto(db: &Connection, sql: &str) -> String {
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn partida(db: &Connection, venta_id: i64) -> i64 {
    db.query_row("SELECT id FROM venta_detalle WHERE venta_id = ?", [venta_id], |r| r.get(0)).unwrap()
}

fn devolver(db: &Connection, venta_id: i64, items: &[(i64, f64)], cuando: &str) -> Result<f64, String> {
    crear_devolucion_core(db, NuevaDevolucion {
        venta_id,
        usuario_id: DUENO,
        autorizado_por: None,
        motivo: "prueba".into(),
        items: items.iter().map(|(vd, c)| ItemDevolucion { venta_detalle_id: *vd, cantidad: *c }).collect(),
        reembolso_efectivo: None,
    }, cuando).map(|d| d.total_devuelto)
}

fn producto_nuevo(codigo: &str, stock_actual: f64) -> NuevoProducto {
    NuevoProducto {
        codigo: Some(codigo.to_string()),
        codigo_tipo: None,
        nombre: format!("Producto {codigo}"),
        descripcion: None,
        categoria_id: None,
        precio_costo: 10.0,
        precio_venta: 20.0,
        stock_actual,
        stock_minimo: 0.0,
        proveedor_id: None,
        foto_url: None,
    }
}

/// Diferencia en segundos entre dos marcas 'YYYY-MM-DD HH:MM:SS'.
fn segundos_entre(a: &str, b: &str) -> i64 {
    let f = |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .unwrap_or_else(|e| panic!("marca inválida '{s}': {e}"));
    (f(a) - f(b)).num_seconds().abs()
}

// ─── Regla de stock ───────────────────────────────────────

#[test]
fn venta_sin_existencias_se_frena_en_cero_y_anular_regresa_todo() {
    let db = nueva_db();
    poner_stock(&db, 1, 0.0);
    let v = vender(&db, "2026-10-01 10:00:00", 2.0, 100.0, 200.0, "efectivo");
    assert_eq!(stock(&db, 1), 0.0, "la venta no deja el stock negativo");

    anular_venta_core(&db, v, DUENO, "no era la pieza".into(), "2026-10-01 11:00:00").unwrap();
    assert_eq!(stock(&db, 1), 2.0, "la anulación regresa las 2 piezas al anaquel");
}

#[test]
fn devoluciones_parciales_regresan_lo_devuelto_y_no_mas() {
    let db = nueva_db();
    poner_stock(&db, 1, 5.0);
    let v = vender(&db, "2026-10-01 10:00:00", 3.0, 100.0, 300.0, "efectivo");
    let vd = partida(&db, v);
    assert_eq!(stock(&db, 1), 2.0);

    assert_eq!(devolver(&db, v, &[(vd, 1.0)], "2026-10-01 11:00:00").unwrap(), 100.0);
    assert_eq!(stock(&db, 1), 3.0);
    assert_eq!(devolver(&db, v, &[(vd, 2.0)], "2026-10-01 12:00:00").unwrap(), 200.0);
    assert_eq!(stock(&db, 1), 5.0);

    assert!(devolver(&db, v, &[(vd, 1.0)], "2026-10-01 13:00:00").is_err(), "ya no queda nada por devolver");
    assert_eq!(stock(&db, 1), 5.0);
}

#[test]
fn devolucion_con_partida_repetida_no_toca_el_stock() {
    let db = nueva_db();
    poner_stock(&db, 1, 5.0);
    let v = vender(&db, "2026-10-01 10:00:00", 2.0, 100.0, 200.0, "efectivo");
    let vd = partida(&db, v);
    assert!(devolver(&db, v, &[(vd, 2.0), (vd, 1.0)], "2026-10-01 11:00:00").is_err());
    assert_eq!(stock(&db, 1), 3.0);
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM devoluciones"), 0);
}

#[test]
fn ajuste_de_stock_negativo_o_no_numerico_se_rechaza() {
    let db = nueva_db();
    poner_stock(&db, 1, 7.0);
    for malo in [-1.0, -0.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let r = ajustar_stock_core(&db, 1, malo, "conteo físico", DUENO, "2026-10-01 10:00:00");
        assert!(r.is_err(), "debió rechazar {malo}");
    }
    assert_eq!(stock(&db, 1), 7.0);
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM audit_log WHERE accion = 'STOCK_AJUSTADO'"), 0);

    assert!(ajustar_stock_core(&db, 1, 0.0, "", DUENO, "2026-10-01 10:00:00").is_err(), "motivo obligatorio");
    ajustar_stock_core(&db, 1, 0.0, "merma", DUENO, "2026-10-01 10:00:00").unwrap();
    assert_eq!(stock(&db, 1), 0.0);
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM audit_log WHERE accion = 'STOCK_AJUSTADO'"), 1);
}

#[test]
fn alta_de_producto_con_stock_negativo_se_rechaza() {
    let db = nueva_db();
    let antes = contar(&db, "SELECT COUNT(*) FROM productos");
    assert!(crear_producto_core(&db, producto_nuevo("NEG-1", -3.0), DUENO, "2026-10-01 10:00:00").is_err());
    assert!(crear_producto_core(&db, producto_nuevo("NAN-1", f64::NAN), DUENO, "2026-10-01 10:00:00").is_err());
    let mut p = producto_nuevo("MIN-1", 1.0);
    p.stock_minimo = -1.0;
    assert!(crear_producto_core(&db, p, DUENO, "2026-10-01 10:00:00").is_err());
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM productos"), antes);

    let ok = crear_producto_core(&db, producto_nuevo("CERO-1", 0.0), DUENO, "2026-10-01 10:00:00").unwrap();
    assert_eq!(ok.stock_actual, 0.0);
}

// ─── Alta de un código que estaba eliminado ───────────────

#[test]
fn alta_con_codigo_de_producto_eliminado_lo_reactiva() {
    let db = nueva_db();
    db.execute(
        "INSERT INTO productos (id, codigo, nombre, precio_venta, stock_actual, activo, deleted_at) \
         VALUES (50, 'BUJIA-D8EA', 'Bujía vieja', 30, 4, 0, '2026-09-01 10:00:00')",
        [],
    ).unwrap();
    let uuid_antes = texto(&db, "SELECT uuid FROM productos WHERE id = 50");
    let total_antes = contar(&db, "SELECT COUNT(*) FROM productos");

    let mut p = producto_nuevo("BUJIA-D8EA", 12.0);
    p.nombre = "BUJIA NGK D8EA".into();
    p.precio_venta = 45.0;
    let creado = crear_producto_core(&db, p, DUENO, "2026-10-01 10:00:00").unwrap();

    assert_eq!(creado.id, 50, "mismo registro");
    assert!(creado.activo);
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM productos"), total_antes, "no se crea otro registro");
    let (nombre, precio, existencias, activo, borrado, uuid): (String, f64, f64, i64, Option<String>, String) = db.query_row(
        "SELECT nombre, precio_venta, stock_actual, activo, deleted_at, uuid FROM productos WHERE id = 50",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ).unwrap();
    assert_eq!((nombre.as_str(), precio, existencias, activo), ("BUJIA NGK D8EA", 45.0, 12.0, 1));
    assert_eq!(borrado, None);
    assert_eq!(uuid, uuid_antes, "mismo uuid: el sync lo trata como la misma fila");
    assert_eq!(
        contar(&db, "SELECT COUNT(*) FROM audit_log WHERE accion = 'PRODUCTO_CREADO' AND registro_id = 50"),
        1
    );

    // Un código de un producto vigente sigue siendo un duplicado.
    let err = crear_producto_core(&db, producto_nuevo("BUJIA-D8EA", 1.0), DUENO, "2026-10-01 11:00:00").unwrap_err();
    assert!(err.contains("Ya existe"), "{err}");
}

// ─── Importación CSV ──────────────────────────────────────

#[test]
fn importacion_rechaza_stock_negativo_y_reactiva_eliminados() {
    let db = nueva_db();
    db.execute(
        "INSERT INTO productos (id, codigo, nombre, precio_venta, stock_actual, activo, deleted_at) \
         VALUES (60, 'FILTRO-1', 'Filtro', 50, 0, 0, '2026-09-01 10:00:00')",
        [],
    ).unwrap();

    let csv = "\
NUEVO-1|Balero 6201|80|5|PROVEEDOR A
NEG-2|Cadena 428|150|-3|PROVEEDOR A
NAN-3|Piñón|90|NaN|
FILTRO-1|Filtro de aire|55|7|
";
    let res = importar_catalogo_texto(&db, csv, false, "2026-10-01 10:00:00").unwrap();
    assert_eq!(res.total_lineas, 4);
    assert_eq!(res.insertados, 1);
    assert_eq!(res.actualizados, 1);
    assert_eq!(res.omitidos, 2);
    assert_eq!(res.errores.len(), 2, "{:?}", res.errores);
    assert!(res.errores.iter().any(|e| e.contains("NEG-2") && e.contains("negativo")), "{:?}", res.errores);
    assert!(res.errores.iter().any(|e| e.contains("NAN-3")), "{:?}", res.errores);
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM productos WHERE codigo IN ('NEG-2', 'NAN-3')"), 0);

    let (activo, borrado, existencias, actualizado): (i64, Option<String>, f64, String) = db.query_row(
        "SELECT activo, deleted_at, stock_actual, updated_at FROM productos WHERE id = 60",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).unwrap();
    assert_eq!((activo, existencias), (1, 7.0));
    assert_eq!(borrado, None, "reactivar también limpia deleted_at");
    assert_eq!(actualizado, "2026-10-01 10:00:00", "updated_at en hora local, sin formato ISO");
}

// ─── Reloj único de updated_at (hora local) ───────────────

#[test]
fn triggers_de_updated_at_usan_hora_local() {
    let db = nueva_db();
    let mut stmt = db.prepare(
        "SELECT name, sql FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'trg_%_bump_updated'",
    ).unwrap();
    let triggers: Vec<(String, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap().filter_map(|r| r.ok()).collect();
    assert!(triggers.len() >= 16, "se esperaban los 16 triggers de bump, hay {}", triggers.len());
    for (nombre, sql) in &triggers {
        assert!(sql.contains("localtime"), "{nombre} no usa hora local: {sql}");
    }
}

#[test]
fn venta_anulacion_devolucion_y_ajuste_sellan_updated_at_con_el_reloj_recibido() {
    // Los comandos pasan ahora_local(); aquí se inyecta un reloj fijo para
    // comprobar que es ESE valor el que queda en updated_at (y no UTC).
    let db = nueva_db();
    poner_stock(&db, 1, 10.0);

    let v = vender(&db, "2030-01-01 10:00:00", 1.0, 100.0, 100.0, "efectivo");
    assert_eq!(texto(&db, "SELECT updated_at FROM productos WHERE id = 1"), "2030-01-01 10:00:00");

    anular_venta_core(&db, v, DUENO, "prueba".into(), "2030-01-01 11:00:00").unwrap();
    assert_eq!(texto(&db, "SELECT updated_at FROM productos WHERE id = 1"), "2030-01-01 11:00:00");
    assert_eq!(texto(&db, &format!("SELECT updated_at FROM ventas WHERE id = {v}")), "2030-01-01 11:00:00");

    let w = vender(&db, "2030-01-01 12:00:00", 1.0, 100.0, 100.0, "efectivo");
    devolver(&db, w, &[(partida(&db, w), 1.0)], "2030-01-01 13:00:00").unwrap();
    assert_eq!(texto(&db, "SELECT updated_at FROM productos WHERE id = 1"), "2030-01-01 13:00:00");

    ajustar_stock_core(&db, 1, 3.0, "conteo", DUENO, "2030-01-01 14:00:00").unwrap();
    assert_eq!(texto(&db, "SELECT updated_at FROM productos WHERE id = 1"), "2030-01-01 14:00:00");
}

#[test]
fn reloj_de_los_comandos_es_la_hora_local_de_sqlite() {
    // ahora_local() (chrono::Local) y datetime('now','localtime') de SQLite
    // usan la zona del equipo: deben coincidir. Una fila nueva sin uuid
    // termina con la hora local del trigger, no con UTC.
    let db = nueva_db();
    let sqlite_local = texto(&db, "SELECT datetime('now','localtime')");
    let rust_local = super::cortes::ahora_local();
    assert!(segundos_entre(&sqlite_local, &rust_local) <= 5, "{sqlite_local} vs {rust_local}");

    let p = crear_producto_core(&db, producto_nuevo("RELOJ-1", 1.0), DUENO, &rust_local).unwrap();
    let sello = texto(&db, &format!("SELECT updated_at FROM productos WHERE id = {}", p.id));
    assert!(segundos_entre(&sello, &sqlite_local) <= 5, "updated_at {sello} vs local {sqlite_local}");

    db.execute(
        "INSERT INTO productos (codigo, nombre, updated_at) VALUES ('RELOJ-2', 'x', '2000-01-01 00:00:00')",
        [],
    ).unwrap();
    let sello2 = texto(&db, "SELECT updated_at FROM productos WHERE codigo = 'RELOJ-2'");
    assert!(segundos_entre(&sello2, &sqlite_local) <= 5, "el trigger debió sellar hora local: {sello2}");
}
