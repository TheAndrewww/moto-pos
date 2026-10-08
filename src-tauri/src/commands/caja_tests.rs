// commands/caja_tests.rs — Pruebas del flujo de dinero de la caja.
//
// Cada prueba levanta una base SQLite en memoria con el MISMO esquema,
// migraciones y datos semilla que usa el POS en producción, y recorre un
// escenario real de operación. Cubren los casos que descuadraban los cortes:
// días sin cerrar, ventas después del cierre, retiros en cortes de turno,
// ventas registradas mientras el corte estaba abierto, anulaciones,
// devoluciones con tarjeta y redondeo.
//
// Correr con:  cargo test --lib caja_tests

use rusqlite::Connection;

use crate::db::migrations::aplicar_migraciones;
use crate::db::schema::{SCHEMA_V1, SEED_DATA};

use super::cortes::{
    calcular_periodo, crear_apertura_core, crear_corte_core, primer_dia_pendiente,
    calcular_esperado_apertura, CorteCreado, NuevaApertura, NuevoCorte, RetiroCorte,
    ERR_DATOS_CAMBIARON,
    apertura_del_dia, inicio_y_fondo, listar_cortes_core, movimientos_sin_corte, ultimo_corte,
    crear_movimiento_core, piso_cadena, MovimientoCajaRs, NuevoMovimiento,
};
use super::devoluciones::{crear_devolucion_core, ItemDevolucion, NuevaDevolucion};
use super::ventas::{anular_venta_core, crear_venta_core, ItemVenta, NuevaVenta};

// ─── Utilidades de prueba ─────────────────────────────────

pub(super) const DUENO: i64 = 1;

pub(super) fn nueva_db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(SCHEMA_V1).unwrap();
    aplicar_migraciones(&db).unwrap();
    db.execute_batch(SEED_DATA).unwrap();
    db.execute(
        "INSERT INTO usuarios (id, nombre_completo, nombre_usuario, pin, password_hash, rol_id, activo) \
         VALUES (1, 'Dueño', 'admin', 'x', 'x', 1, 1)",
        [],
    ).unwrap();
    db.execute(
        "INSERT INTO productos (id, codigo, nombre, precio_venta, stock_actual) VALUES (1, 'P1', 'Balata', 100, 1000)",
        [],
    ).unwrap();
    db
}

/// Venta de un solo renglón. `precio` es el precio unitario; `total` lo que
/// se cobra (permite simular el redondeo al peso).
pub(super) fn vender(db: &Connection, cuando: &str, cantidad: f64, precio: f64, total: f64, metodo: &str) -> i64 {
    let subtotal = precio * cantidad;
    crear_venta_core(db, NuevaVenta {
        usuario_id: DUENO,
        cliente_id: None,
        subtotal,
        descuento: 0.0,
        total,
        metodo_pago: metodo.to_string(),
        monto_recibido: total,
        cambio: 0.0,
        items: vec![ItemVenta {
            producto_id: 1,
            cantidad,
            precio_original: precio,
            descuento_porcentaje: 0.0,
            descuento_monto: 0.0,
            precio_final: precio,
            subtotal,
            autorizado_por: None,
        }],
        presupuesto_origen_id: None,
    }, cuando).unwrap().id
}

fn venta_efectivo(db: &Connection, cuando: &str, total: f64) -> i64 {
    vender(db, cuando, 1.0, total, total, "efectivo")
}

/// Movimiento manual de caja (lo mismo que hace `crear_movimiento_caja`).
fn movimiento(db: &Connection, cuando: &str, tipo: &str, monto: f64) {
    db.execute(
        "INSERT INTO movimientos_caja (tipo, usuario_id, monto, concepto, fecha) VALUES (?, 1, ?, 'prueba', ?)",
        rusqlite::params![tipo, monto, cuando],
    ).unwrap();
}

fn abrir(db: &Connection, cuando: &str, contado: f64, nota: Option<&str>) -> Result<(), String> {
    crear_apertura_core(db, NuevaApertura {
        usuario_id: DUENO,
        fondo_declarado: contado,
        nota: nota.map(String::from),
    }, cuando).map(|_| ())
}

/// Simula el modal: calcula en `abierto`, el cajero cuenta, confirma en `confirma`.
fn corte_con(
    db: &Connection,
    tipo: &str,
    abierto: &str,
    confirma: &str,
    contado: f64,
    fondo_siguiente: f64,
    retiro: Option<f64>,
) -> Result<CorteCreado, String> {
    let datos = calcular_periodo(db, abierto)?;
    crear_corte_core(db, NuevoCorte {
        tipo: tipo.to_string(),
        usuario_id: DUENO,
        fecha_inicio: datos.fecha_inicio.clone(),
        fecha_fin: datos.fecha_fin.clone(),
        datos,
        efectivo_contado: contado,
        nota_diferencia: Some("nota de prueba".into()),
        fondo_siguiente,
        denominaciones: None,
        retiro: retiro.map(|m| RetiroCorte {
            monto: m,
            concepto: "Retiro por cambio de turno".into(),
            pin_autorizacion: None,
        }),
    }, confirma)
}

fn corte(db: &Connection, tipo: &str, cuando: &str, contado: f64, fondo_siguiente: f64) -> CorteCreado {
    corte_con(db, tipo, cuando, cuando, contado, fondo_siguiente, None).unwrap()
}

fn esperado(db: &Connection, cuando: &str) -> f64 {
    calcular_periodo(db, cuando).unwrap().efectivo_esperado
}

