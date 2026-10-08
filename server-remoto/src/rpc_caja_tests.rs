// rpc_caja_tests.rs — Pruebas del motor de caja del servidor (caja.rs) y de
// sus envolturas en rpc.rs.
//
// Dos tipos de prueba:
//   - ESCENARIOS de la caja 'web' con reloj fijo (puertos de los escenarios
//     de src-tauri/src/commands/caja_tests.rs). Corren dentro de UNA
//     transacción que se revierte al final: toman el candado de la caja web,
//     borran (solo dentro de esa transacción) lo que la caja web ya tenía y
//     recorren días completos con fechas explícitas. No dejan nada guardado.
//   - RPC reales (reloj real) contra la copia de producción: caja de la
//     tienda de solo lectura, vista previa contra un cálculo a mano, cliente
//     viejo sin header, concurrencia venta/corte, anulación y devolución.
//     Las que escriben en la caja web se serializan con `candado_caja_web()`
//     (la caja web es UNA cadena compartida por todas las pruebas).
//
// Se SALTAN si no hay TEST_DATABASE_URL. Deben correr contra una COPIA local:
//   psql -h localhost -p 54329 -U postgres -d postgres \
//        -c "CREATE DATABASE wp5b_test TEMPLATE prodcopy_base"
//   TEST_DATABASE_URL=postgres://postgres@localhost:54329/wp5b_test cargo test

use super::*;
use crate::caja::{
    AperturaCaja, CorteCreado, DatosCorteCliente, NuevaApertura, NuevoCorte, RetiroCorte,
};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue};
use axum::Json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgConnection;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

const SECRETO: &[u8] = b"secreto-pruebas-wp5b";
const PIN_DUENO: &str = "5824";
const PIN_VENDEDOR: &str = "6193";

static PINES_VERIFICADOS: AtomicBool = AtomicBool::new(false);

/// Serializa las pruebas que escriben en la caja 'web' con el reloj real
/// (también la usa rpc_precios_tests). La caja web es una sola cadena: dos
/// pruebas cortándola al mismo tiempo se cambiarían los números.
pub(super) fn candado_caja_web() -> &'static tokio::sync::Mutex<()> {
    static M: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    M.get_or_init(|| tokio::sync::Mutex::new(()))
}

struct Ctx {
    st: AppState,
    dueno: i64,
    vendedor: i64,
    tok_dueno: String,
    tok_vendedor: String,
}

fn token(id: i64, nombre_usuario: &str, admin: bool, device: Option<&str>) -> String {
    crate::auth::emitir_token_con_device(
        SECRETO,
        id,
        nombre_usuario,
        if admin { "admin" } else { "device" },
        1,
        device.map(|d| d.to_string()),
        chrono::Duration::hours(1),
    )
    .unwrap()
}

async fn usuario_prueba(
    conn: &mut PgConnection,
    nombre_usuario: &str,
    nombre: &str,
    es_admin: bool,
    pin: &str,
) -> i64 {
    let rol_id: i64 = sqlx::query_scalar("SELECT id FROM roles WHERE es_admin = $1 ORDER BY id LIMIT 1")
        .bind(if es_admin { 1 } else { 0 })
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    let existente = sqlx::query("SELECT id, pin FROM usuarios WHERE nombre_usuario = $1 ORDER BY id LIMIT 1")
        .bind(nombre_usuario)
        .fetch_optional(&mut *conn)
        .await
        .unwrap();
    let hash_pin = bcrypt::hash(pin, 4).unwrap();
    match existente {
        Some(r) => {
            let id: i64 = r.get("id");
            let pin_ok = bcrypt::verify(pin, &r.get::<String, _>("pin")).unwrap_or(false);
            sqlx::query(
                "UPDATE usuarios SET rol_id = $1, activo = 1, deleted_at = NULL, \
                 pin = CASE WHEN $2 THEN pin ELSE $3 END WHERE id = $4",
            )
            .bind(rol_id)
            .bind(pin_ok)
            .bind(&hash_pin)
            .bind(id)
            .execute(&mut *conn)
            .await
            .unwrap();
            id
        }
        None => sqlx::query_scalar(
            "INSERT INTO usuarios (uuid, nombre_completo, nombre_usuario, pin, password_hash, rol_id, activo) \
             VALUES ($1, $2, $3, $4, $5, $6, 1) RETURNING id",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(nombre)
        .bind(nombre_usuario)
        .bind(&hash_pin)
        .bind(bcrypt::hash("no-usar", 4).unwrap())
        .bind(rol_id)
        .fetch_one(&mut *conn)
        .await
        .unwrap(),
    }
}

async fn ctx() -> Option<Ctx> {
    let url = match std::env::var("TEST_DATABASE_URL") {
        Ok(u) if !u.trim().is_empty() => u,
        _ => {
            eprintln!("TEST_DATABASE_URL no definido: se omite la prueba de integración");
            return None;
        }
    };
    assert!(
        url.contains("@localhost") || url.contains("@127.0.0.1"),
        "TEST_DATABASE_URL debe ser una copia LOCAL (nunca producción)"
    );
    let pool = PgPoolOptions::new().max_connections(30).connect(&url).await.unwrap();
    if let Err(e) = crate::db::run_migrations(&pool).await {
        eprintln!("aviso: no se aplicaron las migraciones en la base de prueba: {e}");
    }

    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(7301199)").execute(&mut *tx).await.unwrap();
    let dueno = usuario_prueba(&mut tx, "wp5b_dueno", "Dueño de prueba WP5b", true, PIN_DUENO).await;
    let vendedor =
        usuario_prueba(&mut tx, "wp5b_vendedor", "Vendedor de prueba WP5b", false, PIN_VENDEDOR).await;
    if !PINES_VERIFICADOS.load(Ordering::SeqCst) {
        // Ningún OTRO usuario activo puede tener estos PINs: el login por PIN
        // y la búsqueda del dueño por PIN deben resolver a estos usuarios.
        let otros = sqlx::query(
            "SELECT id, pin FROM usuarios WHERE activo = 1 AND deleted_at IS NULL AND id <> $1 AND id <> $2",
        )
        .bind(dueno)
        .bind(vendedor)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
        for r in &otros {
            let h: String = r.get("pin");
            for pin in [PIN_DUENO, PIN_VENDEDOR] {
                assert!(
                    !bcrypt::verify(pin, &h).unwrap_or(false),
                    "el PIN de prueba {pin} coincide con el del usuario id={}; cambia las constantes",
                    r.get::<i64, _>("id")
                );
            }
        }
        PINES_VERIFICADOS.store(true, Ordering::SeqCst);
    }
    tx.commit().await.unwrap();

    let st = AppState { pool, jwt_secret: SECRETO.to_vec() };
    Some(Ctx {
        st,
        dueno,
        vendedor,
        tok_dueno: token(dueno, "wp5b_dueno", true, None),
        tok_vendedor: token(vendedor, "wp5b_vendedor", false, None),
    })
}

/// Dispositivo web nuevo en el modo indicado.
async fn dispositivo(c: &Ctx, modo: &str) -> String {
    let dev = format!("wp5b-{}-{}", modo, uuid::Uuid::now_v7());
    sqlx::query("INSERT INTO pos_devices (device_uuid, sucursal_id, nombre, modo_caja) VALUES ($1, 1, 'prueba WP5b', $2)")
        .bind(&dev)
        .bind(modo)
        .execute(&c.st.pool)
        .await
        .unwrap();
    dev
}

/// Llama al dispatcher como el navegador. `version` = header x-pos-cliente.
async fn rpc_cliente(
    st: &AppState,
    tok: &str,
    cmd: &str,
    args: Value,
    version: Option<&str>,
) -> Result<Value, ApiError> {
    let mut h = HeaderMap::new();
    h.insert("authorization", HeaderValue::from_str(&format!("Bearer {tok}")).unwrap());
    if let Some(v) = version {
        h.insert("x-pos-cliente", HeaderValue::from_str(v).unwrap());
    }
    dispatch(State(st.clone()), h, Path(cmd.to_string()), Json(args))
        .await
        .map(|Json(v)| v)
}

async fn rpc_con(st: &AppState, tok: &str, cmd: &str, args: Value) -> Result<Value, ApiError> {
    rpc_cliente(st, tok, cmd, args, Some("2")).await
}

async fn rpc(c: &Ctx, tok: &str, cmd: &str, args: Value) -> Result<Value, ApiError> {
    rpc_con(&c.st, tok, cmd, args).await
}

fn msg(e: ApiError) -> String {
    match e {
        ApiError::BadRequest(m) => m,
        otro => format!("{otro:?}"),
    }
}

async fn producto_en(conn: &mut PgConnection, precio: f64, stock: f64) -> i64 {
    let u = uuid::Uuid::now_v7().to_string();
    sqlx::query_scalar(
        "INSERT INTO productos (uuid, codigo, nombre, precio_costo, precio_venta, stock_actual) \
         VALUES ($1, $2, $3, 1, $4, $5) RETURNING id",
    )
    .bind(&u)
    .bind(format!("WP5B-{}", &u[u.len() - 12..]))
    .bind(format!("Producto prueba WP5b {}", &u[u.len() - 6..]))
    .bind(precio)
    .bind(stock)
    .fetch_one(&mut *conn)
    .await
    .unwrap()
}

async fn producto(c: &Ctx, precio: f64, stock: f64) -> i64 {
    let mut conn = c.st.pool.acquire().await.unwrap();
    producto_en(&mut conn, precio, stock).await
}

/// Partida calculada exactamente como ventaStore.ts.
fn linea(producto_id: i64, precio: f64, cant: f64) -> Value {
    json!({
        "producto_id": producto_id, "cantidad": cant, "precio_original": precio,
        "descuento_porcentaje": 0.0, "descuento_monto": 0.0, "precio_final": precio,
        "subtotal": precio * cant,
    })
}

fn venta(usuario_id: i64, lineas: Vec<Value>, metodo: &str) -> Value {
    let raw: f64 = lineas.iter().map(|l| l["subtotal"].as_f64().unwrap()).sum();
    let total = crate::venta_calc::total_a_cobrar(raw);
    json!({ "venta": {
        "usuario_id": usuario_id, "cliente_id": null,
        "subtotal": raw, "descuento": 0.0, "total": total,
        "metodo_pago": metodo, "monto_recibido": total, "cambio": 0.0,
        "items": lineas,
    }})
}

async fn escalar_i64(conn: &mut PgConnection, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).fetch_one(&mut *conn).await.unwrap()
}

// ─── Escenarios con reloj fijo (transacción que se revierte) ──────────────

/// Abre la transacción del escenario: candado de la caja web y caja web
/// vacía (solo dentro de esta transacción). Devuelve también un producto.
///
/// Los escenarios usan fechas fijas (octubre 2026) que pueden ser anteriores
/// a la marca 'caja_v2_desde' de la base de prueba; dentro de la transacción
/// se quita la marca (servidor sin frontera) para que sus aperturas cuenten.
/// El escenario de la frontera la vuelve a poner con `marcar_caja_v2`.
async fn escenario(c: &Ctx) -> (sqlx::Transaction<'static, sqlx::Postgres>, i64) {
    let mut tx = c.st.pool.begin().await.unwrap();
    caja::bloquear(&mut tx, Caja::Web).await.unwrap();
    for sql in [
        "DELETE FROM corte_denominaciones WHERE corte_id IN (SELECT id FROM cortes WHERE caja = 'web')",
        "DELETE FROM corte_vendedores WHERE corte_id IN (SELECT id FROM cortes WHERE caja = 'web')",
        "DELETE FROM cortes WHERE caja = 'web'",
        "DELETE FROM movimientos_caja WHERE caja = 'web'",
        "DELETE FROM aperturas_caja WHERE caja = 'web'",
        "DELETE FROM venta_detalle WHERE venta_id IN (SELECT id FROM ventas WHERE caja = 'web')",
        "DELETE FROM ventas WHERE caja = 'web'",
        "DELETE FROM sync_meta WHERE clave = 'caja_v2_desde'",
    ] {
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
    }
    let p = producto_en(&mut tx, 100.0, 1000.0).await;
    (tx, p)
}

