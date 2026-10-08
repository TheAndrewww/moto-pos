// rpc_precios_tests.rs — Pruebas de integración (Postgres) de las RPC web que
// mueven dinero: precios del servidor, identidad de la sesión, PIN del dueño,
// folios, devoluciones, anulación, recepción y productos.
//
// Se SALTAN si no hay TEST_DATABASE_URL. Deben correr contra una COPIA local
// de producción, nunca contra producción:
//   psql -h localhost -p 54329 -U postgres -d postgres \
//        -c "CREATE DATABASE wp5a_test TEMPLATE prodcopy_base"
//   TEST_DATABASE_URL=postgres://postgres@localhost:54329/wp5a_test cargo test
//
// Cada prueba crea sus propios productos/clientes/ventas. Los usuarios de
// prueba (dueño y vendedor) se reutilizan entre corridas (por nombre_usuario).

use super::*;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue};
use axum::Json;
use sqlx::postgres::PgPoolOptions;
use std::sync::atomic::{AtomicBool, Ordering};

const SECRETO: &[u8] = b"secreto-pruebas-wp5a";
const PIN_DUENO: &str = "7391";
const PIN_VENDEDOR: &str = "2846";

static PINES_VERIFICADOS: AtomicBool = AtomicBool::new(false);

struct Ctx {
    st: AppState,
    dueno: i64,
    vendedor: i64,
    tok_dueno: String,
    tok_vendedor: String,
    max_vendedor: f64,
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

/// Crea (o deja al día) un usuario de prueba con ese PIN y rol.
async fn usuario_prueba(
    conn: &mut sqlx::PgConnection,
    nombre_usuario: &str,
    nombre: &str,
    es_admin: bool,
    pin: &str,
) -> i64 {
    let rol_id: i64 = sqlx::query_scalar(
        "SELECT id FROM roles WHERE es_admin = $1 ORDER BY id LIMIT 1",
    )
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
    let pool = PgPoolOptions::new().max_connections(25).connect(&url).await.unwrap();
    if let Err(e) = crate::db::run_migrations(&pool).await {
        eprintln!("aviso: no se aplicaron las migraciones en la base de prueba: {e}");
    }

    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(7301199)").execute(&mut *tx).await.unwrap();
    let dueno = usuario_prueba(&mut tx, "wp5a_dueno", "Dueño de prueba WP5a", true, PIN_DUENO).await;
    let vendedor =
        usuario_prueba(&mut tx, "wp5a_vendedor", "Vendedor de prueba WP5a", false, PIN_VENDEDOR).await;
    if !PINES_VERIFICADOS.load(Ordering::SeqCst) {
        // Ningún OTRO dueño activo puede tener estos PINs (si no, el servidor
        // podría resolver a otro dueño y las aserciones no serían confiables).
        let otros = sqlx::query(
            "SELECT u.id, u.pin FROM usuarios u JOIN roles r ON r.id = u.rol_id \
             WHERE r.es_admin = 1 AND u.activo = 1 AND u.deleted_at IS NULL AND u.id <> $1",
        )
        .bind(dueno)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
        for r in &otros {
            let h: String = r.get("pin");
            for pin in [PIN_DUENO, PIN_VENDEDOR] {
                assert!(
                    !bcrypt::verify(pin, &h).unwrap_or(false),
                    "el PIN de prueba {pin} coincide con el del dueño id={}; cambia las constantes",
                    r.get::<i64, _>("id")
                );
            }
        }
        PINES_VERIFICADOS.store(true, Ordering::SeqCst);
    }
    tx.commit().await.unwrap();

    let st = AppState { pool, jwt_secret: SECRETO.to_vec() };
    let max_vendedor = max_descuento_vendedor(&st).await.unwrap();
    Some(Ctx {
        st,
        dueno,
        vendedor,
        tok_dueno: token(dueno, "wp5a_dueno", true, None),
        tok_vendedor: token(vendedor, "wp5a_vendedor", false, None),
        max_vendedor,
    })
}

async fn rpc_con(st: &AppState, tok: &str, cmd: &str, args: Value) -> Result<Value, ApiError> {
    let mut h = HeaderMap::new();
    h.insert("authorization", HeaderValue::from_str(&format!("Bearer {tok}")).unwrap());
    h.insert("x-pos-cliente", HeaderValue::from_static("2"));
    dispatch(State(st.clone()), h, Path(cmd.to_string()), Json(args))
        .await
        .map(|Json(v)| v)
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

async fn producto(c: &Ctx, precio: f64, stock: f64) -> i64 {
    let u = uuid::Uuid::now_v7().to_string();
    sqlx::query_scalar(
        "INSERT INTO productos (uuid, codigo, nombre, precio_costo, precio_venta, stock_actual) \
         VALUES ($1, $2, $3, 1, $4, $5) RETURNING id",
    )
    .bind(&u)
    .bind(format!("WP5A-{}", &u[u.len() - 12..]))
    .bind(format!("Producto prueba WP5a {}", &u[u.len() - 6..]))
    .bind(precio)
    .bind(stock)
    .fetch_one(&c.st.pool)
    .await
    .unwrap()
}

async fn escalar_f64(c: &Ctx, sql: &str, id: i64) -> f64 {
    sqlx::query_scalar::<_, f64>(sql).bind(id).fetch_one(&c.st.pool).await.unwrap()
}

async fn escalar_i64(c: &Ctx, sql: &str, id: i64) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).bind(id).fetch_one(&c.st.pool).await.unwrap()
}

/// Partida calculada exactamente como ventaStore.ts.
fn linea(producto_id: i64, precio: f64, pct: f64, cant: f64) -> Value {
    let dm = precio * (pct / 100.0);
    let pf = precio - dm;
    json!({
        "producto_id": producto_id, "cantidad": cant, "precio_original": precio,
        "descuento_porcentaje": pct, "descuento_monto": dm, "precio_final": pf,
        "subtotal": pf * cant,
    })
}

fn venta(usuario_id: i64, cliente_id: Option<i64>, lineas: Vec<Value>, metodo: &str) -> Value {
    let raw: f64 = lineas.iter().map(|l| l["subtotal"].as_f64().unwrap()).sum();
    let desc: f64 = lineas
        .iter()
        .map(|l| l["descuento_monto"].as_f64().unwrap() * l["cantidad"].as_f64().unwrap())
        .sum();
    let total = crate::venta_calc::total_a_cobrar(raw);
    json!({ "venta": {
        "usuario_id": usuario_id, "cliente_id": cliente_id,
        "subtotal": raw, "descuento": desc, "total": total,
        "metodo_pago": metodo, "monto_recibido": total, "cambio": 0.0,
        "items": lineas,
    }})
}