pub(super) fn contar(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

// ─── Escenarios ───────────────────────────────────────────

#[test]
fn dia_normal_cuadra() {
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-10-01 09:00:00", 500.0);
    vender(&db, "2026-10-01 10:00:00", 1.0, 300.0, 300.0, "tarjeta");
    movimiento(&db, "2026-10-01 11:00:00", "RETIRO", 100.0);
    movimiento(&db, "2026-10-01 12:00:00", "ENTRADA", 50.0);

    let c = corte(&db, "DIA", "2026-10-01 20:00:00", 2450.0, 2000.0);
    assert_eq!(c.efectivo_esperado, 2450.0, "2000 + 500 + 50 − 100");
    assert_eq!(c.diferencia, 0.0);
}

#[test]
fn el_dia_siguiente_arranca_con_el_fondo_que_se_dejo() {
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-10-01 10:00:00", 1000.0);
    corte(&db, "DIA", "2026-10-01 20:00:00", 3000.0, 2000.0);

    let esp = calcular_esperado_apertura(&db, "2026-10-02 08:00:00").unwrap();
    assert_eq!(esp.esperado, 2000.0);
    abrir(&db, "2026-10-02 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-10-02 10:00:00", 700.0);

    let c = corte(&db, "DIA", "2026-10-02 20:00:00", 2700.0, 2000.0);
    assert_eq!(c.efectivo_esperado, 2700.0);
    assert_eq!(c.diferencia, 0.0);
}

#[test]
fn venta_despues_del_cierre_entra_al_dia_siguiente() {
    // Antes: el cierre guardaba fecha_fin = 23:59:59 y esta venta quedaba
    // dentro de un rango ya cerrado → nunca se contaba.
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-10-01 10:00:00", 500.0);
    corte(&db, "DIA", "2026-10-01 18:00:00", 2500.0, 2000.0);

    venta_efectivo(&db, "2026-10-01 19:00:00", 200.0); // ya cerrado

    let esp = calcular_esperado_apertura(&db, "2026-10-02 08:00:00").unwrap();
    assert_eq!(esp.esperado, 2200.0, "fondo 2000 + la venta tardía");
    abrir(&db, "2026-10-02 08:00:00", 2200.0, None).unwrap();

    let c = corte(&db, "DIA", "2026-10-02 20:00:00", 2200.0, 2000.0);
    assert_eq!(c.efectivo_esperado, 2200.0);
    assert_eq!(c.diferencia, 0.0);
}

#[test]
fn dia_sin_cerrar_no_pierde_ventas_y_la_apertura_documenta_el_faltante() {
    // Antes: las ventas del día sin cerrar no entraban en ningún corte.
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-10-01 10:00:00", 1000.0);
    corte(&db, "DIA", "2026-10-01 20:00:00", 3000.0, 2000.0);

    // Día 2: se vende pero NO se cierra.
    abrir(&db, "2026-10-02 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-10-02 10:00:00", 800.0);

    // Día 3: se nota que hay ventas pendientes de cierre.
    assert_eq!(primer_dia_pendiente(&db, "2026-10-03").as_deref(), Some("2026-10-02"));

    // Alguien se llevó los 800 del día 2. Al abrir hay 2000, no 2800.
    let esp = calcular_esperado_apertura(&db, "2026-10-03 08:00:00").unwrap();
    assert_eq!(esp.esperado, 2800.0);
    let sin_nota = abrir(&db, "2026-10-03 08:00:00", 2000.0, None);
    assert!(sin_nota.is_err(), "con diferencia la nota es obligatoria");
    abrir(&db, "2026-10-03 08:00:00", 2000.0, Some("El efectivo del día 2 lo guardó el dueño")).unwrap();
    assert_eq!(
        contar(&db, "SELECT COUNT(*) FROM movimientos_caja WHERE tipo='RETIRO' AND monto=800 AND concepto LIKE 'Ajuste de apertura%'"),
        1, "el faltante queda registrado como ajuste explícito"
    );

    venta_efectivo(&db, "2026-10-03 10:00:00", 500.0);
    let c = corte(&db, "DIA", "2026-10-03 20:00:00", 2500.0, 2000.0);
    assert_eq!(c.diferencia, 0.0, "el cierre del día 3 cuadra: el faltante ya se documentó al abrir");

    let ventas_en_corte: f64 = db.query_row(
        "SELECT total_ventas_efectivo FROM cortes WHERE id = ?", [c.id], |r| r.get(0),
    ).unwrap();
    assert_eq!(ventas_en_corte, 1300.0, "incluye la venta del día sin cerrar");
    assert_eq!(primer_dia_pendiente(&db, "2026-10-04"), None);
}

#[test]
fn corte_de_turno_con_retiro_no_cuenta_doble() {
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-10-01 10:00:00", 1000.0);

    let p = corte_con(&db, "PARCIAL", "2026-10-01 14:00:00", "2026-10-01 14:00:00", 3000.0, 3000.0, Some(2500.0)).unwrap();
    assert_eq!(p.efectivo_esperado, 3000.0, "el retiro no altera el corte que lo origina");
    assert_eq!(p.fondo_siguiente, 500.0);
    let ligado: i64 = db.query_row(
        "SELECT corte_id FROM movimientos_caja WHERE tipo='RETIRO' AND monto=2500", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(ligado, p.id);

    venta_efectivo(&db, "2026-10-01 16:00:00", 400.0);
    let datos = calcular_periodo(&db, "2026-10-01 20:00:00").unwrap();
    assert_eq!(datos.efectivo_esperado, 900.0, "500 de fondo + 400; el retiro no se resta otra vez");
    assert_eq!(datos.total_retirado_parciales, 2500.0);
    assert_eq!(datos.cortes_parciales_hoy, 1);

    let c = corte(&db, "DIA", "2026-10-01 20:00:00", 900.0, 900.0);
    assert_eq!(c.diferencia, 0.0);
}

#[test]
fn venta_mientras_el_corte_esta_abierto_obliga_a_recalcular() {
    // Antes: se guardaba el esperado viejo y la venta se iba al siguiente
    // período aunque su efectivo ya estaba en la caja contada.
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-10-01 10:00:00", 100.0);

    let datos_viejos = calcular_periodo(&db, "2026-10-01 15:00:00").unwrap();
    venta_efectivo(&db, "2026-10-01 15:01:00", 50.0);

    let r = crear_corte_core(&db, NuevoCorte {
        tipo: "PARCIAL".into(),
        usuario_id: DUENO,
        fecha_inicio: datos_viejos.fecha_inicio.clone(),
        fecha_fin: datos_viejos.fecha_fin.clone(),
        datos: datos_viejos,
        efectivo_contado: 2150.0,
        nota_diferencia: None,
        fondo_siguiente: 2150.0,
        denominaciones: None,
        retiro: None,
    }, "2026-10-01 15:02:00");
    let err = r.err().expect("debe rechazar");
    assert!(err.starts_with(ERR_DATOS_CAMBIARON), "{err}");
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM cortes"), 0, "no se guardó nada");

    let c = corte(&db, "PARCIAL", "2026-10-01 15:03:00", 2150.0, 2150.0);
    assert_eq!(c.efectivo_esperado, 2150.0);
    assert_eq!(c.diferencia, 0.0);
}

#[test]
fn movimiento_viejo_huerfano_no_se_cuela_al_corte_actual() {
    // Antes: los movimientos sin corte se barrían sin filtro de fecha.
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    corte(&db, "DIA", "2026-10-01 20:00:00", 2000.0, 2000.0);

    // Dato viejo: un retiro del día 1 que quedó sin corte asignado.
    movimiento(&db, "2026-10-01 10:00:00", "RETIRO", 300.0);

    abrir(&db, "2026-10-02 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-10-02 10:00:00", 100.0);
    assert_eq!(esperado(&db, "2026-10-02 20:00:00"), 2100.0, "el retiro viejo no resta hoy");

    let c = corte(&db, "DIA", "2026-10-02 20:00:00", 2100.0, 2000.0);
    assert_eq!(c.diferencia, 0.0);
    assert_eq!(
        contar(&db, "SELECT COUNT(*) FROM movimientos_caja WHERE monto = 300 AND corte_id IS NULL"),
        1, "el huérfano no se asigna a un corte que no lo contó"
    );
}

#[test]
fn anular_venta_del_periodo_abierto_no_crea_retiro() {
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    let v = venta_efectivo(&db, "2026-10-01 10:00:00", 500.0);
    anular_venta_core(&db, v, DUENO, "error de cobro".into(), "2026-10-01 11:00:00").unwrap();

    assert_eq!(contar(&db, "SELECT COUNT(*) FROM movimientos_caja"), 0);
    assert_eq!(esperado(&db, "2026-10-01 20:00:00"), 2000.0);
}

#[test]
fn anular_venta_ya_cortada_registra_el_reembolso() {
    // Antes: el corte de turno ya había contado la venta; al anularla el
    // dinero salía de la caja sin registrarse → faltante en el siguiente corte.
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    let v = venta_efectivo(&db, "2026-10-01 10:00:00", 500.0);
    corte(&db, "PARCIAL", "2026-10-01 12:00:00", 2500.0, 2500.0);

    anular_venta_core(&db, v, DUENO, "cliente se arrepintió".into(), "2026-10-01 13:00:00").unwrap();
    assert_eq!(
        contar(&db, "SELECT COUNT(*) FROM movimientos_caja WHERE tipo='RETIRO' AND monto=500 AND corte_id IS NULL"),
        1
    );

    let c = corte(&db, "DIA", "2026-10-01 20:00:00", 2000.0, 2000.0);
    assert_eq!(c.efectivo_esperado, 2000.0, "2500 que quedaron − 500 reembolsados");
    assert_eq!(c.diferencia, 0.0);
}

#[test]
fn devolucion_de_venta_con_tarjeta_no_saca_dinero_de_la_caja() {
    // Antes: siempre se registraba un retiro de efectivo → sobrante fantasma.
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    let v = vender(&db, "2026-10-01 10:00:00", 1.0, 300.0, 300.0, "tarjeta");
    let vd: i64 = db.query_row("SELECT id FROM venta_detalle WHERE venta_id = ?", [v], |r| r.get(0)).unwrap();

    let d = crear_devolucion_core(&db, NuevaDevolucion {
        venta_id: v,
        usuario_id: DUENO,
        autorizado_por: None,
        motivo: "defectuoso".into(),
        items: vec![ItemDevolucion { venta_detalle_id: vd, cantidad: 1.0 }],
        reembolso_efectivo: None,
    }, "2026-10-01 11:00:00").unwrap();

    assert_eq!(d.total_devuelto, 300.0);
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM movimientos_caja"), 0);
    assert_eq!(esperado(&db, "2026-10-01 20:00:00"), 2000.0);
}

#[test]
fn devolucion_total_regresa_exactamente_lo_cobrado() {
    // La venta de 67.80 se cobra 68 (redondeo al peso). Antes se reembolsaban
    // 67.80 y quedaban 20 centavos de sobrante en el corte.
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    let v = vender(&db, "2026-10-01 10:00:00", 1.0, 67.80, 68.0, "efectivo");
    let vd: i64 = db.query_row("SELECT id FROM venta_detalle WHERE venta_id = ?", [v], |r| r.get(0)).unwrap();

    let d = crear_devolucion_core(&db, NuevaDevolucion {
        venta_id: v,
        usuario_id: DUENO,
        autorizado_por: None,
        motivo: "no le quedó".into(),
        items: vec![ItemDevolucion { venta_detalle_id: vd, cantidad: 1.0 }],
        reembolso_efectivo: None,
    }, "2026-10-01 11:00:00").unwrap();

    assert_eq!(d.total_devuelto, 68.0);
    assert_eq!(esperado(&db, "2026-10-01 20:00:00"), 2000.0);
}

#[test]
fn devolucion_parcial_y_partida_repetida() {
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    let v = vender(&db, "2026-10-01 10:00:00", 2.0, 100.0, 200.0, "efectivo");
    let vd: i64 = db.query_row("SELECT id FROM venta_detalle WHERE venta_id = ?", [v], |r| r.get(0)).unwrap();

    // La misma partida dos veces suma 3 > 2 vendidas: debe rechazarse.
    let doble = crear_devolucion_core(&db, NuevaDevolucion {
        venta_id: v,
        usuario_id: DUENO,
        autorizado_por: None,
        motivo: "x".into(),
        items: vec![
            ItemDevolucion { venta_detalle_id: vd, cantidad: 2.0 },
            ItemDevolucion { venta_detalle_id: vd, cantidad: 1.0 },
        ],
        reembolso_efectivo: None,
    }, "2026-10-01 11:00:00");
    assert!(doble.is_err());

    let d = crear_devolucion_core(&db, NuevaDevolucion {
        venta_id: v,
        usuario_id: DUENO,
        autorizado_por: None,
        motivo: "una no sirvió".into(),
        items: vec![ItemDevolucion { venta_detalle_id: vd, cantidad: 1.0 }],
        reembolso_efectivo: None,
    }, "2026-10-01 11:00:00").unwrap();
    assert_eq!(d.total_devuelto, 100.0);
    assert_eq!(esperado(&db, "2026-10-01 20:00:00"), 2100.0, "2000 + 200 − 100");
}

#[test]
fn apertura_con_sobrante_registra_entrada() {
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    corte(&db, "DIA", "2026-10-01 20:00:00", 2000.0, 2000.0);

    abrir(&db, "2026-10-02 08:00:00", 2150.0, Some("Había cambio extra")).unwrap();
    assert_eq!(
        contar(&db, "SELECT COUNT(*) FROM movimientos_caja WHERE tipo='ENTRADA' AND monto=150"),
        1
    );
    assert_eq!(esperado(&db, "2026-10-02 20:00:00"), 2150.0);
}

#[test]
fn validaciones_del_corte() {
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();

    let fondo_mayor = corte_con(&db, "DIA", "2026-10-01 20:00:00", "2026-10-01 20:00:00", 1000.0, 2000.0, None);
    assert!(fondo_mayor.is_err(), "no se puede dejar más de lo contado");

    let retiro_mayor = corte_con(&db, "PARCIAL", "2026-10-01 20:00:00", "2026-10-01 20:00:00", 1000.0, 0.0, Some(1500.0));
    assert!(retiro_mayor.is_err(), "no se puede retirar más de lo contado");

    corte(&db, "DIA", "2026-10-01 20:00:00", 2000.0, 2000.0);
    let doble = corte_con(&db, "DIA", "2026-10-01 21:00:00", "2026-10-01 21:00:00", 2000.0, 2000.0, None);
    assert!(doble.is_err(), "un solo cierre por día");
}

#[test]
fn instalacion_nueva_arranca_y_los_permisos_no_se_duplican_al_reiniciar() {
    // `nueva_db` ya recorre el camino de una instalación nueva (esquema →
    // migraciones → semilla). Antes la migración 013 tronaba aquí.
    let db = nueva_db();
    let tras_instalar = contar(&db, "SELECT COUNT(*) FROM permisos");
    assert!(tras_instalar > 0);

    // Cada arranque del POS vuelve a correr la semilla.
    for _ in 0..3 {
        db.execute_batch(SEED_DATA).unwrap();
    }
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM permisos"), tras_instalar);
    assert_eq!(
        contar(&db, "SELECT COUNT(*) FROM permisos WHERE rol_id = 2 AND modulo = 'recepcion' AND permitido = 1"),
        2, "el vendedor tiene ver y crear en recepción"
    );
}

#[test]
fn actualizacion_limpia_permisos_duplicados_existentes() {
    // Simula una computadora que ya tenía el POS: permisos duplicados por
    // meses de arranques y la base en la versión 13 (antes de la 014).
    let db = nueva_db();
    db.execute_batch("DROP INDEX idx_permisos_unico;").unwrap();
    for _ in 0..3 {
        db.execute(
            "INSERT INTO permisos (rol_id, modulo, accion, permitido) VALUES (2, 'recepcion', 'ver', 1)", [],
        ).unwrap();
        db.execute(
            "INSERT INTO permisos (rol_id, modulo, accion, permitido) VALUES (2, 'inventario', 'ver', 0)", [],
        ).unwrap();
    }
    let distintos = contar(&db, "SELECT COUNT(*) FROM (SELECT DISTINCT rol_id, modulo, accion FROM permisos)");
    assert!(contar(&db, "SELECT COUNT(*) FROM permisos") > distintos);

    db.execute_batch("PRAGMA user_version = 13;").unwrap();
    aplicar_migraciones(&db).unwrap();

    assert_eq!(contar(&db, "SELECT COUNT(*) FROM permisos"), distintos, "sin duplicados");
    assert_eq!(
        contar(&db, "SELECT permitido FROM permisos WHERE rol_id = 2 AND modulo = 'recepcion' AND accion = 'ver'"),
        1
    );
    assert_eq!(
        contar(&db, "SELECT permitido FROM permisos WHERE rol_id = 2 AND modulo = 'inventario' AND accion = 'ver'"),
        0
    );
}

#[test]
fn auditoria_queda_en_cero_con_el_modelo_nuevo_y_detecta_huerfanos_viejos() {
    use super::auditoria_caja::auditar;
    // Fechas en el pasado lejano: la auditoría compara contra la fecha real.
    let db = nueva_db();
    abrir(&db, "2026-01-05 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-01-05 10:00:00", 1000.0);
    corte_con(&db, "PARCIAL", "2026-01-05 14:00:00", "2026-01-05 14:00:00", 3000.0, 0.0, Some(1000.0)).unwrap();
    venta_efectivo(&db, "2026-01-05 16:00:00", 400.0);
    corte(&db, "DIA", "2026-01-05 20:00:00", 2400.0, 2000.0);
    venta_efectivo(&db, "2026-01-05 21:00:00", 150.0); // después del cierre

    // Día 6 sin cierre; día 7 abre con faltante documentado y cierra.
    abrir(&db, "2026-01-06 08:00:00", 2150.0, None).unwrap();
    venta_efectivo(&db, "2026-01-06 10:00:00", 600.0);
    abrir(&db, "2026-01-07 08:00:00", 2150.0, Some("los 600 del día 6 se depositaron")).unwrap();
    let c = corte(&db, "DIA", "2026-01-07 20:00:00", 2150.0, 2000.0);
    assert_eq!(c.diferencia, 0.0);

    let a = auditar(&db, 3650).unwrap();
    assert_eq!(a.resumen.ventas_sin_corte_count, 0, "ninguna venta fuera de la cadena");
    assert_eq!(a.resumen.movimientos_desalineados_count, 0);
    assert_eq!(a.resumen.movimientos_huerfanos_count, 0);
    assert_eq!(a.resumen.eslabones_rotos_count, 0, "cada corte arranca con lo que dejó el anterior");
    assert_eq!(a.resumen.cortes_inconsistentes_count, 0);
    assert_eq!(a.resumen.dias_sin_cierre_count, 1, "el día 6 se reporta (informativo)");

    // Dato heredado del modelo anterior: un retiro que ningún corte contó.
    movimiento(&db, "2026-01-05 12:00:00", "RETIRO", 300.0);
    let a = auditar(&db, 3650).unwrap();
    assert_eq!(a.resumen.movimientos_huerfanos_count, 1);
    assert_eq!(a.resumen.movimientos_huerfanos_monto, -300.0);
}

#[test]
fn venta_con_partida_invalida_se_rechaza_sin_gastar_folio() {
    let db = nueva_db();
    let folio_antes = contar(&db, "SELECT ultimo_valor FROM folio_secuencia WHERE id = 1");
    for (cantidad, precio, pct) in [(0.0, 100.0, 0.0), (-2.0, 100.0, 0.0), (1.0, -5.0, 0.0), (1.0, 100.0, 120.0)] {
        let r = crear_venta_core(&db, NuevaVenta {
            usuario_id: DUENO,
            cliente_id: None,
            subtotal: precio * cantidad,
            descuento: 0.0,
            total: (precio * cantidad).max(0.0),
            metodo_pago: "efectivo".into(),
            monto_recibido: 1000.0,
            cambio: 0.0,
            items: vec![ItemVenta {
                producto_id: 1,
                cantidad,
                precio_original: precio,
                descuento_porcentaje: pct,
                descuento_monto: 0.0,
                precio_final: precio,
                subtotal: precio * cantidad,
                autorizado_por: None,
            }],
            presupuesto_origen_id: None,
        }, "2026-10-01 10:00:00");
        assert!(r.is_err(), "debió rechazar cantidad={cantidad} precio={precio} pct={pct}");
    }
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM ventas"), 0);
    assert_eq!(contar(&db, "SELECT ultimo_valor FROM folio_secuencia WHERE id = 1"), folio_antes);
    assert_eq!(contar(&db, "SELECT CAST(stock_actual AS INTEGER) FROM productos WHERE id = 1"), 1000);

    // Una venta normal (y una de precio 0, que existen en producción) sigue pasando.
    venta_efectivo(&db, "2026-10-01 10:05:00", 68.0);
    vender(&db, "2026-10-01 10:06:00", 2.0, 0.0, 0.0, "efectivo");
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM ventas"), 2);
}

// ─── Caja web / ventas que llegan por sync (sync v2) ──────
//
// El apply del sync (src-tauri/src/sync) es de otro módulo; aquí las filas web
// se insertan directo con SQL, tal como quedarían después de aplicarse.

/// Venta hecha en la web (origen 'web') con una partida del producto 1.
/// `registrado_at` = momento en que llegó tarde a este equipo (None si llegó
/// dentro del período abierto).
fn venta_web(
    db: &Connection,
    fecha: &str,
    total: f64,
    metodo: &str,
    caja: &str,
    registrado_at: Option<&str>,
) -> i64 {
    let n = contar(db, "SELECT COUNT(*) FROM ventas") + 1;
    db.execute(
        "INSERT INTO ventas (folio, usuario_id, subtotal, descuento, total, metodo_pago, \
                             monto_recibido, cambio, fecha, origen, caja, registrado_at, uuid) \
         VALUES (?, 1, ?, 0, ?, ?, ?, 0, ?, 'web', ?, ?, ?)",
        rusqlite::params![
            format!("V-20261001-{:04}", n), total, total, metodo, total, fecha, caja,
            registrado_at, format!("0192a0b1-0000-7000-8000-{:012}", n)
        ],
    ).unwrap();
    let id = db.last_insert_rowid();
    db.execute(
        "INSERT INTO venta_detalle (venta_id, producto_id, cantidad, precio_original, precio_final, subtotal) \
         VALUES (?, 1, 1, ?, ?, ?)",
        rusqlite::params![id, total, total, total],
    ).unwrap();
    id
}

fn movimiento_web(
    db: &Connection,
    fecha: &str,
    tipo: &str,
    monto: f64,
    caja: &str,
    registrado_at: Option<&str>,
) -> i64 {
    db.execute(
        "INSERT INTO movimientos_caja (tipo, usuario_id, monto, concepto, fecha, origen, caja, registrado_at) \
         VALUES (?, 1, ?, 'movimiento web', ?, 'web', ?, ?)",
        rusqlite::params![tipo, monto, fecha, caja, registrado_at],
    ).unwrap();
    db.last_insert_rowid()
}

/// Corte de la caja web (el servidor es su único escritor; el escritorio
/// nunca debería guardarlo, pero si llegara no debe contar).
fn corte_caja_web(db: &Connection, tipo: &str, momento: &str, contado: f64, fondo_siguiente: f64) {
    db.execute(
        "INSERT INTO cortes (tipo, usuario_id, fecha_inicio, fecha_fin, fondo_inicial, \
                             efectivo_esperado, efectivo_contado, diferencia, fondo_siguiente, \
                             created_at, origen, caja) \
         VALUES (?, 1, '0000-00-00 00:00:00', ?, 0, 1, ?, 0, ?, ?, 'web', 'web')",
        rusqlite::params![tipo, momento, contado, fondo_siguiente, momento],
    ).unwrap();
}

#[test]
fn venta_web_espejo_cuenta_en_el_periodo_abierto() {
    // Una tableta en modo espejo vende con el dinero del cajón de la tienda:
    // la venta llega por sync (origen 'web', caja 'principal') y cuenta.
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-10-01 09:00:00", 500.0);
    venta_web(&db, "2026-10-01 10:00:00", 300.0, "efectivo", "principal", None);
    venta_web(&db, "2026-10-01 10:30:00", 200.0, "tarjeta", "principal", None);
    let mov = movimiento_web(&db, "2026-10-01 11:00:00", "RETIRO", 50.0, "principal", None);

    let datos = calcular_periodo(&db, "2026-10-01 20:00:00").unwrap();
    assert_eq!(datos.total_ventas_efectivo, 800.0, "500 del escritorio + 300 de la web");
    assert_eq!(datos.total_ventas_tarjeta, 200.0);
    assert_eq!(datos.num_transacciones, 3);
    assert_eq!(datos.total_retiros_efectivo, 50.0);
    assert_eq!(datos.movimientos.len(), 1);
    assert_eq!(datos.efectivo_esperado, 2750.0, "2000 + 500 + 300 − 50");

    let c = corte(&db, "DIA", "2026-10-01 20:00:00", 2750.0, 2000.0);
    assert_eq!(c.diferencia, 0.0);
    let ligado: Option<i64> = db.query_row(
        "SELECT corte_id FROM movimientos_caja WHERE id = ?", [mov], |r| r.get(0),
    ).unwrap();
    assert_eq!(ligado, Some(c.id), "el retiro web queda en el corte que lo contó");
}

#[test]
fn caja_web_nunca_cuenta_en_la_cadena_de_la_tienda() {
    use super::auditoria_caja::auditar;
    let db = nueva_db();

    // Apertura de la caja web un día ANTES (con otro fondo): no debe tomarse
    // como "primera apertura" de la tienda ni como apertura de ese día.
    db.execute(
        "INSERT INTO aperturas_caja (usuario_id, fondo_declarado, nota, fecha, origen, caja) \
         VALUES (1, 9999, NULL, '2026-09-30 07:00:00', 'web', 'web')",
        [],
    ).unwrap();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    // Sin cortes, el período arranca en la primera apertura DE LA TIENDA
    // (la web del 30 no cuenta).
    assert_eq!(inicio_y_fondo(&db), ("2026-10-01 07:59:59".to_string(), 2000.0));
    assert!(apertura_del_dia(&db, "2026-09-30").is_none(), "la apertura web no es la de la tienda");
    assert_eq!(apertura_del_dia(&db, "2026-10-01").map(|a| a.fondo_declarado), Some(2000.0));

    venta_efectivo(&db, "2026-10-01 09:00:00", 500.0);
    venta_web(&db, "2026-09-29 10:00:00", 111.0, "efectivo", "web", None);
    venta_web(&db, "2026-10-01 10:00:00", 999.0, "efectivo", "web", None);
    movimiento_web(&db, "2026-10-01 11:00:00", "RETIRO", 77.0, "web", None);
    corte_caja_web(&db, "PARCIAL", "2026-10-01 13:00:00", 20000.0, 12345.0);
    corte_caja_web(&db, "DIA", "2026-10-01 14:00:00", 20000.0, 12345.0);

    assert!(ultimo_corte(&db).is_none(), "un corte web nunca es el último de la tienda");
    let datos = calcular_periodo(&db, "2026-10-01 15:00:00").unwrap();
    assert_eq!(datos.fecha_inicio, "2026-10-01 07:59:59");
    assert_eq!(datos.fondo_inicial, 2000.0);
    assert_eq!(datos.total_ventas_efectivo, 500.0);
    assert_eq!(datos.num_transacciones, 1);
    assert!(datos.movimientos.is_empty());
    assert_eq!(datos.vendedores.iter().map(|v| v.total_vendido).sum::<f64>(), 500.0);
    assert_eq!(datos.cortes_parciales_hoy, 0);
    assert_eq!(datos.total_retirado_parciales, 0.0);
    assert_eq!(datos.efectivo_esperado, 2500.0);
    assert_eq!(primer_dia_pendiente(&db, "2026-10-02").as_deref(), Some("2026-10-01"),
        "la venta web del 29 no es un día pendiente de la tienda");

    // El cierre web del día no bloquea el cierre de la tienda.
    let c = corte(&db, "DIA", "2026-10-01 20:00:00", 2500.0, 2000.0);
    assert_eq!(c.diferencia, 0.0);
    // Más movimientos web después del corte: tampoco se asignan ni se listan.
    movimiento_web(&db, "2026-10-01 21:00:00", "ENTRADA", 5.0, "web", None);
    corte_caja_web(&db, "PARCIAL", "2026-10-01 22:00:00", 1.0, 1.0);

    let u = ultimo_corte(&db).expect("hay corte de la tienda");
    assert_eq!((u.fin.as_str(), u.fondo_siguiente), ("2026-10-01 20:00:00", 2000.0));
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM movimientos_caja WHERE caja = 'web' AND corte_id IS NOT NULL"), 0);
    assert!(movimientos_sin_corte(&db).unwrap().is_empty());
    let lista = listar_cortes_core(&db, 50).unwrap();
    assert_eq!(lista.len(), 1);
    assert_eq!(lista[0].id, c.id);
    assert_eq!(calcular_esperado_apertura(&db, "2026-10-02 08:00:00").unwrap().esperado, 2000.0);

    // Auditoría: nada de la caja web aparece como fuga ni rompe la cadena.
    let a = auditar(&db, 3650).unwrap();
    assert_eq!(a.resumen.ventas_sin_corte_count, 0);
    assert_eq!(a.resumen.movimientos_huerfanos_count, 0);
    assert_eq!(a.resumen.movimientos_desalineados_count, 0);
    assert_eq!(a.resumen.eslabones_rotos_count, 0);
    assert_eq!(a.resumen.cortes_inconsistentes_count, 0, "los cortes web (esperado 1) no se auditan aquí");
    assert_eq!(a.resumen.dias_sin_cierre_count, 0);

    // Apertura de la tienda en un día en que solo hay apertura web: la
    // verificación de "ya existe" no la cuenta. El índice único por día
    // (idx_apertura_dia_unica) sí la bloquea, con un mensaje claro: por eso
    // el sync nunca guarda aperturas de la caja web.
    db.execute(
        "INSERT INTO aperturas_caja (usuario_id, fondo_declarado, fecha, origen, caja) \
         VALUES (1, 0, '2026-10-02 07:00:00', 'web', 'web')",
        [],
    ).unwrap();
    let err = abrir(&db, "2026-10-02 08:00:00", 2000.0, None).unwrap_err();
    assert!(err.contains("otra apertura registrada para hoy"), "{err}");
    db.execute_batch("DROP INDEX idx_apertura_dia_unica;").unwrap();
    abrir(&db, "2026-10-02 08:00:00", 2000.0, None).unwrap();
    assert_eq!(apertura_del_dia(&db, "2026-10-02").map(|a| a.fondo_declarado), Some(2000.0));
}

#[test]
fn venta_web_tardia_entra_al_siguiente_corte_y_la_auditoria_no_la_marca() {
    use super::auditoria_caja::auditar;
    // Fechas en el pasado: la auditoría compara contra la fecha real.
    let db = nueva_db();
    abrir(&db, "2026-01-05 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-01-05 10:00:00", 1000.0);
    let k1 = corte(&db, "DIA", "2026-01-05 18:00:00", 3000.0, 2000.0);
    // K1 con el rango del modelo anterior (por día), como los cortes viejos
    // de producción: no cubre nada del día 4.
    db.execute("UPDATE cortes SET fecha_inicio = '2026-01-05 00:00:00' WHERE id = ?", [k1.id]).unwrap();

    // Llegan por sync DESPUÉS del corte: una venta espejo con fecha anterior
    // al corte (del día 4) y una entrada con fecha 17:59:55. El sync les puso
    // registrado_at = momento de llegada.
    let v = venta_web(&db, "2026-01-04 23:30:00", 300.0, "efectivo", "principal", Some("2026-01-05 18:00:01"));
    let m = movimiento_web(&db, "2026-01-05 17:59:55", "ENTRADA", 20.0, "principal", Some("2026-01-05 18:00:02"));

    assert_eq!(
        calcular_esperado_apertura(&db, "2026-01-06 08:00:00").unwrap().esperado, 2320.0,
        "fondo 2000 + venta tardía 300 + entrada tardía 20"
    );
    abrir(&db, "2026-01-06 08:00:00", 2320.0, None).unwrap();
    let k2 = corte(&db, "DIA", "2026-01-06 20:00:00", 2320.0, 2000.0);
    assert_eq!(k2.diferencia, 0.0);
    let (ventas_k1, ventas_k2, entradas_k2): (f64, f64, f64) = db.query_row(
        "SELECT (SELECT total_ventas_efectivo FROM cortes WHERE id = ?1), \
                (SELECT total_ventas_efectivo FROM cortes WHERE id = ?2), \
                (SELECT total_entradas_efectivo FROM cortes WHERE id = ?2)",
        [k1.id, k2.id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!((ventas_k1, ventas_k2, entradas_k2), (1000.0, 300.0, 20.0),
        "la venta tardía cuenta en el corte SIGUIENTE, una sola vez");
    let ligado: Option<i64> = db.query_row("SELECT corte_id FROM movimientos_caja WHERE id = ?", [m], |r| r.get(0)).unwrap();
    assert_eq!(ligado, Some(k2.id));

    // Por fecha, la venta (día 4) quedaría fuera de todo corte y la entrada
    // (17:59:55) fuera del rango de K2. Por MOMENTO, ambas están en K2.
    let a = auditar(&db, 3650).unwrap();
    assert_eq!(a.resumen.ventas_sin_corte_count, 0, "{:?}", a.ventas_sin_corte.iter().map(|x| &x.folio).collect::<Vec<_>>());
    assert_eq!(a.resumen.movimientos_desalineados_count, 0);
    assert_eq!(a.resumen.movimientos_huerfanos_count, 0);
    assert_eq!(a.resumen.eslabones_rotos_count, 0);
    assert_eq!(a.resumen.cortes_inconsistentes_count, 0);
    assert_eq!(a.resumen.dias_sin_cierre_count, 0);
    let _ = v;
}

#[test]
fn anular_venta_de_caja_web_se_rechaza() {
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    let v = venta_web(&db, "2026-10-01 10:00:00", 300.0, "efectivo", "web", None);
    let stock_antes = contar(&db, "SELECT CAST(stock_actual AS INTEGER) FROM productos WHERE id = 1");

    let err = anular_venta_core(&db, v, DUENO, "error".into(), "2026-10-01 11:00:00").unwrap_err();
    assert!(err.contains("caja web"), "{err}");
    assert_eq!(contar(&db, &format!("SELECT anulada FROM ventas WHERE id = {v}")), 0);
    assert_eq!(contar(&db, "SELECT CAST(stock_actual AS INTEGER) FROM productos WHERE id = 1"), stock_antes);
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM movimientos_caja"), 0);
}

#[test]
fn anular_venta_espejo_tardia_usa_el_momento_en_la_cadena() {
    // Venta web espejo con fecha 11:59 que llegó a las 12:00:01, después del
    // corte de las 12:00: ese corte NO la contó, así que anularla no es
    // "ya cortada" y no debe registrar reembolso (por fecha sí lo haría).
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    corte(&db, "PARCIAL", "2026-10-01 12:00:00", 2000.0, 2000.0);
    let v = venta_web(&db, "2026-10-01 11:59:00", 300.0, "efectivo", "principal", Some("2026-10-01 12:00:01"));
    assert_eq!(esperado(&db, "2026-10-01 13:00:00"), 2300.0);

    anular_venta_core(&db, v, DUENO, "cliente se arrepintió".into(), "2026-10-01 13:00:00").unwrap();
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM movimientos_caja"), 0, "no hubo reembolso de un corte cerrado");
    assert_eq!(esperado(&db, "2026-10-01 20:00:00"), 2000.0);

    // Una venta espejo que sí entró en un corte cerrado registra el reembolso.
    let w = venta_web(&db, "2026-10-01 13:30:00", 150.0, "efectivo", "principal", None);
    corte(&db, "PARCIAL", "2026-10-01 14:00:00", 2150.0, 2150.0);
    anular_venta_core(&db, w, DUENO, "devolvió todo".into(), "2026-10-01 15:00:00").unwrap();
    assert_eq!(
        contar(&db, "SELECT COUNT(*) FROM movimientos_caja WHERE tipo = 'RETIRO' AND monto = 150 AND corte_id IS NULL"),
        1
    );
    assert_eq!(esperado(&db, "2026-10-01 20:00:00"), 2000.0);
}

// ─── Reloj monótono: la cadena nunca retrocede ────────────
//
// En 2026-09 la PC de la tienda arrancó con el reloj ~12 h atrasado y las
// ventas quedaron con fecha anterior al último corte: ningún corte las contó.
// Ahora toda fila del escritorio que entra a la cadena conserva su `fecha`
// real, pero si el reloj quedó en o antes del último corte se le pone
// registrado_at = fin del corte + 1 s, y el corte se estampa en
// max(reloj, fin + 1 s).

/// Movimiento manual por el mismo camino que el comando `crear_movimiento_caja`.
fn mover(db: &Connection, cuando: &str, tipo: &str, monto: f64) -> MovimientoCajaRs {
    crear_movimiento_core(db, NuevoMovimiento {
        tipo: tipo.to_string(),
        usuario_id: DUENO,
        monto,
        concepto: "prueba".into(),
        autorizado_por: None,
        pin_autorizacion: None,
    }, cuando).unwrap()
}

fn texto_opt(db: &Connection, sql: &str) -> Option<String> {
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn fecha_y_registro(db: &Connection, tabla: &str, id: i64) -> (String, Option<String>) {
    db.query_row(
        &format!("SELECT fecha, registrado_at FROM {tabla} WHERE id = ?"),
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap()
}

fn partida_de(db: &Connection, venta_id: i64) -> i64 {
    db.query_row("SELECT id FROM venta_detalle WHERE venta_id = ?", [venta_id], |r| r.get(0)).unwrap()
}

#[test]
fn reloj_12h_atrasado_despues_del_cierre_las_ventas_cuentan_en_el_siguiente_corte() {
    use super::auditoria_caja::auditar;
    // Fechas en el pasado lejano: la auditoría compara contra la fecha real.
    let db = nueva_db();
    abrir(&db, "2026-01-05 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-01-05 10:00:00", 500.0);
    corte(&db, "DIA", "2026-01-05 20:30:00", 2500.0, 2000.0);
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM ventas WHERE registrado_at IS NOT NULL"), 0,
        "con el reloj bien las ventas del escritorio no llevan registrado_at");
    assert_eq!(piso_cadena(&db).as_deref(), Some("2026-01-05 20:30:01"));

    // Día 2: la PC arranca con el reloj ~12 h atrasado (cree que es el día 1
    // por la noche, antes del cierre de las 20:30).
    let v1 = venta_efectivo(&db, "2026-01-05 19:40:00", 300.0);
    let v2 = vender(&db, "2026-01-05 20:10:00", 1.0, 200.0, 200.0, "tarjeta");
    let m = mover(&db, "2026-01-05 20:15:00", "ENTRADA", 50.0);
    assert_eq!(m.fecha, "2026-01-05 20:15:00");
    for (tabla, id, fecha) in [
        ("ventas", v1, "2026-01-05 19:40:00"),
        ("ventas", v2, "2026-01-05 20:10:00"),
        ("movimientos_caja", m.id, "2026-01-05 20:15:00"),
    ] {
        assert_eq!(
            fecha_y_registro(&db, tabla, id),
            (fecha.to_string(), Some("2026-01-05 20:30:01".to_string())),
            "{tabla} {id}: fecha = reloj real, registrado_at = fin del corte + 1 s"
        );
    }

    // Vista previa y esperado de apertura con el reloj atrasado: ya las ven.
    let datos = calcular_periodo(&db, "2026-01-05 20:20:00").unwrap();
    assert_eq!(datos.fecha_inicio, "2026-01-05 20:30:00");
    assert_eq!(datos.fecha_fin, "2026-01-05 20:30:01", "el período termina en el piso, no antes de su inicio");
    assert_eq!(datos.total_ventas_efectivo, 300.0);
    assert_eq!(datos.total_ventas_tarjeta, 200.0);
    assert_eq!(datos.total_entradas_efectivo, 50.0);
    assert_eq!(datos.movimientos.len(), 1);
    assert_eq!(datos.efectivo_esperado, 2350.0);
    assert_eq!(calcular_esperado_apertura(&db, "2026-01-05 20:20:00").unwrap().esperado, 2350.0);
    assert_eq!(movimientos_sin_corte(&db).unwrap().len(), 1);

    // Un cierre del día con el reloj atrasado cuenta para el día del piso,
    // que ya se cerró: se rechaza (se usa corte de turno o se corrige el reloj).
    let err = corte_con(&db, "DIA", "2026-01-05 20:25:00", "2026-01-05 20:25:00", 2350.0, 2000.0, None)
        .err().expect("debió rechazar el segundo cierre del día del piso");
    assert!(err.contains("2026-01-05"), "{err}");

    // Se corrige el reloj y el cierre del día 2 las cuenta, una sola vez.
    let c = corte(&db, "DIA", "2026-01-06 20:00:00", 2350.0, 2000.0);
    assert_eq!(c.diferencia, 0.0);
    let (ef, tar, ent): (f64, f64, f64) = db.query_row(
        "SELECT total_ventas_efectivo, total_ventas_tarjeta, total_entradas_efectivo FROM cortes WHERE id = ?",
        [c.id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!((ef, tar, ent), (300.0, 200.0, 50.0));
    assert_eq!(contar(&db, &format!("SELECT corte_id FROM movimientos_caja WHERE id = {}", m.id)), c.id);
    assert_eq!(
        contar(&db, "SELECT CAST(SUM(total_ventas_efectivo) AS INTEGER) FROM cortes"), 800,
        "corte 1 (500) + corte 2 (300): ninguna venta se perdió ni se contó doble"
    );

    let a = auditar(&db, 3650).unwrap();
    assert_eq!(a.resumen.ventas_sin_corte_count, 0);
    assert_eq!(a.resumen.movimientos_huerfanos_count, 0);
    assert_eq!(a.resumen.movimientos_desalineados_count, 0);
    assert_eq!(a.resumen.eslabones_rotos_count, 0);
    assert_eq!(a.resumen.cortes_inconsistentes_count, 0);
}

#[test]
fn corte_con_el_reloj_atrasado_se_estampa_en_fin_mas_un_segundo() {
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-10-01 10:00:00", 1000.0);
    corte(&db, "PARCIAL", "2026-10-01 14:00:00", 3000.0, 3000.0);

    // El reloj se atrasa una hora.
    let v = venta_efectivo(&db, "2026-10-01 13:00:00", 100.0);
    assert_eq!(fecha_y_registro(&db, "ventas", v).1.as_deref(), Some("2026-10-01 14:00:01"));

    // Vista previa a las 13:30 y confirmación a las 13:31: mismo período,
    // sin DATOS_CAMBIARON. Corte de turno con retiro.
    let p2 = corte_con(&db, "PARCIAL", "2026-10-01 13:30:00", "2026-10-01 13:31:00", 3100.0, 0.0, Some(2500.0))
        .unwrap();
    assert_eq!(p2.created_at, "2026-10-01 14:00:01");
    assert_eq!(p2.efectivo_esperado, 3100.0);
    assert_eq!(p2.diferencia, 0.0);
    assert_eq!(p2.fondo_siguiente, 600.0);
    let (fi, ff, ca): (String, String, String) = db.query_row(
        "SELECT fecha_inicio, fecha_fin, created_at FROM cortes WHERE id = ?",
        [p2.id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(
        (fi.as_str(), ff.as_str(), ca.as_str()),
        ("2026-10-01 14:00:00", "2026-10-01 14:00:01", "2026-10-01 14:00:01")
    );
    assert_eq!(ultimo_corte(&db).unwrap().fin, "2026-10-01 14:00:01");
    // El retiro del corte guarda el reloj real; su momento es el del corte.
    let (fecha_ret, reg_ret, corte_ret): (String, Option<String>, i64) = db.query_row(
        "SELECT fecha, registrado_at, corte_id FROM movimientos_caja WHERE tipo = 'RETIRO' AND monto = 2500",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(
        (fecha_ret.as_str(), reg_ret.as_deref(), corte_ret),
        ("2026-10-01 13:31:00", Some("2026-10-01 14:00:01"), p2.id)
    );

    // Sigue atrasado: la siguiente venta va al nuevo piso, y otro corte también.
    let w = venta_efectivo(&db, "2026-10-01 13:40:00", 40.0);
    assert_eq!(fecha_y_registro(&db, "ventas", w).1.as_deref(), Some("2026-10-01 14:00:02"));
    let p3 = corte(&db, "PARCIAL", "2026-10-01 13:45:00", 640.0, 640.0);
    assert_eq!(p3.created_at, "2026-10-01 14:00:02");
    assert_eq!(p3.efectivo_esperado, 640.0, "600 de fondo + 40");

    // Reloj corregido: las filas vuelven a ir sin registrado_at y todo cuadra.
    let x = venta_efectivo(&db, "2026-10-01 18:00:00", 60.0);
    assert_eq!(fecha_y_registro(&db, "ventas", x).1, None);
    let d = corte(&db, "DIA", "2026-10-01 20:00:00", 700.0, 700.0);
    assert_eq!((d.efectivo_esperado, d.diferencia), (700.0, 0.0));
    assert_eq!(
        contar(&db, "SELECT CAST(SUM(total_ventas_efectivo) AS INTEGER) FROM cortes"), 1200,
        "1000 + 100 + 40 + 60, cada venta en exactamente un corte"
    );
}

#[test]
fn anulacion_y_devolucion_con_el_reloj_atrasado_salen_del_periodo_abierto() {
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    let v1 = venta_efectivo(&db, "2026-10-01 10:00:00", 500.0);
    let v2 = vender(&db, "2026-10-01 10:30:00", 1.0, 300.0, 300.0, "efectivo");
    corte(&db, "PARCIAL", "2026-10-01 12:00:00", 2800.0, 2800.0);

    // Reloj atrasado: anulación y devolución a las 11:00 de ventas ya cortadas.
    anular_venta_core(&db, v1, DUENO, "cobro duplicado".into(), "2026-10-01 11:00:00").unwrap();
    crear_devolucion_core(&db, NuevaDevolucion {
        venta_id: v2,
        usuario_id: DUENO,
        autorizado_por: None,
        motivo: "no le quedó".into(),
        items: vec![ItemDevolucion { venta_detalle_id: partida_de(&db, v2), cantidad: 1.0 }],
        reembolso_efectivo: None,
    }, "2026-10-01 11:05:00").unwrap();
    assert_eq!(
        contar(&db, "SELECT COUNT(*) FROM movimientos_caja WHERE tipo = 'RETIRO' AND corte_id IS NULL \
                     AND registrado_at = '2026-10-01 12:00:01' AND fecha IN ('2026-10-01 11:00:00', '2026-10-01 11:05:00')"),
        2
    );
    assert_eq!(esperado(&db, "2026-10-01 11:10:00"), 2000.0, "2800 − 500 − 300, también con el reloj atrasado");

    let c = corte(&db, "DIA", "2026-10-01 20:00:00", 2000.0, 2000.0);
    assert_eq!(c.diferencia, 0.0);
    let retiros: f64 = db.query_row(
        "SELECT total_retiros_efectivo FROM cortes WHERE id = ?", [c.id], |r| r.get(0),
    ).unwrap();
    assert_eq!(retiros, 800.0);
}

#[test]
fn ajuste_de_apertura_despues_de_un_cierre_con_el_reloj_adelantado_entra_al_periodo() {
    let db = nueva_db();
    abrir(&db, "2026-10-01 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-10-01 10:00:00", 500.0);
    // El reloj iba adelantado al cerrar: el cierre quedó con la mañana siguiente.
    corte(&db, "DIA", "2026-10-02 09:00:00", 2500.0, 2000.0);

    // Reloj corregido; al abrir a las 08:00 faltan 100.
    abrir(&db, "2026-10-02 08:00:00", 1900.0, Some("faltaban 100")).unwrap();
    let ajuste: i64 = db.query_row(
        "SELECT id FROM movimientos_caja WHERE concepto LIKE 'Ajuste de apertura%'", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(
        fecha_y_registro(&db, "movimientos_caja", ajuste),
        ("2026-10-02 08:00:00".to_string(), Some("2026-10-02 09:00:01".to_string()))
    );
    venta_efectivo(&db, "2026-10-02 08:30:00", 100.0);
    assert_eq!(esperado(&db, "2026-10-02 08:45:00"), 2000.0, "2000 − 100 del ajuste + 100");

    let c = corte(&db, "PARCIAL", "2026-10-02 12:00:00", 2000.0, 2000.0);
    assert_eq!(c.diferencia, 0.0);
    assert_eq!(contar(&db, &format!("SELECT corte_id FROM movimientos_caja WHERE id = {ajuste}")), c.id);
}

// ─── Ventas borradas por el sync ──────────────────────────

#[test]
fn venta_borrada_por_el_sync_no_cuenta_en_la_cadena_ni_en_la_auditoria() {
    use super::auditoria_caja::auditar;
    // Fechas en el pasado lejano: la auditoría compara contra la fecha real.
    let db = nueva_db();
    // Venta del escritorio anterior a todo corte, borrada después (dato raro,
    // pero el borrado manda).
    let vieja = venta_efectivo(&db, "2026-01-04 10:00:00", 30.0);

    abrir(&db, "2026-01-05 08:00:00", 2000.0, None).unwrap();
    venta_efectivo(&db, "2026-01-05 09:00:00", 100.0);
    let borrada = venta_web(&db, "2026-01-05 10:00:00", 500.0, "efectivo", "principal", None);
    let borrada_anulada = venta_web(&db, "2026-01-05 10:30:00", 70.0, "efectivo", "principal", None);
    db.execute("UPDATE ventas SET anulada = 1 WHERE id = ?", [borrada_anulada]).unwrap();
    db.execute(
        "UPDATE ventas SET deleted_at = '2026-01-05 11:00:00' WHERE id IN (?, ?, ?)",
        [vieja, borrada, borrada_anulada],
    ).unwrap();

    let d = calcular_periodo(&db, "2026-01-05 20:00:00").unwrap();
    assert_eq!(d.total_ventas_efectivo, 100.0);
    assert_eq!(d.total_ventas, 100.0);
    assert_eq!(d.num_transacciones, 1);
    assert_eq!(d.total_anulaciones, 0.0);
    assert_eq!(d.vendedores.iter().map(|v| v.total_vendido).sum::<f64>(), 100.0);
    assert_eq!(d.efectivo_esperado, 2100.0);

    // Una venta borrada no se anula ni se devuelve (igual que en el servidor).
    let err = anular_venta_core(&db, borrada, DUENO, "x".into(), "2026-01-05 12:00:00").unwrap_err();
    assert!(err.contains("no encontrada"), "{err}");
    let err = crear_devolucion_core(&db, NuevaDevolucion {
        venta_id: borrada,
        usuario_id: DUENO,
        autorizado_por: None,
        motivo: "x".into(),
        items: vec![ItemDevolucion { venta_detalle_id: partida_de(&db, borrada), cantidad: 1.0 }],
        reembolso_efectivo: None,
    }, "2026-01-05 12:00:00").err().expect("no se devuelve una venta borrada");
    assert!(err.contains("no encontrada"), "{err}");
    assert_eq!(contar(&db, "SELECT COUNT(*) FROM movimientos_caja"), 0);
    assert_eq!(contar(&db, &format!("SELECT anulada FROM ventas WHERE id = {borrada}")), 0);

    let c = corte(&db, "DIA", "2026-01-05 20:00:00", 2100.0, 2000.0);
    assert_eq!(c.diferencia, 0.0);

    // Día 6: solo hay una venta (web) borrada → no es un día pendiente.
    let tarde = venta_web(&db, "2026-01-06 10:00:00", 40.0, "efectivo", "principal", None);
    db.execute("UPDATE ventas SET deleted_at = '2026-01-06 11:00:00' WHERE id = ?", [tarde]).unwrap();
    assert_eq!(primer_dia_pendiente(&db, "2026-01-07"), None);
    assert_eq!(esperado(&db, "2026-01-06 20:00:00"), 2000.0);

    let a = auditar(&db, 3650).unwrap();
    assert_eq!(a.resumen.ventas_sin_corte_count, 0, "{:?}", a.ventas_sin_corte.iter().map(|x| &x.folio).collect::<Vec<_>>());
    assert_eq!(a.resumen.dias_sin_cierre_count, 0, "{:?}", a.dias_sin_cierre.iter().map(|x| &x.dia).collect::<Vec<_>>());
}

// ─── PC nueva: sin cortes todavía ─────────────────────────

#[test]
fn pc_nueva_sin_cortes_el_periodo_empieza_en_la_primera_apertura() {
    use super::auditoria_caja::auditar;
    // Fechas en el pasado lejano: la auditoría compara contra la fecha real.
    let db = nueva_db();
    // El primer pull de una PC nueva trae la historia web de la caja de la
    // tienda (los cortes nunca bajan del servidor).
    venta_web(&db, "2025-05-10 10:00:00", 1000.0, "efectivo", "principal", None);
    venta_web(&db, "2025-08-12 13:14:43", 2000.0, "efectivo", "principal", None);
    movimiento_web(&db, "2025-08-12 15:00:00", "RETIRO", 300.0, "principal", None);

    // Sin apertura todavía: el período sigue empezando en el origen.
    assert_eq!(inicio_y_fondo(&db), ("0000-00-00 00:00:00".to_string(), 0.0));
    assert_eq!(piso_cadena(&db), None);
    assert_eq!(esperado(&db, "2026-01-05 07:00:00"), 2700.0);

    // Una apertura borrada o de la caja web no inicia la cadena de la tienda.
    db.execute(
        "INSERT INTO aperturas_caja (usuario_id, fondo_declarado, fecha, deleted_at) \
         VALUES (1, 777, '2026-01-01 08:00:00', '2026-01-01 09:00:00')",
        [],
    ).unwrap();
    db.execute(
        "INSERT INTO aperturas_caja (usuario_id, fondo_declarado, fecha, origen, caja) \
         VALUES (1, 888, '2026-01-02 08:00:00', 'web', 'web')",
        [],
    ).unwrap();
    assert_eq!(inicio_y_fondo(&db).0, "0000-00-00 00:00:00");

    abrir(&db, "2026-01-05 09:00:00", 2000.0, None).unwrap();
    assert_eq!(inicio_y_fondo(&db), ("2026-01-05 08:59:59".to_string(), 2000.0));
    assert_eq!(piso_cadena(&db).as_deref(), Some("2026-01-05 09:00:00"));

    // Reloj atrasado antes de la primera apertura: entra igual al período.
    let v0 = venta_efectivo(&db, "2026-01-05 08:30:00", 50.0);
    assert_eq!(fecha_y_registro(&db, "ventas", v0).1.as_deref(), Some("2026-01-05 09:00:00"));
    let v1 = venta_efectivo(&db, "2026-01-05 10:00:00", 300.0);
    assert_eq!(fecha_y_registro(&db, "ventas", v1).1, None);
    venta_web(&db, "2026-01-05 10:30:00", 200.0, "efectivo", "principal", None);

    let d = calcular_periodo(&db, "2026-01-05 20:00:00").unwrap();
    assert_eq!(d.fecha_inicio, "2026-01-05 08:59:59");
    assert_eq!(d.fondo_inicial, 2000.0);
    assert_eq!(d.total_ventas_efectivo, 550.0, "50 + 300 del escritorio + 200 espejo; nada histórico");
    assert_eq!(d.num_transacciones, 3);
    assert_eq!(d.total_retiros_efectivo, 0.0);
    assert!(d.movimientos.is_empty());
    assert_eq!(d.efectivo_esperado, 2550.0);
    assert!(movimientos_sin_corte(&db).unwrap().is_empty());
    assert_eq!(primer_dia_pendiente(&db, "2026-01-06").as_deref(), Some("2026-01-05"),
        "los días de la historia web no son días pendientes de esta caja");

    let c = corte(&db, "DIA", "2026-01-05 20:00:00", 2550.0, 2000.0);
    assert_eq!(c.diferencia, 0.0);
    assert_eq!(
        texto_opt(&db, &format!("SELECT fecha_inicio FROM cortes WHERE id = {}", c.id)).as_deref(),
        Some("2026-01-05 08:59:59")
    );
    assert_eq!(
        contar(&db, "SELECT COUNT(*) FROM movimientos_caja WHERE corte_id IS NULL AND origen = 'web'"), 1,
        "el retiro web histórico no se asigna al primer corte"
    );

    // La auditoría tampoco reporta la historia web como fuga de este cajón.
    let a = auditar(&db, 3650).unwrap();
    assert_eq!(a.resumen.ventas_sin_corte_count, 0, "{:?}", a.ventas_sin_corte.iter().map(|x| &x.fecha).collect::<Vec<_>>());
    assert_eq!(a.resumen.movimientos_huerfanos_count, 0);
    assert_eq!(a.resumen.dias_sin_cierre_count, 0);
    assert_eq!(a.resumen.impacto_total_estimado, 0.0);
}