/// Venta de la caja web insertada directo (como la dejaría crear_venta), con
/// una partida. Devuelve (venta_id, venta_detalle_id).
#[allow(clippy::too_many_arguments)]
async fn venta_en(
    conn: &mut PgConnection,
    c: &Ctx,
    p: i64,
    caja_venta: &str,
    fecha: &str,
    cantidad: f64,
    precio: f64,
    total: f64,
    metodo: &str,
) -> (i64, i64) {
    let u = uuid::Uuid::now_v7().to_string();
    let vid: i64 = sqlx::query_scalar(
        "INSERT INTO ventas (uuid, sucursal_id, folio, usuario_id, subtotal, total, metodo_pago, \
                             monto_recibido, origen, caja, fecha, updated_at) \
         VALUES ($1, 1, $2, $3, $4, $5, $6, $5, 'web', $7, $8, $8) RETURNING id",
    )
    .bind(&u)
    .bind(format!("V-WP5B-{}", &u[u.len() - 12..]))
    .bind(c.dueno)
    .bind(precio * cantidad)
    .bind(total)
    .bind(metodo)
    .bind(caja_venta)
    .bind(fecha)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let vd: i64 = sqlx::query_scalar(
        "INSERT INTO venta_detalle (uuid, venta_id, producto_id, cantidad, precio_original, \
                                    precio_final, subtotal) \
         VALUES ($1, $2, $3, $4, $5, $5, $6) RETURNING id",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(vid)
    .bind(p)
    .bind(cantidad)
    .bind(precio)
    .bind(precio * cantidad)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    (vid, vd)
}

async fn venta_web(conn: &mut PgConnection, c: &Ctx, p: i64, fecha: &str, total: f64, metodo: &str) -> (i64, i64) {
    venta_en(conn, c, p, "web", fecha, 1.0, total, total, metodo).await
}

async fn movimiento(conn: &mut PgConnection, c: &Ctx, fecha: &str, tipo: &str, monto: f64) -> i64 {
    caja::insertar_movimiento(conn, Caja::Web, tipo, c.dueno, monto, "prueba", None, None, fecha)
        .await
        .unwrap()
        .0
}

async fn abrir(
    conn: &mut PgConnection,
    c: &Ctx,
    cuando: &str,
    contado: f64,
    nota: Option<&str>,
) -> Result<AperturaCaja, String> {
    caja::crear_apertura_core(
        conn,
        Caja::Web,
        c.dueno,
        NuevaApertura { usuario_id: None, fondo_declarado: contado, nota: nota.map(String::from) },
        cuando,
    )
    .await
    .map_err(msg)
}

/// Simula el modal: calcula en `abierto`, el cajero cuenta, confirma en `confirma`.
#[allow(clippy::too_many_arguments)]
async fn corte_con(
    conn: &mut PgConnection,
    usuario: i64,
    tipo: &str,
    abierto: &str,
    confirma: &str,
    contado: f64,
    fondo_siguiente: f64,
    retiro: Option<(f64, Option<&str>)>,
) -> Result<CorteCreado, String> {
    let calc = caja::calcular_periodo(conn, Caja::Web, abierto).await.map_err(msg)?;
    caja::crear_corte_core(
        conn,
        Caja::Web,
        usuario,
        NuevoCorte {
            tipo: tipo.to_string(),
            usuario_id: None,
            fecha_inicio: None,
            fecha_fin: None,
            datos: DatosCorteCliente { efectivo_esperado: calc.efectivo_esperado },
            efectivo_contado: contado,
            nota_diferencia: Some("nota de prueba".into()),
            fondo_siguiente,
            denominaciones: None,
            retiro: retiro.map(|(m, pin)| RetiroCorte {
                monto: m,
                concepto: "Retiro por cambio de turno".into(),
                pin_autorizacion: pin.map(String::from),
            }),
        },
        confirma,
    )
    .await
    .map_err(msg)
}

async fn corte(conn: &mut PgConnection, c: &Ctx, tipo: &str, cuando: &str, contado: f64, fondo: f64) -> CorteCreado {
    corte_con(conn, c.dueno, tipo, cuando, cuando, contado, fondo, None).await.unwrap()
}

async fn esperado(conn: &mut PgConnection, cuando: &str) -> f64 {
    caja::calcular_periodo(conn, Caja::Web, cuando).await.unwrap().efectivo_esperado
}

async fn esperado_apertura(conn: &mut PgConnection, cuando: &str) -> f64 {
    caja::calcular_esperado_apertura(conn, Caja::Web, cuando).await.unwrap().esperado
}

#[tokio::test]
async fn dia_normal_cuadra() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    venta_web(&mut tx, &c, p, "2026-10-01 09:00:00", 500.0, "efectivo").await;
    venta_web(&mut tx, &c, p, "2026-10-01 10:00:00", 300.0, "tarjeta").await;
    movimiento(&mut tx, &c, "2026-10-01 11:00:00", "RETIRO", 100.0).await;
    movimiento(&mut tx, &c, "2026-10-01 12:00:00", "ENTRADA", 50.0).await;

    let datos = caja::calcular_periodo(&mut tx, Caja::Web, "2026-10-01 20:00:00").await.unwrap();
    assert_eq!(datos.fecha_inicio, caja::INICIO_SIN_CORTES);
    assert_eq!(datos.fondo_inicial, 2000.0);
    assert_eq!((datos.total_ventas_efectivo, datos.total_ventas_tarjeta), (500.0, 300.0));
    assert_eq!((datos.num_transacciones, datos.total_ventas), (2, 800.0));
    assert_eq!(datos.movimientos.len(), 2);
    assert_eq!(datos.vendedores.len(), 1);

    let k = corte(&mut tx, &c, "DIA", "2026-10-01 20:00:00", 2450.0, 2000.0).await;
    assert_eq!(k.efectivo_esperado, 2450.0, "2000 + 500 + 50 − 100");
    assert_eq!(k.diferencia, 0.0);
    assert_eq!(k.created_at, "2026-10-01 20:00:00");

    let fila = sqlx::query("SELECT caja, origen, usuario_id, fecha_fin FROM cortes WHERE id = $1")
        .bind(k.id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(fila.get::<String, _>("caja"), "web");
    assert_eq!(fila.get::<String, _>("origen"), "web");
    assert_eq!(fila.get::<i64, _>("usuario_id"), c.dueno);
    assert_eq!(
        escalar_i64(&mut tx, &format!("SELECT COUNT(*)::bigint FROM movimientos_caja WHERE corte_id = {}", k.id)).await,
        2,
        "los movimientos que se contaron quedan ligados al corte"
    );
    assert_eq!(
        escalar_i64(&mut tx, &format!("SELECT COUNT(*)::bigint FROM corte_vendedores WHERE corte_id = {}", k.id)).await,
        1
    );
    let det = caja::detalle_corte(&mut tx, k.id).await.unwrap();
    assert_eq!(det.corte.fondo_inicial, 2000.0);
    assert_eq!(det.movimientos.len(), 2);
    assert_eq!(det.vendedores[0].num_ventas, 2);
}

#[tokio::test]
async fn dia_siguiente_arranca_con_el_fondo() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    venta_web(&mut tx, &c, p, "2026-10-01 10:00:00", 1000.0, "efectivo").await;
    corte(&mut tx, &c, "DIA", "2026-10-01 20:00:00", 3000.0, 2000.0).await;

    assert_eq!(esperado_apertura(&mut tx, "2026-10-02 08:00:00").await, 2000.0);
    abrir(&mut tx, &c, "2026-10-02 08:00:00", 2000.0, None).await.unwrap();
    venta_web(&mut tx, &c, p, "2026-10-02 10:00:00", 700.0, "efectivo").await;

    let k = corte(&mut tx, &c, "DIA", "2026-10-02 20:00:00", 2700.0, 2000.0).await;
    assert_eq!(k.efectivo_esperado, 2700.0);
    assert_eq!(k.diferencia, 0.0);
    assert_eq!(caja::listar_cortes(&mut tx, Caja::Web, 50).await.unwrap()[0].id, k.id);
    assert_eq!(caja::ultimo_corte(&mut tx, Caja::Web).await.unwrap().unwrap().fin, "2026-10-02 20:00:00");
}

#[tokio::test]
async fn venta_despues_del_cierre_entra_al_dia_siguiente() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    venta_web(&mut tx, &c, p, "2026-10-01 10:00:00", 500.0, "efectivo").await;
    corte(&mut tx, &c, "DIA", "2026-10-01 18:00:00", 2500.0, 2000.0).await;

    venta_web(&mut tx, &c, p, "2026-10-01 19:00:00", 200.0, "efectivo").await; // ya cerrado

    assert_eq!(esperado_apertura(&mut tx, "2026-10-02 08:00:00").await, 2200.0, "fondo 2000 + la venta tardía");
    abrir(&mut tx, &c, "2026-10-02 08:00:00", 2200.0, None).await.unwrap();
    let k = corte(&mut tx, &c, "DIA", "2026-10-02 20:00:00", 2200.0, 2000.0).await;
    assert_eq!(k.efectivo_esperado, 2200.0);
    assert_eq!(k.diferencia, 0.0);
}

#[tokio::test]
async fn dia_sin_cerrar_no_pierde_ventas_y_la_apertura_documenta_el_faltante() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    venta_web(&mut tx, &c, p, "2026-10-01 10:00:00", 1000.0, "efectivo").await;
    corte(&mut tx, &c, "DIA", "2026-10-01 20:00:00", 3000.0, 2000.0).await;

    // Día 2: se vende pero NO se cierra.
    abrir(&mut tx, &c, "2026-10-02 08:00:00", 2000.0, None).await.unwrap();
    venta_web(&mut tx, &c, p, "2026-10-02 10:00:00", 800.0, "efectivo").await;

    assert_eq!(
        caja::primer_dia_pendiente(&mut tx, Caja::Web, "2026-10-03").await.unwrap().as_deref(),
        Some("2026-10-02")
    );

    // Alguien se llevó los 800 del día 2. Al abrir hay 2000, no 2800.
    assert_eq!(esperado_apertura(&mut tx, "2026-10-03 08:00:00").await, 2800.0);
    let sin_nota = abrir(&mut tx, &c, "2026-10-03 08:00:00", 2000.0, None).await;
    assert!(sin_nota.unwrap_err().contains("Escribe una nota"), "con diferencia la nota es obligatoria");
    abrir(&mut tx, &c, "2026-10-03 08:00:00", 2000.0, Some("El efectivo del día 2 lo guardó el dueño"))
        .await
        .unwrap();
    assert_eq!(
        escalar_i64(
            &mut tx,
            "SELECT COUNT(*)::bigint FROM movimientos_caja WHERE caja = 'web' AND tipo = 'RETIRO' \
             AND monto = 800 AND concepto LIKE 'Ajuste de apertura%'"
        )
        .await,
        1,
        "el faltante queda registrado como ajuste explícito"
    );
    let otra = abrir(&mut tx, &c, "2026-10-03 09:00:00", 2000.0, Some("x")).await;
    assert_eq!(otra.unwrap_err(), "Ya existe una apertura de caja para hoy");

    venta_web(&mut tx, &c, p, "2026-10-03 10:00:00", 500.0, "efectivo").await;
    let k = corte(&mut tx, &c, "DIA", "2026-10-03 20:00:00", 2500.0, 2000.0).await;
    assert_eq!(k.diferencia, 0.0, "el cierre del día 3 cuadra: el faltante ya se documentó al abrir");
    let ventas_en_corte: f64 = sqlx::query_scalar("SELECT total_ventas_efectivo::float8 FROM cortes WHERE id = $1")
        .bind(k.id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(ventas_en_corte, 1300.0, "incluye la venta del día sin cerrar");
    assert_eq!(caja::primer_dia_pendiente(&mut tx, Caja::Web, "2026-10-04").await.unwrap(), None);
}