/// Venta "del escritorio" insertada directo (origen 'desktop', uuid hex32).
async fn venta_escritorio(c: &Ctx, producto_id: i64, cant: f64, precio: f64) -> (i64, i64) {
    let u = uuid::Uuid::new_v4().simple().to_string();
    let vid: i64 = sqlx::query_scalar(
        "INSERT INTO ventas (uuid, sucursal_id, folio, usuario_id, subtotal, total, metodo_pago, \
                             monto_recibido, origen) \
         VALUES ($1, 1, $2, $3, $4, $4, 'efectivo', $4, 'desktop') RETURNING id",
    )
    .bind(&u)
    .bind(format!("V-T5A-{}", &u[..10]))
    .bind(c.dueno)
    .bind(cant * precio)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    let vdid: i64 = sqlx::query_scalar(
        "INSERT INTO venta_detalle (uuid, venta_id, producto_id, cantidad, precio_original, \
                                    precio_final, subtotal) \
         VALUES ($1, $2, $3, $4, $5, $5, $6) RETURNING id",
    )
    .bind(uuid::Uuid::new_v4().simple().to_string())
    .bind(vid)
    .bind(producto_id)
    .bind(cant)
    .bind(precio)
    .bind(cant * precio)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    (vid, vdid)
}

async fn detalle_ids(c: &Ctx, venta_id: i64) -> Vec<i64> {
    sqlx::query_scalar("SELECT id FROM venta_detalle WHERE venta_id = $1 ORDER BY id")
        .bind(venta_id)
        .fetch_all(&c.st.pool)
        .await
        .unwrap()
}

// -----------------------------------------------------------------------------

#[tokio::test]
async fn venta_con_precio_alterado_da_precio_cambio_y_sin_bitacora() {
    let Some(c) = ctx().await else { return };
    let p = producto(&c, 296.0, 5.0).await;

    // El navegador manda $1 en vez de $296.
    let e = rpc(&c, &c.tok_vendedor, "crear_venta", venta(c.vendedor, None, vec![linea(p, 1.0, 0.0, 1.0)], "efectivo"))
        .await
        .unwrap_err();
    assert!(msg(e).starts_with("PRECIO_CAMBIO"), "debe pedir refrescar precios");

    // Total manipulado con precio correcto → TOTALES_INCONSISTENTES.
    let mut v = venta(c.vendedor, None, vec![linea(p, 296.0, 0.0, 1.0)], "efectivo");
    v["venta"]["total"] = json!(29.0);
    v["venta"]["monto_recibido"] = json!(29.0);
    let e = rpc(&c, &c.tok_vendedor, "crear_venta", v).await.unwrap_err();
    assert!(msg(e).starts_with("TOTALES_INCONSISTENTES"));

    // Precio correcto: pasa; usuario = sesión aunque el body diga otro.
    let r = rpc(&c, &c.tok_vendedor, "crear_venta", venta(c.dueno, None, vec![linea(p, 296.0, 0.0, 2.0)], "efectivo"))
        .await
        .unwrap();
    let vid = r["id"].as_i64().unwrap();
    assert_eq!(r["total"].as_f64().unwrap(), 592.0);
    assert_eq!(escalar_i64(&c, "SELECT usuario_id FROM ventas WHERE id = $1", vid).await, c.vendedor);
    assert_eq!(escalar_f64(&c, "SELECT total::float8 FROM ventas WHERE id = $1", vid).await, 592.0);
    // Sin fila 'VENTA' en la bitácora (bajaría al escritorio con ids crudos).
    assert_eq!(
        escalar_i64(&c, "SELECT COUNT(*)::bigint FROM audit_log WHERE tabla_afectada = 'ventas' AND registro_id = $1", vid).await,
        0
    );
    // Folio web y cursor de sync de la venta.
    let folio = r["folio"].as_str().unwrap().to_string();
    assert!(folio.starts_with("V-") && folio.len() == 15, "folio {folio}");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM sync_cursor sc JOIN ventas v ON v.uuid = sc.uuid \
             WHERE sc.tabla = 'ventas' AND v.id = $1")
        .bind(vid).fetch_one(&c.st.pool).await.unwrap(),
        1
    );
}

#[tokio::test]
async fn descuento_arriba_del_limite_requiere_token_valido() {
    let Some(c) = ctx().await else { return };
    let p = producto(&c, 100.0, 5.0).await;
    let p2 = producto(&c, 100.0, 5.0).await;
    let pct = (c.max_vendedor + 15.0).min(100.0);

    // Sin token → rechazado.
    let e = rpc(&c, &c.tok_vendedor, "crear_venta", venta(c.vendedor, None, vec![linea(p, 100.0, pct, 1.0)], "efectivo"))
        .await
        .unwrap_err();
    assert!(msg(e).starts_with("DESCUENTO_NO_AUTORIZADO"));

    // Dentro del límite → pasa sin autorización.
    let r = rpc(&c, &c.tok_vendedor, "crear_venta",
        venta(c.vendedor, None, vec![linea(p, 100.0, c.max_vendedor.min(10.0), 1.0)], "efectivo"))
        .await
        .unwrap();
    let ids = detalle_ids(&c, r["id"].as_i64().unwrap()).await;
    assert_eq!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT autorizado_por FROM venta_detalle WHERE id = $1")
            .bind(ids[0]).fetch_one(&c.st.pool).await.unwrap(),
        None
    );

    // El dueño autoriza con su PIN → token.
    let a = rpc(&c, &c.tok_vendedor, "autorizar_descuento",
        json!({"pin": PIN_DUENO, "productoId": p, "porcentaje": pct}))
        .await
        .unwrap();
    assert_eq!(a["ok"], json!(true));
    assert_eq!(a["autorizado_por"].as_i64(), Some(c.dueno));
    let tok_aut = a["token"].as_str().unwrap().to_string();

    // Token de otro producto → rechazado.
    let mut l2 = linea(p2, 100.0, pct, 1.0);
    l2["autorizacion_token"] = json!(tok_aut);
    let e = rpc(&c, &c.tok_vendedor, "crear_venta", venta(c.vendedor, None, vec![l2], "efectivo"))
        .await
        .unwrap_err();
    assert!(msg(e).starts_with("DESCUENTO_NO_AUTORIZADO"));

    // Cajero admin: hasta 100% sin token (el token del vendedor no le sirve
    // ni le hace falta) y la partida queda autorizada por él mismo.
    let mut l3 = linea(p, 100.0, 100.0, 1.0);
    l3["autorizacion_token"] = json!(tok_aut);
    let r = rpc(&c, &c.tok_dueno, "crear_venta", venta(c.dueno, None, vec![l3], "efectivo")).await.unwrap();
    let ids = detalle_ids(&c, r["id"].as_i64().unwrap()).await;
    assert_eq!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT autorizado_por FROM venta_detalle WHERE id = $1")
            .bind(ids[0]).fetch_one(&c.st.pool).await.unwrap(),
        Some(c.dueno)
    );

    // Más % que lo autorizado → rechazado.
    if pct < 100.0 {
        let mut l4 = linea(p, 100.0, (pct + 5.0).min(100.0), 1.0);
        l4["autorizacion_token"] = json!(tok_aut);
        let e = rpc(&c, &c.tok_vendedor, "crear_venta", venta(c.vendedor, None, vec![l4], "efectivo"))
            .await
            .unwrap_err();
        assert!(msg(e).starts_with("DESCUENTO_NO_AUTORIZADO"));
    }

    // Con el token correcto → pasa y autorizado_por = el dueño.
    let mut l = linea(p, 100.0, pct, 1.0);
    l["autorizacion_token"] = json!(tok_aut);
    let r = rpc(&c, &c.tok_vendedor, "crear_venta", venta(c.vendedor, None, vec![l], "efectivo"))
        .await
        .unwrap();
    let ids = detalle_ids(&c, r["id"].as_i64().unwrap()).await;
    assert_eq!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT autorizado_por FROM venta_detalle WHERE id = $1")
            .bind(ids[0]).fetch_one(&c.st.pool).await.unwrap(),
        Some(c.dueno)
    );
}

