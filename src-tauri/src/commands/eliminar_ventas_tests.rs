// commands/eliminar_ventas_tests.rs — Pruebas de "Eliminar venta de prueba".
//
// Mismo arranque que caja_tests (esquema → migraciones → semilla, en memoria).
// Usuarios: 1 = Dueño (de caja_tests), 2 = Daniela Bravo ('Dani', con el
// uuid real: la migración 017 le da el permiso), 3 = Laura Bravo (rol dueño,
// sin permiso), 4 = Nancy (vendedora).
//
// La regla de caja se prueba con DOS bases gemelas: la misma operación del
// día con y sin la venta de prueba. Después de eliminarla, cada corte cerrado
// debe guardar exactamente lo que habría guardado si la venta nunca hubiera
// existido (con lo contado igual en las dos: el dinero de una prueba nunca
// entra al cajón).
//
// Correr con:  cargo test --lib eliminar_ventas_tests

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::db::migrations::aplicar_migraciones;

use super::auth::{login_password_core, login_pin_core};
use super::caja_tests::{contar, nueva_db, DUENO};
use super::cortes::{
    calcular_periodo, crear_apertura_core, crear_corte_core, crear_movimiento_core, NuevaApertura,
    NuevoCorte, NuevoMovimiento,
};
use super::devoluciones::{crear_devolucion_core, ItemDevolucion, NuevaDevolucion};
use super::eliminar_ventas::{aplicar_solicitud_web, eliminar_venta_core, ResultadoSolicitud, VentaEliminada};
use super::ventas::{anular_venta_core, crear_venta_core, ItemVenta, NuevaVenta};

// ─── Utilidades ───────────────────────────────────────────

const DANI: i64 = 2;
const LAURA: i64 = 3;
const NANCY: i64 = 4;
const PIN_DANI: &str = "1357";
const PIN_LAURA: &str = "2468";
const UUID_DANI: &str = "c088a77266563a58935e396a1cb55d27";
const AHORA: &str = "2026-10-10 18:00:00";

fn hash(s: &str) -> String {
    bcrypt::hash(s, 4).unwrap()
}

/// Base con los usuarios de la tienda. El permiso de Daniela lo pone la
/// migración 017 por su uuid (no a mano): se vuelve a correr desde la 16.
fn base() -> Connection {
    let db = nueva_db();
    db.execute(
        "INSERT INTO usuarios (id, nombre_completo, nombre_usuario, pin, password_hash, rol_id, activo, uuid) \
         VALUES (2, 'Daniela Bravo', 'Dani', ?1, ?2, 1, 1, ?3)",
        rusqlite::params![hash(PIN_DANI), hash("clave-dani"), UUID_DANI],
    ).unwrap();
    db.execute(
        "INSERT INTO usuarios (id, nombre_completo, nombre_usuario, pin, password_hash, rol_id, activo) \
         VALUES (3, 'Laura Bravo', 'Laura', ?1, ?2, 1, 1), (4, 'Nancy', 'nancy', ?3, ?3, 2, 1)",
        rusqlite::params![hash(PIN_LAURA), hash("clave-laura"), hash("9999")],
    ).unwrap();
    db.execute_batch("PRAGMA user_version = 16").unwrap();
    aplicar_migraciones(&db).unwrap();
    db
}