#[tokio::test]
async fn corte_de_turno_con_retiro_no_cuenta_doble_y_el_pin_de_vendedor_no_autoriza() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    venta_web(&mut tx, &c, p, "2026-10-01 10:00:00", 1000.0, "efectivo").await;

    // Retiro grande hecho por un vendedor: sin PIN, o con el PIN de un
    // vendedor, no se autoriza (y no se guarda nada).
    let t = "2026-10-01 14:00:00";
    let e = corte_con(&mut tx, c.vendedor, "PARCIAL", t, t, 3000.0, 3000.0, Some((2500.0, None))).await;
    assert!(e.unwrap_err().contains("requieren PIN del dueño"));
    let e = corte_con(&mut tx, c.vendedor, "PARCIAL", t, t, 3000.0, 3000.0, Some((2500.0, Some(PIN_VENDEDOR)))).await;
    assert_eq!(e.unwrap_err(), "PIN del dueño incorrecto");
    assert_eq!(escalar_i64(&mut tx, "SELECT COUNT(*)::bigint FROM cortes WHERE caja = 'web'").await, 0);

    let k = corte_con(&mut tx, c.vendedor, "PARCIAL", t, t, 3000.0, 3000.0, Some((2500.0, Some(PIN_DUENO))))
        .await
        .unwrap();
    assert_eq!(k.efectivo_esperado, 3000.0, "el retiro no altera el corte que lo origina");
    assert_eq!(k.fondo_siguiente, 500.0);
    let r = sqlx::query(
        "SELECT corte_id, autorizado_por, usuario_id, caja FROM movimientos_caja \
         WHERE caja = 'web' AND tipo = 'RETIRO' AND monto = 2500",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(r.get::<Option<i64>, _>("corte_id"), Some(k.id));
    assert_eq!(r.get::<Option<i64>, _>("autorizado_por"), Some(c.dueno), "autoriza el dueño del PIN");
    assert_eq!(r.get::<i64, _>("usuario_id"), c.vendedor);

    venta_web(&mut tx, &c, p, "2026-10-01 16:00:00", 400.0, "efectivo").await;
    let datos = caja::calcular_periodo(&mut tx, Caja::Web, "2026-10-01 20:00:00").await.unwrap();
    assert_eq!(datos.efectivo_esperado, 900.0, "500 de fondo + 400; el retiro no se resta otra vez");
    assert_eq!(datos.total_retirado_parciales, 2500.0);
    assert_eq!(datos.cortes_parciales_hoy, 1);

    let k = corte(&mut tx, &c, "DIA", "2026-10-01 20:00:00", 900.0, 900.0).await;
    assert_eq!(k.diferencia, 0.0);
}

#[tokio::test]
async fn venta_mientras_el_corte_esta_abierto_da_datos_cambiaron() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    venta_web(&mut tx, &c, p, "2026-10-01 10:00:00", 100.0, "efectivo").await;

    let viejos = caja::calcular_periodo(&mut tx, Caja::Web, "2026-10-01 15:00:00").await.unwrap();
    venta_web(&mut tx, &c, p, "2026-10-01 15:01:00", 50.0, "efectivo").await;

    let r = caja::crear_corte_core(
        &mut tx,
        Caja::Web,
        c.dueno,
        NuevoCorte {
            tipo: "PARCIAL".into(),
            usuario_id: None,
            fecha_inicio: None,
            fecha_fin: None,
            datos: DatosCorteCliente { efectivo_esperado: viejos.efectivo_esperado },
            efectivo_contado: 2150.0,
            nota_diferencia: None,
            fondo_siguiente: 2150.0,
            denominaciones: None,
            retiro: None,
        },
        "2026-10-01 15:02:00",
    )
    .await;
    let err = msg(r.err().expect("debe rechazar"));
    assert!(err.starts_with(caja::ERR_DATOS_CAMBIARON), "{err}");
    assert_eq!(escalar_i64(&mut tx, "SELECT COUNT(*)::bigint FROM cortes WHERE caja = 'web'").await, 0, "no se guardó nada");

    let k = corte(&mut tx, &c, "PARCIAL", "2026-10-01 15:03:00", 2150.0, 2150.0).await;
    assert_eq!(k.efectivo_esperado, 2150.0);
    assert_eq!(k.diferencia, 0.0);
}

#[tokio::test]
async fn movimiento_viejo_huerfano_no_se_cuela_al_corte_actual() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    corte(&mut tx, &c, "DIA", "2026-10-01 20:00:00", 2000.0, 2000.0).await;

    // Dato viejo: un retiro del día 1 que quedó sin corte asignado.
    movimiento(&mut tx, &c, "2026-10-01 10:00:00", "RETIRO", 300.0).await;

    abrir(&mut tx, &c, "2026-10-02 08:00:00", 2000.0, None).await.unwrap();
    venta_web(&mut tx, &c, p, "2026-10-02 10:00:00", 100.0, "efectivo").await;
    assert_eq!(esperado(&mut tx, "2026-10-02 20:00:00").await, 2100.0, "el retiro viejo no resta hoy");
    assert!(caja::movimientos_sin_corte(&mut tx, Caja::Web).await.unwrap().is_empty());

    let k = corte(&mut tx, &c, "DIA", "2026-10-02 20:00:00", 2100.0, 2000.0).await;
    assert_eq!(k.diferencia, 0.0);
    assert_eq!(
        escalar_i64(
            &mut tx,
            "SELECT COUNT(*)::bigint FROM movimientos_caja WHERE caja = 'web' AND monto = 300 AND corte_id IS NULL"
        )
        .await,
        1,
        "el huérfano no se asigna a un corte que no lo contó"
    );
}

#[tokio::test]
async fn anular_ya_cortada_registra_reembolso_y_en_periodo_abierto_no() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    let (v, _) = venta_web(&mut tx, &c, p, "2026-10-01 10:00:00", 500.0, "efectivo").await;
    corte(&mut tx, &c, "PARCIAL", "2026-10-01 12:00:00", 2500.0, 2500.0).await;

    // Otro día: no se puede anular, se usa devolución.
    let e = anular_venta_tx(&mut tx, c.dueno, v, "tarde", "2026-10-02 09:00:00").await.unwrap_err();
    assert!(msg(e).contains("día en curso"));

    let stock_antes: f64 = sqlx::query_scalar("SELECT stock_actual::float8 FROM productos WHERE id = $1")
        .bind(p)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    anular_venta_tx(&mut tx, c.dueno, v, "cliente se arrepintió", "2026-10-01 13:00:00").await.unwrap();
    let r = sqlx::query(
        "SELECT concepto, corte_id, caja, origen, fecha FROM movimientos_caja \
         WHERE caja = 'web' AND tipo = 'RETIRO' AND monto = 500",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert_eq!(r.len(), 1, "el corte de turno ya había contado la venta: el reembolso sale ahora");
    assert!(r[0].get::<String, _>("concepto").starts_with("Reembolso por anulación de venta"));
    assert_eq!(r[0].get::<Option<i64>, _>("corte_id"), None);
    assert_eq!(r[0].get::<String, _>("fecha"), "2026-10-01 13:00:00");
    let fila = sqlx::query("SELECT anulada, anulada_por, uuid FROM ventas WHERE id = $1")
        .bind(v)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(fila.get::<i32, _>("anulada"), 1);
    assert_eq!(fila.get::<Option<i64>, _>("anulada_por"), Some(c.dueno));
    let vuuid: String = fila.get("uuid");
    let cursores: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM sync_cursor WHERE tabla = 'ventas' AND uuid = $1")
        .bind(&vuuid)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(cursores, 1, "la anulación baja al escritorio");
    let stock_despues: f64 = sqlx::query_scalar("SELECT stock_actual::float8 FROM productos WHERE id = $1")
        .bind(p)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(stock_despues, stock_antes + 1.0);

    // Venta del período abierto: anularla no necesita retiro (el corte
    // excluye anuladas).
    let (w, _) = venta_web(&mut tx, &c, p, "2026-10-01 13:30:00", 300.0, "efectivo").await;
    anular_venta_tx(&mut tx, c.dueno, w, "error de cobro", "2026-10-01 14:00:00").await.unwrap();
    assert_eq!(
        escalar_i64(&mut tx, "SELECT COUNT(*)::bigint FROM movimientos_caja WHERE caja = 'web' AND tipo = 'RETIRO'").await,
        1
    );

    let datos = caja::calcular_periodo(&mut tx, Caja::Web, "2026-10-01 20:00:00").await.unwrap();
    assert_eq!(datos.total_anulaciones, 300.0);
    let k = corte(&mut tx, &c, "DIA", "2026-10-01 20:00:00", 2000.0, 2000.0).await;
    assert_eq!(k.efectivo_esperado, 2000.0, "2500 que quedaron − 500 reembolsados");
    assert_eq!(k.diferencia, 0.0);
}

fn devolucion(venta_id: i64, items: Vec<(i64, f64)>, reembolso: Option<bool>) -> NuevaDevolucionWeb {
    serde_json::from_value(json!({
        "venta_id": venta_id, "motivo": "prueba WP5b", "reembolso_efectivo": reembolso,
        "items": items.iter().map(|(vd, c)| json!({"venta_detalle_id": vd, "cantidad": c})).collect::<Vec<_>>(),
    }))
    .unwrap()
}

#[tokio::test]
async fn devolucion_con_tarjeta_no_saca_dinero_de_la_caja() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    let (v, vd) = venta_web(&mut tx, &c, p, "2026-10-01 10:00:00", 300.0, "tarjeta").await;

    let d = crear_devolucion_tx(&mut tx, Caja::Web, c.dueno, None, &devolucion(v, vec![(vd, 1.0)], None), Some("2026-10-01 11:00:00"))
        .await
        .unwrap();
    assert_eq!(d["total_devuelto"].as_f64(), Some(300.0));
    assert_eq!(d["fecha"].as_str(), Some("2026-10-01 11:00:00"));
    assert!(d["folio"].as_str().unwrap().starts_with("D-20261001-"));
    assert_eq!(escalar_i64(&mut tx, "SELECT COUNT(*)::bigint FROM movimientos_caja WHERE caja = 'web'").await, 0);
    assert_eq!(esperado(&mut tx, "2026-10-01 20:00:00").await, 2000.0);
}

#[tokio::test]
async fn devolucion_total_regresa_exactamente_lo_cobrado_y_partida_repetida() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    // 67.80 se cobra 68 (redondeo al peso).
    let (v, vd) = venta_en(&mut tx, &c, p, "web", "2026-10-01 10:00:00", 1.0, 67.80, 68.0, "efectivo").await;
    let d = crear_devolucion_tx(&mut tx, Caja::Web, c.dueno, None, &devolucion(v, vec![(vd, 1.0)], None), Some("2026-10-01 11:00:00"))
        .await
        .unwrap();
    assert_eq!(d["total_devuelto"].as_f64(), Some(68.0));
    let r = sqlx::query("SELECT monto::float8 AS monto, caja, origen, fecha FROM movimientos_caja WHERE caja = 'web' AND tipo = 'RETIRO'")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(r.get::<f64, _>("monto"), 68.0);
    assert_eq!(r.get::<String, _>("origen"), "web");
    assert_eq!(r.get::<String, _>("fecha"), "2026-10-01 11:00:00");
    assert_eq!(esperado(&mut tx, "2026-10-01 20:00:00").await, 2000.0);

    // Partida repetida: 2 + 1 > 2 vendidas → rechazo; 1 → 100.
    let (v, vd) = venta_en(&mut tx, &c, p, "web", "2026-10-01 12:00:00", 2.0, 100.0, 200.0, "efectivo").await;
    let doble = crear_devolucion_tx(&mut tx, Caja::Web, c.dueno, None, &devolucion(v, vec![(vd, 2.0), (vd, 1.0)], None), Some("2026-10-01 13:00:00")).await;
    assert!(msg(doble.unwrap_err()).contains("excede"));
    let d = crear_devolucion_tx(&mut tx, Caja::Web, c.dueno, None, &devolucion(v, vec![(vd, 1.0)], None), Some("2026-10-01 13:00:00"))
        .await
        .unwrap();
    assert_eq!(d["total_devuelto"].as_f64(), Some(100.0));
    assert_eq!(esperado(&mut tx, "2026-10-01 20:00:00").await, 2100.0, "2000 + 200 − 100");
}