#[tokio::test]
async fn pin_de_vendedor_ya_no_autoriza() {
    let Some(c) = ctx().await else { return };
    let p = producto(&c, 100.0, 5.0).await;

    assert_eq!(rpc(&c, &c.tok_vendedor, "verificar_pin_dueno", json!({"pin": PIN_VENDEDOR})).await.unwrap(), json!(false));
    assert_eq!(rpc(&c, &c.tok_vendedor, "verificar_pin_dueno", json!({"pin": PIN_DUENO})).await.unwrap(), json!(true));
    assert_eq!(rpc(&c, &c.tok_vendedor, "resolver_dueno_por_pin", json!({"pin": PIN_VENDEDOR})).await.unwrap(), Value::Null);
    assert_eq!(rpc(&c, &c.tok_vendedor, "resolver_dueno_por_pin", json!({"pin": PIN_DUENO})).await.unwrap(), json!(c.dueno));

    let a = rpc(&c, &c.tok_vendedor, "autorizar_descuento",
        json!({"pin": PIN_VENDEDOR, "productoId": p, "porcentaje": 50.0}))
        .await
        .unwrap();
    assert_eq!(a["ok"], json!(false));

    // Retiro grande: el PIN de un vendedor no autoriza; el del dueño sí.
    let concepto = format!("Prueba WP5a retiro {}", uuid::Uuid::now_v7());
    let mov = |pin: &str| json!({"datos": {
        "tipo": "RETIRO", "usuario_id": c.dueno, "monto": 600.0, "concepto": concepto,
        "autorizado_por": c.vendedor, "pin_autorizacion": pin }});
    let e = rpc(&c, &c.tok_vendedor, "crear_movimiento_caja", mov(PIN_VENDEDOR)).await.unwrap_err();
    assert!(msg(e).contains("PIN del dueño incorrecto"));
    let r = rpc(&c, &c.tok_vendedor, "crear_movimiento_caja", mov(PIN_DUENO)).await.unwrap();
    assert_eq!(r["usuario_id"].as_i64(), Some(c.vendedor), "usuario = sesión, no el body");
    assert_eq!(r["autorizado_por"].as_i64(), Some(c.dueno));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn folios_unicos_con_20_ventas_concurrentes() {
    let Some(c) = ctx().await else { return };
    // Un producto distinto por venta: si compartieran producto, el candado
    // FOR UPDATE del producto ya las serializaría y la prueba no probaría
    // nada del folio.
    let p = producto(&c, 50.0, 0.0).await;
    let mut prods = vec![p];
    for _ in 1..20 {
        prods.push(producto(&c, 50.0, 0.0).await);
    }
    let mut set = tokio::task::JoinSet::new();
    for &p in &prods {
        let st = c.st.clone();
        let tok = c.tok_vendedor.clone();
        let args = venta(c.vendedor, None, vec![linea(p, 50.0, 0.0, 1.0)], "efectivo");
        set.spawn(async move { rpc_con(&st, &tok, "crear_venta", args).await });
    }
    let mut folios = std::collections::HashSet::new();
    while let Some(r) = set.join_next().await {
        let v = r.unwrap().unwrap_or_else(|e| panic!("venta concurrente falló: {}", msg(e)));
        assert!(folios.insert(v["folio"].as_str().unwrap().to_string()), "folio repetido");
    }
    assert_eq!(folios.len(), 20);
    // Ninguno de esos folios lo comparte otra venta (incluidas las de otras
    // pruebas que corren al mismo tiempo).
    let folios_v: Vec<String> = folios.into_iter().collect();
    let repetidos: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM ventas WHERE folio = ANY($1)",
    )
    .bind(&folios_v)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    assert_eq!(repetidos, 20);
    // El stock se frena en 0 (no queda negativo).
    for p in prods {
        assert_eq!(escalar_f64(&c, "SELECT stock_actual::float8 FROM productos WHERE id = $1", p).await, 0.0);
    }
}

