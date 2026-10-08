// Pruebas del sync v2 contra Postgres (copia fiel de producción).
//
// Se incluyen como módulo desde src/sync.rs (`#[path]`) para poder llamar
// las funciones internas. Solo corren si existe SYNC_PG_URL (si no, cada
// prueba termina sin hacer nada y `cargo test` sigue pasando sin Postgres):
//
//   SYNC_PG_URL=postgres://postgres@localhost:54329/postgres \
//   SYNC_PG_TEMPLATE=prodcopy_base   # (default) BD plantilla, NUNCA se modifica
//   cargo test pruebas_pg
//
// Cada corrida (proceso) usa nombres propios `sv2p<pid>_…`, así dos corridas
// a la vez no se borran las bases: una base `sv2p<pid>_base` (= plantilla +
// migraciones del servidor) y de ahí una BD por prueba, que se borra al
// terminar. La base se borra cuando ya no queda ninguna prueba usándola.
// Las pruebas de la ronda 2 (SPEC2 R1) están en sync_v2_ronda2_pg.rs, las
// de la ronda 3 (SPEC3 S) en sync_v2_ronda3_pg.rs y las de la ronda 4
// (SPEC4 S) en sync_v2_ronda4_pg.rs.

use super::*;
use crate::sync_fk;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgConnection};
use tower::ServiceExt;

#[path = "sync_v2_ronda2_pg.rs"]
mod ronda2;
#[path = "sync_v2_ronda3_pg.rs"]
mod ronda3;
#[path = "sync_v2_ronda4_pg.rs"]
mod ronda4;

/// Escritorio real de la tienda (sync_cursor.origen_device en producción).
const DEV: &str = "336d5ab58660c6335416c3cca39cf4d9";
const SECRETO: &[u8] = b"secreto-de-pruebas-wp2-0123456789";

const DUENIO: &str = "45b6808b31b6b822a2ac0207a444a23c";
const NANCY: &str = "4758fab9e75ab6661d4c4ce179bd639d";
const MARIANA: &str = "2a2054aa1d114d56e5a219af480a5058";
const LAU: &str = "09eb03364c3a8e4b79ac88fd90080509";

/// (base lista, pruebas usando la base).
static BASE: tokio::sync::Mutex<(bool, usize)> = tokio::sync::Mutex::const_new((false, 0));

struct Ctx {
    pool: PgPool,
    db: String,
    url: String,
}

fn url_de(url: &str, db: &str) -> String {
    let base = url.split('?').next().unwrap_or(url);
    let i = base.rfind('/').expect("SYNC_PG_URL sin /base");
    format!("{}/{}", &base[..i], db)
}

fn prefijo() -> String {
    format!("sv2p{}", std::process::id())
}

async fn ejecutar(url: &str, sql: &str) {
    let mut c = PgConnection::connect(url).await.expect("conexión admin");
    sqlx::raw_sql(sql).execute(&mut c).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
    c.close().await.ok();
}

/// CREATE DATABASE … TEMPLATE con reintentos: si otra corrida está clonando
/// la misma plantilla, Postgres responde "is being accessed by other users".
async fn clonar(url: &str, db: &str, plantilla: &str) {
    for intento in 0..60 {
        let mut c = PgConnection::connect(url).await.expect("conexión admin");
        let r = sqlx::raw_sql(&format!("CREATE DATABASE {db} TEMPLATE {plantilla}")).execute(&mut c).await;
        c.close().await.ok();
        match r {
            Ok(_) => return,
            Err(e) if e.to_string().contains("being accessed by other users") && intento < 59 => {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err(e) => panic!("CREATE DATABASE {db} TEMPLATE {plantilla}: {e}"),
        }
    }
}

async fn ctx(nombre: &str) -> Option<Ctx> {
    let Ok(url) = std::env::var("SYNC_PG_URL") else {
        eprintln!("SYNC_PG_URL no definida: se omite la prueba con Postgres '{nombre}'");
        return None;
    };
    let plantilla = std::env::var("SYNC_PG_TEMPLATE").unwrap_or_else(|_| "prodcopy_base".into());
    let base = format!("{}_base", prefijo());
    let db = format!("{}_{nombre}", prefijo());
    {
        let mut g = BASE.lock().await;
        if !g.0 {
            ejecutar(&url, &format!("DROP DATABASE IF EXISTS {base} WITH (FORCE)")).await;
            clonar(&url, &base, &plantilla).await;
            let pool = PgPoolOptions::new().max_connections(2).connect(&url_de(&url, &base)).await.unwrap();
            crate::db::run_migrations(&pool).await.expect("migraciones del servidor sobre la copia de producción");
            pool.close().await;
            g.0 = true;
        }
        ejecutar(&url, &format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)")).await;
        clonar(&url, &db, &base).await;
        g.1 += 1;
    }
    let pool = PgPoolOptions::new().max_connections(5).connect(&url_de(&url, &db)).await.unwrap();
    Some(Ctx { pool, db, url })
}

impl Ctx {
    async fn cerrar(self) {
        self.pool.close().await;
        let conservar = std::env::var("SYNC_PG_CONSERVAR").is_ok();
        if conservar {
            eprintln!("se conserva la BD {} (SYNC_PG_CONSERVAR)", self.db);
        } else {
            ejecutar(&self.url, &format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.db)).await;
        }
        let mut g = BASE.lock().await;
        g.1 = g.1.saturating_sub(1);
        if g.1 == 0 && !conservar {
            ejecutar(&self.url, &format!("DROP DATABASE IF EXISTS {}_base WITH (FORCE)", prefijo())).await;
            g.0 = false;
        }
    }
}

fn hex32() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

async fn ahora(p: &PgPool) -> String {
    sqlx::query_scalar(sync_fk::SQL_AHORA_LOCAL).fetch_one(p).await.unwrap()
}

async fn id_de(p: &PgPool, tabla: &str, uuid: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT id FROM {tabla} WHERE uuid = $1"))
        .bind(uuid)
        .fetch_one(p)
        .await
        .unwrap()
}

async fn entero(p: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar::<_, Option<i64>>(sql).fetch_one(p).await.unwrap().unwrap_or(0)
}

async fn texto(p: &PgPool, sql: &str) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>(sql).fetch_optional(p).await.unwrap().flatten()
}

async fn estado(p: &PgPool, tabla: &str, uuid: &str) -> Option<String> {
    sqlx::query_scalar("SELECT estado FROM sync_fk_estado WHERE tabla = $1 AND uuid = $2")
        .bind(tabla)
        .bind(uuid)
        .fetch_optional(p)
        .await
        .unwrap()
}

fn cambio(tabla: &str, data: Value, children: Option<Value>, refs: Option<Value>) -> CambioIn {
    CambioIn { tabla: tabla.into(), operacion: "UPDATE".into(), data, children, refs, base: None }
}

fn body(proto: Option<i32>, cambios: Vec<CambioIn>) -> PushBody {
    PushBody { device_uuid: DEV.into(), sucursal_id: 1, proto, cambios }
}

/// Venta de escritorio con un renglón (ids LOCALES del escritorio).
fn venta_escritorio(id: i64, uuid: &str, usuario_local: i64, ts: &str) -> Value {
    json!({
        "id": id, "uuid": uuid, "folio": format!("V-T{id}"), "usuario_id": usuario_local,
        "cliente_id": null, "subtotal": 50.0, "descuento": 0.0, "total": 50.0,
        "metodo_pago": "efectivo", "monto_recibido": 50.0, "cambio": 0.0, "anulada": 0,
        "anulada_por": null, "motivo_anulacion": null, "fecha": ts, "updated_at": ts,
        "deleted_at": null, "origen": "desktop", "caja": "principal", "registrado_at": null,
        "columna_solo_del_escritorio": "x"
    })
}

fn detalle(id: i64, uuid: &str, venta_local: i64, producto_local: i64, ts: &str) -> Value {
    json!({
        "id": id, "uuid": uuid, "venta_id": venta_local, "producto_id": producto_local,
        "cantidad": 1.0, "precio_original": 50.0, "descuento_porcentaje": 0.0,
        "descuento_monto": 0.0, "precio_final": 50.0, "subtotal": 50.0,
        "autorizado_por": null, "updated_at": ts, "deleted_at": null
    })
}