#[tokio::test]
async fn apertura_con_sobrante_registra_entrada() {
    let Some(c) = ctx().await else { return };
    let (mut tx, _p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    corte(&mut tx, &c, "DIA", "2026-10-01 20:00:00", 2000.0, 2000.0).await;

    let a = abrir(&mut tx, &c, "2026-10-02 08:00:00", 2150.0, Some("Había cambio extra")).await.unwrap();
    assert_eq!(a.fondo_declarado, 2150.0);
    assert_eq!(a.usuario_id, c.dueno);
    assert_eq!(
        escalar_i64(&mut tx, "SELECT COUNT(*)::bigint FROM movimientos_caja WHERE caja = 'web' AND tipo = 'ENTRADA' AND monto = 150").await,
        1
    );
    assert_eq!(esperado(&mut tx, "2026-10-02 20:00:00").await, 2150.0);
    let hoy = caja::apertura_del_dia(&mut tx, Caja::Web, "2026-10-02").await.unwrap().unwrap();
    assert_eq!(hoy.id, a.id);
}

#[tokio::test]
async fn validaciones_del_corte() {
    let Some(c) = ctx().await else { return };
    let (mut tx, _p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    let t = "2026-10-01 20:00:00";

    let e = corte_con(&mut tx, c.dueno, "DIA", t, t, 1000.0, 2000.0, None).await;
    assert!(e.unwrap_err().contains("de fondo si solo contaste"), "no se puede dejar más de lo contado");
    let e = corte_con(&mut tx, c.dueno, "PARCIAL", t, t, 1000.0, 0.0, Some((1500.0, None))).await;
    assert!(e.unwrap_err().contains("No puedes retirar más"), "no se puede retirar más de lo contado");
    let e = corte_con(&mut tx, c.dueno, "DIA", t, t, 2000.0, 2000.0, Some((100.0, None))).await;
    assert!(e.unwrap_err().contains("solo aplica en cortes de turno"));
    let e = corte_con(&mut tx, c.dueno, "OTRO", t, t, 2000.0, 2000.0, None).await;
    assert_eq!(e.unwrap_err(), "Tipo de corte inválido");

    corte(&mut tx, &c, "DIA", t, 2000.0, 2000.0).await;
    let doble = corte_con(&mut tx, c.dueno, "DIA", "2026-10-01 21:00:00", "2026-10-01 21:00:00", 2000.0, 2000.0, None).await;
    assert!(doble.unwrap_err().contains("Ya se hizo el cierre de caja del 2026-10-01"), "un solo cierre por día");
}

#[tokio::test]
async fn auditoria_queda_en_cero_con_el_modelo_nuevo_y_detecta_huerfanos() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-01-05 08:00:00", 2000.0, None).await.unwrap();
    venta_web(&mut tx, &c, p, "2026-01-05 10:00:00", 1000.0, "efectivo").await;
    corte_con(&mut tx, c.dueno, "PARCIAL", "2026-01-05 14:00:00", "2026-01-05 14:00:00", 3000.0, 0.0, Some((1000.0, None)))
        .await
        .unwrap();
    venta_web(&mut tx, &c, p, "2026-01-05 16:00:00", 400.0, "efectivo").await;
    corte(&mut tx, &c, "DIA", "2026-01-05 20:00:00", 2400.0, 2000.0).await;
    venta_web(&mut tx, &c, p, "2026-01-05 21:00:00", 150.0, "efectivo").await; // después del cierre

    // Día 6 sin cierre; día 7 abre con faltante documentado y cierra.
    abrir(&mut tx, &c, "2026-01-06 08:00:00", 2150.0, None).await.unwrap();
    venta_web(&mut tx, &c, p, "2026-01-06 10:00:00", 600.0, "efectivo").await;
    abrir(&mut tx, &c, "2026-01-07 08:00:00", 2150.0, Some("los 600 del día 6 se depositaron")).await.unwrap();
    let k = corte(&mut tx, &c, "DIA", "2026-01-07 20:00:00", 2150.0, 2000.0).await;
    assert_eq!(k.diferencia, 0.0);

    let a = caja::auditar(&mut tx, Caja::Web, 3650).await.unwrap();
    assert_eq!(a.resumen.ventas_sin_corte_count, 0, "ninguna venta fuera de la cadena");
    assert_eq!(a.resumen.movimientos_desalineados_count, 0);
    assert_eq!(a.resumen.movimientos_huerfanos_count, 0);
    assert_eq!(a.resumen.eslabones_rotos_count, 0, "cada corte arranca con lo que dejó el anterior");
    assert_eq!(a.resumen.cortes_inconsistentes_count, 0);
    assert_eq!(a.resumen.dias_sin_cierre_count, 1, "el día 6 se reporta (informativo)");

    // Dato heredado del modelo anterior: un retiro que ningún corte contó.
    movimiento(&mut tx, &c, "2026-01-05 12:00:00", "RETIRO", 300.0).await;
    let a = caja::auditar(&mut tx, Caja::Web, 3650).await.unwrap();
    assert_eq!(a.resumen.movimientos_huerfanos_count, 1);
    assert_eq!(a.resumen.movimientos_huerfanos_monto, -300.0);
}

#[tokio::test]
async fn misma_segunda_y_reloj_que_no_retrocede() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    // Corte en este mismo segundo; lo siguiente que se registre cae después.
    let r = caja::reloj(&mut tx).await.unwrap();
    let k = corte(&mut tx, &c, "PARCIAL", &r, 0.0, 0.0).await;
    let ahora = caja::ahora_caja(&mut tx, Caja::Web).await.unwrap();
    assert!(ahora > r, "{ahora} debe ser posterior al corte {r}");
    venta_web(&mut tx, &c, p, &ahora, 75.0, "efectivo").await;
    let datos = caja::calcular_periodo(&mut tx, Caja::Web, &ahora).await.unwrap();
    assert_eq!(datos.fecha_inicio, r);
    assert_eq!(datos.total_ventas_efectivo, 75.0, "la venta del mismo segundo cae en el período siguiente");
    let cubre: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM cortes WHERE id = $1 AND $2 > fecha_inicio AND $2 <= fecha_fin",
    )
    .bind(k.id)
    .bind(&ahora)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(cubre, 0);

    // Un corte "en el futuro" (reloj del servidor atrasado): nada se registra
    // antes de él.
    corte(&mut tx, &c, "PARCIAL", "2099-01-01 10:00:00", 75.0, 75.0).await;
    assert_eq!(caja::ahora_caja(&mut tx, Caja::Web).await.unwrap(), "2099-01-01 10:00:01");
}

#[tokio::test]
async fn la_caja_web_y_la_de_la_tienda_no_se_mezclan() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    abrir(&mut tx, &c, "2026-10-01 08:00:00", 2000.0, None).await.unwrap();
    venta_web(&mut tx, &c, p, "2026-10-01 09:00:00", 500.0, "efectivo").await;
    // Venta espejo (caja de la tienda) y un retiro de la tienda el mismo día.
    venta_en(&mut tx, &c, p, "principal", "2026-10-01 09:30:00", 1.0, 777.0, 777.0, "efectivo").await;
    caja::insertar_movimiento(&mut tx, Caja::Principal, "RETIRO", c.dueno, 55.0, "tienda", None, None, "2026-10-01 10:00:00")
        .await
        .unwrap();

    let datos = caja::calcular_periodo(&mut tx, Caja::Web, "2026-10-01 20:00:00").await.unwrap();
    assert_eq!(datos.total_ventas_efectivo, 500.0);
    assert_eq!(datos.num_transacciones, 1);
    assert!(datos.movimientos.is_empty());
    assert_eq!(datos.efectivo_esperado, 2500.0);
    let k = corte(&mut tx, &c, "DIA", "2026-10-01 20:00:00", 2500.0, 2000.0).await;
    assert_eq!(k.diferencia, 0.0);
    let retiro_tienda: Option<i64> = sqlx::query_scalar(
        "SELECT corte_id FROM movimientos_caja WHERE caja = 'principal' AND concepto = 'tienda' AND monto = 55",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(retiro_tienda, None, "un corte web nunca toma movimientos de la tienda");
    // Y la escritura a la cadena de la tienda se rechaza en el núcleo también.
    let e = caja::crear_apertura_core(
        &mut tx,
        Caja::Principal,
        c.dueno,
        NuevaApertura { usuario_id: None, fondo_declarado: 1.0, nota: None },
        "2026-10-02 08:00:00",
    )
    .await
    .unwrap_err();
    assert_eq!(msg(e), caja::MSG_PRINCIPAL_SOLO_ESCRITORIO);
}

// ─── Forma de las respuestas = structs del escritorio ─────────────────────

/// Campos `pub` de un struct del escritorio, leídos de su código fuente.
fn campos_del_escritorio(archivo: &str, estructura: &str) -> Option<Vec<String>> {
    let ruta = format!("{}/../src-tauri/src/commands/{}", env!("CARGO_MANIFEST_DIR"), archivo);
    let src = std::fs::read_to_string(&ruta).ok()?;
    let inicio = src
        .find(&format!("pub struct {} {{", estructura))
        .unwrap_or_else(|| panic!("no encontré `pub struct {estructura}` en {archivo}"));
    let resto = &src[inicio..];
    let fin = resto.find("\n}").unwrap();
    let mut campos: Vec<String> = resto[..fin]
        .lines()
        .skip(1)
        .filter_map(|l| l.trim().strip_prefix("pub ").and_then(|r| r.split(':').next()))
        .map(|s| s.trim().to_string())
        .collect();
    campos.sort();
    Some(campos)
}