#[tokio::test]
async fn cliente_inactivo_sigue_vendiendo_con_su_descuento() {
    let Some(c) = ctx().await else { return };
    let p = producto(&c, 99.0, 5.0).await;
    let pct_cliente = (c.max_vendedor + 5.0).min(100.0);
    let cid: i64 = sqlx::query_scalar(
        "INSERT INTO clientes (uuid, nombre, descuento_porcentaje, activo) VALUES ($1, 'Cliente inactivo WP5a', $2, 0) RETURNING id",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(pct_cliente)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();

    let r = rpc(&c, &c.tok_vendedor, "crear_venta",
        venta(c.vendedor, Some(cid), vec![linea(p, 99.0, pct_cliente, 3.0)], "tarjeta"))
        .await
        .unwrap_or_else(|e| panic!("venta a cliente inactivo rechazada: {}", msg(e)));
    let vid = r["id"].as_i64().unwrap();
    assert_eq!(escalar_i64(&c, "SELECT cliente_id FROM ventas WHERE id = $1", vid).await, cid);
    let ids = detalle_ids(&c, vid).await;
    assert_eq!(
        escalar_f64(&c, "SELECT descuento_porcentaje::float8 FROM venta_detalle WHERE id = $1", ids[0]).await,
        pct_cliente
    );
    // Tarjeta: recibido = total y cambio 0.
    let tot = escalar_f64(&c, "SELECT total::float8 FROM ventas WHERE id = $1", vid).await;
    assert_eq!(escalar_f64(&c, "SELECT monto_recibido::float8 FROM ventas WHERE id = $1", vid).await, tot);
}

#[tokio::test]
async fn devolucion_de_venta_de_escritorio_se_rechaza_en_todo_modo() {
    let Some(c) = ctx().await else { return };
    let p = producto(&c, 80.0, 5.0).await;
    let (vid, vdid) = venta_escritorio(&c, p, 2.0, 80.0).await;

    let args = json!({"datos": {"venta_id": vid, "usuario_id": c.dueno, "motivo": "prueba",
                               "items": [{"venta_detalle_id": vdid, "cantidad": 1.0}]}});
    // Sin dispositivo, en modo espejo y en modo individual. (WP5b: mientras
    // sus FKs sigan crudas —sin fila en sync_fk_estado— la venta del
    // escritorio no se devuelve aquí; un equipo con caja propia nunca
    // devuelve ventas de la tienda.)
    let mut toks = vec![(c.tok_dueno.clone(), "espejo")];
    for modo in ["espejo", "individual"] {
        let dev = format!("wp5a-{}-{}", modo, uuid::Uuid::now_v7());
        sqlx::query("INSERT INTO pos_devices (device_uuid, sucursal_id, nombre, modo_caja) VALUES ($1, 1, 'prueba', $2)")
            .bind(&dev)
            .bind(modo)
            .execute(&c.st.pool)
            .await
            .unwrap();
        toks.push((token(c.dueno, "wp5a_dueno", true, Some(&dev)), modo));
    }
    for (t, modo) in &toks {
        let e = msg(rpc(&c, t, "crear_devolucion", args.clone()).await.unwrap_err());
        if *modo == "individual" {
            assert!(e.contains("caja propia"), "{e}");
        } else {
            assert!(e.contains("todavía no está sincronizada"), "{e}");
        }
    }
    // Y la anulación tampoco (es de la caja de la tienda).
    let e = rpc(&c, &c.tok_dueno, "anular_venta",
        json!({"ventaId": vid, "usuarioId": c.dueno, "motivo": "prueba"}))
        .await
        .unwrap_err();
    assert!(msg(e).contains("escritorio"));
}

#[tokio::test]
async fn devolucion_ignora_filas_de_escritorio_que_chocan() {
    let Some(c) = ctx().await else { return };
    let p = producto(&c, 100.0, 10.0).await;
    let r = rpc(&c, &c.tok_dueno, "crear_venta",
        venta(c.dueno, None, vec![linea(p, 100.0, 0.0, 2.0)], "efectivo"))
        .await
        .unwrap();
    let vid = r["id"].as_i64().unwrap();
    let vd = detalle_ids(&c, vid).await[0];

    // Devolución "del escritorio" con ids crudos: su venta_id es OTRA venta,
    // pero su partida apunta a la de esta venta web (caso D-000309).
    let (otra, _) = venta_escritorio(&c, p, 1.0, 100.0).await;
    let dev: i64 = sqlx::query_scalar(
        "INSERT INTO devoluciones (uuid, sucursal_id, folio, venta_id, usuario_id, motivo, total_devuelto) \
         VALUES ($1, 1, $2, $3, $4, 'escritorio', 200) RETURNING id",
    )
    .bind(uuid::Uuid::new_v4().simple().to_string())
    .bind(format!("D-T5A-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]))
    .bind(otra)
    .bind(c.dueno)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO devolucion_detalle (uuid, devolucion_id, venta_detalle_id, producto_id, cantidad, precio_unitario, subtotal) \
         VALUES ($1, $2, $3, $4, 2, 100, 200)",
    )
    .bind(uuid::Uuid::new_v4().simple().to_string())
    .bind(dev)
    .bind(vd)
    .bind(p)
    .execute(&c.st.pool)
    .await
    .unwrap();

    // La pantalla muestra las 2 piezas disponibles.
    let det = rpc(&c, &c.tok_dueno, "obtener_detalle_venta", json!({"id": vid})).await.unwrap();
    assert_eq!(det["items"][0]["cantidad_disponible"].as_f64(), Some(2.0));

    // Y el servidor deja devolverlas todas, regresando exactamente el total.
    let d = rpc(&c, &c.tok_dueno, "crear_devolucion", json!({"datos": {
        "venta_id": vid, "usuario_id": c.dueno, "motivo": "prueba choque",
        "items": [{"venta_detalle_id": vd, "cantidad": 2.0}]}}))
        .await
        .unwrap_or_else(|e| panic!("devolución rechazada: {}", msg(e)));
    assert_eq!(d["total_devuelto"].as_f64(), Some(200.0));
    assert!(d["folio"].as_str().unwrap().starts_with("D-"));

    // Ya no queda nada por devolver.
    let e = rpc(&c, &c.tok_dueno, "crear_devolucion", json!({"datos": {
        "venta_id": vid, "usuario_id": c.dueno, "motivo": "otra vez",
        "items": [{"venta_detalle_id": vd, "cantidad": 1.0}]}}))
        .await
        .unwrap_err();
    assert!(msg(e).contains("excede"));
}

#[tokio::test]
async fn anular_venta_web_identidad_pin_y_sync() {
    let Some(c) = ctx().await else { return };
    // WP5b: en la web solo se anulan ventas de la caja 'web' (equipo en modo
    // individual); las de la tienda se anulan en el escritorio. La caja web
    // es una sola cadena: se serializa con las demás pruebas que la escriben.
    let _g = super::caja_tests::candado_caja_web().lock().await;
    let dev = format!("wp5a-individual-{}", uuid::Uuid::now_v7());
    sqlx::query("INSERT INTO pos_devices (device_uuid, sucursal_id, nombre, modo_caja) VALUES ($1, 1, 'prueba', 'individual')")
        .bind(&dev)
        .execute(&c.st.pool)
        .await
        .unwrap();
    let tok_vendedor = token(c.vendedor, "wp5a_vendedor", false, Some(&dev));
    let tok_dueno = token(c.dueno, "wp5a_dueno", true, Some(&dev));
    let p = producto(&c, 40.0, 10.0).await;
    let r = rpc(&c, &tok_vendedor, "crear_venta",
        venta(c.vendedor, None, vec![linea(p, 40.0, 0.0, 3.0)], "efectivo"))
        .await
        .unwrap();
    let vid = r["id"].as_i64().unwrap();
    assert_eq!(escalar_f64(&c, "SELECT stock_actual::float8 FROM productos WHERE id = $1", p).await, 7.0);

    // Devolución de escritorio que dice ser de esta venta pero cuyas partidas
    // son de otra: no cuenta como "devolución parcial".
    let (otra, otra_vd) = venta_escritorio(&c, p, 1.0, 40.0).await;
    let dev: i64 = sqlx::query_scalar(
        "INSERT INTO devoluciones (uuid, sucursal_id, folio, venta_id, usuario_id, motivo, total_devuelto) \
         VALUES ($1, 1, $2, $3, $4, 'escritorio', 40) RETURNING id",
    )
    .bind(uuid::Uuid::new_v4().simple().to_string())
    .bind(format!("D-T5A-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]))
    .bind(vid)
    .bind(c.dueno)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO devolucion_detalle (uuid, devolucion_id, venta_detalle_id, producto_id, cantidad, precio_unitario, subtotal) \
         VALUES ($1, $2, $3, $4, 1, 40, 40)",
    )
    .bind(uuid::Uuid::new_v4().simple().to_string())
    .bind(dev)
    .bind(otra_vd)
    .bind(p)
    .execute(&c.st.pool)
    .await
    .unwrap();
    let _ = otra;

    sqlx::query("UPDATE ventas SET updated_at = '2000-01-01 00:00:00' WHERE id = $1")
        .bind(vid)
        .execute(&c.st.pool)
        .await
        .unwrap();

    let args = |pin: Option<&str>| json!({"ventaId": vid, "usuarioId": c.dueno, "motivo": "prueba anulación",
                                          "pinAutorizacion": pin});
    let e = rpc(&c, &tok_vendedor, "anular_venta", args(None)).await.unwrap_err();
    assert!(msg(e).contains("PIN"));
    let e = rpc(&c, &tok_vendedor, "anular_venta", args(Some(PIN_VENDEDOR))).await.unwrap_err();
    assert!(msg(e).contains("PIN del dueño incorrecto"));
    rpc(&c, &tok_vendedor, "anular_venta", args(Some(PIN_DUENO)))
        .await
        .unwrap_or_else(|e| panic!("anulación rechazada: {}", msg(e)));

    let row = sqlx::query("SELECT anulada, anulada_por, updated_at, uuid FROM ventas WHERE id = $1")
        .bind(vid)
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<i32, _>("anulada"), 1);
    assert_eq!(row.get::<Option<i64>, _>("anulada_por"), Some(c.vendedor), "quien anula = la sesión");
    assert!(row.get::<String, _>("updated_at").as_str() > "2000-01-01 00:00:00");
    let vuuid: String = row.get("uuid");
    let cursores_venta: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM sync_cursor WHERE tabla = 'ventas' AND uuid = $1",
    )
    .bind(&vuuid)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    assert_eq!(cursores_venta, 2, "alta + anulación");
    let cursores_prod: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM sync_cursor sc JOIN productos p ON p.uuid = sc.uuid \
         WHERE sc.tabla = 'productos' AND p.id = $1",
    )
    .bind(p)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    assert!(cursores_prod >= 2, "venta + anulación: {cursores_prod}");
    assert_eq!(escalar_f64(&c, "SELECT stock_actual::float8 FROM productos WHERE id = $1", p).await, 10.0);
    assert_eq!(
        escalar_i64(&c, "SELECT usuario_id FROM audit_log WHERE accion = 'ANULACION' AND registro_id = $1 \
                          ORDER BY id DESC LIMIT 1", vid).await,
        c.vendedor
    );

    // Ya anulada → no se puede otra vez.
    assert!(rpc(&c, &tok_dueno, "anular_venta", args(None)).await.is_err());
}