fn producto_escritorio(id: i64, uuid: &str, ts: &str) -> Value {
    json!({
        "id": id, "uuid": uuid, "codigo": format!("WP2-{id}"), "codigo_tipo": "INTERNO",
        "nombre": format!("Producto prueba {id}"), "descripcion": null, "categoria_id": null,
        "precio_costo": 5.0, "precio_venta": 10.0, "stock_actual": 3.0, "stock_minimo": 0.0,
        "proveedor_id": null, "foto_url": null, "search_text": "", "activo": 1,
        "created_at": ts, "updated_at": ts, "deleted_at": null
    })
}

async fn producto_existente(p: &PgPool, id: i64) -> (i64, String) {
    let u: String = sqlx::query_scalar("SELECT uuid FROM productos WHERE id = $1").bind(id).fetch_one(p).await.unwrap();
    (id, u)
}

// ─────────────────────────────────────────────────────────────────────────
// (a) push v2 con refs: venta nueva del escritorio
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn a_push_v2_traduce_por_refs() {
    let Some(c) = ctx("a").await else { return };
    let p = &c.pool;
    let ts = ahora(p).await;
    let (prod_id, prod_uuid) = producto_existente(p, 100).await;
    let (vu, du) = (hex32(), hex32());
    let mut refs_map = Map::new();
    refs_map.insert(vu.clone(), json!({"usuario_id": DUENIO, "cliente_id": null, "anulada_por": null}));
    refs_map.insert(du.clone(), json!({"producto_id": prod_uuid, "autorizado_por": null}));
    let r = procesar_push(
        p,
        &body(
            Some(2),
            vec![cambio(
                "ventas",
                // usuario local 1 (Dueño en el escritorio), producto local 7777 (inventado)
                venta_escritorio(99001, &vu, 1, &ts),
                Some(json!({ "venta_detalle": [detalle(88001, &du, 99001, 7777, &ts)] })),
                Some(Value::Object(refs_map)),
            )],
        ),
    )
    .await
    .unwrap();
    assert_eq!(r.aceptados, vec![vu.clone()], "rechazados: {:?}", r.rechazados);
    assert!(r.faltantes.is_empty());

    let vid = id_de(p, "ventas", &vu).await;
    assert_eq!(entero(p, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{vu}'")).await, 2, "Dueño del servidor = 2");
    assert_eq!(
        entero(p, &format!("SELECT producto_id FROM venta_detalle WHERE uuid = '{du}'")).await,
        prod_id
    );
    assert_eq!(entero(p, &format!("SELECT venta_id FROM venta_detalle WHERE uuid = '{du}'")).await, vid);
    assert_eq!(estado(p, "ventas", &vu).await.as_deref(), Some("traducido"));
    assert_eq!(entero(p, &format!("SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid IN ('{vu}','{du}')")).await, 0);
    // Mapa de ids registrado para padre e hijo.
    assert_eq!(
        texto(p, &format!("SELECT uuid FROM sync_id_map WHERE device_uuid = '{DEV}' AND tabla = 'ventas' AND local_id = 99001")).await,
        Some(vu.clone())
    );
    assert_eq!(
        texto(p, &format!("SELECT fuente FROM sync_id_map WHERE device_uuid = '{DEV}' AND tabla = 'venta_detalle' AND local_id = 88001")).await.as_deref(),
        Some("push")
    );
    // Protocolo del dispositivo y cursor.
    assert_eq!(entero(p, &format!("SELECT protocolo::bigint FROM pos_devices WHERE device_uuid = '{DEV}'")).await, 2);
    assert_eq!(
        texto(p, &format!("SELECT origen_device FROM sync_cursor WHERE tabla = 'ventas' AND uuid = '{vu}'")).await.as_deref(),
        Some(DEV)
    );
    // origen/caja del escritorio, y la columna desconocida no rompió nada.
    assert_eq!(texto(p, &format!("SELECT origen || '/' || caja FROM ventas WHERE uuid = '{vu}'")).await.as_deref(), Some("desktop/principal"));
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// (b) push viejo (sin proto, SPEC2 R1.1): fila del escritorio sin estado →
//     celdas crudas; con estado → por el mapa, y sin entrada se conserva lo
//     guardado; hijo nuevo sin mapa → pendiente que se resuelve después.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn b_push_viejo_crudo_o_por_mapa() {
    let Some(c) = ctx("b").await else { return };
    let p = &c.pool;
    let ts = ahora(p).await;
    // 1) El escritorio viejo empuja a Nancy (id local 2) y un producto nuevo (id local 7001).
    let mut nancy: Map<String, Value> = serde_json::from_str(
        &texto(p, &format!("SELECT row_to_json(u)::text FROM usuarios u WHERE uuid = '{NANCY}'")).await.unwrap(),
    )
    .unwrap();
    nancy.insert("id".into(), json!(2));
    nancy.insert("updated_at".into(), json!(ts));
    let pu = hex32();
    let mut prod = producto_escritorio(7001, &pu, &ts);
    prod["categoria_id"] = json!(3); // id del ESCRITORIO (sin mapa): se guarda crudo
    let r = procesar_push(
        p,
        &body(None, vec![cambio("usuarios", Value::Object(nancy), None, None), cambio("productos", prod, None, None)]),
    )
    .await
    .unwrap();
    assert_eq!(r.aceptados.len(), 2, "{:?}", r.rechazados);
    assert_eq!(texto(p, &format!("SELECT protocolo::text FROM pos_devices WHERE device_uuid = '{DEV}'")).await, None);
    let pid = id_de(p, "productos", &pu).await;
    assert_ne!(pid, 7001);
    assert_eq!(entero(p, &format!("SELECT categoria_id FROM productos WHERE uuid = '{pu}'")).await, 3);
    assert_eq!(estado(p, "productos", &pu).await, None, "crudo: sin estado");

    // 2) Venta vieja nueva con usuario local 2 y producto local 7001: CRUDA,
    //    como antes de v2 (la reparación la traduce cuando haya mapa).
    let (vu, du) = (hex32(), hex32());
    let r = procesar_push(
        p,
        &body(None, vec![cambio("ventas", venta_escritorio(99002, &vu, 2, &ts), Some(json!({"venta_detalle": [detalle(88002, &du, 99002, 7001, &ts)]})), None)]),
    )
    .await
    .unwrap();
    assert_eq!(r.aceptados, vec![vu.clone()], "{:?}", r.rechazados);
    assert!(r.faltantes.is_empty());
    assert_eq!(entero(p, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{vu}'")).await, 2, "crudo");
    assert_eq!(entero(p, &format!("SELECT producto_id FROM venta_detalle WHERE uuid = '{du}'")).await, 7001, "crudo");
    assert_eq!(entero(p, &format!("SELECT venta_id FROM venta_detalle WHERE uuid = '{du}'")).await, id_de(p, "ventas", &vu).await);
    assert_eq!(estado(p, "ventas", &vu).await, None);
    assert_eq!(entero(p, &format!("SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid IN ('{vu}','{du}')")).await, 0);

    // 3) La venta ya está traducida (push v2) → un push viejo posterior (la
    //    anulación) se traduce por el mapa; anulada_por = Dueño local 1 no
    //    está en el mapa → se conserva lo guardado (NULL), sin pendiente.
    let mut rf = Map::new();
    rf.insert(vu.clone(), json!({"usuario_id": NANCY, "cliente_id": null, "anulada_por": null}));
    rf.insert(du.clone(), json!({"producto_id": pu, "autorizado_por": null}));
    let hijos = json!({"venta_detalle": [detalle(88002, &du, 99002, 7001, &ts)]});
    let r = procesar_push(p, &body(Some(2), vec![cambio("ventas", venta_escritorio(99002, &vu, 2, &ts), Some(hijos.clone()), Some(Value::Object(rf)))])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(estado(p, "ventas", &vu).await.as_deref(), Some("traducido"));
    assert_eq!(entero(p, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{vu}'")).await, 3);
    assert_eq!(entero(p, &format!("SELECT producto_id FROM venta_detalle WHERE uuid = '{du}'")).await, pid);
    let mut anulada = venta_escritorio(99002, &vu, 2, &ts);
    anulada["anulada"] = json!(1);
    anulada["anulada_por"] = json!(1);
    let r = procesar_push(p, &body(None, vec![cambio("ventas", anulada.clone(), Some(hijos.clone()), None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(
        texto(p, &format!("SELECT usuario_id || '|' || coalesce(anulada_por::text, '-') || '|' || anulada FROM ventas WHERE uuid = '{vu}'")).await.as_deref(),
        Some("3|-|1"),
        "usuario por el mapa (2 → Nancy 3); anulada_por sin mapa → se queda lo guardado"
    );
    assert_eq!(entero(p, &format!("SELECT producto_id FROM venta_detalle WHERE uuid = '{du}'")).await, pid, "hijo por el mapa");
    assert_eq!(entero(p, &format!("SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid IN ('{vu}','{du}')")).await, 0);
    assert_eq!(estado(p, "ventas", &vu).await.as_deref(), Some("traducido"));
    // Con el mapa de usuarios, el mismo push viejo ya traduce anulada_por.
    procesar_id_map(p, &IdMapBody { device_uuid: DEV.into(), tabla: "usuarios".into(), pares: vec![json!([1, DUENIO])] }).await.unwrap();
    procesar_push(p, &body(None, vec![cambio("ventas", anulada.clone(), Some(hijos), None)])).await.unwrap();
    assert_eq!(entero(p, &format!("SELECT anulada_por FROM ventas WHERE uuid = '{vu}'")).await, 2);

    // 4) Push viejo con un renglón NUEVO cuyo producto (7999) aún no está
    //    en el mapa → centinela 0 + pendiente (valor_puesto 0); llega el
    //    producto → se resuelve y la venta vuelve a 'traducido'.
    let d2 = hex32();
    let hijos2 = json!({"venta_detalle": [detalle(88002, &du, 99002, 7001, &ts), detalle(88003, &d2, 99002, 7999, &ts)]});
    let r = procesar_push(p, &body(None, vec![cambio("ventas", anulada.clone(), Some(hijos2), None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(entero(p, &format!("SELECT producto_id FROM venta_detalle WHERE uuid = '{d2}'")).await, 0);
    assert_eq!(entero(p, &format!("SELECT valor_puesto FROM sync_fk_pendiente WHERE fila_uuid = '{d2}' AND columna = 'producto_id'")).await, 0);
    assert_eq!(entero(p, &format!("SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid = '{d2}' AND valor_puesto = 0")).await, 1);
    assert_eq!(estado(p, "ventas", &vu).await.as_deref(), Some("pendiente"));
    let pu2 = hex32();
    procesar_push(p, &body(None, vec![cambio("productos", producto_escritorio(7999, &pu2, &ts), None, None)])).await.unwrap();
    assert_eq!(entero(p, &format!("SELECT producto_id FROM venta_detalle WHERE uuid = '{d2}'")).await, id_de(p, "productos", &pu2).await);
    assert_eq!(entero(p, &format!("SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid = '{d2}'")).await, 0);
    assert_eq!(estado(p, "ventas", &vu).await.as_deref(), Some("traducido"));
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// (c) referencias faltantes → pendientes → se resuelven después
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn c_pendientes_se_resuelven() {
    let Some(c) = ctx("c").await else { return };
    let p = &c.pool;
    let ts = ahora(p).await;
    // Producto que el servidor todavía no tiene.
    let (vu, du, pu) = (hex32(), hex32(), hex32());
    let mut rf = Map::new();
    rf.insert(vu.clone(), json!({"usuario_id": MARIANA, "cliente_id": null, "anulada_por": null}));
    rf.insert(du.clone(), json!({"producto_id": pu, "autorizado_por": null}));
    let r = procesar_push(
        p,
        &body(Some(2), vec![cambio("ventas", venta_escritorio(99010, &vu, 3, &ts), Some(json!({"venta_detalle": [detalle(88010, &du, 99010, 7002, &ts)]})), Some(Value::Object(rf)))]),
    )
    .await
    .unwrap();
    assert_eq!(r.aceptados, vec![vu.clone()], "{:?}", r.rechazados);
    assert_eq!(r.faltantes, vec![pu.clone()]);
    assert_eq!(entero(p, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{vu}'")).await, 4, "Mariana del servidor = 4");
    assert_eq!(entero(p, &format!("SELECT producto_id FROM venta_detalle WHERE uuid = '{du}'")).await, 0, "centinela 0 (NOT NULL)");
    assert_eq!(estado(p, "ventas", &vu).await.as_deref(), Some("pendiente"));
    assert_eq!(
        texto(p, &format!("SELECT ref_uuid FROM sync_fk_pendiente WHERE tabla = 'venta_detalle' AND fila_uuid = '{du}' AND columna = 'producto_id'")).await,
        Some(pu.clone())
    );

    // Llega el producto → se resuelve al final del push.
    let r = procesar_push(p, &body(Some(2), vec![cambio("productos", producto_escritorio(7002, &pu, &ts), None, Some(json!({})))])).await.unwrap();
    assert_eq!(r.aceptados, vec![pu.clone()]);
    assert_eq!(entero(p, &format!("SELECT producto_id FROM venta_detalle WHERE uuid = '{du}'")).await, id_de(p, "productos", &pu).await);
    assert_eq!(entero(p, &format!("SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid = '{du}'")).await, 0);
    assert_eq!(estado(p, "ventas", &vu).await.as_deref(), Some("traducido"));

    // Push v2 sin refs de una venta nueva con usuario local 4 (Lau) sin
    // entrada en el mapa → centinela 0 + pendiente por id crudo.
    let vu2 = hex32();
    let r = procesar_push(p, &body(Some(2), vec![cambio("ventas", venta_escritorio(99011, &vu2, 4, &ts), Some(json!({"venta_detalle": []})), None)])).await.unwrap();
    assert_eq!(r.aceptados, vec![vu2.clone()]);
    assert_eq!(entero(p, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{vu2}'")).await, 0);
    assert_eq!(estado(p, "ventas", &vu2).await.as_deref(), Some("pendiente"));
    // El escritorio sube su mapa de usuarios → se resuelve (Lau = 66 en el servidor).
    let r = procesar_id_map(
        p,
        &IdMapBody { device_uuid: DEV.into(), tabla: "usuarios".into(), pares: vec![json!([4, LAU]), json!([9, "no-existe-aqui"]), json!(["x"])] },
    )
    .await
    .unwrap();
    assert_eq!(r["recibidos"], json!(2));
    assert_eq!(r["invalidos"], json!(1));
    assert_eq!(r["desconocidos"], json!(["no-existe-aqui"]));
    assert_eq!(entero(p, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{vu2}'")).await, 66);
    assert_eq!(estado(p, "ventas", &vu2).await.as_deref(), Some("traducido"));
    // Push VIEJO (sin proto) de otra venta nueva con Lau: cruda, sin
    // pendiente ni estado (SPEC2 R1.1); el mapa no la toca (la repara la
    // reparación).
    let vu3 = hex32();
    let r = procesar_push(p, &body(None, vec![cambio("ventas", venta_escritorio(99012, &vu3, 4, &ts), Some(json!({"venta_detalle": []})), None)])).await.unwrap();
    assert_eq!(r.aceptados, vec![vu3.clone()]);
    assert_eq!(entero(p, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{vu3}'")).await, 4);
    assert_eq!(estado(p, "ventas", &vu3).await, None);
    assert_eq!(entero(p, &format!("SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid = '{vu3}'")).await, 0);
    assert_eq!(
        texto(p, &format!("SELECT fuente FROM sync_id_map WHERE device_uuid = '{DEV}' AND tabla = 'usuarios' AND local_id = 4")).await.as_deref(),
        Some("bootstrap")
    );
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// (d) LWW (igual aplica, futuro recortado), merge de productos, columnas,
//     origen/caja inmutables, stock_sucursal y DELETE, hijos estables
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn d_lww_merge_columnas_e_hijos() {
    let Some(c) = ctx("d").await else { return };
    let p = &c.pool;
    let ts = ahora(p).await;

    // Igual → aplica.
    let mut cli: Map<String, Value> =
        serde_json::from_str(&texto(p, "SELECT row_to_json(c)::text FROM clientes c WHERE id = 1").await.unwrap()).unwrap();
    let cu = cli["uuid"].as_str().unwrap().to_string();
    cli.insert("nombre".into(), json!("MIGUE IGUAL"));
    let r = procesar_push(p, &body(Some(2), vec![cambio("clientes", Value::Object(cli.clone()), None, None)])).await.unwrap();
    assert_eq!(r.aceptados, vec![cu.clone()], "{:?}", r.rechazados);
    assert_eq!(texto(p, &format!("SELECT nombre FROM clientes WHERE uuid = '{cu}'")).await.as_deref(), Some("MIGUE IGUAL"));
    // La web lo edita después; un push más viejo del escritorio → remoto_mas_nuevo.
    sqlx::query("UPDATE clientes SET nombre = 'WEB', updated_at = $2 WHERE uuid = $1").bind(&cu).bind(&ts).execute(p).await.unwrap();
    sqlx::query("INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) VALUES ('clientes', $1, 1, 'web-pos')").bind(&cu).execute(p).await.unwrap();
    cli.insert("updated_at".into(), json!("2020-01-01 00:00:00"));
    cli.insert("nombre".into(), json!("VIEJO"));
    let r = procesar_push(p, &body(Some(2), vec![cambio("clientes", Value::Object(cli.clone()), None, None)])).await.unwrap();
    assert_eq!(r.rechazados.len(), 1);
    assert_eq!(r.rechazados[0].motivo, "remoto_mas_nuevo");
    assert_eq!(texto(p, &format!("SELECT nombre FROM clientes WHERE uuid = '{cu}'")).await.as_deref(), Some("WEB"));
    // Futuro → se recorta a la hora del servidor.
    cli.insert("updated_at".into(), json!("2099-01-01T00:00:00Z"));
    cli.insert("nombre".into(), json!("FUTURO"));
    let r = procesar_push(p, &body(Some(2), vec![cambio("clientes", Value::Object(cli.clone()), None, None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1);
    let guardado = texto(p, &format!("SELECT updated_at FROM clientes WHERE uuid = '{cu}'")).await.unwrap();
    assert!(guardado < "2099".to_string() && guardado >= ts, "updated_at recortado: {guardado}");

    // Merge de productos: la web editó el precio después; el escritorio manda stock.
    let (_, pu) = producto_existente(p, 200).await;
    sqlx::query("UPDATE productos SET precio_venta = 999, updated_at = $2 WHERE uuid = $1").bind(&pu).bind(&ts).execute(p).await.unwrap();
    sqlx::query("INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) VALUES ('productos', $1, 1, 'web-pos')").bind(&pu).execute(p).await.unwrap();
    let cursor_antes = entero(p, "SELECT max(id) FROM sync_cursor").await;
    let mut prod: Map<String, Value> =
        serde_json::from_str(&texto(p, &format!("SELECT row_to_json(x)::text FROM productos x WHERE uuid = '{pu}'")).await.unwrap()).unwrap();
    prod.insert("id".into(), json!(200));
    prod.insert("stock_actual".into(), json!(42.0));
    prod.insert("precio_venta".into(), json!(1.0));
    prod.insert("updated_at".into(), json!("2026-01-01 00:00:00"));
    let r = procesar_push(p, &body(Some(2), vec![cambio("productos", Value::Object(prod.clone()), None, Some(json!({})))])).await.unwrap();
    assert_eq!(r.aceptados, vec![pu.clone()], "merge aceptado: {:?}", r.rechazados);
    assert_eq!(texto(p, &format!("SELECT stock_actual::text FROM productos WHERE uuid = '{pu}'")).await.as_deref(), Some("42.00"));
    assert_eq!(texto(p, &format!("SELECT precio_venta::text FROM productos WHERE uuid = '{pu}'")).await.as_deref(), Some("999.00"));
    assert_eq!(
        texto(p, &format!("SELECT origen_device FROM sync_cursor WHERE tabla = 'productos' AND uuid = '{pu}' ORDER BY id DESC LIMIT 1")).await.as_deref(),
        Some("merge")
    );
    // El escritorio baja la fila combinada (el cursor 'merge' no es su eco).
    let pr = procesar_pull(p, &PullQuery { cursor: cursor_antes.to_string(), sucursal_id: 1, device_uuid: Some(DEV.into()), proto: Some(2) }).await.unwrap();
    let bajado = pr.cambios.iter().find(|x| x["__data"]["uuid"] == json!(pu)).expect("merge en el pull");
    assert_eq!(bajado["__origen"], json!("merge"));
    assert_eq!(bajado["__data"]["stock_actual"], json!(42.0));
    assert_eq!(bajado["__data"]["precio_venta"], json!(999.0));
    // Conflicto contra una versión del MISMO dispositivo → se aplica (SPEC2
    // R1.3: el escritorio siempre manda su fila actual).
    let (_, pu2) = producto_existente(p, 201).await;
    sqlx::query("UPDATE productos SET updated_at = $2 WHERE uuid = $1").bind(&pu2).bind(&ts).execute(p).await.unwrap();
    sqlx::query("INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) VALUES ('productos', $1, 1, $2)").bind(&pu2).bind(DEV).execute(p).await.unwrap();
    let mut prod2: Map<String, Value> =
        serde_json::from_str(&texto(p, &format!("SELECT row_to_json(x)::text FROM productos x WHERE uuid = '{pu2}'")).await.unwrap()).unwrap();
    prod2.insert("updated_at".into(), json!("2026-01-01 00:00:00"));
    prod2.insert("nombre".into(), json!("MISMO EQUIPO"));
    let r = procesar_push(p, &body(Some(2), vec![cambio("productos", Value::Object(prod2), None, None)])).await.unwrap();
    assert_eq!(r.aceptados, vec![pu2.clone()], "{:?}", r.rechazados);
    assert_eq!(
        texto(p, &format!("SELECT nombre || '|' || updated_at FROM productos WHERE uuid = '{pu2}'")).await.as_deref(),
        Some("MISMO EQUIPO|2026-01-01 00:00:00")
    );

    // origen/caja no se sobreescriben (venta web empujada con otros valores).
    let wu = texto(p, "SELECT uuid FROM ventas WHERE origen = 'web' ORDER BY id LIMIT 1").await.unwrap();
    let mut wv: Map<String, Value> =
        serde_json::from_str(&texto(p, &format!("SELECT row_to_json(v)::text FROM ventas v WHERE uuid = '{wu}'")).await.unwrap()).unwrap();
    wv.insert("origen".into(), json!("desktop"));
    wv.insert("caja".into(), json!("web"));
    wv.insert("updated_at".into(), json!(ts));
    let r = procesar_push(p, &body(Some(2), vec![cambio("ventas", Value::Object(wv), None, None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(texto(p, &format!("SELECT origen || '/' || caja FROM ventas WHERE uuid = '{wu}'")).await.as_deref(), Some("web/principal"));

    // Columnas: desconocida/maliciosa se ignora; '' en texto NOT NULL se queda ''.
    let mu = hex32();
    let mut rf = Map::new();
    rf.insert(mu.clone(), json!({"usuario_id": NANCY, "autorizado_por": null, "corte_id": null}));
    let mov = json!({
        "id": 5001, "uuid": mu, "sucursal_id": 1, "tipo": "RETIRO", "usuario_id": 2, "monto": "12.5",
        "concepto": "", "autorizado_por": null, "corte_id": null, "fecha": ts, "updated_at": ts,
        "deleted_at": null, "foo": 1, "nombre) VALUES (1)--": "x", "origen": "desktop", "caja": "principal"
    });
    let r = procesar_push(p, &body(Some(2), vec![cambio("movimientos_caja", mov, None, Some(Value::Object(rf)))])).await.unwrap();
    assert_eq!(r.aceptados, vec![mu.clone()], "{:?}", r.rechazados);
    assert_eq!(texto(p, &format!("SELECT concepto FROM movimientos_caja WHERE uuid = '{mu}'")).await.as_deref(), Some(""));
    assert_eq!(texto(p, &format!("SELECT monto::text FROM movimientos_caja WHERE uuid = '{mu}'")).await.as_deref(), Some("12.50"));
    assert_eq!(entero(p, &format!("SELECT usuario_id FROM movimientos_caja WHERE uuid = '{mu}'")).await, 3);

    // stock_sucursal: aceptado sin hacer nada (UPDATE y DELETE).
    let su = hex32();
    let ss = json!({"id": 1, "uuid": su, "producto_id": 1, "sucursal_id": 1, "stock_actual": 1, "stock_minimo": 0, "updated_at": ts});
    let mut del = cambio("stock_sucursal", json!({"uuid": su, "deleted": true}), None, None);
    del.operacion = "DELETE".into();
    let r = procesar_push(p, &body(Some(2), vec![cambio("stock_sucursal", ss, None, None), del])).await.unwrap();
    assert_eq!(r.aceptados.len(), 2);
    assert_eq!(entero(p, &format!("SELECT count(*) FROM stock_sucursal WHERE uuid = '{su}'")).await, 0);
    assert_eq!(entero(p, &format!("SELECT count(*) FROM sync_cursor WHERE uuid = '{su}'")).await, 0);

    // DELETE → borrado lógico con hora local.
    let mut del = cambio("clientes", json!({"uuid": cu, "deleted": true}), None, None);
    del.operacion = "DELETE".into();
    let r = procesar_push(p, &body(Some(2), vec![del])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1);
    let borrado = texto(p, &format!("SELECT deleted_at FROM clientes WHERE uuid = '{cu}'")).await.unwrap();
    assert!(borrado >= ts && borrado < "2099".to_string());

    // Hijos: ids estables, los que ya no vienen se borran, no se roban hijos ajenos.
    let (vu, d1, d2) = (hex32(), hex32(), hex32());
    let (_, prod_u) = producto_existente(p, 100).await;
    let refs_v = |uuids: &[&String]| {
        let mut m = Map::new();
        m.insert(vu.clone(), json!({"usuario_id": DUENIO, "cliente_id": null, "anulada_por": null}));
        for u in uuids {
            m.insert((*u).clone(), json!({"producto_id": prod_u, "autorizado_por": null}));
        }
        Value::Object(m)
    };
    let hijos = json!({"venta_detalle": [detalle(1, &d1, 99020, 100, &ts), detalle(2, &d2, 99020, 100, &ts)]});
    let r = procesar_push(p, &body(Some(2), vec![cambio("ventas", venta_escritorio(99020, &vu, 1, &ts), Some(hijos.clone()), Some(refs_v(&[&d1, &d2])))])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    let (id1, id2) = (id_de(p, "venta_detalle", &d1).await, id_de(p, "venta_detalle", &d2).await);
    let r = procesar_push(p, &body(Some(2), vec![cambio("ventas", venta_escritorio(99020, &vu, 1, &ts), Some(hijos), Some(refs_v(&[&d1, &d2])))])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1);
    assert_eq!((id_de(p, "venta_detalle", &d1).await, id_de(p, "venta_detalle", &d2).await), (id1, id2), "ids de hijos estables");
    let solo1 = json!({"venta_detalle": [detalle(1, &d1, 99020, 100, &ts)]});
    procesar_push(p, &body(Some(2), vec![cambio("ventas", venta_escritorio(99020, &vu, 1, &ts), Some(solo1), Some(refs_v(&[&d1])))])).await.unwrap();
    assert_eq!(entero(p, &format!("SELECT count(*) FROM venta_detalle WHERE uuid = '{d2}'")).await, 0);
    assert_eq!(id_de(p, "venta_detalle", &d1).await, id1);
    // Otra venta trae un hijo con el uuid de d1 → no se mueve.
    let vu3 = hex32();
    let robado = json!({"venta_detalle": [detalle(1, &d1, 99021, 100, &ts)]});
    let r = procesar_push(p, &body(Some(2), vec![cambio("ventas", venta_escritorio(99021, &vu3, 1, &ts), Some(robado), None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1);
    assert_eq!(
        entero(p, &format!("SELECT venta_id FROM venta_detalle WHERE uuid = '{d1}'")).await,
        id_de(p, "ventas", &vu).await,
        "el hijo sigue con su padre original"
    );
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// (e) Reparación de FK con el mapa del escritorio de la tienda
// ─────────────────────────────────────────────────────────────────────────

/// md5 de uuid||updated_at por tabla (las que toca la reparación).
async fn sumas_updated_at(p: &PgPool) -> Vec<(String, String)> {
    let mut tablas: Vec<&str> = sync_fk::celdas_reparables().iter().map(|c| c.tabla).collect();
    tablas.extend(sync_fk::tablas_padre_reparables());
    tablas.sort();
    tablas.dedup();
    let mut out = Vec::new();
    for t in tablas {
        let s = texto(p, &format!("SELECT md5(string_agg(uuid || '|' || updated_at, ',' ORDER BY uuid)) FROM {t}")).await;
        out.push((t.to_string(), s.unwrap_or_default()));
    }
    out
}

/// Huella de las FK de filas hechas en la web (no deben cambiar).
async fn huella_web(p: &PgPool) -> String {
    texto(
        p,
        "SELECT md5(concat( \
           (SELECT string_agg(uuid || ':' || usuario_id || ':' || coalesce(cliente_id::text,'-'), ',' ORDER BY uuid) \
              FROM ventas WHERE uuid !~ '^[0-9a-f]{32}$'), \
           (SELECT string_agg(uuid || ':' || coalesce(usuario_id::text,'-') || ':' || coalesce(registro_id::text,'-'), ',' ORDER BY uuid) \
              FROM audit_log WHERE origen = 'WEB'), \
           (SELECT string_agg(uuid || ':' || coalesce(categoria_id::text,'-') || ':' || coalesce(proveedor_id::text,'-'), ',' ORDER BY uuid) \
              FROM productos WHERE uuid !~ '^[0-9a-f]{32}$'), \
           (SELECT string_agg(uuid || ':' || usuario_id, ',' ORDER BY uuid) FROM aperturas_caja WHERE uuid !~ '^[0-9a-f]{32}$')))",
    )
    .await
    .unwrap()
}

async fn vendedores_hex32(p: &PgPool) -> Vec<(i64, i64)> {
    sqlx::query_as("SELECT usuario_id, count(*) FROM ventas WHERE uuid ~ '^[0-9a-f]{32}$' GROUP BY 1 ORDER BY 1")
        .fetch_all(p)
        .await
        .unwrap()
}

/// LOGIN/LOGOUT del escritorio cuyo usuario coincide con el nombre del texto.
async fn logins_bien(p: &PgPool) -> (i64, i64) {
    let r: (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE u.nombre_usuario = substring(a.descripcion_legible from ': ([^:]*)$')) \
           FROM audit_log a LEFT JOIN usuarios u ON u.id = a.usuario_id \
          WHERE a.origen = 'POS' AND a.accion IN ('LOGIN', 'LOGOUT')",
    )
    .fetch_one(p)
    .await
    .unwrap();
    r
}

#[tokio::test]
async fn e_reparacion_con_mapa_del_escritorio() {
    let Some(c) = ctx("e").await else { return };
    let p = &c.pool;
    let reloj = std::time::Instant::now();
    let marca = |paso: &str| eprintln!("[e] {paso}: {:?}", reloj.elapsed());

    // Una venta que ya llegó con el push v2 (traducida, Lau = 66 aquí): la
    // reparación no debe volver a traducirla.
    let ts = ahora(p).await;
    let (_, prod_u) = producto_existente(p, 100).await;
    let (pv, pd) = (hex32(), hex32());
    let mut rf = Map::new();
    rf.insert(pv.clone(), json!({"usuario_id": LAU, "cliente_id": null, "anulada_por": null}));
    rf.insert(pd.clone(), json!({"producto_id": prod_u, "autorizado_por": null}));
    let r = procesar_push(p, &body(Some(2), vec![cambio("ventas", venta_escritorio(99050, &pv, 4, &ts), Some(json!({"venta_detalle": [detalle(88050, &pd, 99050, 100, &ts)]})), Some(Value::Object(rf)))])).await.unwrap();
    assert_eq!(r.aceptados, vec![pv.clone()]);
    assert_eq!(estado(p, "ventas", &pv).await.as_deref(), Some("traducido"));

    let vend0 = vendedores_hex32(p).await;
    let (logins_total, logins_ok0) = logins_bien(p).await;
    let sumas0 = sumas_updated_at(p).await;
    let cursor0 = entero(p, "SELECT max(id) FROM sync_cursor").await;
    let web0 = huella_web(p).await;
    let colgadas0 = entero(p, "SELECT count(*) FROM venta_detalle vd WHERE NOT EXISTS (SELECT 1 FROM productos x WHERE x.id = vd.producto_id)").await;
    assert_eq!(vend0.iter().map(|x| x.0).collect::<Vec<_>>(), vec![1, 2, 3, 4, 66], "ids crudos del escritorio + la venta v2");
    assert!(logins_total > 0);

    marca("línea base");
    // Sin mapa de usuarios la verificación falla y no se confirma nada.
    let r = sync_fk::reparar(p, DEV, false).await.unwrap();
    assert!(!r.ok, "{}", r.reporte);
    assert_eq!(vendedores_hex32(p).await, vend0);
    assert_eq!(entero(p, "SELECT count(*) FROM sync_fk_estado").await, 1);
    assert_eq!(entero(p, "SELECT count(*) FROM sync_fk_repair_log").await, 0);

    marca("reparación sin mapa");
    // Mapa del escritorio (hechos de producción): usuarios 1..4, productos
    // idénticos hasta 5948, categorías por nombre en el orden de la semilla.
    let usuarios = vec![json!([1, DUENIO]), json!([2, NANCY]), json!([3, MARIANA]), json!([4, LAU])];
    let r = procesar_id_map(p, &IdMapBody { device_uuid: DEV.into(), tabla: "usuarios".into(), pares: usuarios }).await.unwrap();
    assert_eq!(r["desconocidos"], json!([]));
    let prods: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, uuid FROM productos WHERE id <= 5948 AND uuid ~ '^[0-9a-f]{32}$' ORDER BY id")
            .fetch_all(p)
            .await
            .unwrap();
    for trozo in prods.chunks(5000) {
        let pares = trozo.iter().map(|(i, u)| json!([i, u])).collect();
        let r = procesar_id_map(p, &IdMapBody { device_uuid: DEV.into(), tabla: "productos".into(), pares }).await.unwrap();
        assert_eq!(r["desconocidos"], json!([]));
    }
    let semilla = [
        "Frenos", "Cadenas y Piñones", "Aceites y Lubricantes", "Eléctrico", "Suspensión",
        "Motor", "Carrocería", "Transmisión", "Neumáticos", "Accesorios",
    ];
    let mut pares = Vec::new();
    for (i, n) in semilla.iter().enumerate() {
        let u: String = sqlx::query_scalar("SELECT uuid FROM categorias WHERE nombre = $1").bind(n).fetch_one(p).await.unwrap();
        pares.push(json!([i as i64 + 1, u]));
    }
    procesar_id_map(p, &IdMapBody { device_uuid: DEV.into(), tabla: "categorias".into(), pares }).await.unwrap();

    marca("mapa subido");
    // Otro escritorio con el mismo mapa no toca filas que no subió él.
    let otros = vec![json!([1, DUENIO]), json!([2, NANCY]), json!([3, MARIANA]), json!([4, LAU])];
    procesar_id_map(p, &IdMapBody { device_uuid: "otro-escritorio".into(), tabla: "usuarios".into(), pares: otros }).await.unwrap();
    let ro = sync_fk::reparar(p, "otro-escritorio", true).await.unwrap();
    assert_eq!(ro.reporte["celdas_cambiadas"], json!(0), "{}", ro.reporte);

    // Simulación: reporte correcto, nada confirmado.
    let sim = sync_fk::reparar(p, DEV, true).await.unwrap();
    assert!(sim.ok, "{}", sim.reporte);
    assert_eq!(vendedores_hex32(p).await, vend0);
    assert_eq!(entero(p, "SELECT count(*) FROM sync_fk_estado").await, 1);

    marca("simulación");
    // Reparación real.
    let t0 = std::time::Instant::now();
    let r = sync_fk::reparar(p, DEV, false).await.unwrap();
    eprintln!("reparación: {:?} — {}", t0.elapsed(), r.reporte);
    assert!(r.ok, "{}", r.reporte);
    let lote1 = r.reporte["lote"].as_str().unwrap().to_string();
    assert_eq!(r.reporte["celdas_cambiadas"], sim.reporte["celdas_cambiadas"]);

    marca("reparación");
    // Ninguna venta del escritorio a nombre de 'Andrew Web'.
    assert_eq!(
        entero(p, "SELECT count(*) FROM ventas v JOIN usuarios u ON u.id = v.usuario_id \
                    WHERE v.uuid ~ '^[0-9a-f]{32}$' AND u.uuid LIKE 'web-admin-%'").await,
        0
    );
    // Vendedores: 1→Dueño(2), 2→Nancy(3), 3→Mariana(4), 4→Lau(66), mismas
    // cantidades; la venta v2 (ya en 66) no se vuelve a traducir.
    let mut esperado: HashMap<i64, i64> = HashMap::new();
    for (u, n) in &vend0 {
        *esperado.entry(match u { 1 => 2, 2 => 3, 3 => 4, 4 => 66, x => *x }).or_default() += n;
    }
    let mut esperado: Vec<(i64, i64)> = esperado.into_iter().collect();
    esperado.sort();
    assert_eq!(vendedores_hex32(p).await, esperado);
    assert_eq!(entero(p, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{pv}'")).await, 66);
    assert_eq!(estado(p, "ventas", &pv).await.as_deref(), Some("traducido"));
    // Bitácora: LOGIN/LOGOUT nombran al usuario correcto (los 4 están en el mapa).
    assert_eq!(logins_bien(p).await, (logins_total, logins_total));
    assert_eq!(logins_ok0, 0);
    // Filas web intactas; updated_at y sync_cursor intactos.
    assert_eq!(huella_web(p).await, web0);
    assert_eq!(sumas_updated_at(p).await, sumas0);
    assert_eq!(entero(p, "SELECT max(id) FROM sync_cursor").await, cursor0);
    assert!(entero(p, "SELECT count(*) FROM venta_detalle vd WHERE NOT EXISTS (SELECT 1 FROM productos x WHERE x.id = vd.producto_id)").await <= colgadas0);
    assert_eq!(texto(p, "SELECT valor FROM sync_meta WHERE clave = 'fk_reparado'").await.as_deref(), Some("1"));
    // Toda celda cambiada está en la bitácora con su valor anterior/nuevo.
    let log_celdas = entero(p, &format!("SELECT count(*) FROM sync_fk_repair_log WHERE lote = '{lote1}' AND columna <> '__estado'")).await;
    assert_eq!(json!(log_celdas), r.reporte["celdas_cambiadas"]);
    assert!(entero(p, "SELECT count(*) FROM sync_fk_estado WHERE tabla = 'ventas' AND estado IN ('reparado', 'sin_mapa')").await >= 9969);

    marca("verificaciones");
    // Segunda corrida: 0 celdas, ningún estado nuevo.
    let r2 = sync_fk::reparar(p, DEV, false).await.unwrap();
    assert!(r2.ok, "{}", r2.reporte);
    assert_eq!(r2.reporte["celdas_cambiadas"], json!(0));
    assert_eq!(r2.reporte["filas"], json!({}));

    // Llega el mapa de clientes (identidad) → las filas 'sin_mapa' se
    // reintentan: 0 celdas cambian (identidad) pero bajan las 'sin_mapa'.
    let sin_mapa_antes = entero(p, "SELECT count(*) FROM sync_fk_estado WHERE estado = 'sin_mapa'").await;
    let clis: Vec<(i64, String)> = sqlx::query_as("SELECT id, uuid FROM clientes WHERE uuid ~ '^[0-9a-f]{32}$'").fetch_all(p).await.unwrap();
    let pares = clis.iter().map(|(i, u)| json!([i, u])).collect();
    procesar_id_map(p, &IdMapBody { device_uuid: DEV.into(), tabla: "clientes".into(), pares }).await.unwrap();
    let r3 = sync_fk::reparar(p, DEV, false).await.unwrap();
    assert!(r3.ok, "{}", r3.reporte);
    assert_eq!(r3.reporte["celdas_cambiadas"], json!(0));
    let lote3 = r3.reporte["lote"].as_str().unwrap().to_string();
    let sin_mapa_despues = entero(p, "SELECT count(*) FROM sync_fk_estado WHERE estado = 'sin_mapa'").await;
    eprintln!("sin_mapa: {sin_mapa_antes} → {sin_mapa_despues}");
    assert!(sin_mapa_despues < sin_mapa_antes);
    assert_eq!(vendedores_hex32(p).await, esperado);

    marca("segunda corrida");
    // Reversa del lote 1: todo vuelve a como estaba.
    // (en orden inverso: primero el lote 3, luego el 1)
    let rv3 = sync_fk::revertir_lote(p, &lote3).await.unwrap();
    assert_eq!(rv3["celdas_no_restauradas"], json!(0), "{rv3}");
    assert_eq!(entero(p, "SELECT count(*) FROM sync_fk_estado WHERE estado = 'sin_mapa'").await, sin_mapa_antes);
    let rv = sync_fk::revertir_lote(p, &lote1).await.unwrap();
    marca("reversa");
    assert_eq!(rv["celdas_no_restauradas"], json!(0), "{rv}");
    assert_eq!(vendedores_hex32(p).await, vend0);
    assert_eq!(logins_bien(p).await, (logins_total, logins_ok0));
    assert_eq!(sumas_updated_at(p).await, sumas0);
    assert_eq!(huella_web(p).await, web0);
    // Solo queda el estado de la venta v2.
    assert_eq!(entero(p, "SELECT count(*) FROM sync_fk_estado").await, 1);
    assert_eq!(estado(p, "ventas", &pv).await.as_deref(), Some("traducido"));
    assert_eq!(entero(p, "SELECT count(*) FROM sync_fk_repair_log").await, 0);
    assert_eq!(texto(p, "SELECT valor FROM sync_meta WHERE clave = 'fk_reparado'").await, None);
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// (f) pull v2 (__refs solo en idioma del servidor) y pull legacy
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn f_pull_v2_y_legacy() {
    let Some(c) = ctx("f").await else { return };
    let p = &c.pool;
    let ts = ahora(p).await;
    let c0 = entero(p, "SELECT max(id) FROM sync_cursor").await;
    let (prod_id, prod_uuid) = producto_existente(p, 100).await;

    // T: venta del escritorio traducida (push v2).
    let (tu, td) = (hex32(), hex32());
    let mut rf = Map::new();
    rf.insert(tu.clone(), json!({"usuario_id": DUENIO, "cliente_id": null, "anulada_por": null}));
    rf.insert(td.clone(), json!({"producto_id": prod_uuid, "autorizado_por": null}));
    procesar_push(p, &body(Some(2), vec![cambio("ventas", venta_escritorio(99030, &tu, 1, &ts), Some(json!({"venta_detalle": [detalle(88030, &td, 99030, 100, &ts)]})), Some(Value::Object(rf)))])).await.unwrap();

    // W: venta web (uuid v7) con renglón; M: movimiento web.
    let wu = uuid::Uuid::now_v7().to_string();
    let wd = uuid::Uuid::now_v7().to_string();
    let mu = uuid::Uuid::now_v7().to_string();
    sqlx::raw_sql(&format!(
        "INSERT INTO ventas (uuid, sucursal_id, folio, usuario_id, total, origen, caja) VALUES ('{wu}', 1, 'V-W1', 3, 10, 'web', 'principal'); \
         INSERT INTO venta_detalle (uuid, venta_id, producto_id) VALUES ('{wd}', (SELECT id FROM ventas WHERE uuid = '{wu}'), {prod_id}); \
         INSERT INTO movimientos_caja (uuid, sucursal_id, tipo, usuario_id, monto, concepto, origen, caja) VALUES ('{mu}', 1, 'RETIRO', 3, 5, 'x', 'web', 'web'); \
         INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) VALUES ('ventas', '{wu}', 1, 'web-pos'), ('movimientos_caja', '{mu}', 1, 'web-pos');"
    ))
    .execute(p)
    .await
    .unwrap();
    // U: venta vieja del escritorio sin traducir, tocada por la web.
    let uu = texto(p, "SELECT uuid FROM ventas WHERE uuid ~ '^[0-9a-f]{32}$' AND usuario_id = 4 ORDER BY id LIMIT 1").await.unwrap();
    // D: producto borrado (lógico) por la web; usuario web-admin; stock_sucursal.
    let (_, du) = producto_existente(p, 300).await;
    let wa = texto(p, "SELECT uuid FROM usuarios WHERE uuid LIKE 'web-admin-%'").await.unwrap();
    sqlx::raw_sql(&format!(
        "UPDATE productos SET deleted_at = '{ts}', updated_at = '{ts}' WHERE uuid = '{du}'; \
         INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) VALUES \
           ('ventas', '{uu}', 1, 'web-pos'), ('productos', '{du}', 1, 'web-pos'), \
           ('usuarios', '{wa}', 1, 'web-pos'), ('stock_sucursal', 'ss-1', 1, 'web-pos');"
    ))
    .execute(p)
    .await
    .unwrap();
    let c_max = entero(p, "SELECT max(id) FROM sync_cursor").await;

    let buscar = |cs: &Vec<Value>, u: &str| cs.iter().find(|x| x["__data"]["uuid"] == json!(u)).cloned();

    // ── v2 desde otro equipo ──
    let r = procesar_pull(p, &PullQuery { cursor: c0.to_string(), sucursal_id: 1, device_uuid: Some("otro-equipo".into()), proto: Some(2) }).await.unwrap();
    assert_eq!(r.next_cursor, c_max.to_string());
    let caja_v2 = texto(p, "SELECT valor FROM sync_meta WHERE clave = 'caja_v2_desde'").await;
    assert!(caja_v2.is_some(), "la migración 011 anota caja_v2_desde");
    assert_eq!(r.caja_v2_desde, caja_v2);
    let t = buscar(&r.cambios, &tu).expect("T en v2");
    assert_eq!(t["__operacion"], json!("UPSERT"));
    assert_eq!(t["__origen"], json!(DEV));
    assert!(t["__cursor"].is_i64());
    assert_eq!(t["__refs"][&tu]["usuario_id"], json!(DUENIO));
    assert_eq!(t["__refs"][&td]["producto_id"], json!(prod_uuid));
    let w = buscar(&r.cambios, &wu).expect("W en v2");
    assert_eq!(w["__refs"][&wu]["usuario_id"], json!(NANCY), "server 3 = Nancy");
    assert_eq!(w["__refs"][&wd]["producto_id"], json!(prod_uuid));
    let u = buscar(&r.cambios, &uu).expect("U en v2");
    assert!(u.get("__refs").is_none(), "fila del escritorio sin traducir: sin __refs");
    assert!(buscar(&r.cambios, &mu).is_some(), "movimiento web en v2");
    let d = buscar(&r.cambios, &du).expect("D en v2");
    assert_eq!(d["__operacion"], json!("UPSERT"));
    assert_eq!(d["__data"]["deleted_at"], json!(ts));
    assert!(buscar(&r.cambios, &wa).is_none() && buscar(&r.cambios, "ss-1").is_none());

    // ── v2 desde el mismo escritorio: sin eco ──
    let r = procesar_pull(p, &PullQuery { cursor: c0.to_string(), sucursal_id: 1, device_uuid: Some(DEV.into()), proto: Some(2) }).await.unwrap();
    assert!(buscar(&r.cambios, &tu).is_none());

    // ── legacy ──
    let r = procesar_pull(p, &PullQuery { cursor: c0.to_string(), sucursal_id: 1, device_uuid: None, proto: None }).await.unwrap();
    assert_eq!(r.next_cursor, c_max.to_string(), "avanza aunque omita filas");
    assert_eq!(r.caja_v2_desde, caja_v2, "también en el pull viejo");
    let v = serde_json::to_value(&r).unwrap();
    assert!(v.get("caja_v2_desde").map(|x| x.is_string()).unwrap_or(false), "{}", v["caja_v2_desde"]);
    let t = buscar(&r.cambios, &tu).expect("T en legacy");
    let td_l = t["__data"].as_object().unwrap();
    assert!(!td_l.contains_key("usuario_id") && !td_l.contains_key("cliente_id") && !td_l.contains_key("anulada_por"));
    assert_eq!(td_l["sucursal_id"], json!(1));
    let hijo = &t["__children"]["venta_detalle"][0];
    assert!(hijo.get("producto_id").is_none() && hijo.get("autorizado_por").is_none());
    assert!(hijo.get("venta_id").is_some());
    assert!(t.get("__refs").is_none());
    assert!(buscar(&r.cambios, &wu).is_none(), "venta web omitida para clientes viejos");
    assert!(buscar(&r.cambios, &mu).is_none(), "movimiento web omitido para clientes viejos");
    let u = buscar(&r.cambios, &uu).expect("U en legacy");
    assert_eq!(u["__data"]["usuario_id"], json!(4), "sin estado: valor crudo");
    assert_eq!(buscar(&r.cambios, &du).unwrap()["__operacion"], json!("DELETE"));

    // ── /sync/fila ──
    let f = procesar_fila(p, &FilaQuery { tabla: "ventas".into(), uuid: tu.clone() }).await.unwrap();
    assert_eq!(f["__refs"][&tu]["usuario_id"], json!(DUENIO));
    assert!(f["__cursor"].is_i64());
    let f = procesar_fila(p, &FilaQuery { tabla: "ventas".into(), uuid: "no-existe".into() }).await.unwrap();
    assert_eq!(f["__operacion"], json!("DELETE"));
    assert_eq!(procesar_fila(p, &FilaQuery { tabla: "usuarios".into(), uuid: wa.clone() }).await.unwrap(), json!({"cambio": null}));
    assert!(procesar_fila(p, &FilaQuery { tabla: "admin_users".into(), uuid: "x".into() }).await.is_err());

    // ── /sync/refs ──
    let m = procesar_refs(p, &RefsBody { tabla: "ventas".into(), uuids: vec![tu.clone(), uu.clone(), wu.clone()] }).await.unwrap();
    let m = m.as_object().unwrap();
    assert!(m.contains_key(&tu) && m.contains_key(&td) && m.contains_key(&wu) && m.contains_key(&wd));
    assert!(!m.contains_key(&uu), "fila sin traducir no da refs");
    assert_eq!(m[&wu]["usuario_id"], json!(NANCY));
    let m = procesar_refs(p, &RefsBody { tabla: "venta_detalle".into(), uuids: vec![td.clone(), wd.clone()] }).await.unwrap();
    assert_eq!(m[&td]["producto_id"], json!(prod_uuid));
    assert_eq!(m[&wd]["producto_id"], json!(prod_uuid));

    // ── Error leyendo una fila: se corta el lote sin DELETE falso ──
    let pres = texto(p, "SELECT uuid FROM presupuestos ORDER BY id LIMIT 1").await.unwrap();
    sqlx::query("INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) VALUES ('presupuestos', $1, 1, 'web-pos'), ('ventas', $2, 1, 'web-pos')")
        .bind(&pres).bind(&uu).execute(p).await.unwrap();
    let id_pres = entero(p, "SELECT max(id) FROM sync_cursor WHERE tabla = 'presupuestos'").await;
    sqlx::raw_sql("ALTER TABLE presupuestos RENAME TO presupuestos_x").execute(p).await.unwrap();
    let r = procesar_pull(p, &PullQuery { cursor: c_max.to_string(), sucursal_id: 1, device_uuid: Some("otro-equipo".into()), proto: Some(2) }).await;
    sqlx::raw_sql("ALTER TABLE presupuestos_x RENAME TO presupuestos").execute(p).await.unwrap();
    let r = r.unwrap();
    assert!(r.cambios.is_empty(), "nada después del error: {:?}", r.cambios);
    assert_eq!(r.next_cursor, (id_pres - 1).to_string());
    assert!(r.hay_mas);
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// (g) Autorización: solo el token del escritorio entra a /sync/*
// ─────────────────────────────────────────────────────────────────────────

fn app(pool: PgPool) -> axum::Router {
    let state = crate::AppState { pool, jwt_secret: SECRETO.to_vec() };
    axum::Router::new()
        .route("/auth/login", post(crate::auth::login))
        .route("/sync/push", post(push))
        .route("/sync/pull", get(pull))
        .route("/sync/id-map", post(id_map))
        .route("/sync/reparar", post(reparar))
        .route("/sync/refs", post(refs))
        .route("/sync/fila", get(fila))
        .with_state(state)
}

async fn llamar(app: &axum::Router, metodo: &str, ruta: &str, token: Option<&str>, cuerpo: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder().method(metodo).uri(ruta);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    let req = match cuerpo {
        Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let st = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[test]
fn token_de_sync_solo_admin_sin_dispositivo() {
    let mk = |role: &str, device: Option<&str>| Claims {
        sub: 1,
        email: "x".into(),
        role: role.into(),
        sucursal: 1,
        device: device.map(|s| s.to_string()),
        exp: 0,
    };
    assert!(token_de_sync(&mk("admin", None)));
    assert!(!token_de_sync(&mk("admin", Some("web-1"))));
    assert!(!token_de_sync(&mk("device", None)));
    assert!(!token_de_sync(&mk("device", Some("web-1"))));
}

#[tokio::test]
async fn g_autorizacion_sync() {
    let Some(c) = ctx("g").await else { return };
    let p = &c.pool;
    // Usuario del panel (como el que configura el POS de escritorio).
    let hash = bcrypt::hash("clave-de-prueba", 4).unwrap();
    sqlx::query("INSERT INTO admin_users (email, password_hash, nombre, sucursal_id) VALUES ('wp2@prueba.local', $1, 'Prueba', 1)")
        .bind(&hash)
        .execute(p)
        .await
        .unwrap();
    let a = app(p.clone());
    let (st, login) = llamar(&a, "POST", "/auth/login", None, Some(json!({"email": "wp2@prueba.local", "password": "clave-de-prueba"}))).await;
    assert_eq!(st, StatusCode::OK, "{login}");
    let token_escritorio = login["token"].as_str().unwrap().to_string();

    let dias = chrono::Duration::days(7);
    let web_vendedor = crate::auth::emitir_token_con_device(SECRETO, 4, "Mariana", "device", 1, Some("web-dev-1".into()), dias).unwrap();
    let web_admin = crate::auth::emitir_token_con_device(SECRETO, 2, "admin", "admin", 1, Some("web-dev-1".into()), dias).unwrap();
    let web_device_sin_dev = crate::auth::emitir_token_con_device(SECRETO, 4, "Mariana", "device", 1, None, dias).unwrap();
    // login_pin de un admin SIN deviceUuid: rol admin y sin dispositivo, pero
    // sub/email son de `usuarios` (Dueño = 2 / 'admin'), no del panel.
    let web_pin_admin = crate::auth::emitir_token_con_device(SECRETO, 2, "admin", "admin", 1, None, dias).unwrap();
    // Usuario del panel desactivado (o token con el email de otro).
    sqlx::query("INSERT INTO admin_users (email, password_hash, nombre, sucursal_id, activo) VALUES ('baja@prueba.local', 'x', 'Baja', 1, false)")
        .execute(p)
        .await
        .unwrap();
    let id_baja = entero(p, "SELECT id FROM admin_users WHERE email = 'baja@prueba.local'").await;
    let id_wp2 = entero(p, "SELECT id FROM admin_users WHERE email = 'wp2@prueba.local'").await;
    let panel_inactivo = crate::auth::emitir_token(SECRETO, id_baja, "baja@prueba.local", "admin", 1, chrono::Duration::days(365)).unwrap();
    let panel_otro_email = crate::auth::emitir_token(SECRETO, id_wp2, "otro@prueba.local", "admin", 1, chrono::Duration::days(365)).unwrap();

    let pull_url = "/sync/pull?cursor=999999999&sucursal_id=1&proto=2&device_uuid=x";
    let push_body = json!({"device_uuid": DEV, "sucursal_id": 1, "proto": 2, "cambios": []});
    for t in [&web_vendedor, &web_admin, &web_device_sin_dev, &web_pin_admin, &panel_inactivo, &panel_otro_email] {
        let t = t.as_str();
        assert_eq!(llamar(&a, "GET", pull_url, Some(t), None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(llamar(&a, "POST", "/sync/push", Some(t), Some(push_body.clone())).await.0, StatusCode::FORBIDDEN);
        assert_eq!(llamar(&a, "POST", "/sync/id-map", Some(t), Some(json!({"device_uuid": DEV, "tabla": "usuarios", "pares": []}))).await.0, StatusCode::FORBIDDEN);
        assert_eq!(llamar(&a, "POST", "/sync/reparar", Some(t), Some(json!({"device_uuid": DEV, "simulacion": true}))).await.0, StatusCode::FORBIDDEN);
        assert_eq!(llamar(&a, "POST", "/sync/refs", Some(t), Some(json!({"tabla": "ventas", "uuids": []}))).await.0, StatusCode::FORBIDDEN);
        assert_eq!(llamar(&a, "GET", "/sync/fila?tabla=ventas&uuid=x", Some(t), None).await.0, StatusCode::FORBIDDEN);
    }
    assert_eq!(llamar(&a, "GET", pull_url, None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(llamar(&a, "GET", pull_url, Some("basura"), None).await.0, StatusCode::UNAUTHORIZED);

    // El token de /auth/login sí pasa (también con el email en otras mayúsculas).
    let (st, r) = llamar(&a, "GET", pull_url, Some(&token_escritorio), None).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(r.get("caja_v2_desde").is_some(), "{r}");
    let mayus = crate::auth::emitir_token(SECRETO, id_wp2, "WP2@Prueba.Local", "admin", 1, chrono::Duration::days(365)).unwrap();
    assert_eq!(llamar(&a, "GET", pull_url, Some(&mayus), None).await.0, StatusCode::OK);
    let (st, r) = llamar(&a, "POST", "/sync/push", Some(&token_escritorio), Some(push_body)).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["aceptados"], json!([]));
    assert!(r.get("faltantes").is_none(), "faltantes vacío no se manda");
    let (st, r) = llamar(&a, "POST", "/sync/refs", Some(&token_escritorio), Some(json!({"tabla": "ventas", "uuids": []}))).await;
    assert_eq!((st, r), (StatusCode::OK, json!({})));
    let (st, _) = llamar(&a, "GET", "/sync/fila?tabla=ventas&uuid=x", Some(&token_escritorio), None).await;
    assert_eq!(st, StatusCode::OK);
    // Reparación sin mapa: la verificación falla → 409 con el reporte.
    let (st, r) = llamar(&a, "POST", "/sync/reparar", Some(&token_escritorio), Some(json!({"device_uuid": DEV}))).await;
    assert_eq!(st, StatusCode::CONFLICT, "{r}");
    assert_eq!(r["ok"], json!(false));
    c.cerrar().await;
}