fn llaves<T: serde::Serialize>(v: T) -> Vec<String> {
    let mut k: Vec<String> = serde_json::to_value(v).unwrap().as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

#[test]
fn forma_de_las_respuestas_igual_que_el_escritorio() {
    use crate::caja::*;
    let casos: Vec<(&str, &str, Vec<String>)> = vec![
        ("cortes.rs", "MovimientoCajaRs", llaves(MovimientoCaja::default())),
        ("cortes.rs", "VendedorResumenRs", llaves(VendedorResumen::default())),
        ("cortes.rs", "DatosCorte", llaves(DatosCorte::default())),
        ("cortes.rs", "CorteCreado", llaves(CorteCreado::default())),
        ("cortes.rs", "CorteResumen", llaves(CorteResumen::default())),
        ("cortes.rs", "CorteDetalle", llaves(CorteDetalle::default())),
        ("cortes.rs", "DenominacionDetalle", llaves(DenominacionDetalle::default())),
        ("cortes.rs", "EsperadoApertura", llaves(EsperadoApertura::default())),
        ("cortes.rs", "AperturaCaja", llaves(AperturaCaja::default())),
        ("auditoria_caja.rs", "VentaSinCorte", llaves(VentaSinCorte::default())),
        ("auditoria_caja.rs", "MovimientoDesalineado", llaves(MovimientoDesalineado::default())),
        ("auditoria_caja.rs", "MovimientoHuerfano", llaves(MovimientoHuerfano::default())),
        ("auditoria_caja.rs", "EslabonRoto", llaves(EslabonRoto::default())),
        ("auditoria_caja.rs", "CorteInconsistente", llaves(CorteInconsistente::default())),
        ("auditoria_caja.rs", "DiaSinCierre", llaves(DiaSinCierre::default())),
        ("auditoria_caja.rs", "ResumenAuditoria", llaves(ResumenAuditoria::default())),
        ("auditoria_caja.rs", "AuditoriaCaja", llaves(AuditoriaCaja::default())),
    ];
    for (archivo, estructura, servidor) in casos {
        let Some(escritorio) = campos_del_escritorio(archivo, estructura) else {
            eprintln!("sin el código del escritorio ({archivo}): se omite la comparación de forma");
            return;
        };
        assert_eq!(servidor, escritorio, "la forma de {estructura} difiere del escritorio");
    }
}

// ─── RPC con reloj real ──────────────────────────────────────────────────

#[tokio::test]
async fn la_cadena_de_la_tienda_es_de_solo_lectura_en_la_web() {
    let Some(c) = ctx().await else { return };
    let espejo = dispositivo(&c, "espejo").await;
    let individual = dispositivo(&c, "individual").await;
    let tok_espejo = token(c.dueno, "wp5b_dueno", true, Some(&espejo));
    let tok_ind = token(c.dueno, "wp5b_dueno", true, Some(&individual));

    let cortes_tienda = |c: &Ctx| {
        let pool = c.st.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT (SELECT COUNT(*) FROM cortes WHERE caja = 'principal') \
                      + (SELECT COUNT(*) FROM aperturas_caja WHERE caja = 'principal')",
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let antes = cortes_tienda(&c).await;
    let corte = json!({"datos": {"tipo": "PARCIAL", "usuario_id": c.dueno,
        "datos": {"efectivo_esperado": 0.0}, "efectivo_contado": 0.0, "fondo_siguiente": 0.0}});
    let apertura = json!({"datos": {"usuario_id": c.dueno, "fondo_declarado": 0.0}});
    for tok in [&c.tok_dueno, &tok_espejo] {
        let e = rpc(&c, tok, "crear_corte", corte.clone()).await.unwrap_err();
        assert_eq!(msg(e), caja::MSG_PRINCIPAL_SOLO_ESCRITORIO);
        let e = rpc(&c, tok, "crear_apertura_caja", apertura.clone()).await.unwrap_err();
        assert_eq!(msg(e), caja::MSG_PRINCIPAL_SOLO_ESCRITORIO);
        // Lectura sí: vista previa, historial, aviso de cierre (siempre null).
        let d = rpc(&c, tok, "calcular_datos_corte", json!({})).await.unwrap();
        assert!(d["efectivo_esperado"].is_number());
        assert!(rpc(&c, tok, "listar_cortes", json!({"limite": 5})).await.unwrap().is_array());
        assert_eq!(rpc(&c, tok, "verificar_corte_dia_pendiente", json!({})).await.unwrap(), Value::Null);
    }
    assert_eq!(cortes_tienda(&c).await, antes, "la web no escribe la cadena de la tienda");

    // Modo y caja de cada tipo de sesión.
    let m = rpc(&c, &c.tok_dueno, "obtener_modo_caja", json!({})).await.unwrap();
    assert_eq!((m["modo"].as_str(), m["caja"].as_str()), (Some("espejo"), Some("principal")));
    assert_eq!(m["cortes_en_este_equipo"], json!(false));
    let m = rpc(&c, &tok_espejo, "obtener_modo_caja", json!({})).await.unwrap();
    assert_eq!((m["caja"].as_str(), m["cortes_en_este_equipo"].as_bool()), (Some("principal"), Some(false)));
    assert!(m["escritorio_recibe_web"].is_boolean());
    let m = rpc(&c, &tok_ind, "obtener_modo_caja", json!({})).await.unwrap();
    assert_eq!((m["caja"].as_str(), m["cortes_en_este_equipo"].as_bool()), (Some("web"), Some(true)));
}

#[tokio::test]
async fn vista_previa_de_la_tienda_igual_a_la_cadena_calculada_a_mano() {
    let Some(c) = ctx().await else { return };
    // Momento fijo dentro de la copia de producción. Lo que las pruebas
    // registran (con el reloj real) queda después y no entra.
    const FIJO: &str = "2026-10-06 19:40:00";
    let mut conn = c.st.pool.acquire().await.unwrap();

    // ── A mano, sin SQL de la cadena: ──
    let norm = |s: &str| s.chars().take(19).collect::<String>().replace('T', " ");
    let cortes = sqlx::query(
        "SELECT id, fecha_fin, created_at, fondo_siguiente::float8 AS fs FROM cortes \
         WHERE caja = 'principal' AND deleted_at IS NULL",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert!(!cortes.is_empty(), "la copia de producción debe tener cortes de la tienda");
    let (fin, _, fondo) = cortes
        .iter()
        .map(|r| {
            let ff = norm(&r.get::<String, _>("fecha_fin"));
            let ca = norm(&r.get::<String, _>("created_at"));
            (ff.min(ca), r.get::<i64, _>("id"), r.get::<f64, _>("fs"))
        })
        .max_by(|a, b| (a.0.as_str(), a.1).cmp(&(b.0.as_str(), b.1)))
        .unwrap();
    let ventas = sqlx::query(
        "SELECT total::float8 AS total, metodo_pago, anulada, deleted_at, registrado_at, fecha \
         FROM ventas WHERE caja = 'principal'",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    let mut ventas_ef = 0.0;
    for v in &ventas {
        let m = v.get::<Option<String>, _>("registrado_at").unwrap_or_else(|| v.get("fecha"));
        if v.get::<i32, _>("anulada") == 0
            && v.get::<Option<String>, _>("deleted_at").is_none()
            && v.get::<String, _>("metodo_pago") == "efectivo"
            && m.as_str() > fin.as_str()
            && m.as_str() <= FIJO
        {
            ventas_ef += v.get::<f64, _>("total");
        }
    }
    let movs = sqlx::query(
        "SELECT id, tipo, monto::float8 AS monto, corte_id, deleted_at, registrado_at, fecha \
         FROM movimientos_caja WHERE caja = 'principal'",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    let (mut entradas, mut retiros, mut ids) = (0.0, 0.0, Vec::new());
    for m in &movs {
        let mo = m.get::<Option<String>, _>("registrado_at").unwrap_or_else(|| m.get("fecha"));
        if m.get::<Option<i64>, _>("corte_id").is_none()
            && m.get::<Option<String>, _>("deleted_at").is_none()
            && mo.as_str() > fin.as_str()
            && mo.as_str() <= FIJO
        {
            ids.push(m.get::<i64, _>("id"));
            if m.get::<String, _>("tipo") == "ENTRADA" {
                entradas += m.get::<f64, _>("monto");
            } else {
                retiros += m.get::<f64, _>("monto");
            }
        }
    }
    let esperado_a_mano = caja::round2(fondo + ventas_ef + entradas - retiros);

    // ── Motor ──
    let d = caja::calcular_periodo(&mut conn, Caja::Principal, FIJO).await.unwrap();
    eprintln!(
        "vista previa de la tienda en {FIJO}: inicio {} fondo {} ventas ef {} entradas {} retiros {} esperado {}",
        d.fecha_inicio, d.fondo_inicial, d.total_ventas_efectivo, d.total_entradas_efectivo,
        d.total_retiros_efectivo, d.efectivo_esperado
    );
    assert_eq!(d.fecha_inicio, fin);
    assert_eq!(d.fondo_inicial, fondo);
    assert!((d.total_ventas_efectivo - ventas_ef).abs() < 0.001);
    assert!((d.total_entradas_efectivo - entradas).abs() < 0.001);
    assert!((d.total_retiros_efectivo - retiros).abs() < 0.001);
    assert_eq!(d.efectivo_esperado, esperado_a_mano);
    let mut ids_motor: Vec<i64> = d.movimientos.iter().map(|m| m.id).collect();
    ids_motor.sort();
    ids.sort();
    assert_eq!(ids_motor, ids);
    // Los 4 retiros huérfanos viejos del escritorio no se cuelan.
    for viejo in [1272_i64, 1295, 1445, 1906] {
        assert!(!ids_motor.contains(&viejo));
    }
    // Copia de producción tomada el 2026-10-06: último corte 323 (FIN 17:04:59,
    // fondo 1166), como en el diseño.
    let max_id: i64 = escalar_i64(&mut conn, "SELECT COALESCE(MAX(id), 0)::bigint FROM cortes WHERE caja = 'principal'").await;
    if max_id == 323 {
        assert_eq!((d.fecha_inicio.as_str(), d.fondo_inicial), ("2026-10-06 17:04:59", 1166.0));
    }

    // La RPC (reloj real) arranca la cadena en el mismo punto.
    let r = rpc(&c, &c.tok_dueno, "calcular_datos_corte", json!({"fechaInicio": "x", "fechaFin": "y"})).await.unwrap();
    assert_eq!(r["fecha_inicio"].as_str(), Some(fin.as_str()));
    assert_eq!(r["fondo_inicial"].as_f64(), Some(fondo));
    assert_eq!(rpc(&c, &c.tok_dueno, "obtener_inicio_proximo_cierre", json!({})).await.unwrap(), json!(fin));
    assert_eq!(rpc(&c, &c.tok_dueno, "obtener_fondo_sugerido", json!({})).await.unwrap(), json!(fondo));
    let ea = rpc(&c, &c.tok_dueno, "obtener_esperado_apertura", json!({})).await.unwrap();
    assert_eq!(ea["hay_referencia"], json!(true));
    assert_eq!(ea["ultimo_corte_fecha"].as_str(), Some(fin.as_str()));

    // Auditoría de la tienda (solo dueños): los huérfanos viejos aparecen.
    let e = rpc(&c, &c.tok_vendedor, "auditar_caja", json!({"limiteDias": 3650})).await.unwrap_err();
    assert!(matches!(e, ApiError::Forbidden));
    let a = rpc(&c, &c.tok_dueno, "auditar_caja", json!({"limiteDias": 3650})).await.unwrap();
    let huerfanos: Vec<i64> = a["movimientos_huerfanos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect();
    for viejo in [1272_i64, 1295, 1445, 1906] {
        let existe: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM movimientos_caja WHERE id = $1 AND caja = 'principal' AND corte_id IS NULL",
        )
        .bind(viejo)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        if existe == 1 {
            assert!(huerfanos.contains(&viejo), "el retiro huérfano {viejo} debe reportarse");
        }
    }
    assert_eq!(a["resumen"]["movimientos_huerfanos_count"].as_i64(), Some(huerfanos.len() as i64));
}

#[tokio::test]
async fn cliente_viejo_sin_header_no_mueve_la_caja() {
    let Some(c) = ctx().await else { return };
    let concepto = format!("WP5B sin header {}", uuid::Uuid::now_v7());
    let args = json!({"datos": {"tipo": "RETIRO", "usuario_id": c.dueno, "monto": 10.0, "concepto": concepto}});
    for version in [None, Some("1"), Some("abc")] {
        for cmd in super::RPC_QUE_MUEVEN_CAJA {
            let a = if *cmd == "crear_movimiento_caja" { args.clone() } else { json!({}) };
            let e = rpc_cliente(&c.st, &c.tok_dueno, cmd, a, version).await.unwrap_err();
            assert!(msg(e).contains("Recarga la página (F5)"), "{cmd} con versión {version:?}");
        }
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM movimientos_caja WHERE concepto = $1")
        .bind(&concepto)
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "un retiro de una pestaña vieja no se registra");
    // Las lecturas siguen funcionando para la pestaña vieja.
    assert!(rpc_cliente(&c.st, &c.tok_dueno, "calcular_datos_corte", json!({}), None).await.is_ok());
    // Con el header actual sí se registra (en la caja de la tienda, origen web).
    let r = rpc_cliente(&c.st, &c.tok_dueno, "crear_movimiento_caja", args, Some("2")).await.unwrap();
    let fila = sqlx::query("SELECT caja, origen FROM movimientos_caja WHERE id = $1")
        .bind(r["id"].as_i64().unwrap())
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
    assert_eq!((fila.get::<String, _>("caja"), fila.get::<String, _>("origen")), ("principal".into(), "web".into()));
    let bit: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM audit_log WHERE accion = 'RETIRO_CAJA' AND tabla_afectada = 'movimientos_caja' AND registro_id = $1",
    )
    .bind(r["id"].as_i64().unwrap())
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    assert_eq!(bit, 1);
}

/// Corte de turno de la caja web por RPC (como el modal): vista previa,
/// cuenta = esperado (mínimo 0) y confirma; reintenta si entró algo.
async fn cortar_web(st: &AppState, tok: &str, fondo: Option<f64>, retiro: Option<f64>) -> Value {
    for _ in 0..50 {
        let d = rpc_con(st, tok, "calcular_datos_corte", json!({})).await.unwrap();
        let e = d["efectivo_esperado"].as_f64().unwrap();
        let contado = e.max(0.0);
        let fondo_siguiente = fondo.unwrap_or(contado).min(contado);
        let mut datos = json!({
            "tipo": "PARCIAL", "usuario_id": 0, "fecha_inicio": d["fecha_inicio"], "fecha_fin": d["fecha_fin"],
            "datos": d, "efectivo_contado": contado, "nota_diferencia": "prueba WP5b",
            "fondo_siguiente": fondo_siguiente,
        });
        if let Some(r) = retiro {
            datos["retiro"] = json!({"monto": r, "concepto": "Retiro de prueba WP5b", "pin_autorizacion": null});
        }
        match rpc_con(st, tok, "crear_corte", json!({ "datos": datos })).await {
            Ok(v) => return v,
            Err(e) => {
                let m = msg(e);
                assert!(m.starts_with(caja::ERR_DATOS_CAMBIARON), "{m}");
            }
        }
    }
    panic!("el corte no se pudo confirmar en 50 intentos");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn venta_contra_corte_cada_venta_en_exactamente_un_corte() {
    let Some(c) = ctx().await else { return };
    let _g = candado_caja_web().lock().await;
    let dev = dispositivo(&c, "individual").await;
    let tok = token(c.dueno, "wp5b_dueno", true, Some(&dev));
    let p = producto(&c, 10.0, 0.0).await;

    // Deja la caja web en cero para medir solo lo de esta prueba.
    let inicial = cortar_web(&c.st, &tok, Some(0.0), None).await;
    let primer_corte = inicial["id"].as_i64().unwrap();

    let mut ventas: Vec<i64> = Vec::new();
    for _ in 0..50 {
        let (v, k) = tokio::join!(
            rpc_con(&c.st, &tok, "crear_venta", venta(c.dueno, vec![linea(p, 10.0, 1.0)], "efectivo")),
            cortar_web(&c.st, &tok, None, None),
        );
        let v = v.unwrap_or_else(|e| panic!("venta falló: {}", msg(e)));
        ventas.push(v["id"].as_i64().unwrap());
        assert!(k["id"].as_i64().unwrap() > primer_corte);
    }
    cortar_web(&c.st, &tok, None, None).await; // cierra lo que haya quedado abierto

    let cortes = sqlx::query(
        "SELECT id, fecha_inicio, fecha_fin, created_at, total_ventas_efectivo::float8 AS tve, \
                fondo_inicial::float8 AS fi, fondo_siguiente::float8 AS fs \
         FROM cortes WHERE caja = 'web' AND id > $1 ORDER BY id",
    )
    .bind(primer_corte)
    .fetch_all(&c.st.pool)
    .await
    .unwrap();
    assert_eq!(cortes.len(), 51);
    let mut suma = 0.0;
    let mut fin_anterior: String = sqlx::query_scalar("SELECT fecha_fin FROM cortes WHERE id = $1")
        .bind(primer_corte)
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
    let mut fondo_anterior = 0.0;
    for k in &cortes {
        let ini: String = k.get("fecha_inicio");
        let fin: String = k.get("fecha_fin");
        assert_eq!(ini, fin_anterior, "la cadena es continua");
        assert_eq!(k.get::<f64, _>("fi"), fondo_anterior, "arranca con el fondo que dejó el anterior");
        assert_eq!(fin, k.get::<String, _>("created_at"));
        // Lo que el corte guardó = lo que hay HOY en su rango: ninguna venta
        // quedó con fecha dentro de un corte que no la contó.
        let recuento: f64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(total), 0)::float8 FROM ventas \
             WHERE caja = 'web' AND deleted_at IS NULL AND anulada = 0 AND metodo_pago = 'efectivo' \
               AND COALESCE(registrado_at, fecha) > $1 AND COALESCE(registrado_at, fecha) <= $2",
        )
        .bind(&ini)
        .bind(&fin)
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
        assert_eq!(recuento, k.get::<f64, _>("tve"), "corte {} perdió o duplicó ventas", k.get::<i64, _>("id"));
        suma += k.get::<f64, _>("tve");
        fin_anterior = fin;
        fondo_anterior = k.get("fs");
    }
    assert_eq!(suma, 500.0, "las 50 ventas, una vez cada una");
    for v in &ventas {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM cortes c JOIN ventas v ON v.id = $1 \
             WHERE c.caja = 'web' AND c.id > $2 \
               AND COALESCE(v.registrado_at, v.fecha) > c.fecha_inicio \
               AND COALESCE(v.registrado_at, v.fecha) <= c.fecha_fin",
        )
        .bind(v)
        .bind(primer_corte)
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
        assert_eq!(n, 1, "la venta {v} debe caer en exactamente un corte");
        let caja_v: String = sqlx::query_scalar("SELECT caja FROM ventas WHERE id = $1").bind(v).fetch_one(&c.st.pool).await.unwrap();
        assert_eq!(caja_v, "web");
    }
}

#[tokio::test]
async fn anular_por_rpc_respeta_la_caja_y_registra_el_reembolso() {
    let Some(c) = ctx().await else { return };
    let _g = candado_caja_web().lock().await;
    let dev = dispositivo(&c, "individual").await;
    let tok = token(c.dueno, "wp5b_dueno", true, Some(&dev));
    let tok_vend = token(c.vendedor, "wp5b_vendedor", false, Some(&dev));
    let p = producto(&c, 100.0, 10.0).await;

    // Venta de la tienda (sin dispositivo = espejo): no se anula en la web.
    let r = rpc(&c, &c.tok_dueno, "crear_venta", venta(c.dueno, vec![linea(p, 100.0, 1.0)], "efectivo")).await.unwrap();
    let e = rpc(&c, &tok, "anular_venta", json!({"ventaId": r["id"], "motivo": "x"})).await.unwrap_err();
    assert_eq!(msg(e), super::MSG_ANULAR_CAJA_TIENDA);

    cortar_web(&c.st, &tok, Some(0.0), None).await;
    let r = rpc(&c, &tok_vend, "crear_venta", venta(c.vendedor, vec![linea(p, 100.0, 1.0)], "efectivo")).await.unwrap();
    let vid = r["id"].as_i64().unwrap();
    let fila = sqlx::query("SELECT caja, origen, fecha, usuario_id FROM ventas WHERE id = $1")
        .bind(vid)
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
    assert_eq!((fila.get::<String, _>("caja"), fila.get::<String, _>("origen")), ("web".into(), "web".into()));
    assert_eq!(r["fecha"].as_str(), Some(fila.get::<String, _>("fecha").as_str()));
    assert!(r["folio"].as_str().unwrap().contains(&fila.get::<String, _>("fecha")[..10].replace('-', "")));

    // Corte de turno con el retiro DENTRO del corte.
    let d = rpc(&c, &tok, "calcular_datos_corte", json!({})).await.unwrap();
    assert_eq!(d["efectivo_esperado"].as_f64(), Some(100.0));
    let k = cortar_web(&c.st, &tok, None, Some(60.0)).await;
    assert_eq!(k["fondo_siguiente"].as_f64(), Some(40.0));
    let kid = k["id"].as_i64().unwrap();
    let ligado: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM movimientos_caja WHERE corte_id = $1 AND tipo = 'RETIRO' AND monto = 60 AND caja = 'web'",
    )
    .bind(kid)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    assert_eq!(ligado, 1, "el retiro de turno queda dentro del corte");
    let det = rpc(&c, &tok, "obtener_detalle_corte", json!({"id": kid})).await.unwrap();
    assert_eq!(det["corte"]["total_ventas_efectivo"].as_f64(), Some(100.0));
    assert_eq!(det["movimientos"].as_array().unwrap().len(), 1);
    assert!(rpc(&c, &tok, "listar_cortes", json!({"limite": 3})).await.unwrap()[0]["id"].as_i64().is_some());

    // El vendedor anula con el PIN del dueño: la venta ya entró en el corte →
    // reembolso como RETIRO de la caja web, en el período en curso.
    let e = rpc(&c, &tok_vend, "anular_venta", json!({"ventaId": vid, "motivo": "x", "pinAutorizacion": PIN_VENDEDOR}))
        .await
        .unwrap_err();
    assert_eq!(msg(e), "PIN del dueño incorrecto");
    rpc(&c, &tok_vend, "anular_venta", json!({"ventaId": vid, "usuarioId": c.dueno, "motivo": "se arrepintió", "pinAutorizacion": PIN_DUENO}))
        .await
        .unwrap_or_else(|e| panic!("anulación rechazada: {}", msg(e)));
    let reemb = sqlx::query(
        "SELECT usuario_id, corte_id, caja FROM movimientos_caja \
         WHERE concepto LIKE 'Reembolso por anulación de venta ' || $1 || '%'",
    )
    .bind(r["folio"].as_str().unwrap())
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    assert_eq!(reemb.get::<i64, _>("usuario_id"), c.vendedor);
    assert_eq!(reemb.get::<Option<i64>, _>("corte_id"), None);
    assert_eq!(reemb.get::<String, _>("caja"), "web");
    let d = rpc(&c, &tok, "calcular_datos_corte", json!({})).await.unwrap();
    assert_eq!(d["efectivo_esperado"].as_f64(), Some(-60.0), "40 de fondo − 100 reembolsados");
    let pendientes = rpc(&c, &tok, "listar_movimientos_sin_corte", json!({})).await.unwrap();
    assert_eq!(pendientes.as_array().unwrap().len(), 1);

    // Apertura de la caja web por RPC (una por día).
    let hoy = rpc(&c, &tok, "obtener_apertura_hoy", json!({})).await.unwrap();
    let ap = json!({"datos": {"usuario_id": 0, "fondo_declarado": 0.0, "nota": "conteo de prueba"}});
    if hoy.is_null() {
        let a = rpc(&c, &tok, "crear_apertura_caja", ap.clone()).await.unwrap();
        assert_eq!(a["usuario_id"].as_i64(), Some(c.dueno));
        let ajuste: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM movimientos_caja WHERE caja = 'web' AND tipo = 'ENTRADA' \
             AND monto = 60 AND concepto LIKE 'Ajuste de apertura%' AND fecha = $1",
        )
        .bind(a["fecha"].as_str().unwrap())
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
        assert_eq!(ajuste, 1, "contó 0 contra −60 esperados: sobran 60, queda como ajuste");
    }
    let e = rpc(&c, &tok, "crear_apertura_caja", ap).await.unwrap_err();
    assert_eq!(msg(e), "Ya existe una apertura de caja para hoy");

    cortar_web(&c.st, &tok, Some(0.0), None).await; // deja la caja web en cero
}