#[tokio::test]
async fn movimiento_sin_cursor_no_queda_guardado() {
    let Some(c) = ctx().await else { return };
    let marca = format!("WP5A-FALLA-CURSOR {}", uuid::Uuid::now_v7());
    // Trigger de prueba: hace fallar el sync_cursor SOLO de movimientos con
    // concepto que empieza con la marca (no afecta a las demás pruebas).
    sqlx::query(
        "CREATE OR REPLACE FUNCTION wp5a_falla_cursor() RETURNS trigger AS $$ \
         BEGIN \
           IF NEW.tabla = 'movimientos_caja' AND EXISTS ( \
                SELECT 1 FROM movimientos_caja WHERE uuid = NEW.uuid \
                  AND concepto LIKE 'WP5A-FALLA-CURSOR%') THEN \
             RAISE EXCEPTION 'falla de prueba en sync_cursor'; \
           END IF; \
           RETURN NEW; \
         END $$ LANGUAGE plpgsql",
    )
    .execute(&c.st.pool)
    .await
    .unwrap();
    sqlx::query("DROP TRIGGER IF EXISTS wp5a_falla_cursor ON sync_cursor")
        .execute(&c.st.pool)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER wp5a_falla_cursor BEFORE INSERT ON sync_cursor \
                 FOR EACH ROW EXECUTE FUNCTION wp5a_falla_cursor()")
        .execute(&c.st.pool)
        .await
        .unwrap();

    let r = rpc(&c, &c.tok_dueno, "crear_movimiento_caja", json!({"datos": {
        "tipo": "ENTRADA", "usuario_id": c.dueno, "monto": 10.0, "concepto": marca }}))
        .await;

    sqlx::query("DROP TRIGGER IF EXISTS wp5a_falla_cursor ON sync_cursor")
        .execute(&c.st.pool)
        .await
        .unwrap();

    assert!(r.is_err(), "el cursor falló, la RPC debe fallar");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM movimientos_caja WHERE concepto = $1")
        .bind(&marca)
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "no debe quedar un movimiento que nunca baja al escritorio");
}

#[tokio::test]
async fn recepcion_audita_cambio_de_precio_y_avisa_la_orden() {
    let Some(c) = ctx().await else { return };
    let p = producto(&c, 100.0, 1.0).await;
    let cuenta_precio = |c: &Ctx| {
        let pool = c.st.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*)::bigint FROM audit_log WHERE accion = 'PRECIO_ACTUALIZADO' \
                 AND tabla_afectada = 'productos' AND registro_id = $1",
            )
            .bind(p)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };

    let rec = |pv: Option<f64>, orden: Option<i64>| json!({"recepcion": {
        "usuario_id": c.dueno, "orden_id": orden, "notas": "prueba",
        "items": [{"producto_id": p, "cantidad": 2.0, "precio_costo": 60.0, "precio_venta": pv}]}});
    rpc(&c, &c.tok_vendedor, "crear_recepcion", rec(Some(140.0), None)).await.unwrap();
    assert_eq!(cuenta_precio(&c).await, 1);
    assert_eq!(escalar_f64(&c, "SELECT precio_venta::float8 FROM productos WHERE id = $1", p).await, 140.0);
    let hist = rpc(&c, &c.tok_dueno, "historial_precios_producto", json!({"productoId": p})).await.unwrap();
    assert_eq!(hist[0]["precio_anterior"].as_f64(), Some(100.0));
    assert_eq!(hist[0]["precio_nuevo"].as_f64(), Some(140.0));

    // Mismo precio → sin fila nueva. Sin precio → tampoco.
    rpc(&c, &c.tok_vendedor, "crear_recepcion", rec(Some(140.0), None)).await.unwrap();
    rpc(&c, &c.tok_vendedor, "crear_recepcion", rec(None, None)).await.unwrap();
    assert_eq!(cuenta_precio(&c).await, 1);
    assert_eq!(escalar_f64(&c, "SELECT stock_actual::float8 FROM productos WHERE id = $1", p).await, 7.0);

    // Cantidad negativa → rechazada.
    let mut mala = rec(None, None);
    mala["recepcion"]["items"][0]["cantidad"] = json!(-3.0);
    assert!(rpc(&c, &c.tok_vendedor, "crear_recepcion", mala).await.is_err());

    // Recepción contra una orden: la orden cambia y se avisa al escritorio.
    let o = rpc(&c, &c.tok_dueno, "crear_orden_pedido", json!({"orden": {
        "usuario_id": c.dueno, "items": [{"producto_id": p, "cantidad_pedida": 2.0, "precio_costo": 60.0}]}}))
        .await
        .unwrap();
    let oid = o["id"].as_i64().unwrap();
    let cursores = |c: &Ctx| {
        let pool = c.st.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*)::bigint FROM sync_cursor sc JOIN ordenes_pedido o ON o.uuid = sc.uuid \
                 WHERE sc.tabla = 'ordenes_pedido' AND o.id = $1",
            )
            .bind(oid)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let antes = cursores(&c).await;
    rpc(&c, &c.tok_dueno, "crear_recepcion", rec(None, Some(oid))).await.unwrap();
    assert_eq!(cursores(&c).await, antes + 1);
    let estado: String = sqlx::query_scalar("SELECT estado FROM ordenes_pedido WHERE id = $1")
        .bind(oid)
        .fetch_one(&c.st.pool)
        .await
        .unwrap();
    assert_eq!(estado, "recibida_completa");
}