fn vender_como(
    db: &Connection,
    cuando: &str,
    usuario: i64,
    cantidad: f64,
    precio: f64,
    total: f64,
    descuento: f64,
    metodo: &str,
) -> i64 {
    let subtotal = precio * cantidad;
    crear_venta_core(db, NuevaVenta {
        usuario_id: usuario,
        cliente_id: None,
        subtotal,
        descuento,
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

fn efectivo(db: &Connection, cuando: &str, total: f64) -> i64 {
    vender_como(db, cuando, DUENO, 1.0, total, total, 0.0, "efectivo")
}

fn abrir(db: &Connection, cuando: &str, fondo: f64) {
    crear_apertura_core(db, NuevaApertura { usuario_id: DUENO, fondo_declarado: fondo, nota: None }, cuando).unwrap();
}

fn retiro_manual(db: &Connection, cuando: &str, monto: f64) {
    crear_movimiento_core(db, NuevoMovimiento {
        tipo: "RETIRO".into(),
        usuario_id: DUENO,
        monto,
        concepto: "Pago a proveedor".into(),
        autorizado_por: None,
        pin_autorizacion: None,
    }, cuando).unwrap();
}

/// Corte confirmado con lo contado (siempre con nota: la de prueba puede
/// tener diferencia).
fn cortar(db: &Connection, tipo: &str, cuando: &str, contado: f64, fondo: f64) -> i64 {
    let datos = calcular_periodo(db, cuando).unwrap();
    crear_corte_core(db, NuevoCorte {
        tipo: tipo.into(),
        usuario_id: DUENO,
        fecha_inicio: datos.fecha_inicio.clone(),
        fecha_fin: datos.fecha_fin.clone(),
        datos,
        efectivo_contado: contado,
        nota_diferencia: Some("nota de prueba".into()),
        fondo_siguiente: fondo,
        denominaciones: None,
        retiro: None,
    }, cuando).unwrap().id
}

fn partida(db: &Connection, venta_id: i64) -> i64 {
    db.query_row("SELECT id FROM venta_detalle WHERE venta_id = ?", [venta_id], |r| r.get(0)).unwrap()
}

fn devolver(db: &Connection, venta_id: i64, cantidad: f64, cuando: &str) {
    crear_devolucion_core(db, NuevaDevolucion {
        venta_id,
        usuario_id: DUENO,
        autorizado_por: None,
        motivo: "prueba".into(),
        items: vec![ItemDevolucion { venta_detalle_id: partida(db, venta_id), cantidad }],
        reembolso_efectivo: None,
    }, cuando).unwrap();
}

fn eliminar(db: &Connection, venta_id: i64) -> VentaEliminada {
    eliminar_venta_core(db, venta_id, DANI, PIN_DANI, AHORA).unwrap()
}

fn esperado(db: &Connection, cuando: &str) -> f64 {
    calcular_periodo(db, cuando).unwrap().efectivo_esperado
}

fn stock(db: &Connection) -> f64 {
    db.query_row("SELECT stock_actual FROM productos WHERE id = 1", [], |r| r.get(0)).unwrap()
}

fn uuid_de(db: &Connection, tabla: &str, id: i64) -> String {
    db.query_row(&format!("SELECT uuid FROM {tabla} WHERE id = ?"), [id], |r| r.get(0)).unwrap()
}

fn i64_de(db: &Connection, sql: &str) -> i64 {
    contar(db, sql)
}

fn bandera(db: &Connection) -> i64 {
    contar(db, "SELECT COUNT(*) FROM sync_suppress_flag")
}

fn purgas(db: &Connection) -> Vec<(String, String)> {
    let mut st = db.prepare("SELECT tabla, uuid FROM sync_purgas_pendientes ORDER BY id").unwrap();
    let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect();
    v
}

/// La cola de purgas sin los renglones de bitácora.
fn purgas_sin_bitacora(db: &Connection) -> Vec<(String, String)> {
    purgas(db).into_iter().filter(|(t, _)| t != "audit_log").collect()
}

/// uuids de bitácora en la cola de purgas.
fn purgas_de_bitacora(db: &Connection) -> Vec<String> {
    purgas(db).into_iter().filter(|(t, _)| t == "audit_log").map(|(_, u)| u).collect()
}

/// (ids, uuids) de los renglones de bitácora que apuntan a (tabla, id).
fn bitacora_de(db: &Connection, tabla: &str, id: i64) -> (Vec<i64>, Vec<String>) {
    let mut st = db
        .prepare("SELECT id, uuid FROM audit_log WHERE tabla_afectada = ?1 AND registro_id = ?2 ORDER BY id")
        .unwrap();
    let filas: Vec<(i64, String)> = st
        .query_map(rusqlite::params![tabla, id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    (filas.iter().map(|f| f.0).collect(), filas.into_iter().map(|f| f.1).collect())
}

/// (id, accion, descripción, datos_anteriores) de toda la bitácora.
fn bitacora(db: &Connection) -> Vec<(i64, String, String, Option<String>)> {
    let mut st = db
        .prepare("SELECT id, accion, descripcion_legible, datos_anteriores FROM audit_log ORDER BY id")
        .unwrap();
    let v = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    v
}

const CIFRAS: &[&str] = &[
    "fondo_inicial", "total_ventas_efectivo", "total_ventas_tarjeta", "total_ventas_transferencia",
    "total_ventas", "num_transacciones", "total_descuentos", "total_anulaciones",
    "total_entradas_efectivo", "total_retiros_efectivo", "efectivo_esperado", "efectivo_contado",
    "diferencia", "fondo_siguiente",
];

fn cifras(db: &Connection, corte_id: i64) -> Vec<f64> {
    db.query_row(
        &format!("SELECT {} FROM cortes WHERE id = ?", CIFRAS.join(", ")),
        [corte_id],
        |r| (0..CIFRAS.len()).map(|i| r.get::<_, f64>(i)).collect::<rusqlite::Result<Vec<f64>>>(),
    ).unwrap()
}

/// El corte `ca` de la base A guarda lo mismo que el corte `cb` de la gemela B.
fn corte_igual(a: &Connection, ca: i64, b: &Connection, cb: i64) {
    let (x, y) = (cifras(a, ca), cifras(b, cb));
    for (i, col) in CIFRAS.iter().enumerate() {
        assert!((x[i] - y[i]).abs() < 0.005, "corte {ca} vs {cb}, {col}: {} ≠ {}", x[i], y[i]);
    }
}

type Vendedor = (i64, i64, f64, String, String);

fn vendedores(db: &Connection, corte_id: i64) -> Vec<Vendedor> {
    let mut st = db
        .prepare(
            "SELECT usuario_id, num_ventas, total_vendido, hora_inicio, hora_fin FROM corte_vendedores \
             WHERE corte_id = ? ORDER BY usuario_id",
        )
        .unwrap();
    let v = st
        .query_map([corte_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    v
}

// ─── Permiso ──────────────────────────────────────────────

#[test]
fn solo_daniela_con_su_propio_pin_puede_eliminar() {
    let db = base();
    assert_eq!(
        (
            i64_de(&db, "SELECT COUNT(*) FROM usuarios WHERE puede_eliminar_ventas = 1"),
            i64_de(&db, "SELECT MIN(id) FROM usuarios WHERE puede_eliminar_ventas = 1"),
        ),
        (1, DANI),
        "la migración 017 da el permiso solo a Daniela (por uuid)"
    );
    abrir(&db, "2026-10-10 08:00:00", 2000.0);
    let v = efectivo(&db, "2026-10-10 09:00:00", 150.0);
    let bitacora_antes = bitacora(&db);

    let err = |u: i64, pin: &str| eliminar_venta_core(&db, v, u, pin, AHORA).unwrap_err();
    // Otros usuarios con rol dueño, aun con su PIN correcto: no.
    assert!(err(LAURA, PIN_LAURA).contains("permiso"), "{}", err(LAURA, PIN_LAURA));
    assert!(err(DUENO, PIN_DANI).contains("permiso"));
    assert!(err(NANCY, "9999").contains("permiso"));
    // Daniela con un PIN que no es el suyo.
    assert_eq!(err(DANI, "0000"), "PIN incorrecto");
    assert_eq!(err(DANI, PIN_LAURA), "PIN incorrecto");
    assert!(err(DANI, "12a4").contains("4 dígitos"));
    assert!(err(DANI, "").contains("4 dígitos"));
    // Daniela desactivada o borrada.
    db.execute("UPDATE usuarios SET activo = 0 WHERE id = 2", []).unwrap();
    assert!(err(DANI, PIN_DANI).contains("desactivado"));
    db.execute("UPDATE usuarios SET activo = 1, deleted_at = '2026-10-10 09:30:00' WHERE id = 2", []).unwrap();
    assert!(err(DANI, PIN_DANI).contains("desactivado"));
    db.execute("UPDATE usuarios SET deleted_at = NULL WHERE id = 2", []).unwrap();
    assert!(err(99, PIN_DANI).contains("no encontrado"));

    // Ninguna negativa tocó nada.
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM ventas WHERE id = {v}")), 1);
    assert_eq!(stock(&db), 999.0);
    assert_eq!(bitacora(&db), bitacora_antes);
    assert!(purgas(&db).is_empty());
    assert_eq!(bandera(&db), 0);

    let r = eliminar(&db, v);
    assert_eq!(r.folio, "V-000001");
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM ventas WHERE id = {v}")), 0);
    assert_eq!(
        eliminar_venta_core(&db, v, DANI, PIN_DANI, AHORA).unwrap_err(),
        "Venta no encontrada",
        "una venta ya eliminada no se vuelve a eliminar"
    );
}

#[test]
fn el_login_trae_el_permiso_solo_para_daniela() {
    let db = base();
    let por_pin = |pin: &str| login_pin_core(&db, pin).usuario.unwrap();
    assert!(por_pin(PIN_DANI).puede_eliminar_ventas);
    assert_eq!(por_pin(PIN_DANI).nombre_usuario, "Dani");
    assert!(!por_pin(PIN_LAURA).puede_eliminar_ventas);
    assert!(por_pin(PIN_LAURA).es_admin, "Laura es dueña pero sin el permiso individual");
    assert!(!por_pin("9999").puede_eliminar_ventas);
    let por_clave = |u: &str, c: &str| login_password_core(&db, u, c).usuario.unwrap();
    assert!(por_clave("Dani", "clave-dani").puede_eliminar_ventas);
    assert!(!por_clave("Laura", "clave-laura").puede_eliminar_ventas);
    // La sesión viaja a la pantalla con el campo.
    let json = serde_json::to_value(por_pin(PIN_DANI)).unwrap();
    assert_eq!(json["puede_eliminar_ventas"], Value::Bool(true));
}

// ─── Período abierto ──────────────────────────────────────

#[test]
fn venta_del_periodo_abierto_desaparece_y_regresa_el_inventario() {
    let db = base();
    abrir(&db, "2026-10-10 08:00:00", 2000.0);
    let v = vender_como(&db, "2026-10-10 10:00:00", DANI, 2.0, 250.0, 500.0, 0.0, "efectivo");
    let otra = efectivo(&db, "2026-10-10 11:00:00", 300.0);
    assert_eq!(stock(&db), 997.0);
    assert_eq!(esperado(&db, "2026-10-10 12:00:00"), 2800.0);
    let uuid_v = uuid_de(&db, "ventas", v);
    let bitacora_antes = bitacora(&db);
    let (ids_bitacora_v, uuids_bitacora_v) = bitacora_de(&db, "ventas", v);
    assert!(!ids_bitacora_v.is_empty(), "la venta dejó su renglón VENTA");
    db.execute("UPDATE sync_outbox SET synced_at = datetime('now') WHERE synced_at IS NULL", []).unwrap();

    let r = eliminar(&db, v);
    assert_eq!((r.folio.as_str(), r.total, r.piezas_regresadas), ("V-000001", 500.0, 2.0));
    assert_eq!(r.corte_ajustado_id, None, "período abierto: no hay corte que ajustar");
    assert!(r.cortes_ajustados.is_empty());
    assert!(!r.folio_reutilizable, "V-000002 ya existe");

    // Caja: el esperado baja exactamente lo que valía la venta.
    let d = calcular_periodo(&db, "2026-10-10 12:00:00").unwrap();
    assert_eq!(d.efectivo_esperado, 2300.0);
    assert_eq!((d.total_ventas, d.num_transacciones), (300.0, 1));
    assert!(d.vendedores.iter().all(|x| x.usuario_id != DANI));
    // Inventario y renglones.
    assert_eq!(stock(&db), 999.0);
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM ventas WHERE id = {v}")), 0);
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM venta_detalle WHERE venta_id = {v}")), 0);
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM ventas WHERE id = {otra}")), 1);
    // Cola de purgas para el servidor: la venta y sus renglones de bitácora.
    assert_eq!(purgas_sin_bitacora(&db), vec![("ventas".to_string(), uuid_v.clone())]);
    assert_eq!(purgas_de_bitacora(&db), uuids_bitacora_v);

    // Bitácora (pedido del dueño: una venta de prueba no deja rastro): se
    // borraron SOLO los renglones de esta venta y no se agregó ninguno.
    let esperada: Vec<_> = bitacora_antes.iter().filter(|f| !ids_bitacora_v.contains(&f.0)).cloned().collect();
    assert_eq!(bitacora(&db), esperada);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM audit_log WHERE accion = 'VENTA_ELIMINADA'"), 0);

    // Sync: ningún DELETE en el outbox; la existencia sí sube; nada de bitácora.
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_outbox WHERE operacion = 'DELETE'"), 0);
    assert_eq!(
        i64_de(&db, &format!("SELECT COUNT(*) FROM sync_outbox WHERE synced_at IS NULL AND tabla = 'ventas' AND uuid = '{uuid_v}'")),
        0
    );
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_outbox WHERE synced_at IS NULL AND tabla = 'productos'"), 1);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM sync_outbox WHERE synced_at IS NULL AND tabla = 'audit_log'"), 0);
    assert_eq!(
        db.query_row("SELECT updated_at FROM productos WHERE id = 1", [], |r| r.get::<_, String>(0)).unwrap(),
        AHORA,
        "updated_at del producto en hora local"
    );
    assert_eq!(bandera(&db), 0);
}

#[test]
fn venta_sin_subir_deja_el_outbox_limpio() {
    // Venta hecha sin internet y eliminada antes de subir: su renglón del
    // outbox ya no tiene nada que mandar (no se queda pendiente para siempre).
    let db = base();
    let v = efectivo(&db, "2026-10-10 09:00:00", 80.0);
    let u = uuid_de(&db, "ventas", v);
    assert_eq!(
        i64_de(&db, &format!("SELECT COUNT(*) FROM sync_outbox WHERE synced_at IS NULL AND tabla = 'ventas' AND uuid = '{u}'")),
        1
    );
    eliminar(&db, v);
    assert_eq!(
        i64_de(&db, &format!("SELECT COUNT(*) FROM sync_outbox WHERE synced_at IS NULL AND tabla = 'ventas' AND uuid = '{u}'")),
        0
    );
    assert_eq!(purgas_sin_bitacora(&db), vec![("ventas".to_string(), u)], "el servidor la borra si alcanzó a llegar");
    assert_eq!(
        i64_de(&db, "SELECT COUNT(*) FROM sync_outbox WHERE synced_at IS NULL AND tabla = 'audit_log'"),
        0,
        "su renglón de bitácora tampoco se queda pendiente de subir"
    );
}

#[test]
fn folios_se_reutilizan_solo_si_era_el_ultimo() {
    let db = base();
    let folio_seq = |db: &Connection| i64_de(db, "SELECT ultimo_valor FROM folio_secuencia WHERE id = 1");
    let dev_seq = |db: &Connection| i64_de(db, "SELECT ultimo_valor FROM devolucion_folio_secuencia WHERE id = 1");
    let a = efectivo(&db, "2026-10-10 09:00:00", 100.0); // V-000001
    let b = efectivo(&db, "2026-10-10 09:10:00", 100.0); // V-000002
    assert!(!eliminar(&db, a).folio_reutilizable);
    assert_eq!(folio_seq(&db), 2, "V-000001 no era el último: no se mueve");
    assert!(eliminar(&db, b).folio_reutilizable);
    assert_eq!(folio_seq(&db), 1);
    let c = efectivo(&db, "2026-10-10 09:20:00", 100.0);
    let folio_c: String = db.query_row("SELECT folio FROM ventas WHERE id = ?", [c], |r| r.get(0)).unwrap();
    assert_eq!(folio_c, "V-000002", "la siguiente venta reutiliza el número, sin hueco");

    // Devoluciones: D-000001 (de x) y D-000002, D-000003 (de y).
    let x = vender_como(&db, "2026-10-10 10:00:00", DUENO, 3.0, 10.0, 30.0, 0.0, "efectivo");
    let y = vender_como(&db, "2026-10-10 10:05:00", DUENO, 3.0, 10.0, 30.0, 0.0, "efectivo");
    devolver(&db, x, 1.0, "2026-10-10 10:10:00");
    devolver(&db, y, 1.0, "2026-10-10 10:11:00");
    devolver(&db, y, 1.0, "2026-10-10 10:12:00");
    assert_eq!(dev_seq(&db), 3);
    eliminar(&db, x);
    assert_eq!(dev_seq(&db), 3, "D-000001 no era la última");
    eliminar(&db, y);
    assert_eq!(dev_seq(&db), 1, "D-000003 y D-000002 eran las últimas seguidas");
}

// ─── Cortes cerrados (bases gemelas) ──────────────────────

#[test]
fn venta_en_un_corte_de_turno_cerrado_queda_como_si_no_hubiera_existido() {
    let escenario = |con_prueba: bool| {
        let db = base();
        abrir(&db, "2026-10-10 08:00:00", 2000.0);
        efectivo(&db, "2026-10-10 09:00:00", 400.0);
        let prueba = con_prueba.then(|| {
            vender_como(&db, "2026-10-10 10:00:00", DANI, 3.0, 100.0, 270.0, 30.0, "efectivo")
        });
        vender_como(&db, "2026-10-10 11:00:00", DUENO, 1.0, 300.0, 300.0, 0.0, "tarjeta");
        vender_como(&db, "2026-10-10 11:15:00", DUENO, 1.0, 120.0, 120.0, 0.0, "transferencia");
        retiro_manual(&db, "2026-10-10 11:30:00", 100.0);
        // Lo que de verdad hay en el cajón: la venta de prueba no metió dinero.
        let c = cortar(&db, "PARCIAL", "2026-10-10 14:00:00", 2300.0, 2300.0);
        efectivo(&db, "2026-10-10 15:00:00", 50.0);
        (db, prueba, c)
    };
    let (a, prueba, ca) = escenario(true);
    let (b, _, cb) = escenario(false);
    assert_eq!(cifras(&a, ca)[12], -270.0, "con la prueba el corte marcó faltante");

    let r = eliminar(&a, prueba.unwrap());
    assert_eq!(r.corte_ajustado_id, Some(ca));
    assert_eq!(r.cortes_ajustados, vec![ca]);
    corte_igual(&a, ca, &b, cb);
    assert_eq!(cifras(&a, ca)[12], 0.0, "la diferencia queda en cero");
    assert_eq!(esperado(&a, "2026-10-10 16:00:00"), esperado(&b, "2026-10-10 16:00:00"));
    assert_eq!(stock(&a), stock(&b));
    assert_eq!(bitacora_de(&a, "ventas", prueba.unwrap()).0.len(), 0, "sin rastro en la bitácora");
    assert_eq!(i64_de(&a, "SELECT COUNT(*) FROM audit_log WHERE accion = 'VENTA_ELIMINADA'"), 0);
    // El corte sube con sus cifras nuevas.
    let u = uuid_de(&a, "cortes", ca);
    assert_eq!(
        i64_de(&a, &format!("SELECT COUNT(*) FROM sync_outbox WHERE synced_at IS NULL AND tabla = 'cortes' AND uuid = '{u}'")),
        1
    );
}

#[test]
fn corte_del_dia_ajusta_el_renglon_de_cada_vendedor() {
    let escenario = |con_prueba: bool| {
        let db = base();
        abrir(&db, "2026-10-10 08:00:00", 2000.0);
        efectivo(&db, "2026-10-10 09:00:00", 200.0);
        let mut pruebas = Vec::new();
        if con_prueba {
            // Primera venta de Nancy: su hora de inicio cambia al quitarla.
            pruebas.push(vender_como(&db, "2026-10-10 10:00:00", NANCY, 1.0, 150.0, 150.0, 0.0, "efectivo"));
        }
        vender_como(&db, "2026-10-10 12:00:00", NANCY, 1.0, 80.0, 80.0, 0.0, "tarjeta");
        vender_como(&db, "2026-10-10 12:30:00", NANCY, 1.0, 40.0, 40.0, 0.0, "efectivo");
        if con_prueba {
            // Única venta de Daniela: su renglón desaparece.
            pruebas.push(vender_como(&db, "2026-10-10 13:00:00", DANI, 1.0, 60.0, 60.0, 0.0, "efectivo"));
        }
        let c = cortar(&db, "DIA", "2026-10-10 20:00:00", 2240.0, 2000.0);
        (db, pruebas, c)
    };
    let (a, pruebas, ca) = escenario(true);
    let (b, _, cb) = escenario(false);
    assert_eq!(vendedores(&a, ca).len(), 3);
    for p in pruebas {
        assert_eq!(eliminar(&a, p).corte_ajustado_id, Some(ca));
    }
    corte_igual(&a, ca, &b, cb);
    assert_eq!(vendedores(&a, ca), vendedores(&b, cb));
    assert_eq!(
        vendedores(&a, ca).iter().find(|v| v.0 == NANCY).map(|v| (v.1, v.2, v.3.clone())),
        Some((2, 120.0, "2026-10-10 12:00:00".to_string()))
    );
}

#[test]
fn venta_con_tarjeta_no_cambia_el_efectivo_esperado() {
    // Período abierto.
    let db = base();
    abrir(&db, "2026-10-10 08:00:00", 2000.0);
    efectivo(&db, "2026-10-10 09:00:00", 100.0);
    let t = vender_como(&db, "2026-10-10 10:00:00", DANI, 1.0, 300.0, 300.0, 0.0, "tarjeta");
    let antes = calcular_periodo(&db, "2026-10-10 12:00:00").unwrap();
    eliminar(&db, t);
    let despues = calcular_periodo(&db, "2026-10-10 12:00:00").unwrap();
    assert_eq!(despues.efectivo_esperado, antes.efectivo_esperado);
    assert_eq!((antes.total_ventas_tarjeta, despues.total_ventas_tarjeta), (300.0, 0.0));

    // Dentro de un corte cerrado: el esperado y la diferencia no se mueven.
    let escenario = |con_prueba: bool| {
        let db = base();
        abrir(&db, "2026-10-10 08:00:00", 2000.0);
        efectivo(&db, "2026-10-10 09:00:00", 100.0);
        let p = con_prueba.then(|| vender_como(&db, "2026-10-10 10:00:00", DANI, 1.0, 300.0, 300.0, 0.0, "tarjeta"));
        let c = cortar(&db, "PARCIAL", "2026-10-10 12:00:00", 2100.0, 2100.0);
        (db, p, c)
    };
    let (a, p, ca) = escenario(true);
    let (b, _, cb) = escenario(false);
    let antes = cifras(&a, ca);
    eliminar(&a, p.unwrap());
    let despues = cifras(&a, ca);
    assert_eq!((antes[10], antes[12]), (despues[10], despues[12]), "esperado y diferencia iguales");
    assert_eq!((antes[2], despues[2]), (300.0, 0.0));
    corte_igual(&a, ca, &b, cb);
}

#[test]
fn venta_anulada_despues_del_corte_quita_el_reembolso_y_ajusta_los_dos_cortes() {
    let escenario = |con_prueba: bool| {
        let db = base();
        abrir(&db, "2026-10-10 08:00:00", 2000.0);
        efectivo(&db, "2026-10-10 09:00:00", 100.0);
        let p = con_prueba.then(|| efectivo(&db, "2026-10-10 10:00:00", 500.0));
        let c1 = cortar(&db, "PARCIAL", "2026-10-10 12:00:00", 2100.0, 2100.0);
        if let Some(p) = p {
            // Ya cortada: la anulación registra el reembolso como retiro.
            anular_venta_core(&db, p, DUENO, "prueba".into(), "2026-10-10 13:00:00").unwrap();
        }
        let c2 = cortar(&db, "PARCIAL", "2026-10-10 15:00:00", 2100.0, 2100.0);
        (db, p, c1, c2)
    };
    let (a, p, a1, a2) = escenario(true);
    let (b, _, b1, b2) = escenario(false);
    let p = p.unwrap();
    let (mov, corte_mov): (i64, i64) = a.query_row(
        "SELECT id, corte_id FROM movimientos_caja WHERE concepto LIKE 'Reembolso por anulación de venta V-000002 %'",
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(corte_mov, a2, "el reembolso quedó en el segundo corte");
    let uuid_mov = uuid_de(&a, "movimientos_caja", mov);
    let uuid_p = uuid_de(&a, "ventas", p);
    assert_eq!(stock(&a), 999.0, "la anulación ya regresó la pieza");

    let r = eliminar(&a, p);
    assert!(r.anulada);
    assert_eq!(r.piezas_regresadas, 0.0, "anulada: el inventario no se toca otra vez");
    assert_eq!(stock(&a), stock(&b));
    assert_eq!(r.corte_ajustado_id, Some(a1));
    assert_eq!(r.cortes_ajustados, vec![a1, a2]);
    assert_eq!(r.movimientos_eliminados, 1);
    assert_eq!(i64_de(&a, &format!("SELECT COUNT(*) FROM movimientos_caja WHERE id = {mov}")), 0);
    assert_eq!(
        purgas_sin_bitacora(&a),
        vec![("ventas".to_string(), uuid_p), ("movimientos_caja".to_string(), uuid_mov)]
    );
    assert_eq!(purgas_de_bitacora(&a).len(), 2, "renglones VENTA y ANULACION");
    corte_igual(&a, a1, &b, b1);
    corte_igual(&a, a2, &b, b2);
    assert_eq!(esperado(&a, "2026-10-10 16:00:00"), esperado(&b, "2026-10-10 16:00:00"));
    assert_eq!(bitacora_de(&a, "ventas", p).0.len(), 0, "sin rastro en la bitácora");
}

#[test]
fn venta_anulada_en_el_periodo_abierto_y_en_el_abierto_actual() {
    // Anulada ANTES del corte: el corte la contó solo en anulaciones.
    let escenario = |con_prueba: bool| {
        let db = base();
        abrir(&db, "2026-10-10 08:00:00", 2000.0);
        efectivo(&db, "2026-10-10 09:00:00", 100.0);
        let p = con_prueba.then(|| efectivo(&db, "2026-10-10 10:00:00", 500.0));
        if let Some(p) = p {
            anular_venta_core(&db, p, DUENO, "prueba".into(), "2026-10-10 10:30:00").unwrap();
        }
        let c = cortar(&db, "PARCIAL", "2026-10-10 12:00:00", 2100.0, 2100.0);
        (db, p, c)
    };
    let (a, p, ca) = escenario(true);
    let (b, _, cb) = escenario(false);
    assert_eq!(cifras(&a, ca)[7], 500.0, "total_anulaciones");
    let r = eliminar(&a, p.unwrap());
    assert_eq!(r.movimientos_eliminados, 0, "anulada sin cortar: nunca hubo reembolso");
    corte_igual(&a, ca, &b, cb);
    assert_eq!(stock(&a), stock(&b));

    // Anulada y eliminada dentro del período abierto: nada que ajustar.
    let db = base();
    abrir(&db, "2026-10-10 08:00:00", 2000.0);
    let p = efectivo(&db, "2026-10-10 10:00:00", 500.0);
    anular_venta_core(&db, p, DUENO, "prueba".into(), "2026-10-10 10:30:00").unwrap();
    let antes = calcular_periodo(&db, "2026-10-10 12:00:00").unwrap();
    assert_eq!(antes.total_anulaciones, 500.0);
    let r = eliminar(&db, p);
    assert_eq!((r.corte_ajustado_id, r.piezas_regresadas), (None, 0.0));
    let d = calcular_periodo(&db, "2026-10-10 12:00:00").unwrap();
    assert_eq!((d.total_anulaciones, d.efectivo_esperado), (0.0, antes.efectivo_esperado));
    assert_eq!(stock(&db), 1000.0);
}

#[test]
fn venta_con_tarjeta_anulada_despues_del_corte() {
    // Sin reembolso en caja: la hora de la anulación (y lo que guardó el
    // corte) dice que el corte la contó viva.
    let escenario = |con_prueba: bool| {
        let db = base();
        abrir(&db, "2026-10-10 08:00:00", 2000.0);
        efectivo(&db, "2026-10-10 09:00:00", 100.0);
        let p = con_prueba.then(|| vender_como(&db, "2026-10-10 10:00:00", DANI, 1.0, 300.0, 300.0, 0.0, "tarjeta"));
        let c1 = cortar(&db, "PARCIAL", "2026-10-10 12:00:00", 2100.0, 2100.0);
        if let Some(p) = p {
            anular_venta_core(&db, p, DUENO, "prueba".into(), "2026-10-10 13:00:00").unwrap();
        }
        let c2 = cortar(&db, "PARCIAL", "2026-10-10 15:00:00", 2100.0, 2100.0);
        (db, p, c1, c2)
    };
    let (a, p, a1, a2) = escenario(true);
    let (b, _, b1, b2) = escenario(false);
    eliminar(&a, p.unwrap());
    corte_igual(&a, a1, &b, b1);
    corte_igual(&a, a2, &b, b2);
}

#[test]
fn venta_con_devoluciones_en_efectivo_borra_devoluciones_y_retiros() {
    let escenario = |con_prueba: bool| {
        let db = base();
        abrir(&db, "2026-10-10 08:00:00", 2000.0);
        efectivo(&db, "2026-10-10 09:00:00", 100.0);
        let p = con_prueba.then(|| vender_como(&db, "2026-10-10 10:00:00", DANI, 3.0, 100.0, 300.0, 0.0, "efectivo"));
        let c1 = cortar(&db, "PARCIAL", "2026-10-10 12:00:00", 2100.0, 2100.0);
        if let Some(p) = p {
            devolver(&db, p, 1.0, "2026-10-10 13:00:00"); // retiro que entra al corte 2
        }
        let c2 = cortar(&db, "PARCIAL", "2026-10-10 15:00:00", 2100.0, 2100.0);
        if let Some(p) = p {
            devolver(&db, p, 1.0, "2026-10-10 16:00:00"); // retiro del período abierto
        }
        (db, p, c1, c2)
    };
    let (a, p, a1, a2) = escenario(true);
    let (b, _, b1, b2) = escenario(false);
    let p = p.unwrap();
    assert_eq!(stock(&a), 998.0, "1000 − 3 vendidas + 2 devueltas − 1 de la otra venta");
    assert_eq!(i64_de(&a, "SELECT COUNT(*) FROM movimientos_caja WHERE concepto LIKE 'Devolución D-%'"), 2);
    let uuids_dev: Vec<String> = {
        let mut st = a.prepare("SELECT uuid FROM devoluciones ORDER BY id").unwrap();
        let v = st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        v
    };

    let r = eliminar(&a, p);
    assert_eq!(r.piezas_regresadas, 1.0, "3 vendidas − 2 ya devueltas: el inventario regresa una sola vez");
    assert_eq!(stock(&a), stock(&b));
    assert_eq!((r.devoluciones_eliminadas, r.movimientos_eliminados), (2, 2));
    assert_eq!(i64_de(&a, "SELECT COUNT(*) FROM devoluciones"), 0);
    assert_eq!(i64_de(&a, "SELECT COUNT(*) FROM devolucion_detalle"), 0);
    assert_eq!(i64_de(&a, "SELECT COUNT(*) FROM movimientos_caja WHERE concepto LIKE 'Devolución D-%'"), 0);
    let q = purgas_sin_bitacora(&a);
    assert_eq!(q.len(), 5, "venta + 2 devoluciones + 2 retiros: {q:?}");
    assert_eq!(bitacora_de(&a, "ventas", p).0.len(), 0, "sin rastro en la bitácora");
    assert_eq!(i64_de(&a, "SELECT COUNT(*) FROM audit_log WHERE tabla_afectada = 'devoluciones'"), 0);
    for u in &uuids_dev {
        assert!(q.contains(&("devoluciones".to_string(), u.clone())));
    }
    assert_eq!(i64_de(&a, "SELECT ultimo_valor FROM devolucion_folio_secuencia"), 0);
    // La venta la contó el corte 1; el primer reembolso, el corte 2; el
    // segundo reembolso sigue en el período abierto (borrarlo basta).
    assert_eq!(r.corte_ajustado_id, Some(a1));
    assert_eq!(r.cortes_ajustados, vec![a1, a2]);
    corte_igual(&a, a1, &b, b1);
    corte_igual(&a, a2, &b, b2);
    assert_eq!(esperado(&a, "2026-10-10 17:00:00"), esperado(&b, "2026-10-10 17:00:00"));
    assert_eq!(bandera(&a), 0);
}

// ─── Rechazos y otros enlaces ─────────────────────────────

#[test]
fn rechazos_no_cambian_nada() {
    let db = base();
    let w = db.execute(
        "INSERT INTO ventas (folio, usuario_id, total, metodo_pago, fecha, caja, origen, uuid) \
         VALUES ('W-1', 1, 90, 'efectivo', '2026-10-10 09:00:00', 'web', 'web', '019dd079-a8fb-7bd0-88ab-000000000001')",
        [],
    ).map(|_| db.last_insert_rowid()).unwrap();
    let antes = bitacora(&db);
    assert_eq!(eliminar_venta_core(&db, 4040, DANI, PIN_DANI, AHORA).unwrap_err(), "Venta no encontrada");
    assert!(eliminar_venta_core(&db, w, DANI, PIN_DANI, AHORA).unwrap_err().contains("caja web"));
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM ventas WHERE id = {w}")), 1);
    assert_eq!(bitacora(&db), antes);
    assert!(purgas(&db).is_empty());
    assert_eq!(bandera(&db), 0);
}

#[test]
fn presupuesto_convertido_vuelve_a_pendiente() {
    let db = base();
    db.execute(
        "INSERT INTO presupuestos (id, folio, usuario_id, estado, total) VALUES (7, 'P-000007', 1, 'pendiente', 100)",
        [],
    ).unwrap();
    let v = crear_venta_core(&db, NuevaVenta {
        usuario_id: DUENO, cliente_id: None, subtotal: 100.0, descuento: 0.0, total: 100.0,
        metodo_pago: "efectivo".into(), monto_recibido: 100.0, cambio: 0.0,
        items: vec![ItemVenta {
            producto_id: 1, cantidad: 1.0, precio_original: 100.0, descuento_porcentaje: 0.0,
            descuento_monto: 0.0, precio_final: 100.0, subtotal: 100.0, autorizado_por: None,
        }],
        presupuesto_origen_id: Some(7),
    }, "2026-10-10 09:00:00").unwrap().id;
    let estado = |db: &Connection| -> (String, Option<i64>) {
        db.query_row("SELECT estado, venta_id FROM presupuestos WHERE id = 7", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
    };
    assert_eq!(estado(&db), ("convertido".to_string(), Some(v)));
    eliminar(&db, v);
    assert_eq!(estado(&db), ("pendiente".to_string(), None));
}

#[test]
fn migracion_017_es_idempotente() {
    let db = base(); // ya la corrió dos veces (inicial y desde la 16)
    db.execute("INSERT INTO sync_purgas_pendientes (tabla, uuid) VALUES ('ventas', 'abc')", []).unwrap();
    db.execute("UPDATE sync_outbox SET synced_at = datetime('now') WHERE synced_at IS NULL", []).unwrap();
    for _ in 0..2 {
        db.execute_batch("PRAGMA user_version = 16").unwrap();
        aplicar_migraciones(&db).unwrap();
    }
    assert_eq!(i64_de(&db, "PRAGMA user_version"), 17);
    assert_eq!(i64_de(&db, "SELECT COUNT(*) FROM usuarios WHERE puede_eliminar_ventas = 1"), 1);
    assert_eq!(i64_de(&db, "SELECT puede_eliminar_ventas FROM usuarios WHERE id = 2"), 1);
    assert_eq!(purgas(&db), vec![("ventas".to_string(), "abc".to_string())], "la cola no se borra");
    assert_eq!(
        i64_de(&db, "SELECT COUNT(*) FROM sync_outbox WHERE synced_at IS NULL AND tabla = 'usuarios'"),
        0,
        "el permiso no se empuja al servidor"
    );
    assert_eq!(bandera(&db), 0);
    let col: Option<i64> = db
        .query_row(
            "SELECT 1 FROM pragma_table_info('usuarios') WHERE name = 'puede_eliminar_ventas'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(col, Some(1));
}


// ─── Eliminación pedida desde la web ──────────────────────

#[test]
fn solicitud_de_la_web_la_aplica_el_escritorio_completa() {
    let db = base();
    abrir(&db, "2026-10-10 08:00:00", 2000.0);
    let v = efectivo(&db, "2026-10-10 10:00:00", 500.0);
    let uv = uuid_de(&db, "ventas", v);
    assert_eq!(esperado(&db, "2026-10-10 12:00:00"), 2500.0);

    // Quien la pidió debe tener el permiso individual AQUÍ.
    let uuid_laura: String = db.query_row("SELECT uuid FROM usuarios WHERE id = ?", [LAURA], |r| r.get(0)).unwrap();
    match aplicar_solicitud_web(&db, &uv, &uuid_laura, AHORA) {
        ResultadoSolicitud::Rechazada(m) => assert!(m.contains("permiso"), "{m}"),
        otro => panic!("debía rechazarse: {otro:?}"),
    }
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM ventas WHERE id = {v}")), 1);

    // Daniela: se aplica igual que desde el escritorio (sin pedir PIN aquí:
    // el servidor ya lo verificó).
    match aplicar_solicitud_web(&db, &uv, UUID_DANI, AHORA) {
        ResultadoSolicitud::Aplicada(Some(r)) => assert_eq!(r.piezas_regresadas, 1.0),
        otro => panic!("debía aplicarse: {otro:?}"),
    }
    assert_eq!(i64_de(&db, &format!("SELECT COUNT(*) FROM ventas WHERE id = {v}")), 0);
    assert_eq!(esperado(&db, "2026-10-10 12:00:00"), 2000.0);
    assert_eq!(purgas_sin_bitacora(&db), vec![("ventas".to_string(), uv.clone())]);

    // Repetida (la confirmación no llegó): ya se eliminó aquí → aplicada.
    assert!(matches!(aplicar_solicitud_web(&db, &uv, UUID_DANI, AHORA), ResultadoSolicitud::Aplicada(None)));
    // Una venta que todavía no baja a este equipo: se reintenta después.
    assert!(matches!(
        aplicar_solicitud_web(&db, "aun-no-llega-0000000000000000000", UUID_DANI, AHORA),
        ResultadoSolicitud::Pendiente(_)
    ));
}