#[tokio::test]
async fn devolucion_por_rpc_segun_la_caja_y_el_estado_del_sync() {
    let Some(c) = ctx().await else { return };
    let _g = candado_caja_web().lock().await;
    let espejo = dispositivo(&c, "espejo").await;
    let individual = dispositivo(&c, "individual").await;
    let tok_espejo = token(c.dueno, "wp5b_dueno", true, Some(&espejo));
    let tok_ind = token(c.dueno, "wp5b_dueno", true, Some(&individual));
    let p = producto(&c, 80.0, 5.0).await;

    // Venta hecha en el escritorio (uuid hex32, caja de la tienda).
    let vu = uuid::Uuid::new_v4().simple().to_string();
    let vid: i64 = sqlx::query_scalar(
        "INSERT INTO ventas (uuid, sucursal_id, folio, usuario_id, subtotal, total, metodo_pago, monto_recibido) \
         VALUES ($1, 1, $2, $3, 160, 160, 'efectivo', 160) RETURNING id",
    )
    .bind(&vu)
    .bind(format!("V-T5B-{}", &vu[..10]))
    .bind(c.dueno)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    let vd: i64 = sqlx::query_scalar(
        "INSERT INTO venta_detalle (uuid, venta_id, producto_id, cantidad, precio_original, precio_final, subtotal) \
         VALUES ($1, $2, $3, 2, 80, 80, 160) RETURNING id",
    )
    .bind(uuid::Uuid::new_v4().simple().to_string())
    .bind(vid)
    .bind(p)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    let args = json!({"datos": {"venta_id": vid, "usuario_id": c.dueno, "motivo": "prueba WP5b",
                                "items": [{"venta_detalle_id": vd, "cantidad": 1.0}]}});

    // Con FKs crudas del escritorio: no (devolvería stock a otro producto).
    for tok in [&c.tok_dueno, &tok_espejo] {
        let e = rpc(&c, tok, "crear_devolucion", args.clone()).await.unwrap_err();
        assert_eq!(msg(e), super::MSG_VENTA_SIN_SINCRONIZAR);
    }
    // Un equipo con caja propia nunca devuelve ventas de la tienda.
    let e = rpc(&c, &tok_ind, "crear_devolucion", args.clone()).await.unwrap_err();
    assert!(msg(e).contains("caja propia"));

    // Ya traducida por el sync v2: el equipo espejo sí la devuelve, y el
    // dinero sale de la caja de la tienda.
    sqlx::query("INSERT INTO sync_fk_estado (tabla, uuid, estado) VALUES ('ventas', $1, 'traducido')")
        .bind(&vu)
        .execute(&c.st.pool)
        .await
        .unwrap();
    let d = rpc(&c, &tok_espejo, "crear_devolucion", args.clone()).await.unwrap();
    assert_eq!(d["total_devuelto"].as_f64(), Some(80.0));
    let m = sqlx::query(
        "SELECT m.caja, m.origen, m.tipo, m.fecha, dv.fecha AS fecha_dev FROM devoluciones dv \
         JOIN movimientos_caja m ON m.id = dv.movimiento_caja_id WHERE dv.id = $1",
    )
    .bind(d["id"].as_i64().unwrap())
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    assert_eq!(m.get::<String, _>("caja"), "principal");
    assert_eq!(m.get::<String, _>("origen"), "web");
    assert_eq!(m.get::<String, _>("fecha"), m.get::<String, _>("fecha_dev"));
    assert_eq!(
        sqlx::query_scalar::<_, f64>("SELECT stock_actual::float8 FROM productos WHERE id = $1")
            .bind(p).fetch_one(&c.st.pool).await.unwrap(),
        6.0
    );

    // Venta de la caja web devuelta desde el equipo individual: el reembolso
    // sale de la caja web; con tarjeta no sale nada.
    let r = rpc(&c, &tok_ind, "crear_venta", venta(c.dueno, vec![linea(p, 80.0, 1.0)], "tarjeta")).await.unwrap();
    let wvd: i64 = sqlx::query_scalar("SELECT id FROM venta_detalle WHERE venta_id = $1")
        .bind(r["id"].as_i64().unwrap())
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
    let d = rpc(&c, &tok_ind, "crear_devolucion", json!({"datos": {"venta_id": r["id"], "motivo": "tarjeta",
        "items": [{"venta_detalle_id": wvd, "cantidad": 1.0}]}})).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT movimiento_caja_id FROM devoluciones WHERE id = $1")
            .bind(d["id"].as_i64().unwrap()).fetch_one(&c.st.pool).await.unwrap(),
        None
    );
    let r = rpc(&c, &tok_ind, "crear_venta", venta(c.dueno, vec![linea(p, 80.0, 1.0)], "efectivo")).await.unwrap();
    let wvd: i64 = sqlx::query_scalar("SELECT id FROM venta_detalle WHERE venta_id = $1")
        .bind(r["id"].as_i64().unwrap())
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
    let d = rpc(&c, &tok_ind, "crear_devolucion", json!({"datos": {"venta_id": r["id"], "motivo": "efectivo",
        "items": [{"venta_detalle_id": wvd, "cantidad": 1.0}]}})).await.unwrap();
    let caja_mov: String = sqlx::query_scalar(
        "SELECT m.caja FROM devoluciones dv JOIN movimientos_caja m ON m.id = dv.movimiento_caja_id WHERE dv.id = $1",
    )
    .bind(d["id"].as_i64().unwrap())
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    assert_eq!(caja_mov, "web");

    // El detalle de la venta trae caja, origen y lo devuelto.
    let det = rpc(&c, &tok_ind, "obtener_detalle_venta", json!({"ventaId": r["id"]})).await.unwrap();
    assert_eq!(det["caja"].as_str(), Some("web"));
    assert_eq!(det["origen"].as_str(), Some("web"));
    assert_eq!(det["total_devuelto"].as_f64(), Some(80.0));

    cortar_web(&c.st, &tok_ind, Some(0.0), None).await;
}