#[tokio::test]
async fn devolucion_reembolso_y_pin() {
    let Some(c) = ctx().await else { return };
    let p = producto(&c, 50.0, 10.0).await;

    // Venta con tarjeta, devolución sin indicar reembolso → no sale de caja.
    let r = rpc(&c, &c.tok_dueno, "crear_venta",
        venta(c.dueno, None, vec![linea(p, 50.0, 0.0, 2.0)], "tarjeta"))
        .await
        .unwrap();
    let vid = r["id"].as_i64().unwrap();
    let vd = detalle_ids(&c, vid).await[0];
    let d = rpc(&c, &c.tok_dueno, "crear_devolucion", json!({"datos": {
        "venta_id": vid, "usuario_id": c.dueno, "motivo": "tarjeta",
        "items": [{"venta_detalle_id": vd, "cantidad": 1.0}]}}))
        .await
        .unwrap();
    let did = d["id"].as_i64().unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Option<i64>>("SELECT movimiento_caja_id FROM devoluciones WHERE id = $1")
            .bind(did).fetch_one(&c.st.pool).await.unwrap(),
        None
    );

    // Venta en efectivo: el vendedor necesita el PIN del DUEÑO.
    let r = rpc(&c, &c.tok_vendedor, "crear_venta",
        venta(c.vendedor, None, vec![linea(p, 50.0, 0.0, 2.0)], "efectivo"))
        .await
        .unwrap();
    let vid = r["id"].as_i64().unwrap();
    let vd = detalle_ids(&c, vid).await[0];
    let dev = |pin: Option<&str>| json!({"datos": {
        "venta_id": vid, "usuario_id": c.dueno, "autorizado_por": c.vendedor,
        "pin_autorizacion": pin, "reembolso_efectivo": true, "motivo": "efectivo",
        "items": [{"venta_detalle_id": vd, "cantidad": 1.0}]}});
    assert!(rpc(&c, &c.tok_vendedor, "crear_devolucion", dev(None)).await.is_err());
    let e = rpc(&c, &c.tok_vendedor, "crear_devolucion", dev(Some(PIN_VENDEDOR))).await.unwrap_err();
    assert!(msg(e).contains("PIN del dueño incorrecto"));
    let d = rpc(&c, &c.tok_vendedor, "crear_devolucion", dev(Some(PIN_DUENO))).await.unwrap();
    let did = d["id"].as_i64().unwrap();
    let row = sqlx::query(
        "SELECT d.usuario_id, d.autorizado_por, m.origen, m.tipo, m.monto::float8 AS monto \
         FROM devoluciones d JOIN movimientos_caja m ON m.id = d.movimiento_caja_id WHERE d.id = $1",
    )
    .bind(did)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    assert_eq!(row.get::<i64, _>("usuario_id"), c.vendedor);
    assert_eq!(row.get::<Option<i64>, _>("autorizado_por"), Some(c.dueno));
    assert_eq!(row.get::<String, _>("origen"), "web");
    assert_eq!(row.get::<String, _>("tipo"), "RETIRO");
    assert_eq!(row.get::<f64, _>("monto"), 50.0);
}

#[tokio::test]
async fn productos_codigo_unico_y_stock_no_negativo() {
    let Some(c) = ctx().await else { return };
    let p = producto(&c, 10.0, 3.0).await;
    let codigo: String = sqlx::query_scalar("SELECT codigo FROM productos WHERE id = $1")
        .bind(p).fetch_one(&c.st.pool).await.unwrap();
    // El producto se elimina (soft): su código sigue ocupado.
    sqlx::query("UPDATE productos SET deleted_at = '2026-01-01 00:00:00', activo = 0 WHERE id = $1")
        .bind(p).execute(&c.st.pool).await.unwrap();

    let nuevo = |cod: Option<&str>, stock: f64| json!({"usuarioId": c.dueno, "producto": {
        "codigo": cod, "nombre": "Prueba WP5a", "precio_costo": 1.0, "precio_venta": 2.0,
        "stock_actual": stock, "stock_minimo": 0.0}});
    let e = rpc(&c, &c.tok_dueno, "crear_producto", nuevo(Some(&codigo), 1.0)).await.unwrap_err();
    assert!(msg(e).contains("eliminado"), "código de producto eliminado sigue ocupado");
    let e = rpc(&c, &c.tok_dueno, "crear_producto", nuevo(Some("WP5A-NEG-X"), -1.0)).await.unwrap_err();
    assert!(msg(e).contains("negativo"));

    // Código automático: nunca uno ya usado.
    let creado = rpc(&c, &c.tok_dueno, "crear_producto", nuevo(None, 2.0)).await.unwrap();
    let cod_auto = creado["codigo"].as_str().unwrap().to_string();
    assert!(cod_auto.starts_with("MR-"));
    let usos: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM productos WHERE codigo = $1")
        .bind(&cod_auto).fetch_one(&c.st.pool).await.unwrap();
    assert_eq!(usos, 1);
    let id_auto = creado["id"].as_i64().unwrap();

    // Ajuste negativo → rechazado; ajuste válido no escribe cursor de stock_sucursal.
    let e = rpc(&c, &c.tok_dueno, "ajustar_stock",
        json!({"productoId": id_auto, "nuevoStock": -1.0, "motivo": "x", "usuarioId": c.dueno}))
        .await
        .unwrap_err();
    assert!(msg(e).contains("negativo"));
    let ss_antes: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM sync_cursor WHERE tabla = 'stock_sucursal'")
        .fetch_one(&c.st.pool).await.unwrap();
    rpc(&c, &c.tok_dueno, "ajustar_stock",
        json!({"productoId": id_auto, "nuevoStock": 5.0, "motivo": "conteo", "usuarioId": c.dueno}))
        .await
        .unwrap();
    let ss_despues: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM sync_cursor WHERE tabla = 'stock_sucursal'")
        .fetch_one(&c.st.pool).await.unwrap();
    assert_eq!(ss_antes, ss_despues);

    // Editar: cambiar al código de otro producto → rechazado; mismo código → ok.
    let otro = producto(&c, 10.0, 1.0).await;
    let cod_otro: String = sqlx::query_scalar("SELECT codigo FROM productos WHERE id = $1")
        .bind(otro).fetch_one(&c.st.pool).await.unwrap();
    let editar = |cod: &str, stock_min: f64| json!({"usuarioId": c.dueno, "producto": {
        "id": id_auto, "codigo": cod, "nombre": "Prueba WP5a editado", "precio_costo": 1.0,
        "precio_venta": 3.0, "stock_minimo": stock_min}});
    let e = rpc(&c, &c.tok_dueno, "actualizar_producto", editar(&cod_otro, 0.0)).await.unwrap_err();
    assert!(msg(e).contains("ya lo usa"));
    let e = rpc(&c, &c.tok_dueno, "actualizar_producto", editar(&cod_auto, -2.0)).await.unwrap_err();
    assert!(msg(e).contains("negativo"));
    rpc(&c, &c.tok_dueno, "actualizar_producto", editar(&cod_auto, 1.0)).await.unwrap();
}

#[tokio::test]
async fn token_que_no_coincide_con_un_usuario_activo_da_401() {
    let Some(c) = ctx().await else { return };
    let p = producto(&c, 10.0, 1.0).await;
    // sub correcto pero nombre_usuario distinto (p. ej. token de /auth/login).
    let malo = token(c.dueno, "otro-correo@ejemplo.com", true, None);
    let e = rpc(&c, &malo, "crear_venta", venta(c.dueno, None, vec![linea(p, 10.0, 0.0, 1.0)], "efectivo"))
        .await
        .unwrap_err();
    assert!(matches!(e, ApiError::Unauthorized));
}

#[tokio::test]
async fn panel_api_productos_valida_codigo_y_escribe_cursor() {
    let Some(c) = ctx().await else { return };
    let p = producto(&c, 10.0, 1.0).await;
    let codigo: String = sqlx::query_scalar("SELECT codigo FROM productos WHERE id = $1")
        .bind(p).fetch_one(&c.st.pool).await.unwrap();
    let mut h = HeaderMap::new();
    h.insert("authorization", HeaderValue::from_str(&format!("Bearer {}", c.tok_dueno)).unwrap());
    let input = |cod: &str, stock_min: f64| -> crate::api::ProductoInput {
        serde_json::from_value(json!({
            "codigo": cod, "nombre": "Panel WP5a", "precio_costo": 1.0, "precio_venta": 2.0,
            "stock_minimo": stock_min }))
        .unwrap()
    };
    let e = crate::api::productos_create(State(c.st.clone()), h.clone(), Json(input(&codigo, 0.0)))
        .await
        .unwrap_err();
    assert!(msg(e).contains("ya lo usa"));
    let e = crate::api::productos_create(State(c.st.clone()), h.clone(), Json(input("WP5A-PANEL-NEG", -1.0)))
        .await
        .unwrap_err();
    assert!(msg(e).contains("negativo"));

    let nuevo = format!("WP5A-PANEL-{}", &uuid::Uuid::now_v7().simple().to_string()[20..]);
    let Json(v) = crate::api::productos_create(State(c.st.clone()), h.clone(), Json(input(&nuevo, 0.0)))
        .await
        .unwrap();
    let u = v["uuid"].as_str().unwrap().to_string();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM sync_cursor WHERE tabla = 'productos' AND uuid = $1")
        .bind(&u).fetch_one(&c.st.pool).await.unwrap();
    assert_eq!(n, 1);
    // Editar sin cambiar el código: permitido aunque el código exista.
    let _ = crate::api::productos_update(State(c.st.clone()), h.clone(), Path(u.clone()), Json(input(&nuevo, 2.0)))
        .await
        .unwrap();
    // Cambiar al código de otro producto: rechazado.
    let e = crate::api::productos_update(State(c.st.clone()), h, Path(u), Json(input(&codigo, 2.0)))
        .await
        .unwrap_err();
    assert!(msg(e).contains("ya lo usa"));
}

// ─── Ronda 2 ───────────────────────────────────────────────────────────────

/// Producto "del escritorio" (uuid de 32 hex) con categoría y proveedor.
async fn producto_escritorio(c: &Ctx, categoria: Option<i64>, proveedor: Option<i64>) -> (i64, String, String) {
    let u = uuid::Uuid::new_v4().simple().to_string();
    let codigo = format!("R2-ESC-{}", &u[..12]);
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO productos (uuid, codigo, nombre, precio_costo, precio_venta, stock_actual, \
                               categoria_id, proveedor_id) \
         VALUES ($1, $2, 'Producto del escritorio R2', 1, 10, 3, $3, $4) RETURNING id",
    )
    .bind(&u)
    .bind(&codigo)
    .bind(categoria)
    .bind(proveedor)
    .fetch_one(&c.st.pool)
    .await
    .unwrap();
    (id, u, codigo)
}

async fn celdas_web(c: &Ctx, uuid: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT columna FROM sync_fk_celda_web WHERE tabla = 'productos' AND uuid = $1 ORDER BY columna",
    )
    .bind(uuid)
    .fetch_all(&c.st.pool)
    .await
    .unwrap()
}

async fn dos_ids(c: &Ctx, tabla: &str) -> (i64, i64) {
    let v: Vec<i64> = sqlx::query_scalar(&format!(
        "SELECT id FROM {tabla} WHERE deleted_at IS NULL ORDER BY id LIMIT 2"
    ))
    .fetch_all(&c.st.pool)
    .await
    .unwrap();
    assert_eq!(v.len(), 2, "la copia de producción tiene al menos 2 {tabla}");
    (v[0], v[1])
}