#[tokio::test]
async fn modo_de_caja_dispositivo_nuevo_y_cambio_de_modo() {
    let Some(c) = ctx().await else { return };
    let _g = candado_caja_web().lock().await;

    // Equipo nuevo: entra en espejo (caja de la tienda) y pide configurar.
    let dev = format!("wp5b-nuevo-{}", uuid::Uuid::now_v7());
    let l = rpc_cliente(&c.st, "", "login_pin", json!({"pin": PIN_DUENO, "deviceUuid": dev}), None)
        .await
        .unwrap();
    assert_eq!(l["ok"], json!(true));
    assert_eq!(l["usuario"]["id"].as_i64(), Some(c.dueno));
    assert_eq!(l["modo_caja"].as_str(), Some("espejo"));
    assert_eq!(l["modo_configurado"], json!(false));
    assert_eq!(l["caja"].as_str(), Some("principal"));
    assert_eq!(l["cortes_en_este_equipo"], json!(false));
    assert!(l["escritorio_recibe_web"].is_boolean());
    let tok = l["token"].as_str().unwrap().to_string();
    let guardado: String = sqlx::query_scalar("SELECT modo_caja FROM pos_devices WHERE device_uuid = $1")
        .bind(&dev)
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
    assert_eq!(guardado, "espejo");

    // espejo → individual: siempre.
    let m = rpc(&c, &tok, "configurar_modo_caja", json!({"modo": "individual"})).await.unwrap();
    assert_eq!((m["caja"].as_str(), m["cortes_en_este_equipo"].as_bool()), (Some("web"), Some(true)));

    // Con dinero sin cortar en la caja web no puede volver a espejo.
    cortar_web(&c.st, &tok, Some(0.0), None).await;
    let p = producto(&c, 30.0, 5.0).await;
    rpc(&c, &tok, "crear_venta", venta(c.dueno, vec![linea(p, 30.0, 1.0)], "efectivo")).await.unwrap();
    let e = rpc(&c, &tok, "configurar_modo_caja", json!({"modo": "espejo"})).await.unwrap_err();
    assert!(msg(e).contains("dinero sin cortar"));
    cortar_web(&c.st, &tok, Some(0.0), None).await;
    let m = rpc(&c, &tok, "configurar_modo_caja", json!({"modo": "espejo"})).await.unwrap();
    assert_eq!(m["caja"].as_str(), Some("principal"));

    // Historial sin filtro por modo: un equipo individual ve las ventas de la
    // tienda, cada una con su caja y origen.
    let dev_ind = dispositivo(&c, "individual").await;
    let tok_ind = token(c.dueno, "wp5b_dueno", true, Some(&dev_ind));
    let r = rpc(&c, &c.tok_dueno, "crear_venta", venta(c.dueno, vec![linea(p, 30.0, 1.0)], "efectivo")).await.unwrap();
    let folio = r["folio"].as_str().unwrap();
    let hoy = &r["fecha"].as_str().unwrap()[..10];
    let lista = rpc(&c, &tok_ind, "buscar_ventas", json!({"folio": folio, "fechaInicio": hoy, "fechaFin": hoy}))
        .await
        .unwrap();
    assert_eq!(lista.as_array().unwrap().len(), 1);
    assert_eq!(lista[0]["caja"].as_str(), Some("principal"));
    assert_eq!(lista[0]["origen"].as_str(), Some("web"));
    let n = rpc(&c, &tok_ind, "contar_ventas", json!({"folio": folio})).await.unwrap();
    assert_eq!(n["total"].as_i64(), Some(1));
}

// ─── Ronda 2: sesiones sin dispositivo y frontera de la caja web ──────────

/// Claims de un token emitido por el login (para revisar el claim `device`).
fn claims_de(tok: &str) -> crate::auth::Claims {
    let mut h = HeaderMap::new();
    h.insert("authorization", HeaderValue::from_str(&format!("Bearer {tok}")).unwrap());
    crate::auth::autenticar(&h, SECRETO).unwrap()
}

#[tokio::test]
async fn sesion_web_sin_dispositivo_lleva_el_centinela_y_es_de_la_tienda() {
    let Some(c) = ctx().await else { return };
    let filas_falsas = |c: &Ctx| {
        let pool = c.st.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*)::bigint FROM pos_devices WHERE trim(device_uuid) IN ('', $1)",
            )
            .bind(caja::DISPOSITIVO_SIN_EQUIPO)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let antes = filas_falsas(&c).await;

    // Sin deviceUuid, con uno vacío o con el propio centinela (y por
    // contraseña): el token NUNCA sale sin dispositivo (un token de rol admin
    // sin dispositivo es lo que /sync/* acepta como el escritorio).
    let logins = [
        ("login_pin", json!({"pin": PIN_DUENO})),
        ("login_pin", json!({"pin": PIN_DUENO, "deviceUuid": ""})),
        ("login_pin", json!({"pin": PIN_DUENO, "deviceUuid": caja::DISPOSITIVO_SIN_EQUIPO})),
        ("login_password", json!({"nombreUsuario": "wp5b_dueno", "password": "no-usar"})),
    ];
    let mut tok_sin_equipo = String::new();
    for (cmd, args) in logins {
        let l = rpc_cliente(&c.st, "", cmd, args.clone(), None).await.unwrap();
        assert_eq!(l["ok"], json!(true), "{cmd} {args}");
        assert_eq!(l["usuario"]["id"].as_i64(), Some(c.dueno));
        assert_eq!(l["modo_caja"].as_str(), Some("espejo"));
        assert_eq!(l["modo_configurado"], json!(true), "sin equipo no hay modal de bienvenida");
        assert_eq!(l["caja"].as_str(), Some("principal"));
        assert_eq!(l["cortes_en_este_equipo"], json!(false));
        let tok = l["token"].as_str().unwrap().to_string();
        let cl = claims_de(&tok);
        assert_eq!(cl.role, "admin");
        assert_eq!(cl.device.as_deref(), Some(caja::DISPOSITIVO_SIN_EQUIPO), "{cmd} {args}");
        assert!(cl.device.is_some(), "un token web nunca va sin dispositivo");
        tok_sin_equipo = tok;
    }
    // El vendedor también (rol 'device').
    let l = rpc_cliente(&c.st, "", "login_pin", json!({"pin": PIN_VENDEDOR}), None).await.unwrap();
    let cl = claims_de(l["token"].as_str().unwrap());
    assert_eq!((cl.role.as_str(), cl.device.as_deref()), ("device", Some(caja::DISPOSITIVO_SIN_EQUIPO)));
    assert_eq!(filas_falsas(&c).await, antes, "el centinela no se registra como equipo");

    // La sesión centinela trabaja con la caja de la tienda y no tiene modo propio.
    let m = rpc(&c, &tok_sin_equipo, "obtener_modo_caja", json!({})).await.unwrap();
    assert_eq!(m["device_uuid"], Value::Null);
    assert_eq!((m["modo"].as_str(), m["caja"].as_str()), (Some("espejo"), Some("principal")));
    assert_eq!(m["configurado"], json!(true));
    let e = rpc(&c, &tok_sin_equipo, "configurar_modo_caja", json!({"modo": "individual"})).await.unwrap_err();
    assert!(msg(e).contains("no tiene un dispositivo registrado"));
    assert_eq!(filas_falsas(&c).await, antes, "configurar_modo_caja no le guarda modo al centinela");
    let e = rpc(&c, &tok_sin_equipo, "crear_apertura_caja",
        json!({"datos": {"usuario_id": 0, "fondo_declarado": 0.0}})).await.unwrap_err();
    assert_eq!(msg(e), caja::MSG_PRINCIPAL_SOLO_ESCRITORIO);
    let s = claims_de(&tok_sin_equipo);
    assert_eq!(caja::caja_de(&c.st.pool, &s).await.unwrap(), Caja::Principal);
    assert_eq!(caja::modo_de_dispositivo(&c.st.pool, &s).await.unwrap(), "espejo");

    // Aunque existiera un pos_devices con el nombre del centinela en modo
    // individual (p. ej. creado a mano), la sesión sin equipo sigue en la
    // caja de la tienda: el centinela nunca se busca en pos_devices.
    assert!(caja::dispositivo_de(&s).is_none());
    if antes == 0 {
        sqlx::query(
            "INSERT INTO pos_devices (device_uuid, sucursal_id, nombre, modo_caja) VALUES ($1, 1, 'x', 'individual')",
        )
        .bind(caja::DISPOSITIVO_SIN_EQUIPO)
        .execute(&c.st.pool)
        .await
        .unwrap();
        let caja_sesion = caja::caja_de(&c.st.pool, &s).await.unwrap();
        let m = rpc(&c, &tok_sin_equipo, "obtener_modo_caja", json!({})).await.unwrap();
        sqlx::query("DELETE FROM pos_devices WHERE device_uuid = $1")
            .bind(caja::DISPOSITIVO_SIN_EQUIPO)
            .execute(&c.st.pool)
            .await
            .unwrap();
        assert_eq!(caja_sesion, Caja::Principal);
        assert_eq!((m["modo"].as_str(), m["caja"].as_str()), (Some("espejo"), Some("principal")));
    }

    // Con un equipo real el claim es ese equipo.
    let dev = format!("wp5b-r2-{}", uuid::Uuid::now_v7());
    let l = rpc_cliente(&c.st, "", "login_pin", json!({"pin": PIN_DUENO, "deviceUuid": dev}), None)
        .await
        .unwrap();
    assert_eq!(claims_de(l["token"].as_str().unwrap()).device.as_deref(), Some(dev.as_str()));
    assert_eq!(l["modo_configurado"], json!(false), "equipo nuevo: modal de bienvenida");
}

/// Pone la marca 'caja_v2_desde' (dentro de la transacción del escenario).
async fn marcar_caja_v2(conn: &mut PgConnection, cuando: &str) {
    sqlx::query(
        "INSERT INTO sync_meta (clave, valor) VALUES ('caja_v2_desde', $1) \
         ON CONFLICT (clave) DO UPDATE SET valor = EXCLUDED.valor",
    )
    .bind(cuando)
    .execute(&mut *conn)
    .await
    .unwrap();
}

/// Apertura web del modelo viejo (modal diario) que la migración 010 pasó a
/// la caja web.
async fn apertura_vieja(conn: &mut PgConnection, c: &Ctx, fecha: &str, fondo: f64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO aperturas_caja (uuid, sucursal_id, usuario_id, fondo_declarado, fecha, origen, caja, updated_at) \
         VALUES ($1, 1, $2, $3, $4, 'web', 'web', $4) RETURNING id",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(c.dueno)
    .bind(fondo)
    .bind(fecha)
    .fetch_one(&mut *conn)
    .await
    .unwrap()
}

#[tokio::test]
async fn aperturas_web_del_modelo_viejo_no_definen_el_fondo_del_primer_corte() {
    let Some(c) = ctx().await else { return };
    let (mut tx, p) = escenario(&c).await;
    // El servidor nuevo arrancó el día 7 a las 09:00.
    marcar_caja_v2(&mut tx, "2026-10-07 09:00:00").await;
    assert_eq!(
        caja::aperturas_validas_desde(&mut tx, Caja::Web).await.unwrap().as_deref(),
        Some("2026-10-07 09:00:00")
    );
    assert_eq!(caja::aperturas_validas_desde(&mut tx, Caja::Principal).await.unwrap(), None);

    // Aperturas del modal diario viejo, incluida una de ESE MISMO día antes
    // del cambio.
    apertura_vieja(&mut tx, &c, "2026-04-24 21:35:52", 0.0).await;
    apertura_vieja(&mut tx, &c, "2026-07-26 10:00:00", 2000.0).await;
    apertura_vieja(&mut tx, &c, "2026-10-07 08:15:00", 0.0).await;

    // Ninguna cuenta como la de hoy: el equipo (ya en modo individual) puede
    // declarar su fondo.
    assert!(caja::apertura_del_dia(&mut tx, Caja::Web, "2026-10-07").await.unwrap().is_none());
    let esp = caja::calcular_esperado_apertura(&mut tx, Caja::Web, "2026-10-07 10:00:00").await.unwrap();
    assert!(!esp.hay_referencia, "sin cortes web no hay contra qué verificar");
    let a = abrir(&mut tx, &c, "2026-10-07 10:00:00", 500.0, None).await.unwrap();
    assert_eq!(a.fondo_declarado, 500.0);
    assert_eq!(caja::apertura_del_dia(&mut tx, Caja::Web, "2026-10-07").await.unwrap().map(|x| x.id), Some(a.id));
    let otra = abrir(&mut tx, &c, "2026-10-07 11:00:00", 500.0, None).await;
    assert_eq!(otra.unwrap_err(), "Ya existe una apertura de caja para hoy");

    venta_web(&mut tx, &c, p, "2026-10-07 10:05:00", 60.0, "efectivo").await;
    let d = caja::calcular_periodo(&mut tx, Caja::Web, "2026-10-07 10:10:00").await.unwrap();
    assert_eq!(d.fecha_inicio, caja::INICIO_SIN_CORTES);
    assert_eq!(d.fondo_inicial, 500.0, "el fondo es el de la apertura de hoy, no el de abril");
    assert_eq!(d.efectivo_esperado, 560.0);

    // Sin la marca (comportamiento anterior) el fondo salía de la apertura
    // más vieja ($0): justo el descuadre que se corrige.
    sqlx::query("DELETE FROM sync_meta WHERE clave = 'caja_v2_desde'").execute(&mut *tx).await.unwrap();
    assert_eq!(esperado(&mut tx, "2026-10-07 10:10:00").await, 60.0);
    marcar_caja_v2(&mut tx, "2026-10-07 09:00:00").await;

    // El primer corte cuadra con lo que hay en el cajón.
    let k = corte(&mut tx, &c, "DIA", "2026-10-07 20:00:00", 560.0, 500.0).await;
    assert_eq!((k.efectivo_esperado, k.diferencia), (560.0, 0.0));
    assert_eq!(caja::detalle_corte(&mut tx, k.id).await.unwrap().corte.fondo_inicial, 500.0);
    // Ya con corte, la cadena sigue normal.
    assert_eq!(esperado_apertura(&mut tx, "2026-10-08 08:00:00").await, 500.0);
}

/// Deja la caja web como recién desplegada: sin cortes, ventas ni
/// movimientos, y solo con las aperturas anteriores a 'caja_v2_desde'
/// (borrado lógico, confirmado; corre con `candado_caja_web()` tomado).
/// Devuelve la marca.
async fn caja_web_recien_desplegada(c: &Ctx) -> String {
    let mut tx = c.st.pool.begin().await.unwrap();
    caja::bloquear(&mut tx, Caja::Web).await.unwrap();
    let desde = caja::aperturas_validas_desde(&mut tx, Caja::Web)
        .await
        .unwrap()
        .expect("la migración 011 deja la marca caja_v2_desde");
    for sql in [
        "UPDATE cortes SET deleted_at = '2000-01-01 00:00:00' WHERE caja = 'web' AND deleted_at IS NULL",
        "UPDATE movimientos_caja SET deleted_at = '2000-01-01 00:00:00' WHERE caja = 'web' AND deleted_at IS NULL",
        "UPDATE ventas SET deleted_at = '2000-01-01 00:00:00' WHERE caja = 'web' AND deleted_at IS NULL",
    ] {
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
    }
    sqlx::query(
        "UPDATE aperturas_caja SET deleted_at = '2000-01-01 00:00:00' \
         WHERE caja = 'web' AND deleted_at IS NULL AND replace(substr(fecha, 1, 19), 'T', ' ') >= $1",
    )
    .bind(&desde)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    desde
}

#[tokio::test]
async fn equipo_individual_tras_el_despliegue_abre_con_su_fondo_por_rpc() {
    let Some(c) = ctx().await else { return };
    let _g = candado_caja_web().lock().await;
    let desde = caja_web_recien_desplegada(&c).await;

    // Aperturas web viejas: la más antigua de todas con $777 (si contara, el
    // fondo sería 777) y otra un segundo antes del cambio (si es de hoy, el
    // equipo no podría abrir).
    let un_segundo_antes = chrono::NaiveDateTime::parse_from_str(&desde, "%Y-%m-%d %H:%M:%S")
        .map(|t| (t - chrono::Duration::seconds(1)).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap();
    {
        let mut conn = c.st.pool.acquire().await.unwrap();
        apertura_vieja(&mut conn, &c, "2000-01-01 00:00:00", 777.0).await;
        apertura_vieja(&mut conn, &c, &un_segundo_antes, 0.0).await;
    }

    // El dueño pasa un equipo espejo a individual.
    let dev = dispositivo(&c, "espejo").await;
    let tok = token(c.dueno, "wp5b_dueno", true, Some(&dev));
    let m = rpc(&c, &tok, "configurar_modo_caja", json!({"modo": "individual"})).await.unwrap();
    assert_eq!(m["caja"].as_str(), Some("web"));

    assert_eq!(rpc(&c, &tok, "obtener_apertura_hoy", json!({})).await.unwrap(), Value::Null);
    let esp = rpc(&c, &tok, "obtener_esperado_apertura", json!({})).await.unwrap();
    assert_eq!(esp["hay_referencia"], json!(false));
    let a = rpc(&c, &tok, "crear_apertura_caja",
        json!({"datos": {"usuario_id": 0, "fondo_declarado": 500.0}})).await.unwrap();
    assert_eq!(a["fondo_declarado"].as_f64(), Some(500.0));
    let hoy = rpc(&c, &tok, "obtener_apertura_hoy", json!({})).await.unwrap();
    assert_eq!(hoy["id"], a["id"]);

    let p = producto(&c, 60.0, 5.0).await;
    let v = rpc(&c, &tok, "crear_venta", venta(c.dueno, vec![linea(p, 60.0, 1.0)], "efectivo")).await.unwrap();
    let caja_v: String = sqlx::query_scalar("SELECT caja FROM ventas WHERE id = $1")
        .bind(v["id"].as_i64().unwrap())
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
    assert_eq!(caja_v, "web");

    let d = rpc(&c, &tok, "calcular_datos_corte", json!({})).await.unwrap();
    assert_eq!(d["fecha_inicio"].as_str(), Some(caja::INICIO_SIN_CORTES));
    assert_eq!(d["fondo_inicial"].as_f64(), Some(500.0));
    assert_eq!(d["efectivo_esperado"].as_f64(), Some(560.0));

    // El primer corte web arranca con ese fondo y cuadra.
    let k = cortar_web(&c.st, &tok, Some(0.0), None).await;
    assert_eq!(k["efectivo_esperado"].as_f64(), Some(560.0));
    assert_eq!(k["diferencia"].as_f64(), Some(0.0));
    let fi: f64 = sqlx::query_scalar("SELECT fondo_inicial::float8 FROM cortes WHERE id = $1")
        .bind(k["id"].as_i64().unwrap())
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
    assert_eq!(fi, 500.0);
}