#[tokio::test]
async fn web_cambia_categoria_o_proveedor_de_producto_del_escritorio_sin_reparar() {
    let Some(c) = ctx().await else { return };
    let (cat_a, cat_b) = dos_ids(&c, "categorias").await;
    let (prov_a, prov_b) = dos_ids(&c, "proveedores").await;
    let (pid, u, codigo) = producto_escritorio(&c, Some(cat_a), Some(prov_a)).await;
    let editar = |id: i64, codigo: &str, cat: Option<i64>, prov: Option<i64>, precio: f64| {
        json!({"usuarioId": c.dueno, "producto": {
            "id": id, "codigo": codigo, "nombre": "Producto del escritorio R2", "precio_costo": 1.0,
            "precio_venta": precio, "stock_minimo": 0.0, "categoria_id": cat, "proveedor_id": prov}})
    };

    // Solo el precio (el formulario manda las mismas FKs): nada que anotar;
    // la reparación traducirá esas celdas desde el id del escritorio.
    rpc(&c, &c.tok_dueno, "actualizar_producto", editar(pid, &codigo, Some(cat_a), Some(prov_a), 12.0))
        .await
        .unwrap();
    assert!(celdas_web(&c, &u).await.is_empty());

    // Cambia la categoría: esa celda ya es un id del servidor.
    rpc(&c, &c.tok_dueno, "actualizar_producto", editar(pid, &codigo, Some(cat_b), Some(prov_a), 12.0))
        .await
        .unwrap();
    assert_eq!(celdas_web(&c, &u).await, vec!["categoria_id".to_string()]);
    let cat: Option<i64> = sqlx::query_scalar("SELECT categoria_id FROM productos WHERE id = $1")
        .bind(pid).fetch_one(&c.st.pool).await.unwrap();
    assert_eq!(cat, Some(cat_b));

    // El panel (/api/productos/:uuid) cambia el proveedor.
    let mut h = HeaderMap::new();
    h.insert("authorization", HeaderValue::from_str(&format!("Bearer {}", c.tok_dueno)).unwrap());
    let input: crate::api::ProductoInput = serde_json::from_value(json!({
        "codigo": codigo, "nombre": "Producto del escritorio R2", "precio_costo": 1.0,
        "precio_venta": 12.0, "categoria_id": cat_b, "proveedor_id": prov_b }))
    .unwrap();
    let Json(r) = crate::api::productos_update(State(c.st.clone()), h.clone(), Path(u.clone()), Json(input))
        .await
        .unwrap();
    assert_eq!(r["uuid"].as_str(), Some(u.as_str()));
    assert_eq!(celdas_web(&c, &u).await, vec!["categoria_id".to_string(), "proveedor_id".to_string()]);

    // Quitar la categoría también es un cambio; repetirlo no duplica.
    rpc(&c, &c.tok_dueno, "actualizar_producto", editar(pid, &codigo, None, Some(prov_b), 12.0))
        .await
        .unwrap();
    assert_eq!(celdas_web(&c, &u).await.len(), 2);

    // Si la edición falla, no queda nada anotado (misma transacción).
    let (pid_f, u_f, _) = producto_escritorio(&c, Some(cat_a), None).await;
    let e = rpc(&c, &c.tok_dueno, "actualizar_producto", editar(pid_f, &codigo, Some(cat_b), None, 12.0))
        .await
        .unwrap_err();
    assert!(msg(e).contains("ya lo usa"));
    assert!(celdas_web(&c, &u_f).await.is_empty());

    // Producto del escritorio ya traducido/reparado: sus FKs ya son de aquí.
    let (pid2, u2, cod2) = producto_escritorio(&c, Some(cat_a), Some(prov_a)).await;
    sqlx::query("INSERT INTO sync_fk_estado (tabla, uuid, estado) VALUES ('productos', $1, 'reparado')")
        .bind(&u2).execute(&c.st.pool).await.unwrap();
    rpc(&c, &c.tok_dueno, "actualizar_producto", editar(pid2, &cod2, Some(cat_b), Some(prov_b), 12.0))
        .await
        .unwrap();
    assert!(celdas_web(&c, &u2).await.is_empty());

    // Producto hecho en la web: nunca tuvo ids del escritorio.
    let pid3 = producto(&c, 10.0, 1.0).await;
    let (u3, cod3): (String, String) = sqlx::query_as("SELECT uuid, codigo FROM productos WHERE id = $1")
        .bind(pid3).fetch_one(&c.st.pool).await.unwrap();
    rpc(&c, &c.tok_dueno, "actualizar_producto", editar(pid3, &cod3, Some(cat_b), Some(prov_b), 12.0))
        .await
        .unwrap();
    assert!(celdas_web(&c, &u3).await.is_empty());
}

/// Llama al dispatcher como una pestaña con el bundle `version` (None = sin
/// el header x-pos-cliente, como el bundle anterior).
async fn rpc_version(c: &Ctx, tok: &str, cmd: &str, args: Value, version: Option<&str>) -> Result<Value, ApiError> {
    let mut h = HeaderMap::new();
    h.insert("authorization", HeaderValue::from_str(&format!("Bearer {tok}")).unwrap());
    if let Some(v) = version {
        h.insert("x-pos-cliente", HeaderValue::from_str(v).unwrap());
    }
    dispatch(State(c.st.clone()), h, Path(cmd.to_string()), Json(args))
        .await
        .map(|Json(v)| v)
}

#[tokio::test]
async fn venta_de_pestana_vieja_rechazada_por_precio_o_descuento_pide_recargar() {
    let Some(c) = ctx().await else { return };
    let p = producto(&c, 100.0, 50.0).await;
    let aviso = super::AVISO_RECARGAR_VENTA;
    assert_eq!(aviso, " Hay una versión nueva del POS web. Recarga la página (F5).");

    // Precio viejo en el carrito de una pestaña sin el bundle nuevo: el
    // bundle viejo no sabe refrescar precios, así que se le dice que recargue.
    for version in [None, Some("1"), Some("abc")] {
        let e = rpc_version(&c, &c.tok_vendedor, "crear_venta",
            venta(c.vendedor, None, vec![linea(p, 90.0, 0.0, 1.0)], "efectivo"), version)
            .await
            .unwrap_err();
        let m = msg(e);
        assert!(m.starts_with("PRECIO_CAMBIO"), "{m}");
        assert!(m.ends_with(aviso), "versión {version:?}: {m}");
    }
    // Bundle actual: el mismo error sin el aviso (él refresca los precios).
    let e = rpc_version(&c, &c.tok_vendedor, "crear_venta",
        venta(c.vendedor, None, vec![linea(p, 90.0, 0.0, 1.0)], "efectivo"), Some("2"))
        .await
        .unwrap_err();
    let m = msg(e);
    assert!(m.starts_with("PRECIO_CAMBIO") && !m.contains("versión nueva"), "{m}");

    // Descuento arriba del límite sin token (el bundle viejo nunca lo pide).
    let pct = (c.max_vendedor + 15.0).min(100.0);
    let e = rpc_version(&c, &c.tok_vendedor, "crear_venta",
        venta(c.vendedor, None, vec![linea(p, 100.0, pct, 1.0)], "efectivo"), None)
        .await
        .unwrap_err();
    let m = msg(e);
    assert!(m.starts_with("DESCUENTO_NO_AUTORIZADO") && m.ends_with(aviso), "{m}");
    let e = rpc_version(&c, &c.tok_vendedor, "crear_venta",
        venta(c.vendedor, None, vec![linea(p, 100.0, pct, 1.0)], "efectivo"), Some("2"))
        .await
        .unwrap_err();
    let m = msg(e);
    assert!(m.starts_with("DESCUENTO_NO_AUTORIZADO") && !m.contains("versión nueva"), "{m}");

    // Otros rechazos no cambian.
    let mut v = venta(c.vendedor, None, vec![linea(p, 100.0, 0.0, 1.0)], "efectivo");
    v["venta"]["total"] = json!(10.0);
    v["venta"]["monto_recibido"] = json!(10.0);
    let m = msg(rpc_version(&c, &c.tok_vendedor, "crear_venta", v, None).await.unwrap_err());
    assert!(m.starts_with("TOTALES_INCONSISTENTES") && !m.contains("versión nueva"), "{m}");

    // Una venta correcta de la pestaña vieja sigue pasando (crear_venta no
    // exige el header).
    let r = rpc_version(&c, &c.tok_vendedor, "crear_venta",
        venta(c.vendedor, None, vec![linea(p, 100.0, 0.0, 1.0)], "efectivo"), None)
        .await
        .unwrap();
    assert_eq!(r["total"].as_f64(), Some(100.0));
}
