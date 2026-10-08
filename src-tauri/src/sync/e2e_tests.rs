// sync/e2e_tests.rs — Prueba de punta a punta del sync v2.
//
// Usa el código REAL del escritorio (worker::push / worker::pull, tareas,
// bandeja, cortes, ventas) contra el binario REAL de server-remoto corriendo
// sobre un CLON local de producción (Postgres). Revisa con SQL los dos lados.
//
// Se SALTA si no está E2E_SERVER_URL. Variables:
//   E2E_SERVER_URL     http://127.0.0.1:<puerto>  servidor sobre el clon A
//   E2E_PG_URL         postgres://postgres@localhost:54329/e2e_srv (default)
//   E2E_SERVER_URL_B   (opcional) servidor sobre el clon B: reparación con el
//                      device_uuid REAL de la tienda (escenario f2)
//   E2E_PG_URL_B       postgres://postgres@localhost:54329/e2e_srv_b (default)
//   E2E_REPLAY_COMPLETO=1  (opcional) en el clon A también corre la
//                      re-descarga completa con un equipo nuevo (~97 mil cursores)
// Las consultas a Postgres van por `psql` y SOLO a localhost (nunca
// producción, nunca prodcopy_base).
//
// Correr (servidores ya levantados, ver scratchpad/e2e/start_srv.sh):
//   E2E_SERVER_URL=http://127.0.0.1:47811 E2E_SERVER_URL_B=http://127.0.0.1:47812 \
//   cargo test --lib e2e -- --nocapture --test-threads=1
//
//   E2E_SRV_LOG        (opcional) bitácora del servidor A: r1/r3 cuentan los
//                      reenvíos idénticos que el servidor reconoció
//
// Escenario del escritorio (igual que la tienda): aquí 1 = Dueño, 2 = Nancy,
// 3 = Mariana, 4 = Lau, con los MISMOS uuids que el servidor (donde son 2, 3,
// 4 y 66). Los productos compartidos tienen ids distintos en cada lado.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::commands::cortes::{self, calcular_periodo, crear_corte_core, NuevoCorte};
use crate::commands::ventas::{crear_venta_core, ItemVenta, NuevaVenta};
use crate::db::migrations::aplicar_migraciones;
use crate::db::schema::{SCHEMA_V1, SEED_DATA};

use super::client::RemoteClient;
use super::{inbox, outbox, state, tareas, worker};

/// device_uuid real del POS de la tienda (pos_devices id 1 en producción).
const DEVICE_TIENDA: &str = "336d5ab58660c6335416c3cca39cf4d9";
const HEX32: &str = "'^[0-9a-f]{32}$'";

type Db = Arc<Mutex<Connection>>;

// ─── Informe (no se detiene en la primera falla) ──────────

#[derive(Default)]
struct Informe {
    ok: usize,
    fallas: Vec<String>,
    notas: Vec<String>,
}

impl Informe {
    fn check(&mut self, esc: &str, cond: bool, msg: String) {
        if cond {
            self.ok += 1;
            println!("  ok    [{esc}] {msg}");
        } else {
            println!("  FALLA [{esc}] {msg}");
            self.fallas.push(format!("[{esc}] {msg}"));
        }
    }
    fn nota(&mut self, esc: &str, msg: String) {
        println!("  nota  [{esc}] {msg}");
        self.notas.push(format!("[{esc}] {msg}"));
    }
    fn cerrar(self, prueba: &str) {
        println!("\n===== {prueba}: {} ok, {} fallas, {} notas =====", self.ok, self.fallas.len(), self.notas.len());
        for f in &self.fallas {
            println!("  FALLA {f}");
        }
        for n in &self.notas {
            println!("  NOTA  {n}");
        }
        assert!(self.fallas.is_empty(), "{prueba}: {} verificaciones fallaron", self.fallas.len());
    }
}

macro_rules! chk {
    ($inf:expr, $esc:expr, $cond:expr, $($fmt:tt)+) => {
        $inf.check($esc, $cond, format!($($fmt)+))
    };
}

// ─── Postgres por psql (solo localhost) ───────────────────

struct Pg {
    url: String,
}

impl Pg {
    fn nuevo(url: &str) -> Pg {
        assert!(
            url.contains("@localhost") || url.contains("@127.0.0.1"),
            "E2E solo contra un Postgres LOCAL: {url}"
        );
        assert!(!url.contains("prodcopy_base"), "nunca contra la plantilla prodcopy_base");
        Pg { url: url.to_string() }
    }

    fn exec(&self, sql: &str) -> String {
        let psql = std::env::var("E2E_PSQL").unwrap_or_else(|_| "psql".into());
        let out = Command::new(&psql)
            .arg(&self.url)
            .args(["-X", "-q", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-F", "\u{1f}", "-c", sql])
            .output()
            .expect("no se pudo ejecutar psql");
        if !out.status.success() {
            panic!("psql: {}\nSQL: {sql}", String::from_utf8_lossy(&out.stderr));
        }
        String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string()
    }

    fn filas(&self, sql: &str) -> Vec<Vec<String>> {
        let s = self.exec(sql);
        if s.is_empty() {
            return vec![];
        }
        s.lines().map(|l| l.split('\u{1f}').map(String::from).collect()).collect()
    }

    fn uno(&self, sql: &str) -> Option<String> {
        self.filas(sql).into_iter().next().and_then(|f| f.into_iter().next())
    }

    fn i64(&self, sql: &str) -> i64 {
        self.uno(sql)
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or_else(|| panic!("se esperaba un entero: {sql}"))
    }

    fn f64(&self, sql: &str) -> f64 {
        self.uno(sql)
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or_else(|| panic!("se esperaba un número: {sql}"))
    }

    fn max_cursor(&self) -> i64 {
        self.i64("SELECT COALESCE(max(id), 0) FROM sync_cursor")
    }
}

// ─── HTTP (web y endpoints de sync crudos) ────────────────

struct Web {
    url: String,
    http: reqwest::Client,
}

impl Web {
    async fn post(&self, ruta: &str, token: Option<&str>, body: &Value) -> (u16, String) {
        let mut r = self.http.post(format!("{}{ruta}", self.url)).header("x-pos-cliente", "2").json(body);
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        let resp = r.send().await.expect("red");
        let st = resp.status().as_u16();
        (st, resp.text().await.unwrap_or_default())
    }

    async fn get(&self, ruta: &str, token: Option<&str>) -> (u16, String) {
        let mut r = self.http.get(format!("{}{ruta}", self.url));
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        let resp = r.send().await.expect("red");
        let st = resp.status().as_u16();
        (st, resp.text().await.unwrap_or_default())
    }

    /// RPC del POS web (con el header del bundle actual).
    async fn rpc(&self, token: &str, cmd: &str, args: Value) -> Result<Value, String> {
        let (st, txt) = self.post(&format!("/rpc/{cmd}"), Some(token), &args).await;
        if !(200..300).contains(&st) {
            return Err(format!("{st}: {txt}"));
        }
        serde_json::from_str(&txt).map_err(|e| format!("json {e}: {txt}"))
    }

    async fn login_pin(&self, pin: &str, device: Option<&str>) -> Value {
        let mut args = json!({ "pin": pin });
        if let Some(d) = device {
            args["deviceUuid"] = json!(d);
        }
        let (st, txt) = self.post("/rpc/login_pin", None, &args).await;
        assert_eq!(st, 200, "login_pin: {txt}");
        let v: Value = serde_json::from_str(&txt).unwrap();
        assert_eq!(v["ok"], json!(true), "login_pin rechazado: {txt}");
        v
    }
}

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn cerca(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.005
}

fn pin_aleatorio() -> String {
    format!("{}", 10_000_000 + rand::random::<u32>() % 89_999_999)
}

fn hex32() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// JWT como los de login_pin del servidor VIEJO (sin claim de dispositivo,
/// 7 días), firmado con el secreto del servidor de prueba.
fn token_pin_viejo(secreto: &str, sub: i64, email: &str, rol: &str) -> String {
    #[derive(serde::Serialize)]
    struct Claims<'a> {
        sub: i64,
        email: &'a str,
        role: &'a str,
        sucursal: i64,
        exp: i64,
    }
    let exp = chrono::Utc::now().timestamp() + 7 * 24 * 3600;
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &Claims { sub, email, role: rol, sucursal: 1, exp },
        &jsonwebtoken::EncodingKey::from_secret(secreto.as_bytes()),
    )
    .unwrap()
}

/// Claims de un JWT (sin verificar la firma; solo para revisar qué emitió el servidor).
fn claims_de(token: &str) -> Value {
    use base64::Engine;
    token
        .split('.')
        .nth(1)
        .and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p.trim_end_matches('=')).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

/// Fila local como la mandaba v0.1.38 en un push legado: sin las columnas
/// que esa versión no tiene (origen, caja, registrado_at), con sus hijos y
/// marca UTC.
fn fila_legada(db: &Db, tabla: &str, uuid: &str) -> (Value, Option<Value>) {
    let c = db.lock().unwrap();
    let mut data = super::payload::fila_a_json(&c, tabla, uuid).unwrap().expect("fila local");
    let quitar = |o: &mut Value| {
        if let Some(m) = o.as_object_mut() {
            for k in ["origen", "caja", "registrado_at"] {
                m.remove(k);
            }
        }
    };
    quitar(&mut data);
    data["updated_at"] = json!(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
    let hijos_defs = super::fk::hijos_de(tabla);
    if hijos_defs.is_empty() {
        return (data, None);
    }
    let pid = data["id"].as_i64().unwrap();
    let mut children = serde_json::Map::new();
    for (h, col) in hijos_defs {
        let mut v = super::payload::hijos_a_json(&c, h, col, pid).unwrap();
        v.iter_mut().for_each(quitar);
        children.insert(h.to_string(), Value::Array(v));
    }
    (data, Some(Value::Object(children)))
}

/// POST /sync/push SIN proto (escritorio v0.1.38). Devuelve (status, cuerpo).
async fn push_legado(web: &Web, token: &str, device: &str, cambios: Vec<Value>) -> (u16, Value) {
    let (st, txt) = web
        .post("/sync/push", Some(token), &json!({ "device_uuid": device, "sucursal_id": 1, "cambios": cambios }))
        .await;
    (st, serde_json::from_str(&txt).unwrap_or(Value::String(txt)))
}

fn aceptado(resp: &Value, uuid: &str) -> bool {
    resp["aceptados"].as_array().map(|a| a.contains(&json!(uuid))).unwrap_or(false)
}

/// Suma segundos a una marca 'YYYY-MM-DD HH:MM:SS'.
fn mas_segundos(marca: &str, s: i64) -> String {
    chrono::NaiveDateTime::parse_from_str(&marca[..19], "%Y-%m-%d %H:%M:%S")
        .map(|d| (d + chrono::Duration::seconds(s)).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap()
}

/// Venta web en efectivo con el precio real del servidor.
async fn venta_web(web: &Web, tok: &str, items: &[(i64, f64, f64)]) -> Result<Value, String> {
    let mut its = Vec::new();
    let mut total = 0.0;
    for (pid, cant, precio) in items {
        let sub = r2(cant * precio);
        total += sub;
        its.push(json!({
            "producto_id": pid, "cantidad": cant, "precio_original": precio,
            "descuento_porcentaje": 0, "descuento_monto": 0, "precio_final": precio, "subtotal": sub,
        }));
    }
    let total = r2(total);
    web.rpc(tok, "crear_venta", json!({ "venta": {
        "usuario_id": 0, "cliente_id": null, "subtotal": total, "descuento": 0, "total": total,
        "metodo_pago": "efectivo", "monto_recibido": total, "cambio": 0, "items": its,
    }}))
    .await
}

// ─── Escritorio ───────────────────────────────────────────

/// Base del POS igual que init_database: esquema → migraciones → bandera
/// limpia → semilla.
fn db_como_la_tienda() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(SCHEMA_V1).unwrap();
    aplicar_migraciones(&db).unwrap();
    crate::db::connection::limpiar_bandera_supresion(&db);
    db.execute_batch(SEED_DATA).unwrap();
    db
}

fn d_i64(db: &Db, sql: &str) -> i64 {
    db.lock().unwrap().query_row(sql, [], |r| r.get(0)).unwrap_or_else(|e| panic!("{e}: {sql}"))
}

fn d_opt_i64(db: &Db, sql: &str) -> Option<i64> {
    db.lock()
        .unwrap()
        .query_row(sql, [], |r| r.get::<_, Option<i64>>(0))
        .optional()
        .unwrap_or_else(|e| panic!("{e}: {sql}"))
        .flatten()
}

fn d_txt(db: &Db, sql: &str) -> Option<String> {
    db.lock()
        .unwrap()
        .query_row(sql, [], |r| r.get::<_, Option<String>>(0))
        .optional()
        .unwrap_or_else(|e| panic!("{e}: {sql}"))
        .flatten()
}

fn d_f64(db: &Db, sql: &str) -> f64 {
    db.lock().unwrap().query_row(sql, [], |r| r.get(0)).unwrap_or_else(|e| panic!("{e}: {sql}"))
}

/// Foto de columnas de una tabla local (para comprobar que nada se tocó).
fn d_foto(db: &Db, sql: &str) -> Vec<String> {
    let c = db.lock().unwrap();
    let mut st = c.prepare(sql).unwrap();
    let n = st.column_count();
    let v = st
        .query_map([], |r| {
            let mut s = String::new();
            for i in 0..n {
                let x: rusqlite::types::Value = r.get(i)?;
                s.push_str(&format!("{x:?}|"));
            }
            Ok(s)
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    v
}

/// Renglones que difieren entre dos fotos (para diagnosticar).
fn diferencias(a: &[String], b: &[String]) -> Vec<String> {
    let sa: BTreeSet<&String> = a.iter().collect();
    let sb: BTreeSet<&String> = b.iter().collect();
    let mut v: Vec<String> = sa.difference(&sb).map(|x| format!("antes: {x}")).collect();
    v.extend(sb.difference(&sa).map(|x| format!("después: {x}")));
    v.truncate(12);
    v
}

/// Usuarios del escritorio de la tienda: 1 Dueño, 2 Nancy, 3 Mariana, 4 Lau,
/// con los uuids y datos del servidor. Devuelve (id local, uuid).
fn insertar_usuarios_tienda(conn: &Connection, pg: &Pg) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    for (local, nu) in [(1, "admin"), (2, "Nancy"), (3, "Mariana"), (4, "Lau")] {
        let f = pg.filas(&format!(
            "SELECT uuid, nombre_completo, pin, password_hash, rol_id, updated_at FROM usuarios \
              WHERE nombre_usuario = '{nu}' AND uuid ~ {HEX32}"
        ));
        let f = &f[0];
        conn.execute(
            "INSERT INTO usuarios (id, uuid, nombre_completo, nombre_usuario, pin, password_hash, rol_id, activo, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)",
            rusqlite::params![local, f[0], f[1], nu, f[2], f[3], f[4].parse::<i64>().unwrap(), f[5]],
        )
        .unwrap();
        out.push((local, f[0].clone()));
    }
    out
}

/// Categorías de la semilla con el uuid que tienen en el servidor (por nombre).
fn alinear_categorias(conn: &Connection, pg: &Pg) {
    for f in pg.filas("SELECT nombre, uuid FROM categorias WHERE deleted_at IS NULL") {
        conn.execute("UPDATE categorias SET uuid = ?1 WHERE nombre = ?2", rusqlite::params![f[1], f[0]]).unwrap();
    }
}

/// Proveedores con los mismos ids y uuids que el servidor (supuesto: la
/// tienda los creó en el mismo orden en que subieron).
fn insertar_proveedores_identidad(conn: &Connection, pg: &Pg) {
    for f in pg.filas("SELECT id, uuid, nombre, updated_at FROM proveedores ORDER BY id") {
        conn.execute(
            "INSERT INTO proveedores (id, uuid, nombre, updated_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![f[0].parse::<i64>().unwrap(), f[1], f[2], f[3]],
        )
        .unwrap();
    }
}

fn configurar_sync(conn: &Connection, device: &str, url: &str, token: &str, cursor: i64) {
    conn.execute(
        "UPDATE sync_state SET device_uuid = ?1, remote_url = ?2, remote_token = ?3, activo = 1, \
                sucursal_id = 1, last_pull_cursor = ?4 WHERE id = 1",
        rusqlite::params![device, url, token, cursor.to_string()],
    )
    .unwrap();
}

fn limpiar_outbox(conn: &Connection) {
    conn.execute("UPDATE sync_outbox SET synced_at = datetime('now','localtime') WHERE synced_at IS NULL", [])
        .unwrap();
}

struct Escritorio {
    db: Db,
    client: RemoteClient,
}

impl Escritorio {
    fn cfg(&self) -> state::SyncConfig {
        state::leer(&self.db.lock().unwrap()).unwrap().unwrap()
    }
    async fn push(&self) -> Result<(), String> {
        let cfg = self.cfg();
        worker::push(&self.db, &self.client, &cfg).await
    }
    /// Pull hasta vaciar (el worker hace máx. 10 tandas por ciclo).
    async fn pull(&self) -> Result<(), String> {
        for _ in 0..50 {
            let antes = self.cfg().last_pull_cursor.unwrap_or_default();
            let cfg = self.cfg();
            worker::pull(&self.db, &self.client, &cfg).await?;
            if self.cfg().last_pull_cursor.unwrap_or_default() == antes {
                break;
            }
        }
        Ok(())
    }
    fn pendientes_outbox(&self) -> i64 {
        outbox::contar_pendientes(&self.db.lock().unwrap()).unwrap()
    }
    fn errores_outbox(&self) -> Vec<String> {
        d_foto(&self.db, "SELECT tabla, uuid, ultimo_error FROM sync_outbox WHERE ultimo_error IS NOT NULL AND synced_at IS NULL")
    }
    fn inbox(&self) -> Vec<String> {
        d_foto(&self.db, "SELECT tabla, uuid, motivo, error FROM sync_inbox ORDER BY id")
    }
    fn periodo(&self, ahora: &str) -> cortes::DatosCorte {
        calcular_periodo(&self.db.lock().unwrap(), ahora).unwrap()
    }
    /// Corte de turno del escritorio en `ahora` contando exactamente lo esperado.
    fn corte(&self, ahora: &str) -> (i64, String, f64) {
        let c = self.db.lock().unwrap();
        let datos = calcular_periodo(&c, ahora).unwrap();
        let esperado = datos.efectivo_esperado;
        let creado = crear_corte_core(
            &c,
            NuevoCorte {
                tipo: "PARCIAL".into(),
                usuario_id: 1,
                fecha_inicio: datos.fecha_inicio.clone(),
                fecha_fin: datos.fecha_fin.clone(),
                datos,
                efectivo_contado: esperado,
                nota_diferencia: None,
                fondo_siguiente: esperado,
                denominaciones: None,
                retiro: None,
            },
            ahora,
        )
        .unwrap();
        let fin: String = c
            .query_row(
                &format!("SELECT {} FROM cortes WHERE id = ?", cortes::FIN_CORTE_SQL),
                [creado.id],
                |r| r.get(0),
            )
            .unwrap();
        (creado.id, fin, esperado)
    }
    /// Lo que hace un ciclo del worker sin las tareas únicas: push → pull →
    /// bandeja.
    async fn asentar(&self) {
        let _ = self.push().await;
        let _ = self.pull().await;
        let _ = inbox::reintentar(&self.db, &self.client).await;
    }
}

/// Venta del escritorio (tarjeta) de 1 pieza del producto LOCAL `pid` a su
/// precio local, fechada en `ahora`. Devuelve el id local de la venta.
fn venta_escritorio(db: &Db, pid: i64, usuario: i64, ahora: &str) -> i64 {
    let c = db.lock().unwrap();
    let p: f64 = c.query_row("SELECT precio_venta FROM productos WHERE id = ?", [pid], |r| r.get(0)).unwrap();
    crear_venta_core(
        &c,
        NuevaVenta {
            usuario_id: usuario, cliente_id: None, subtotal: p, descuento: 0.0, total: p,
            metodo_pago: "tarjeta".into(), monto_recibido: p, cambio: 0.0,
            items: vec![ItemVenta { producto_id: pid, cantidad: 1.0, precio_original: p, descuento_porcentaje: 0.0,
                                    descuento_monto: 0.0, precio_final: p, subtotal: p, autorizado_por: None }],
            presupuesto_origen_id: None,
        },
        ahora,
    )
    .unwrap()
    .id
}

/// Reloj de la PC adelantado: la venta y el producto quedan con la marca `t`.
/// Solo donde la marca es otra: escribir la MISMA marca dispara el trigger
/// de updated_at (NEW = OLD) y la regresa a la hora real.
fn marcar_hora_local(db: &Db, producto: i64, venta: i64, t: &str) {
    let c = db.lock().unwrap();
    c.execute("UPDATE productos SET updated_at = ?1 WHERE id = ?2 AND updated_at <> ?1", rusqlite::params![t, producto]).unwrap();
    c.execute("UPDATE ventas SET updated_at = ?1 WHERE id = ?2 AND updated_at <> ?1", rusqlite::params![t, venta]).unwrap();
}

/// El dueño cambia en la web el precio de venta de un producto (id del
/// servidor); lo demás queda como está en el servidor.
async fn editar_precio_web(web: &Web, pg: &Pg, tok: &str, id: i64, precio: f64) -> Result<Value, String> {
    let f = pg.filas(&format!(
        "SELECT codigo, nombre, COALESCE(descripcion, ''), COALESCE(categoria_id::text, ''), precio_costo::float8, \
                stock_minimo::float8, COALESCE(proveedor_id::text, '') FROM productos WHERE id = {id}"
    ));
    let f = &f[0];
    web.rpc(tok, "actualizar_producto", json!({ "usuario_id": 0, "producto": {
        "id": id, "codigo": f[0], "nombre": f[1], "descripcion": if f[2].is_empty() { Value::Null } else { json!(f[2]) },
        "categoria_id": f[3].parse::<i64>().ok(), "precio_costo": f[4].parse::<f64>().unwrap(),
        "precio_venta": precio, "stock_minimo": f[5].parse::<f64>().unwrap(),
        "proveedor_id": f[6].parse::<i64>().ok(), "foto_url": null } }))
    .await
}

/// Renglones de la bitácora del servidor A (E2E_SRV_LOG) que contienen todos
/// los textos dados. None si no se configuró la bitácora.
fn lineas_bitacora_srv(textos: &[&str]) -> Option<usize> {
    let ruta = var("E2E_SRV_LOG")?;
    let bytes = std::fs::read(&ruta).ok()?;
    let txt = String::from_utf8_lossy(&bytes);
    Some(txt.lines().filter(|l| textos.iter().all(|t| l.contains(t))).count())
}

// ─── Proxy entre el escritorio y el servidor ──────────────
//
// Reenvía todo al servidor de prueba. Puede "perder" la respuesta de un push
// (el servidor SÍ lo aplica; el escritorio recibe un 504, igual que con un
// tiempo de espera vencido o el internet que se cae a medio camino) y
// demorarlo (para que otro ciclo arranque mientras el push está en vuelo).
// Guarda el cuerpo de cada push que pasa.

#[derive(Clone)]
struct EstadoProxy {
    destino: String,
    http: reqwest::Client,
    perder_push: Arc<AtomicUsize>,
    demora_push_ms: Arc<AtomicU64>,
    pushes: Arc<Mutex<Vec<String>>>,
}

struct Proxy {
    url: String,
    estado: EstadoProxy,
}

async fn reenviar_al_servidor(
    axum::extract::State(p): axum::extract::State<EstadoProxy>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let (partes, cuerpo) = req.into_parts();
    let bytes = axum::body::to_bytes(cuerpo, 256 * 1024 * 1024).await.unwrap_or_default();
    let ruta = partes.uri.path_and_query().map(|x| x.as_str().to_string()).unwrap_or_else(|| "/".into());
    let es_push = partes.method == reqwest::Method::POST && partes.uri.path() == "/sync/push";
    if es_push {
        p.pushes.lock().unwrap().push(String::from_utf8_lossy(&bytes).into_owned());
        let ms = p.demora_push_ms.load(Ordering::SeqCst);
        if ms > 0 {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
    }
    let mut rb = p.http.request(partes.method.clone(), format!("{}{ruta}", p.destino)).body(bytes.to_vec());
    for (k, v) in partes.headers.iter() {
        if k == reqwest::header::AUTHORIZATION || k == reqwest::header::CONTENT_TYPE || k.as_str() == "x-pos-cliente" {
            rb = rb.header(k.clone(), v.clone());
        }
    }
    let resp = match rb.send().await {
        Ok(r) => r,
        Err(e) => return (reqwest::StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };
    let status = resp.status();
    let tipo = resp.headers().get(reqwest::header::CONTENT_TYPE).cloned();
    let cuerpo = resp.bytes().await.unwrap_or_default();
    if es_push && p.perder_push.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() {
        return (reqwest::StatusCode::GATEWAY_TIMEOUT, "respuesta del push perdida (simulada por la prueba e2e)").into_response();
    }
    let mut r = axum::response::Response::builder().status(status);
    if let Some(t) = tipo {
        r = r.header(reqwest::header::CONTENT_TYPE, t);
    }
    r.body(axum::body::Body::from(cuerpo)).unwrap()
}

impl Proxy {
    async fn arrancar(destino: &str) -> Proxy {
        let estado = EstadoProxy {
            destino: destino.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder().timeout(Duration::from_secs(600)).build().unwrap(),
            perder_push: Arc::new(AtomicUsize::new(0)),
            demora_push_ms: Arc::new(AtomicU64::new(0)),
            pushes: Arc::new(Mutex::new(Vec::new())),
        };
        let app = axum::Router::new().fallback(reenviar_al_servidor).with_state(estado.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Proxy { url, estado }
    }
    /// El siguiente push llega al servidor pero su respuesta se pierde.
    fn perder_siguiente_push(&self) {
        self.estado.perder_push.store(1, Ordering::SeqCst);
    }
    fn demorar_push(&self, ms: u64) {
        self.estado.demora_push_ms.store(ms, Ordering::SeqCst);
    }
    fn reiniciar_cuenta(&self) {
        self.estado.pushes.lock().unwrap().clear();
    }
    fn total_pushes(&self) -> usize {
        self.estado.pushes.lock().unwrap().len()
    }
    /// Cuántos push llevaron un cambio de (tabla, uuid) como fila padre.
    fn pushes_de(&self, tabla: &str, uuid: &str) -> usize {
        self.estado
            .pushes
            .lock()
            .unwrap()
            .iter()
            .filter(|b| {
                serde_json::from_str::<Value>(b)
                    .ok()
                    .and_then(|v| v["cambios"].as_array().cloned())
                    .map(|cs| cs.iter().any(|c| c["tabla"] == json!(tabla) && c["data"]["uuid"] == json!(uuid)))
                    .unwrap_or(false)
            })
            .count()
    }
}

// ─── Contexto común ───────────────────────────────────────

struct Ctx {
    web: Web,
    pg: Pg,
    token_sync: String,
    run: String,
}

fn var(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Prepara: admin_users de prueba en el clon y token del escritorio
/// obtenido igual que lo hace el POS (POST /auth/login).
async fn preparar(var_url: &str, var_pg: &str, pg_default: &str) -> Option<Ctx> {
    let url = var(var_url)?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = env_logger::builder().is_test(true).try_init();
    let pg = Pg::nuevo(&var(var_pg).unwrap_or_else(|| pg_default.to_string()));
    let run = hex32()[..8].to_string();
    let web = Web {
        url: url.trim_end_matches('/').to_string(),
        http: reqwest::Client::builder().timeout(Duration::from_secs(600)).build().unwrap(),
    };
    let email = format!("e2e-{run}@local.test");
    let pass = "e2e-pass-123";
    let hash = bcrypt::hash(pass, 4).unwrap();
    pg.exec(&format!(
        "INSERT INTO admin_users (email, password_hash, nombre, sucursal_id, es_super_admin, activo) \
         VALUES ('{email}', '{hash}', 'E2E sync', 1, false, true)"
    ));
    let (st, txt) = web.post("/auth/login", None, &json!({ "email": email, "password": pass })).await;
    assert_eq!(st, 200, "POST /auth/login: {txt}");
    let token_sync = serde_json::from_str::<Value>(&txt).unwrap()["token"].as_str().unwrap().to_string();
    Some(Ctx { web, pg, token_sync, run })
}

/// Producto del servidor para usar como "compartido".
struct ProdSrv {
    id: i64,
    uuid: String,
    codigo: String,
    nombre: String,
    precio: f64,
    costo: f64,
    stock: f64,
    categoria_id: Option<i64>,
    proveedor_id: Option<i64>,
    updated_at: String,
}

fn producto_srv(pg: &Pg, filtro: &str) -> ProdSrv {
    let f = pg.filas(&format!(
        "SELECT id, uuid, codigo, nombre, precio_venta::float8, precio_costo::float8, stock_actual::float8, \
                COALESCE(categoria_id::text, ''), COALESCE(proveedor_id::text, ''), updated_at \
           FROM productos WHERE deleted_at IS NULL AND activo = 1 AND precio_venta > 0 AND stock_actual >= 5 \
            AND uuid ~ {HEX32} AND {filtro} ORDER BY id LIMIT 1"
    ));
    let f = &f[0];
    ProdSrv {
        id: f[0].parse().unwrap(),
        uuid: f[1].clone(),
        codigo: f[2].clone(),
        nombre: f[3].clone(),
        precio: f[4].parse().unwrap(),
        costo: f[5].parse().unwrap(),
        stock: f[6].parse().unwrap(),
        categoria_id: f[7].parse().ok(),
        proveedor_id: f[8].parse().ok(),
        updated_at: f[9].clone(),
    }
}

// ═════════════════════════════════════════════════════════
// Escenarios a–h (clon A, equipo nuevo)
// ═════════════════════════════════════════════════════════

#[tokio::test]
async fn e2e_sync_v2_escenarios() {
    let Some(ctx) = preparar("E2E_SERVER_URL", "E2E_PG_URL", "postgres://postgres@localhost:54329/e2e_srv").await else {
        eprintln!("E2E_SERVER_URL no definido: se omite la prueba de punta a punta");
        return;
    };
    let Ctx { web, pg, token_sync, run } = ctx;
    let mut inf = Informe::default();
    println!("\n== e2e sync v2 (corrida {run}) contra {} ==", web.url);

    // ── Ids del servidor ──
    let srv_usuario = |uuid: &str| pg.i64(&format!("SELECT id FROM usuarios WHERE uuid = '{uuid}'"));

    // ── Escritorio como el de la tienda, con ids distintos a propósito ──
    let device = hex32();
    let pa = producto_srv(&pg, "id <= 5948 AND categoria_id IS NULL");
    let pb = producto_srv(&pg, "id > 5948");
    let p3_uuid = hex32();
    let p3_codigo = format!("E2E-{run}");
    let cursor0 = pg.max_cursor();
    let (db, usuarios) = {
        let conn = db_como_la_tienda();
        let usuarios = insertar_usuarios_tienda(&conn, &pg);
        alinear_categorias(&conn, &pg);
        insertar_proveedores_identidad(&conn, &pg);
        for (local, p) in [(1i64, &pa), (2, &pb)] {
            conn.execute(
                "INSERT INTO productos (id, uuid, codigo, nombre, precio_venta, precio_costo, stock_actual, \
                                        categoria_id, proveedor_id, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![local, p.uuid, p.codigo, p.nombre, p.precio, p.costo, p.stock,
                                  p.categoria_id, p.proveedor_id, p.updated_at],
            )
            .unwrap();
        }
        // Producto que solo existe en el escritorio (aún no sube).
        conn.execute(
            "INSERT INTO productos (id, uuid, codigo, nombre, precio_venta, precio_costo, stock_actual) \
             VALUES (3, ?1, ?2, ?3, 123.5, 80, 10)",
            rusqlite::params![p3_uuid, p3_codigo, format!("Solo escritorio E2E {run}")],
        )
        .unwrap();
        // Folios del escritorio lejos de los reales (solo estética).
        conn.execute("UPDATE folio_secuencia SET ultimo_valor = ?1 WHERE id = 1", [700_000 + (rand::random::<u32>() % 99_999) as i64]).unwrap();
        configurar_sync(&conn, &device, &web.url, &token_sync, cursor0);
        // Lo de la preparación ya "subió"; la venta de (a) encola lo suyo
        // (incluido el producto solo-escritorio, por su cambio de stock).
        limpiar_outbox(&conn);
        (Arc::new(Mutex::new(conn)) as Db, usuarios)
    };
    let esc = Escritorio { db: db.clone(), client: RemoteClient::new(&web.url, &token_sync).unwrap() };
    let u_dueno = usuarios[0].1.clone();
    let u_nancy = usuarios[1].1.clone();
    let dueno_srv = srv_usuario(&u_dueno);
    let nancy_srv = srv_usuario(&u_nancy);
    println!("  escritorio {device}: P_A local 1 ↔ servidor {}, P_B local 2 ↔ servidor {}, P3 solo escritorio; Dueño local 1 ↔ servidor {dueno_srv}", pa.id, pb.id);

    // ═══ a) push v2 ═══════════════════════════════════════
    println!("\n-- a) push v2 de una venta del escritorio --");
    let ahora_a = cortes::ahora_local();
    let total_a = r2(123.5 + 2.0 * pa.precio);
    let venta_a = {
        let c = db.lock().unwrap();
        crear_venta_core(
            &c,
            NuevaVenta {
                usuario_id: 1,
                cliente_id: None,
                subtotal: total_a,
                descuento: 0.0,
                total: total_a,
                metodo_pago: "efectivo".into(),
                monto_recibido: total_a,
                cambio: 0.0,
                items: vec![
                    ItemVenta { producto_id: 3, cantidad: 1.0, precio_original: 123.5, descuento_porcentaje: 0.0,
                                descuento_monto: 0.0, precio_final: 123.5, subtotal: 123.5, autorizado_por: None },
                    ItemVenta { producto_id: 1, cantidad: 2.0, precio_original: pa.precio, descuento_porcentaje: 0.0,
                                descuento_monto: 0.0, precio_final: pa.precio, subtotal: r2(2.0 * pa.precio),
                                autorizado_por: None },
                ],
                presupuesto_origen_id: None,
            },
            &ahora_a,
        )
        .unwrap()
    };
    let va_uuid = d_txt(&db, &format!("SELECT uuid FROM ventas WHERE id = {}", venta_a.id)).unwrap();
    let encolados = d_foto(&db, "SELECT tabla, operacion FROM sync_outbox WHERE synced_at IS NULL ORDER BY id");
    println!("  outbox antes del push: {encolados:?}");
    let r = esc.push().await;
    chk!(inf, "a", r.is_ok(), "push sin error de red/HTTP: {r:?}");
    chk!(inf, "a", esc.pendientes_outbox() == 0, "outbox vacío tras el push (errores: {:?})", esc.errores_outbox());

    let fila = pg.filas(&format!(
        "SELECT id, usuario_id, total::float8, caja, origen FROM ventas WHERE uuid = '{va_uuid}'"
    ));
    chk!(inf, "a", fila.len() == 1, "la venta llegó al servidor");
    if let Some(f) = fila.first() {
        let vid_srv: i64 = f[0].parse().unwrap();
        chk!(inf, "a", f[1] == dueno_srv.to_string(), "usuario_id en el servidor = Dueño del servidor ({dueno_srv}); quedó {}", f[1]);
        chk!(inf, "a", cerca(f[2].parse().unwrap(), total_a), "total {} = {total_a}", f[2]);
        chk!(inf, "a", f[3] == "principal" && f[4] == "desktop", "caja/origen = principal/desktop ({}/{})", f[3], f[4]);
        let det = pg.filas(&format!(
            "SELECT vd.producto_id, COALESCE(p.uuid, '<sin producto>') FROM venta_detalle vd \
               LEFT JOIN productos p ON p.id = vd.producto_id WHERE vd.venta_id = {vid_srv} ORDER BY vd.id"
        ));
        let uuids: BTreeSet<String> = det.iter().map(|d| d[1].clone()).collect();
        chk!(inf, "a", uuids == BTreeSet::from([p3_uuid.clone(), pa.uuid.clone()]), "renglones apuntan a P3 y P_A por uuid: {det:?}");
        chk!(inf, "a", det.iter().any(|d| d[0] == pa.id.to_string()), "renglón de P_A usa el id del servidor {}", pa.id);
        let p3_srv = pg.uno(&format!("SELECT id FROM productos WHERE uuid = '{p3_uuid}'"));
        chk!(inf, "a", p3_srv.is_some(), "el producto solo-escritorio llegó (antes que la venta)");
        if let Some(p3) = &p3_srv {
            chk!(inf, "a", det.iter().any(|d| &d[0] == p3), "renglón de P3 apunta al id nuevo del servidor {p3}");
            chk!(inf, "a", pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {p3}")) == 9.0, "stock de P3 en el servidor = 9");
        }
        let estado = pg.uno(&format!("SELECT estado FROM sync_fk_estado WHERE tabla = 'ventas' AND uuid = '{va_uuid}'"));
        chk!(inf, "a", estado.as_deref() == Some("traducido"), "sync_fk_estado = traducido ({estado:?})");
        let pend = pg.i64(&format!(
            "SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid = '{va_uuid}' \
                OR fila_uuid IN (SELECT uuid FROM venta_detalle WHERE venta_id = {vid_srv})"
        ));
        chk!(inf, "a", pend == 0, "sin celdas pendientes ({pend})");
        // Bitácora de la venta: usuario y registro traducidos.
        if let Some(au) = d_txt(&db, &format!(
            "SELECT uuid FROM audit_log WHERE accion = 'VENTA' AND registro_id = {}", venta_a.id
        )) {
            let a = pg.filas(&format!("SELECT usuario_id, registro_id FROM audit_log WHERE uuid = '{au}'"));
            chk!(inf, "a", a.len() == 1 && a[0][0] == dueno_srv.to_string() && a[0][1] == vid_srv.to_string(),
                 "bitácora VENTA en el servidor: usuario {dueno_srv} y registro {vid_srv} ({a:?})");
        } else {
            inf.nota("a", "la venta local no generó fila de bitácora con uuid".into());
        }
    }
    let proto = pg.uno(&format!("SELECT protocolo FROM pos_devices WHERE device_uuid = '{device}'"));
    chk!(inf, "a", proto.as_deref() == Some("2"), "pos_devices.protocolo = 2 ({proto:?})");
    // Categoría/proveedor del producto compartido viajaron por uuid.
    let prov_srv = pg.uno(&format!("SELECT COALESCE(proveedor_id::text,'') FROM productos WHERE id = {}", pa.id));
    chk!(inf, "a", prov_srv == pa.proveedor_id.map(|x| x.to_string()), "proveedor de P_A se conserva ({prov_srv:?} vs {:?})", pa.proveedor_id);

    // ═══ b) venta web en modo espejo ══════════════════════
    println!("\n-- b) venta web modo espejo --");
    // Admin web de prueba (solo existe en el servidor) para dar de alta al vendedor.
    let pin_admin = pin_aleatorio();
    let h = bcrypt::hash(&pin_admin, 4).unwrap();
    pg.exec(&format!(
        "INSERT INTO usuarios (uuid, nombre_completo, nombre_usuario, pin, password_hash, rol_id, activo) \
         VALUES ('e2e-admin-{run}', 'Admin E2E {run}', 'e2e_admin_{run}', '{h}', '{h}', 1, 1); \
         INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) VALUES ('usuarios', 'e2e-admin-{run}', 1, 'web-pos')"
    ));
    let tok_admin = web.login_pin(&pin_admin, None).await["token"].as_str().unwrap().to_string();
    let pin_vend = pin_aleatorio();
    let nu_vend = format!("e2e_vend_{run}");
    let r = web
        .rpc(&tok_admin, "crear_usuario", json!({ "usuario": {
            "nombre_completo": format!("Vendedor E2E {run}"), "nombre_usuario": nu_vend,
            "pin": pin_vend, "password": "e2e-pass", "rol_id": 2 } }))
        .await;
    chk!(inf, "b", r.is_ok(), "crear_usuario por RPC: {r:?}");
    let vend = pg.filas(&format!("SELECT id, uuid FROM usuarios WHERE nombre_usuario = '{nu_vend}'"));
    let (vend_srv, vend_uuid) = (vend[0][0].parse::<i64>().unwrap(), vend[0][1].clone());
    let dev_espejo = uuid::Uuid::new_v4().to_string();
    let s = web.login_pin(&pin_vend, Some(&dev_espejo)).await;
    chk!(inf, "b", s["modo_caja"] == json!("espejo") && s["caja"] == json!("principal"),
         "equipo nuevo entra en espejo / caja principal ({} / {})", s["modo_caja"], s["caja"]);
    let tok_espejo = s["token"].as_str().unwrap().to_string();
    let precio_b = pg.f64(&format!("SELECT precio_venta::float8 FROM productos WHERE id = {}", pb.id));
    let precio_a = pg.f64(&format!("SELECT precio_venta::float8 FROM productos WHERE id = {}", pa.id));
    let rv = venta_web(&web, &tok_espejo, &[(pa.id, 1.0, precio_a), (pb.id, 1.0, precio_b)]).await;
    chk!(inf, "b", rv.is_ok(), "crear_venta web espejo: {rv:?}");
    let rv = rv.unwrap_or(json!({}));
    let wb_total = rv["total"].as_f64().unwrap_or(0.0);
    let wb_uuid = pg.uno(&format!("SELECT uuid FROM ventas WHERE id = {}", rv["id"].as_i64().unwrap_or(0))).unwrap_or_default();
    let f = pg.filas(&format!("SELECT caja, origen, usuario_id FROM ventas WHERE uuid = '{wb_uuid}'"));
    chk!(inf, "b", !f.is_empty() && f[0][0] == "principal" && f[0][1] == "web" && f[0][2] == vend_srv.to_string(),
         "servidor: caja principal, origen web, usuario de la sesión ({f:?})");
    // Movimiento de caja web (espejo → principal).
    let concepto_ent = format!("Entrada E2E {run}");
    let rm = web
        .rpc(&tok_espejo, "crear_movimiento_caja", json!({ "datos": {
            "tipo": "ENTRADA", "monto": 50.0, "concepto": concepto_ent } }))
        .await;
    chk!(inf, "b", rm.is_ok(), "crear_movimiento_caja espejo: {rm:?}");
    let mov_uuid = pg.uno(&format!("SELECT uuid FROM movimientos_caja WHERE concepto = '{concepto_ent}'")).unwrap_or_default();

    let r = esc.pull().await;
    chk!(inf, "b", r.is_ok(), "pull sin error: {r:?}");
    let vend_local = d_opt_i64(&db, &format!("SELECT id FROM usuarios WHERE uuid = '{vend_uuid}'"));
    chk!(inf, "b", vend_local.is_some(), "el vendedor web llegó como usuario local (id {vend_local:?})");
    let vl = d_foto(&db, &format!(
        "SELECT usuario_id, caja, origen, total, registrado_at FROM ventas WHERE uuid = '{wb_uuid}'"
    ));
    chk!(inf, "b", vl.len() == 1, "la venta espejo llegó al escritorio ({vl:?}; bandeja {:?})", esc.inbox());
    if let (Some(vlocal), 1) = (vend_local, vl.len()) {
        let uid = d_i64(&db, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{wb_uuid}'"));
        chk!(inf, "b", uid == vlocal, "usuario_id = id LOCAL del vendedor ({uid} vs {vlocal}; en el servidor {vend_srv})");
        chk!(inf, "b", d_txt(&db, &format!("SELECT caja || '/' || origen FROM ventas WHERE uuid = '{wb_uuid}'")).as_deref() == Some("principal/web"), "caja principal, origen web");
        let det = d_foto(&db, &format!(
            "SELECT vd.venta_id = v.id, p.uuid FROM venta_detalle vd JOIN ventas v ON v.uuid = '{wb_uuid}' \
               LEFT JOIN productos p ON p.id = vd.producto_id WHERE vd.venta_id = v.id ORDER BY vd.id"
        ));
        chk!(inf, "b", det.len() == 2, "2 renglones colgados del padre LOCAL ({det:?})");
        let pids: BTreeSet<i64> = {
            let c = db.lock().unwrap();
            let mut st = c.prepare(&format!(
                "SELECT producto_id FROM venta_detalle WHERE venta_id = (SELECT id FROM ventas WHERE uuid = '{wb_uuid}')"
            )).unwrap();
            let v = st.query_map([], |r| r.get(0)).unwrap().map(|x| x.unwrap()).collect();
            v
        };
        chk!(inf, "b", pids == BTreeSet::from([1, 2]), "producto_id = ids LOCALES 1 y 2 (no {} / {}): {pids:?}", pa.id, pb.id);
    }
    let stock_a_srv = pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {}", pa.id));
    let stock_a_loc = d_f64(&db, "SELECT stock_actual FROM productos WHERE id = 1");
    chk!(inf, "b", cerca(stock_a_srv, stock_a_loc), "stock de P_A igual en los dos lados ({stock_a_loc} / {stock_a_srv})");
    let ml = d_foto(&db, &format!("SELECT caja, origen, monto, usuario_id FROM movimientos_caja WHERE uuid = '{mov_uuid}'"));
    chk!(inf, "b", ml.len() == 1, "el movimiento espejo llegó una vez ({ml:?})");
    let ahora_b = cortes::ahora_local();
    let p = esc.periodo(&ahora_b);
    chk!(inf, "b", cerca(p.total_ventas_efectivo, total_a + wb_total),
         "calcular_periodo cuenta la venta espejo: efectivo {} = {total_a} + {wb_total}", p.total_ventas_efectivo);
    chk!(inf, "b", p.num_transacciones == 2, "2 ventas en el período ({})", p.num_transacciones);
    chk!(inf, "b", cerca(p.total_entradas_efectivo, 50.0), "la entrada espejo cuenta ({})", p.total_entradas_efectivo);
    chk!(inf, "b", esc.pendientes_outbox() == 0, "lo recibido no regresa al outbox ({})", esc.pendientes_outbox());
    chk!(inf, "b", esc.inbox().is_empty(), "bandeja vacía: {:?}", esc.inbox());

    // b2) venta espejo de un usuario 'web-admin-*' (como 'Andrew Web').
    println!("\n-- b2) venta espejo de un usuario web-admin --");
    let pin_wa = pin_aleatorio();
    let h = bcrypt::hash(&pin_wa, 4).unwrap();
    pg.exec(&format!(
        "INSERT INTO usuarios (uuid, nombre_completo, nombre_usuario, pin, password_hash, rol_id, activo) \
         VALUES ('web-admin-e2e-{run}', 'Web admin E2E {run}', 'e2e_webadmin_{run}', '{h}', '{h}', 1, 1)"
    ));
    let dev_wa = uuid::Uuid::new_v4().to_string();
    let tok_wa = web.login_pin(&pin_wa, Some(&dev_wa)).await["token"].as_str().unwrap().to_string();
    let rv = venta_web(&web, &tok_wa, &[(pb.id, 1.0, precio_b)]).await;
    chk!(inf, "b2", rv.is_ok(), "crear_venta web-admin espejo: {rv:?}");
    let wa_total = rv.as_ref().map(|v| v["total"].as_f64().unwrap_or(0.0)).unwrap_or(0.0);
    let wa_uuid = pg.uno(&format!("SELECT uuid FROM ventas WHERE id = {}", rv.as_ref().map(|v| v["id"].as_i64().unwrap_or(0)).unwrap_or(0))).unwrap_or_default();
    let wa_caja = pg.uno(&format!("SELECT caja FROM ventas WHERE uuid = '{wa_uuid}'"));
    let _ = esc.pull().await;
    let llego = d_i64(&db, &format!("SELECT count(*) FROM ventas WHERE uuid = '{wa_uuid}'"));
    let en_bandeja = d_txt(&db, &format!("SELECT motivo || ': ' || COALESCE(error,'') FROM sync_inbox WHERE uuid = '{wa_uuid}'"));
    chk!(inf, "b2", llego == 1,
         "venta espejo (caja {wa_caja:?}) cobrada por un usuario web-admin llega al escritorio y cuenta en su corte (llegó {llego}; bandeja: {en_bandeja:?})");
    if llego == 1 {
        let wa_usuario = d_i64(&db, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{wa_uuid}'"));
        chk!(inf, "b2", wa_usuario == 1, "queda a nombre del administrador local (Dueño = 1): {wa_usuario}");
    }
    // Lo que cuenta en la caja de la tienda desde aquí (la venta web-admin,
    // si llegó y es de la caja principal).
    let wa_cuenta = if llego == 1 && wa_caja.as_deref() == Some("principal") { wa_total } else { 0.0 };

    // ═══ c) venta web en modo individual (caja web) ═══════
    println!("\n-- c) venta web modo individual --");
    let antes_c = esc.periodo(&cortes::ahora_local());
    let dev_ind = uuid::Uuid::new_v4().to_string();
    let tok_ind = web.login_pin(&pin_vend, Some(&dev_ind)).await["token"].as_str().unwrap().to_string();
    let r = web.rpc(&tok_ind, "configurar_modo_caja", json!({ "modo": "individual" })).await;
    chk!(inf, "c", r.is_ok(), "configurar_modo_caja individual: {r:?}");
    let rv = venta_web(&web, &tok_ind, &[(pb.id, 1.0, precio_b)]).await;
    chk!(inf, "c", rv.is_ok(), "crear_venta individual: {rv:?}");
    let wc_uuid = pg.uno(&format!("SELECT uuid FROM ventas WHERE id = {}", rv.as_ref().map(|v| v["id"].as_i64().unwrap_or(0)).unwrap_or(0))).unwrap_or_default();
    chk!(inf, "c", pg.uno(&format!("SELECT caja FROM ventas WHERE uuid = '{wc_uuid}'")).as_deref() == Some("web"), "servidor: caja web");
    let concepto_web = format!("Entrada caja web E2E {run}");
    let rm = web
        .rpc(&tok_ind, "crear_movimiento_caja", json!({ "datos": { "tipo": "ENTRADA", "monto": 30.0, "concepto": concepto_web } }))
        .await;
    chk!(inf, "c", rm.is_ok(), "movimiento en caja web: {rm:?}");
    let movw_uuid = pg.uno(&format!("SELECT uuid FROM movimientos_caja WHERE concepto = '{concepto_web}'")).unwrap_or_default();
    chk!(inf, "c", pg.uno(&format!("SELECT caja FROM movimientos_caja WHERE uuid = '{movw_uuid}'")).as_deref() == Some("web"), "servidor: movimiento en caja web");
    let _ = esc.pull().await;
    let vc = d_foto(&db, &format!("SELECT caja, origen, usuario_id FROM ventas WHERE uuid = '{wc_uuid}'"));
    chk!(inf, "c", vc.len() == 1 && vc[0].starts_with("Text(\"web\")|Text(\"web\")"), "venta caja web guardada en el escritorio (historial/stock): {vc:?}");
    let despues_c = esc.periodo(&cortes::ahora_local());
    chk!(inf, "c", cerca(despues_c.total_ventas, antes_c.total_ventas) && despues_c.num_transacciones == antes_c.num_transacciones,
         "calcular_periodo NO la cuenta ({} → {}, {} → {})", antes_c.total_ventas, despues_c.total_ventas, antes_c.num_transacciones, despues_c.num_transacciones);
    chk!(inf, "c", cerca(despues_c.total_entradas_efectivo, antes_c.total_entradas_efectivo), "la entrada de la caja web no cuenta");
    chk!(inf, "c", d_i64(&db, &format!("SELECT count(*) FROM movimientos_caja WHERE uuid = '{movw_uuid}'")) == 0,
         "el movimiento de la caja web NO se guarda en el escritorio");

    // ═══ d) venta espejo que llega después de un corte ═══
    println!("\n-- d) llegada tardía --");
    let rv = venta_web(&web, &tok_espejo, &[(pa.id, 1.0, precio_a)]).await;
    chk!(inf, "d", rv.is_ok(), "venta espejo antes del corte: {rv:?}");
    let rv = rv.unwrap_or(json!({}));
    let wd_total = rv["total"].as_f64().unwrap_or(0.0);
    let wd_fecha = rv["fecha"].as_str().unwrap_or("").to_string();
    let wd_uuid = pg.uno(&format!("SELECT uuid FROM ventas WHERE id = {}", rv["id"].as_i64().unwrap_or(0))).unwrap_or_default();
    tokio::time::sleep(Duration::from_millis(1300)).await;
    let ahora_corte = cortes::ahora_local();
    let (corte_id, corte_fin, corte_esperado) = esc.corte(&ahora_corte);
    let foto_corte = d_foto(&db, &format!("SELECT * FROM cortes WHERE id = {corte_id}"));
    println!("  venta web {wd_uuid} fecha {wd_fecha}; corte {corte_id} fin {corte_fin} (esperado {corte_esperado})");
    let cuenta_corte = d_f64(&db, &format!("SELECT total_ventas_efectivo FROM cortes WHERE id = {corte_id}"));
    chk!(inf, "d", cerca(cuenta_corte, total_a + wb_total + wa_cuenta),
         "el corte cubrió solo lo que ya había llegado ({cuenta_corte} = {total_a} + {wb_total} + web-admin {wa_cuenta})");
    let _ = esc.pull().await;
    let reg = d_txt(&db, &format!("SELECT registrado_at FROM ventas WHERE uuid = '{wd_uuid}'"));
    chk!(inf, "d", d_i64(&db, &format!("SELECT count(*) FROM ventas WHERE uuid = '{wd_uuid}'")) == 1, "la venta tardía llegó");
    chk!(inf, "d", reg.as_deref().map(|r| r > corte_fin.as_str()).unwrap_or(false),
         "registrado_at {reg:?} > fin del corte {corte_fin}");
    let ver = reg.clone().map(|r| std::cmp::max(r, cortes::ahora_local())).unwrap_or_else(cortes::ahora_local);
    let p = esc.periodo(&ver);
    chk!(inf, "d", p.fecha_inicio == corte_fin, "el período abierto arranca en el fin del corte ({})", p.fecha_inicio);
    chk!(inf, "d", p.num_transacciones == 1 && cerca(p.total_ventas_efectivo, wd_total),
         "cuenta en el SIGUIENTE período: {} venta(s), efectivo {} (= {wd_total})", p.num_transacciones, p.total_ventas_efectivo);
    chk!(inf, "d", cerca(p.efectivo_esperado, corte_esperado + wd_total), "esperado = fondo {corte_esperado} + {wd_total} ({})", p.efectivo_esperado);
    chk!(inf, "d", d_foto(&db, &format!("SELECT * FROM cortes WHERE id = {corte_id}")) == foto_corte, "el corte guardado no cambió");
    let total_principal = d_f64(&db, "SELECT COALESCE(SUM(total),0) FROM ventas WHERE caja = 'principal' AND anulada = 0 AND metodo_pago = 'efectivo'");
    chk!(inf, "d", cerca(cuenta_corte + p.total_ventas_efectivo, total_principal - if llego == 1 { r2(precio_b) } else { 0.0 })
                    || cerca(cuenta_corte + p.total_ventas_efectivo, total_principal),
         "nada cuenta doble: corte {cuenta_corte} + abierto {} vs ventas principal {total_principal}", p.total_ventas_efectivo);

    // ═══ e) cortes/aperturas web nunca bajan ══════════════
    println!("\n-- e) cortes y aperturas web --");
    let n_cortes = d_i64(&db, "SELECT count(*) FROM cortes");
    let n_aperturas = d_i64(&db, "SELECT count(*) FROM aperturas_caja");
    let esp = web.rpc(&tok_ind, "obtener_esperado_apertura", json!({})).await;
    let fondo = esp.as_ref().ok().and_then(|v| v["esperado"].as_f64()).unwrap_or(0.0);
    let ap = web.rpc(&tok_ind, "crear_apertura_caja", json!({ "datos": { "fondo_declarado": fondo, "nota": "apertura e2e" } })).await;
    if let Err(e) = &ap {
        inf.nota("e", format!("crear_apertura_caja web: {e}"));
    }
    let datos = web.rpc(&tok_ind, "calcular_datos_corte", json!({})).await;
    chk!(inf, "e", datos.is_ok(), "calcular_datos_corte caja web: {datos:?}");
    let esperado_w = datos.as_ref().ok().and_then(|v| v["efectivo_esperado"].as_f64()).unwrap_or(0.0);
    let cw = web
        .rpc(&tok_ind, "crear_corte", json!({ "datos": {
            "tipo": "PARCIAL", "datos": { "efectivo_esperado": esperado_w },
            "efectivo_contado": esperado_w, "nota_diferencia": null, "fondo_siguiente": 0.0 } }))
        .await;
    chk!(inf, "e", cw.is_ok(), "corte de la caja web en el servidor: {cw:?}");
    let ce = web
        .rpc(&tok_espejo, "crear_corte", json!({ "datos": {
            "tipo": "PARCIAL", "datos": { "efectivo_esperado": 0 }, "efectivo_contado": 0, "fondo_siguiente": 0 } }))
        .await;
    chk!(inf, "e", ce.as_ref().err().map(|e| e.starts_with("400") && e.contains("escritorio")).unwrap_or(false),
         "corte de la caja principal desde la web se rechaza: {ce:?}");
    let uuids_web_caja: Vec<String> = pg
        .filas(&format!(
            "SELECT uuid FROM cortes WHERE uuid !~ {HEX32} AND id > 0 \
             UNION ALL SELECT uuid FROM aperturas_caja WHERE uuid !~ {HEX32} AND fecha >= '{}'",
            &ahora_a[..10]
        ))
        .into_iter()
        .map(|f| f[0].clone())
        .collect();
    let _ = esc.pull().await;
    chk!(inf, "e", d_i64(&db, "SELECT count(*) FROM cortes") == n_cortes, "ningún corte web bajó");
    chk!(inf, "e", d_i64(&db, "SELECT count(*) FROM aperturas_caja") == n_aperturas, "ninguna apertura web bajó");
    let colados = uuids_web_caja
        .iter()
        .filter(|u| {
            d_i64(&db, &format!(
                "SELECT (SELECT count(*) FROM cortes WHERE uuid = '{u}') + (SELECT count(*) FROM aperturas_caja WHERE uuid = '{u}')"
            )) > 0
        })
        .count();
    chk!(inf, "e", colados == 0, "0 de {} cortes/aperturas web en el escritorio", uuids_web_caja.len());
    chk!(inf, "e", d_i64(&db, "SELECT count(*) FROM corte_denominaciones WHERE corte_id NOT IN (SELECT id FROM cortes)") == 0
                 && d_i64(&db, "SELECT count(*) FROM corte_vendedores WHERE corte_id NOT IN (SELECT id FROM cortes)") == 0,
         "sin hijos de cortes web");
    chk!(inf, "e", d_i64(&db, "SELECT count(*) FROM movimientos_caja WHERE caja = 'web'") == 0, "ningún movimiento de caja web");
    let bandeja_e: Vec<String> = esc.inbox().into_iter().filter(|x| !x.contains(&wa_uuid)).collect();
    chk!(inf, "e", bandeja_e.is_empty(), "nada en la bandeja (aparte de b2): {bandeja_e:?}");

    // ═══ b3) devolución web (espejo) de la venta del escritorio ═══
    println!("\n-- b3) devolución web de una venta del escritorio --");
    let va_srv = pg.i64(&format!("SELECT id FROM ventas WHERE uuid = '{va_uuid}'"));
    let vd_pa_srv = pg.i64(&format!("SELECT id FROM venta_detalle WHERE venta_id = {va_srv} AND producto_id = {}", pa.id));
    // registrado_at de una venta tardía puede quedar hasta 1 s adelante del
    // reloj (fin del corte + 1 s): se mide el período con 2 s de margen.
    let ahora_b3 = mas_segundos(&cortes::ahora_local(), 2);
    let periodo_antes_b3 = esc.periodo(&ahora_b3);
    let motivo_dev = format!("Devolucion E2E {run}");
    let rd = web
        .rpc(&tok_espejo, "crear_devolucion", json!({ "datos": {
            "venta_id": va_srv, "motivo": motivo_dev, "pin_autorizacion": pin_admin, "reembolso_efectivo": true,
            "items": [{ "venta_detalle_id": vd_pa_srv, "cantidad": 1 }] } }))
        .await;
    chk!(inf, "b3", rd.is_ok(), "crear_devolucion web (espejo) de la venta del escritorio traducida: {rd:?}");
    let dev = pg.filas(&format!(
        "SELECT d.uuid, d.venta_id, d.total_devuelto::float8, COALESCE(m.uuid, ''), COALESCE(m.caja, '') \
           FROM devoluciones d LEFT JOIN movimientos_caja m ON m.id = d.movimiento_caja_id WHERE d.motivo = '{motivo_dev}'"
    ));
    if let Some(dv) = dev.first() {
        let (dev_uuid, dev_total, ret_uuid) = (dv[0].clone(), dv[2].parse::<f64>().unwrap_or(0.0), dv[3].clone());
        chk!(inf, "b3", dv[1] == va_srv.to_string() && cerca(dev_total, pa.precio) && dv[4] == "principal",
             "servidor: venta_id {va_srv}, total {dev_total}, RETIRO en caja principal ({dv:?})");
        let _ = esc.pull().await;
        let va_local = venta_a.id;
        let vd_local = d_i64(&db, &format!("SELECT id FROM venta_detalle WHERE venta_id = {va_local} AND producto_id = 1"));
        let dl = d_foto(&db, &format!(
            "SELECT d.venta_id, dd.venta_detalle_id, dd.producto_id, d.usuario_id, d.total_devuelto, m.uuid \
               FROM devoluciones d JOIN devolucion_detalle dd ON dd.devolucion_id = d.id \
               LEFT JOIN movimientos_caja m ON m.id = d.movimiento_caja_id WHERE d.uuid = '{dev_uuid}'"
        ));
        chk!(inf, "b3", dl.len() == 1, "la devolución web llegó al escritorio ({dl:?}; bandeja {:?})", esc.inbox());
        if let Some(x) = dl.first() {
            let esperado = format!(
                "Integer({va_local})|Integer({vd_local})|Integer(1)|Integer({})|Real({dev_total:?})|",
                vend_local.unwrap_or(-1)
            );
            chk!(inf, "b3", x.starts_with(&esperado),
                 "venta_id/venta_detalle_id/producto_id/usuario_id LOCALES ({va_local}/{vd_local}/1/{:?}): {x}", vend_local);
            chk!(inf, "b3", x.ends_with(&format!("Text({ret_uuid:?})|")),
                 "movimiento_caja_id apunta al RETIRO local (mismo lote; movimientos antes que devoluciones): {x}");
        }
        let ret = d_foto(&db, &format!("SELECT caja, origen, monto FROM movimientos_caja WHERE uuid = '{ret_uuid}'"));
        chk!(inf, "b3", ret.len() == 1, "el RETIRO de la devolución llegó ({ret:?})");
        let periodo_b3 = esc.periodo(&mas_segundos(&cortes::ahora_local(), 2));
        chk!(inf, "b3", cerca(periodo_b3.total_retiros_efectivo, periodo_antes_b3.total_retiros_efectivo + dev_total)
                       && cerca(periodo_b3.efectivo_esperado, periodo_antes_b3.efectivo_esperado - dev_total),
             "el reembolso sale del período abierto: retiros {} → {}, esperado {} → {}",
             periodo_antes_b3.total_retiros_efectivo, periodo_b3.total_retiros_efectivo,
             periodo_antes_b3.efectivo_esperado, periodo_b3.efectivo_esperado);
        let s_srv = pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {}", pa.id));
        let s_loc = d_f64(&db, "SELECT stock_actual FROM productos WHERE id = 1");
        chk!(inf, "b3", cerca(s_srv, s_loc), "stock de P_A igual tras la devolución ({s_loc} / {s_srv})");
        chk!(inf, "b3", d_foto(&db, &format!("SELECT * FROM ventas WHERE id = {va_local}")).len() == 1
                       && d_i64(&db, &format!("SELECT anulada FROM ventas WHERE id = {va_local}")) == 0,
             "la venta del escritorio no se modificó");
    } else {
        chk!(inf, "b3", false, "la devolución no quedó en el servidor");
    }

    // ═══ g) cliente viejo (v0.1.38) ══════════════════════
    println!("\n-- g) cliente legado --");
    // g1) pull sin proto ni device_uuid (como v0.1.38).
    let mut legado: Vec<Value> = Vec::new();
    let mut cur = cursor0.to_string();
    for _ in 0..100 {
        let (st, txt) = web.get(&format!("/sync/pull?cursor={cur}&sucursal_id=1"), Some(&token_sync)).await;
        assert_eq!(st, 200, "pull legado: {txt}");
        let v: Value = serde_json::from_str(&txt).unwrap();
        legado.extend(v["cambios"].as_array().cloned().unwrap_or_default());
        let sig = v["next_cursor"].as_str().unwrap_or("").to_string();
        if !v["hay_mas"].as_bool().unwrap_or(false) || sig == cur {
            break;
        }
        cur = sig;
    }
    let buscar = |t: &str, u: &str| legado.iter().find(|c| c["__tabla"] == json!(t) && c["__data"]["uuid"] == json!(u)).cloned();
    match buscar("ventas", &va_uuid) {
        Some(c) => {
            let d = &c["__data"];
            chk!(inf, "g", d.get("usuario_id").is_none() && d.get("cliente_id").is_none() && d.get("anulada_por").is_none(),
                 "venta del escritorio traducida: sin usuario_id/cliente_id/anulada_por para el cliente viejo");
            let hijos = c["__children"]["venta_detalle"].as_array().cloned().unwrap_or_default();
            chk!(inf, "g", !hijos.is_empty() && hijos.iter().all(|h| h.get("producto_id").is_none() && h.get("autorizado_por").is_none()),
                 "sus renglones sin producto_id/autorizado_por ({} renglones)", hijos.len());
            chk!(inf, "g", c.get("__refs").is_none(), "sin __refs en legado");
        }
        None => chk!(inf, "g", false, "el pull legado devuelve la venta del escritorio (eco sin device_uuid)"),
    }
    if let Some(c) = buscar("productos", &p3_uuid) {
        chk!(inf, "g", c["__data"].get("categoria_id").is_none() && c["__data"].get("proveedor_id").is_none(),
             "producto del escritorio sin categoria_id/proveedor_id");
    }
    for (t, u, que) in [("ventas", &wb_uuid, "venta espejo"), ("ventas", &wc_uuid, "venta caja web"),
                        ("ventas", &wd_uuid, "venta tardía"), ("movimientos_caja", &mov_uuid, "movimiento espejo"),
                        ("movimientos_caja", &movw_uuid, "movimiento caja web")] {
        chk!(inf, "g", buscar(t, u).is_none(), "{que} web omitida en el pull legado");
    }
    chk!(inf, "g", !legado.iter().any(|c| (c["__tabla"] == json!("cortes") || c["__tabla"] == json!("aperturas_caja"))
                                         && !super::fk::es_uuid_escritorio(c["__data"]["uuid"].as_str().unwrap_or("x"))),
         "ningún corte/apertura web en el pull legado");
    chk!(inf, "g", !legado.iter().any(|c| c["__tabla"] == json!("usuarios")
                                         && c["__data"]["uuid"].as_str().unwrap_or("").starts_with("web-admin-")),
         "ningún usuario web-admin en el pull legado");
    // g2) push sin proto ni refs (ids crudos del escritorio, marca UTC como v0.1.38).
    //     Ninguna de sus FK tiene aún entrada en el mapa de este equipo:
    //     usuario 2 (Nancy aquí), cliente 1 (el cliente de h2 tendrá el id
    //     local 1), renglón con producto 1 (P_A) y autorizado_por 3 (Mariana).
    let vg_uuid = hex32();
    let dg_uuid = hex32();
    let utc = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let local_now = cortes::ahora_local();
    let body = json!({
        "device_uuid": device, "sucursal_id": 1,
        "cambios": [{
            "tabla": "ventas", "operacion": "UPDATE",
            "data": { "id": 900, "folio": format!("V-LEG-{run}"), "usuario_id": 2, "cliente_id": 1,
                      "subtotal": precio_a, "descuento": 0, "total": precio_a, "metodo_pago": "efectivo",
                      "monto_recibido": precio_a, "cambio": 0, "anulada": 0, "anulada_por": null,
                      "motivo_anulacion": null, "fecha": local_now, "synced_at": null, "uuid": vg_uuid,
                      "sucursal_id": 1, "updated_at": utc, "deleted_at": null },
            "children": { "venta_detalle": [{ "id": 901, "venta_id": 900, "producto_id": 1, "cantidad": 1,
                      "precio_original": precio_a, "descuento_porcentaje": 0, "descuento_monto": 0,
                      "precio_final": precio_a, "subtotal": precio_a, "autorizado_por": 3, "uuid": dg_uuid }] }
        }]
    });
    let (st, txt) = web.post("/sync/push", Some(&token_sync), &body).await;
    chk!(inf, "g", st == 200 && txt.contains(&vg_uuid) && !txt.contains("rechaz") || st == 200 && serde_json::from_str::<Value>(&txt).map(|v| v["aceptados"].as_array().map(|a| a.contains(&json!(vg_uuid))).unwrap_or(false)).unwrap_or(false),
         "push legado aceptado: {st} {txt}");
    let g = pg.filas(&format!(
        "SELECT v.usuario_id, vd.producto_id, v.updated_at, COALESCE(e.estado, ''), COALESCE(v.cliente_id::text, 'NULL'), \
                COALESCE(vd.autorizado_por::text, 'NULL'), (vd.venta_id = v.id)::text FROM ventas v \
           JOIN venta_detalle vd ON vd.venta_id = v.id LEFT JOIN sync_fk_estado e ON e.tabla = 'ventas' AND e.uuid = v.uuid \
          WHERE v.uuid = '{vg_uuid}'"
    ));
    chk!(inf, "g", g.len() == 1, "venta legada guardada con su renglón ({g:?})");
    if let Some(g) = g.first() {
        // Ronda 2 (SPEC2 R1.1): el push legado de una fila del escritorio sin
        // estado guarda las FK CRUDAS (como antes de v2: sin centinelas NULL/0
        // ni traducciones a medias); las traduce la reparación (f1).
        chk!(inf, "g", g[1] == "1" && g[0] == "2" && g[4] == "1" && g[5] == "3" && g[3].is_empty(),
             "push legado sin mapa: FK crudas tal cual, ni NULL ni 0 (usuario_id {}, cliente_id {}, producto_id {}, autorizado_por {}), sin estado ({:?})",
             g[0], g[4], g[1], g[5], g[3]);
        chk!(inf, "g", g[6] == "true", "el renglón cuelga del id del padre en el servidor");
        chk!(inf, "g", g[2].as_str() <= mas_segundos(&cortes::ahora_local(), 1).as_str(),
             "marca UTC del cliente viejo recortada a la hora local del servidor ({} vs utc {utc})", g[2]);
    }
    let pend_g = pg.i64(&format!("SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid IN ('{vg_uuid}', '{dg_uuid}')"));
    chk!(inf, "g", pend_g == 0, "push legado: sin celdas pendientes ({pend_g})");
    // g3) autenticación de /sync/*: tokens web.
    let (st_vend, _) = web.get("/sync/pull?cursor=0&sucursal_id=1&proto=2", Some(&tok_espejo)).await;
    chk!(inf, "g", st_vend == 403, "token de vendedor web → 403 en /sync/pull ({st_vend})");
    let tok_admin_dev = web.login_pin(&pin_admin, Some(&uuid::Uuid::new_v4().to_string())).await["token"].as_str().unwrap().to_string();
    let (st_ad, _) = web.get(&format!("/sync/pull?cursor={}&sucursal_id=1&proto=2", pg.max_cursor()), Some(&tok_admin_dev)).await;
    chk!(inf, "g", st_ad == 403, "token de admin web con dispositivo → 403 ({st_ad})");
    let (st_admin_sin_dev, _) = web.get(&format!("/sync/pull?cursor={}&sucursal_id=1&proto=2", pg.max_cursor()), Some(&tok_admin)).await;
    chk!(inf, "g3", st_admin_sin_dev == 403,
         "token de sesión web (login_pin de un dueño SIN deviceUuid, 7 días) → 403 en /sync/* (SPEC 3.2.1); respondió {st_admin_sin_dev}");
    // Ronda 2 (SPEC2 R2.1): el claim de dispositivo de un token de sesión web
    // nunca va vacío; sin equipo lleva el centinela.
    let claim_dev = claims_de(&tok_admin)["device"].clone();
    chk!(inf, "g3", claim_dev == json!("web-sin-equipo"), "login_pin sin deviceUuid emite el centinela de dispositivo ({claim_dev})");
    // Los demás endpoints de /sync/* con ese mismo token.
    for (ruta, body) in [
        ("/sync/push", json!({ "device_uuid": device, "sucursal_id": 1, "proto": 2, "cambios": [] })),
        ("/sync/id-map", json!({ "device_uuid": device, "tabla": "usuarios", "pares": [[1, u_dueno]] })),
        ("/sync/reparar", json!({ "device_uuid": device, "simulacion": true })),
        ("/sync/refs", json!({ "tabla": "ventas", "uuids": [va_uuid] })),
    ] {
        let (st, _) = web.post(ruta, Some(&tok_admin), &body).await;
        chk!(inf, "g3", st == 403, "token de PIN web sin dispositivo → 403 en {ruta} ({st})");
    }
    // Tokens de PIN emitidos ANTES del despliegue (sin claim de dispositivo,
    // válidos 7 días): el mismo JWT que firmaba el servidor viejo.
    match var("E2E_JWT_SECRET") {
        Some(secreto) => {
            let admin_srv = pg.i64(&format!("SELECT id FROM usuarios WHERE uuid = 'e2e-admin-{run}'"));
            let viejo = token_pin_viejo(&secreto, admin_srv, &format!("e2e_admin_{run}"), "admin");
            let (st, _) = web.get(&format!("/sync/pull?cursor={}&sucursal_id=1&proto=2", pg.max_cursor()), Some(&viejo)).await;
            chk!(inf, "g3", st == 403, "token de PIN de dueño SIN dispositivo emitido por el servidor viejo → 403 en /sync/pull ({st})");
            let (st, _) = web.post("/sync/id-map", Some(&viejo), &json!({ "device_uuid": device, "tabla": "usuarios", "pares": [[1, u_dueno]] })).await;
            chk!(inf, "g3", st == 403, "… y 403 en /sync/id-map ({st})");
            // Control: un token igual pero de un usuario del panel (como el de
            // /auth/login del escritorio) sí entra.
            let panel = pg.filas(&format!(
                "SELECT id, email FROM admin_users WHERE email = 'e2e-{run}@local.test'"
            ));
            let tok_panel = token_pin_viejo(&secreto, panel[0][0].parse().unwrap(), &panel[0][1], "admin");
            let (st, _) = web.get(&format!("/sync/pull?cursor={}&sucursal_id=1&proto=2", pg.max_cursor()), Some(&tok_panel)).await;
            chk!(inf, "g3", st == 200, "control: token de un usuario activo del panel (como el de la tienda) → 200 ({st})");
        }
        None => inf.nota("g3", "sin E2E_JWT_SECRET: no se probó el token de PIN viejo sin dispositivo".into()),
    }
    let (st_tienda, _) = web.get(&format!("/sync/pull?cursor={}&sucursal_id=1&proto=2", pg.max_cursor()), Some(&token_sync)).await;
    chk!(inf, "g3", st_tienda == 200, "el token del escritorio (/auth/login) sigue entrando ({st_tienda})");

    // ═══ h) reloj ═════════════════════════════════════════
    println!("\n-- h) reloj --");
    let p3_srv_id = pg.i64(&format!("SELECT id FROM productos WHERE uuid = '{p3_uuid}'"));
    let futuro = mas_segundos(&cortes::ahora_local(), 6 * 3600);
    {
        let c = db.lock().unwrap();
        c.execute("UPDATE productos SET nombre = 'E2E futuro', updated_at = ?1 WHERE id = 3", [&futuro]).unwrap();
    }
    let _ = esc.push().await;
    let srv = pg.filas(&format!("SELECT nombre, updated_at FROM productos WHERE id = {p3_srv_id}"));
    let ahora_h = cortes::ahora_local();
    chk!(inf, "h", srv[0][0] == "E2E futuro", "cambio con marca futura aceptado ({})", srv[0][0]);
    chk!(inf, "h", srv[0][1].as_str() < futuro.as_str() && srv[0][1].as_str() <= mas_segundos(&ahora_h, 1).as_str(),
         "marca recortada a la hora del servidor: {} (escritorio {futuro})", srv[0][1]);
    let tope = srv[0][1].clone();
    {
        let c = db.lock().unwrap();
        c.execute("UPDATE productos SET nombre = 'E2E igual', updated_at = ?1 WHERE id = 3", [&tope]).unwrap();
    }
    let _ = esc.push().await;
    let n = pg.uno(&format!("SELECT nombre FROM productos WHERE id = {p3_srv_id}"));
    chk!(inf, "h", n.as_deref() == Some("E2E igual"), "re-push con la MISMA marca se aplica ({n:?}; outbox {:?})", esc.errores_outbox());
    {
        let c = db.lock().unwrap();
        c.execute("UPDATE productos SET nombre = 'E2E viejo', updated_at = ?1 WHERE id = 3", [&mas_segundos(&tope, -3600)]).unwrap();
    }
    let _ = esc.push().await;
    let n = pg.uno(&format!("SELECT nombre FROM productos WHERE id = {p3_srv_id}"));
    let err = esc.errores_outbox();
    // Ronda 2 (SPEC2 R1.3): la versión guardada la escribió ESTE equipo, así
    // que su fila actual se aplica aunque la marca sea más vieja (reloj que
    // se atrasó): el escritorio siempre manda su fila actual.
    chk!(inf, "h", n.as_deref() == Some("E2E viejo") && esc.pendientes_outbox() == 0,
         "marca más vieja del MISMO equipo se aplica (servidor {n:?}; outbox {err:?})");
    let _ = esc.pull().await;
    let local_n = d_txt(&db, "SELECT nombre FROM productos WHERE id = 3");
    chk!(inf, "h", local_n.as_deref() == n.as_deref(),
         "escritorio y servidor iguales tras la marca vieja ('{}' / '{}')",
         local_n.clone().unwrap_or_default(), n.clone().unwrap_or_default());

    // h2) rechazo 'remoto_mas_nuevo' por un cambio web más nuevo (otro
    //     equipo): la bandeja pide la versión del servidor y la aplica; el
    //     renglón rechazado del outbox queda resuelto (SPEC2 R3.10).
    let cli_uuid = hex32();
    {
        let c = db.lock().unwrap();
        c.execute(
            "INSERT INTO clientes (nombre, uuid, updated_at) VALUES ('E2E cliente', ?1, ?2)",
            rusqlite::params![cli_uuid, cortes::ahora_local()],
        ).unwrap();
    }
    let _ = esc.push().await;
    let cli_srv_id = pg.i64(&format!("SELECT id FROM clientes WHERE uuid = '{cli_uuid}'"));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let ed = web
        .rpc(&tok_admin, "actualizar_cliente", json!({ "usuario_id": 0, "datos": {
            "id": cli_srv_id, "nombre": "E2E cliente editado en la web", "telefono": null, "email": null,
            "descuento_porcentaje": 0.0, "notas": null } }))
        .await;
    chk!(inf, "h", ed.is_ok(), "edición web del cliente: {ed:?}");
    {
        let c = db.lock().unwrap();
        c.execute(
            "UPDATE clientes SET nombre = 'E2E cliente local viejo', updated_at = ?1 WHERE uuid = ?2",
            rusqlite::params![mas_segundos(&cortes::ahora_local(), -3600), cli_uuid],
        ).unwrap();
    }
    let _ = esc.push().await;
    let en_bandeja = d_txt(&db, &format!("SELECT motivo FROM sync_inbox WHERE tabla = 'clientes' AND uuid = '{cli_uuid}'"));
    chk!(inf, "h", en_bandeja.as_deref() == Some("pendiente_local"),
         "rechazo remoto_mas_nuevo → la bandeja pide la versión del servidor ({en_bandeja:?}; outbox {:?})", esc.errores_outbox());
    let ri = inbox::reintentar(&db, &esc.client).await;
    let local_cli = d_txt(&db, &format!("SELECT nombre FROM clientes WHERE uuid = '{cli_uuid}'"));
    let resuelto = d_txt(&db, &format!(
        "SELECT ultimo_error FROM sync_outbox WHERE tabla = 'clientes' AND uuid = '{cli_uuid}' AND synced_at IS NOT NULL ORDER BY id DESC LIMIT 1"
    ));
    chk!(inf, "h", local_cli.as_deref() == Some("E2E cliente editado en la web")
                 && resuelto.as_deref() == Some(super::apply::RESUELTO_SERVIDOR)
                 && esc.pendientes_outbox() == 0,
         "la versión web del cliente baja y el rechazo queda resuelto (reintento {ri:?}; local {local_cli:?}; outbox {resuelto:?}; pendientes {})",
         esc.pendientes_outbox());
    // Escritorio con reloj adelantado (SPEC3 D1): el servidor guarda la fila
    // con SU marca y la devuelve en `marcas`; la fila local la toma, así que
    // una edición web posterior (anterior a la marca adelantada) sí baja.
    {
        let c = db.lock().unwrap();
        c.execute("UPDATE productos SET nombre = 'E2E reloj adelantado', updated_at = ?1 WHERE id = 3", [&mas_segundos(&cortes::ahora_local(), 600)]).unwrap();
    }
    let _ = esc.push().await;
    let marca_srv = pg.uno(&format!("SELECT updated_at FROM productos WHERE id = {p3_srv_id}")).unwrap_or_default();
    let marca_local = d_txt(&db, "SELECT updated_at FROM productos WHERE id = 3").unwrap_or_default();
    chk!(inf, "h", !marca_srv.is_empty() && marca_local == marca_srv.chars().take(19).collect::<String>().replace('T', " "),
         "la fila local toma la marca con la que la guardó el servidor (local {marca_local}, servidor {marca_srv}; pendientes {})",
         esc.pendientes_outbox());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let ed = web
        .rpc(&tok_admin, "actualizar_producto", json!({ "usuario_id": 0, "producto": {
            "id": p3_srv_id, "codigo": p3_codigo, "nombre": "E2E editado en la web", "descripcion": null,
            "categoria_id": null, "precio_costo": 80.0, "precio_venta": 130.0, "stock_minimo": 0.0,
            "proveedor_id": null, "foto_url": null } }))
        .await;
    chk!(inf, "h", ed.is_ok(), "edición web del producto: {ed:?}");
    let _ = esc.pull().await;
    let local_n = d_txt(&db, "SELECT nombre || ' / ' || precio_venta FROM productos WHERE id = 3");
    let srv_n = pg.uno(&format!("SELECT nombre || ' / ' || precio_venta FROM productos WHERE id = {p3_srv_id}"));
    let mismo_precio = |x: &Option<String>| {
        x.as_deref().and_then(|t| t.rsplit(" / ").next()).and_then(|p| p.parse::<f64>().ok()).map(|p| cerca(p, 130.0)).unwrap_or(false)
    };
    chk!(inf, "h", local_n.as_deref().unwrap_or("").starts_with("E2E editado en la web / ")
                 && srv_n.as_deref().unwrap_or("").starts_with("E2E editado en la web / ")
                 && mismo_precio(&local_n) && mismo_precio(&srv_n),
         "reloj del escritorio 10 min adelantado: la edición web posterior baja (escritorio {local_n:?}, servidor {srv_n:?})");

    // ═══ j) edición web de precio vs. venta del escritorio sin bajar aún ═══
    println!("\n-- j) precio editado en la web y venta en el escritorio antes del pull --");
    let pf = pg.filas(&format!(
        "SELECT codigo, nombre, COALESCE(descripcion, ''), COALESCE(categoria_id::text, ''), precio_costo::float8, \
                precio_venta::float8, stock_minimo::float8, COALESCE(proveedor_id::text, '') FROM productos WHERE id = {}",
        pa.id
    ));
    let pf = &pf[0];
    let precio_viejo: f64 = pf[5].parse().unwrap();
    let precio_web = r2(precio_viejo + 10.0);
    let stock_j0 = pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {}", pa.id));
    let base_j = d_txt(&db, &format!("SELECT updated_at FROM sync_base WHERE tabla = 'productos' AND uuid = '{}'", pa.uuid));
    chk!(inf, "j", base_j.is_some(), "el escritorio tiene base de P_A (sync_base: {base_j:?})");
    let ed = web
        .rpc(&tok_admin, "actualizar_producto", json!({ "usuario_id": 0, "producto": {
            "id": pa.id, "codigo": pf[0], "nombre": pf[1], "descripcion": if pf[2].is_empty() { Value::Null } else { json!(pf[2]) },
            "categoria_id": pf[3].parse::<i64>().ok(), "precio_costo": pf[4].parse::<f64>().unwrap(),
            "precio_venta": precio_web, "stock_minimo": pf[6].parse::<f64>().unwrap(),
            "proveedor_id": pf[7].parse::<i64>().ok(), "foto_url": null } }))
        .await;
    chk!(inf, "j", ed.is_ok(), "el dueño cambia el precio de P_A en la web a {precio_web}: {ed:?}");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    // La tienda vende P_A (todavía con el precio que conoce) y el worker
    // empuja antes de jalar (push → pull en cada ciclo).
    let venta_j = {
        let c = db.lock().unwrap();
        let precio_local: f64 = c.query_row("SELECT precio_venta FROM productos WHERE id = 1", [], |r| r.get(0)).unwrap();
        crear_venta_core(
            &c,
            NuevaVenta {
                usuario_id: 2, cliente_id: None, subtotal: precio_local, descuento: 0.0, total: precio_local,
                metodo_pago: "tarjeta".into(), monto_recibido: precio_local, cambio: 0.0,
                items: vec![ItemVenta { producto_id: 1, cantidad: 1.0, precio_original: precio_local, descuento_porcentaje: 0.0,
                                        descuento_monto: 0.0, precio_final: precio_local, subtotal: precio_local, autorizado_por: None }],
                presupuesto_origen_id: None,
            },
            &cortes::ahora_local(),
        )
        .unwrap()
        .id
    };
    let _ = esc.push().await;
    let cur_j = pg.uno(&format!(
        "SELECT origen_device FROM sync_cursor WHERE tabla = 'productos' AND uuid = '{}' ORDER BY id DESC LIMIT 1", pa.uuid
    ));
    let _ = esc.pull().await;
    let p_srv = pg.f64(&format!("SELECT precio_venta::float8 FROM productos WHERE id = {}", pa.id));
    let p_loc = d_f64(&db, "SELECT precio_venta FROM productos WHERE id = 1");
    chk!(inf, "j", cerca(p_srv, precio_web) && cerca(p_loc, precio_web),
         "el precio editado en la web sobrevive a la venta del escritorio (servidor {p_srv}, escritorio {p_loc}, web puso {precio_web}, antes {precio_viejo})");
    let s_srv = pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {}", pa.id));
    let s_loc = d_f64(&db, "SELECT stock_actual FROM productos WHERE id = 1");
    chk!(inf, "j", cerca(s_srv, stock_j0 - 1.0) && cerca(s_loc, s_srv),
         "y la venta del escritorio también: stock {stock_j0} → servidor {s_srv}, escritorio {s_loc}");
    chk!(inf, "j", cur_j.as_deref() == Some("merge"), "fusión de 3 vías con cursor 'merge' ({cur_j:?})");
    chk!(inf, "j", esc.pendientes_outbox() == 0 && esc.inbox().is_empty(), "outbox y bandeja limpios ({:?} / {:?})", esc.errores_outbox(), esc.inbox());

    // ═══ n) venta web y venta del escritorio del mismo producto en el mismo ciclo ═══
    println!("\n-- n) ventas simultáneas del mismo producto (web y escritorio) --");
    let _ = esc.pull().await;
    let s0_srv = pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {}", pb.id));
    let s0_loc = d_f64(&db, "SELECT stock_actual FROM productos WHERE id = 2");
    let precio_b_n = pg.f64(&format!("SELECT precio_venta::float8 FROM productos WHERE id = {}", pb.id));
    let rv = venta_web(&web, &tok_espejo, &[(pb.id, 1.0, precio_b_n)]).await;
    chk!(inf, "n", rv.is_ok(), "venta web de P_B: {rv:?}");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    {
        let c = db.lock().unwrap();
        let p_local: f64 = c.query_row("SELECT precio_venta FROM productos WHERE id = 2", [], |r| r.get(0)).unwrap();
        crear_venta_core(
            &c,
            NuevaVenta {
                usuario_id: 3, cliente_id: None, subtotal: p_local, descuento: 0.0, total: p_local,
                metodo_pago: "tarjeta".into(), monto_recibido: p_local, cambio: 0.0,
                items: vec![ItemVenta { producto_id: 2, cantidad: 1.0, precio_original: p_local, descuento_porcentaje: 0.0,
                                        descuento_monto: 0.0, precio_final: p_local, subtotal: p_local, autorizado_por: None }],
                presupuesto_origen_id: None,
            },
            &cortes::ahora_local(),
        )
        .unwrap();
    }
    // Ciclo del worker: push → pull.
    let _ = esc.push().await;
    let _ = esc.pull().await;
    let s1_srv = pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {}", pb.id));
    let s1_loc = d_f64(&db, "SELECT stock_actual FROM productos WHERE id = 2");
    chk!(inf, "n", cerca(s1_srv, s0_srv - 2.0) && cerca(s1_loc, s1_srv),
         "dos piezas vendidas (1 web + 1 escritorio): stock {s0_srv} → servidor {s1_srv}, escritorio {s1_loc} (esperado {}; local antes {s0_loc})",
         s0_srv - 2.0);
    chk!(inf, "n", esc.pendientes_outbox() == 0, "outbox vacío ({:?})", esc.errores_outbox());

    // n2) ajuste de existencia en la web + venta del escritorio en el mismo
    //     ciclo: queda lo ajustado menos lo vendido.
    println!("\n-- n2) ajuste de stock web y venta del escritorio en el mismo ciclo --");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let ajustado = s1_srv + 7.0;
    let rr = web
        .rpc(&tok_admin, "ajustar_stock", json!({ "productoId": pb.id, "nuevoStock": ajustado, "motivo": "conteo e2e", "usuarioId": 0 }))
        .await;
    chk!(inf, "n2", rr.is_ok(), "ajustar_stock web de P_B a {ajustado}: {rr:?}");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    {
        let c = db.lock().unwrap();
        let p_local: f64 = c.query_row("SELECT precio_venta FROM productos WHERE id = 2", [], |r| r.get(0)).unwrap();
        crear_venta_core(
            &c,
            NuevaVenta {
                usuario_id: 2, cliente_id: None, subtotal: p_local, descuento: 0.0, total: p_local,
                metodo_pago: "tarjeta".into(), monto_recibido: p_local, cambio: 0.0,
                items: vec![ItemVenta { producto_id: 2, cantidad: 1.0, precio_original: p_local, descuento_porcentaje: 0.0,
                                        descuento_monto: 0.0, precio_final: p_local, subtotal: p_local, autorizado_por: None }],
                presupuesto_origen_id: None,
            },
            &cortes::ahora_local(),
        )
        .unwrap();
    }
    let _ = esc.push().await;
    let _ = esc.pull().await;
    let s2_srv = pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {}", pb.id));
    let s2_loc = d_f64(&db, "SELECT stock_actual FROM productos WHERE id = 2");
    chk!(inf, "n2", cerca(s2_srv, ajustado - 1.0) && cerca(s2_loc, s2_srv),
         "ajuste web {ajustado} − 1 vendida en el escritorio: servidor {s2_srv}, escritorio {s2_loc} (esperado {})", ajustado - 1.0);
    chk!(inf, "n2", esc.pendientes_outbox() == 0, "outbox vacío ({:?})", esc.errores_outbox());

    // ═══ k) anulación en el escritorio de una venta ya subida ═══
    println!("\n-- k) anulación del escritorio --");
    let vj = venta_j;
    let vj_uuid = d_txt(&db, &format!("SELECT uuid FROM ventas WHERE id = {vj}")).unwrap();
    let an = {
        let c = db.lock().unwrap();
        crate::commands::ventas::anular_venta_core(&c, vj, 1, "Anulación E2E".into(), &cortes::ahora_local())
    };
    chk!(inf, "k", an.is_ok(), "anular_venta_core: {an:?}");
    let _ = esc.push().await;
    let a = pg.filas(&format!("SELECT anulada, COALESCE(anulada_por::text, ''), usuario_id FROM ventas WHERE uuid = '{vj_uuid}'"));
    chk!(inf, "k", a.first().map(|a| a[0] == "1" && a[1] == dueno_srv.to_string() && a[2] == nancy_srv.to_string()).unwrap_or(false),
         "servidor: anulada=1, anulada_por = Dueño ({dueno_srv}), vendedor Nancy ({nancy_srv}): {a:?}");
    chk!(inf, "k", esc.pendientes_outbox() == 0, "outbox vacío ({:?})", esc.errores_outbox());

    // ═══ m) recepción de mercancía hecha en la web ═══
    println!("\n-- m) recepción web --");
    let prov_srv_id = pg.i64("SELECT id FROM proveedores WHERE nombre = 'LEON' ORDER BY id LIMIT 1");
    // (sin empate de segundo con lo anterior: eso lo prueba el escenario o)
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let stock_b_antes = pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {}", pb.id));
    let rr = web
        .rpc(&tok_admin, "crear_recepcion", json!({ "recepcion": {
            "usuario_id": 0, "proveedor_id": prov_srv_id, "orden_id": null, "notas": format!("Recepcion E2E {run}"),
            "items": [{ "producto_id": pb.id, "cantidad": 3, "precio_costo": 10.0, "precio_venta": null }] } }))
        .await;
    chk!(inf, "m", rr.is_ok(), "crear_recepcion web: {rr:?}");
    let rec = pg.filas(&format!("SELECT uuid FROM recepciones WHERE notas = 'Recepcion E2E {run}'"));
    if let Some(rec) = rec.first() {
        let _ = esc.pull().await;
        let rl = d_foto(&db, &format!(
            "SELECT r.proveedor_id, rd.producto_id, rd.cantidad FROM recepciones r \
               JOIN recepcion_detalle rd ON rd.recepcion_id = r.id WHERE r.uuid = '{}'", rec[0]
        ));
        let prov_local = d_opt_i64(&db, &format!(
            "SELECT id FROM proveedores WHERE uuid = (SELECT uuid FROM proveedores WHERE id = {prov_srv_id})"
        ));
        let prov_local = prov_local.or_else(|| {
            let u = pg.uno(&format!("SELECT uuid FROM proveedores WHERE id = {prov_srv_id}")).unwrap();
            d_opt_i64(&db, &format!("SELECT id FROM proveedores WHERE uuid = '{u}'"))
        });
        chk!(inf, "m", rl.len() == 1 && rl[0].starts_with(&format!("Integer({})|Integer(2)|", prov_local.unwrap_or(-1))),
             "recepción web en el escritorio con proveedor y producto LOCALES ({rl:?}; bandeja {:?})", esc.inbox());
        let s_srv = pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {}", pb.id));
        let s_loc = d_f64(&db, "SELECT stock_actual FROM productos WHERE id = 2");
        let u_srv = pg.uno(&format!("SELECT updated_at FROM productos WHERE id = {}", pb.id));
        let u_loc = d_txt(&db, "SELECT updated_at FROM productos WHERE id = 2");
        chk!(inf, "m", cerca(s_srv, stock_b_antes + 3.0) && cerca(s_loc, s_srv),
             "stock de P_B +3 en los dos lados ({stock_b_antes} → {s_srv}; local {s_loc}; updated_at servidor {u_srv:?} / local {u_loc:?})");
    }

    // ═══ o) empate de marca: cambio del escritorio y cambio web en el mismo segundo ═══
    println!("\n-- o) empate de updated_at en el mismo segundo --");
    let mut empate = None;
    for _ in 0..4 {
        // Arrancar al inicio de un segundo para que todo quepa en él.
        let ms = chrono::Local::now().timestamp_subsec_millis() as u64;
        tokio::time::sleep(Duration::from_millis(1000 - ms + 20)).await;
        {
            let c = db.lock().unwrap();
            c.execute("UPDATE productos SET stock_actual = stock_actual + 1 WHERE id = 2", []).unwrap();
        }
        let t_esc = d_txt(&db, "SELECT updated_at FROM productos WHERE id = 2").unwrap_or_default();
        let _ = esc.push().await;
        let nuevo = pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {}", pb.id)) + 5.0;
        let rr = web
            .rpc(&tok_admin, "ajustar_stock", json!({ "productoId": pb.id, "nuevoStock": nuevo,
                                                      "motivo": "empate e2e", "usuarioId": 0 }))
            .await;
        let t_srv = pg.uno(&format!("SELECT updated_at FROM productos WHERE id = {}", pb.id)).unwrap_or_default();
        if rr.is_err() {
            inf.nota("o", format!("ajustar_stock web no disponible con esos argumentos: {rr:?}"));
            break;
        }
        if t_srv == t_esc {
            empate = Some(t_srv);
            break;
        }
    }
    if let Some(t) = empate {
        let _ = esc.pull().await;
        let s_srv = pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {}", pb.id));
        let s_loc = d_f64(&db, "SELECT stock_actual FROM productos WHERE id = 2");
        chk!(inf, "o", cerca(s_srv, s_loc),
             "cambio web en el mismo segundo ({t}) que el del escritorio ya subido: el escritorio lo recibe (servidor {s_srv}, escritorio {s_loc})");
    } else {
        inf.nota("o", "no se logró un empate de segundo (no se evaluó)".into());
    }

    // ═══ p) cambio local sin subir MÁS VIEJO que una edición web ═══
    //     (p. ej. el push falló por red y el pull sí pasó): la edición web
    //     espera en la bandeja ('pendiente_local'); el push choca con la
    //     versión más nueva del servidor → merge (stock del escritorio, lo
    //     demás de la web) → el escritorio baja la fila combinada.
    println!("\n-- p) cambio local más viejo que una edición web: bandeja + merge --");
    let _ = esc.push().await;
    let _ = esc.pull().await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    {
        let c = db.lock().unwrap();
        let p_local: f64 = c.query_row("SELECT precio_venta FROM productos WHERE id = 1", [], |r| r.get(0)).unwrap();
        crear_venta_core(
            &c,
            NuevaVenta {
                usuario_id: 2, cliente_id: None, subtotal: p_local, descuento: 0.0, total: p_local,
                metodo_pago: "tarjeta".into(), monto_recibido: p_local, cambio: 0.0,
                items: vec![ItemVenta { producto_id: 1, cantidad: 1.0, precio_original: p_local, descuento_porcentaje: 0.0,
                                        descuento_monto: 0.0, precio_final: p_local, subtotal: p_local, autorizado_por: None }],
                presupuesto_origen_id: None,
            },
            &cortes::ahora_local(),
        )
        .unwrap();
    }
    let s_loc_p = d_f64(&db, "SELECT stock_actual FROM productos WHERE id = 1");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let pf = pg.filas(&format!(
        "SELECT codigo, nombre, COALESCE(descripcion, ''), COALESCE(categoria_id::text, ''), precio_costo::float8, \
                precio_venta::float8, stock_minimo::float8, COALESCE(proveedor_id::text, '') FROM productos WHERE id = {}",
        pa.id
    ));
    let pf = &pf[0];
    let precio_p = r2(pf[5].parse::<f64>().unwrap() + 7.0);
    let ed = web
        .rpc(&tok_admin, "actualizar_producto", json!({ "usuario_id": 0, "producto": {
            "id": pa.id, "codigo": pf[0], "nombre": pf[1], "descripcion": if pf[2].is_empty() { Value::Null } else { json!(pf[2]) },
            "categoria_id": pf[3].parse::<i64>().ok(), "precio_costo": pf[4].parse::<f64>().unwrap(),
            "precio_venta": precio_p, "stock_minimo": pf[6].parse::<f64>().unwrap(),
            "proveedor_id": pf[7].parse::<i64>().ok(), "foto_url": null } }))
        .await;
    chk!(inf, "p", ed.is_ok(), "web: precio de P_A a {precio_p} DESPUÉS de la venta local sin subir: {ed:?}");
    // El pull pasa antes que el push.
    let _ = esc.pull().await;
    let parked = d_txt(&db, &format!("SELECT motivo FROM sync_inbox WHERE tabla = 'productos' AND uuid = '{}'", pa.uuid));
    chk!(inf, "p", parked.as_deref() == Some("pendiente_local"),
         "la edición web espera en la bandeja como 'pendiente_local' ({parked:?})");
    chk!(inf, "p", cerca(d_f64(&db, "SELECT stock_actual FROM productos WHERE id = 1"), s_loc_p),
         "el cambio local (stock) no se pisa mientras no sube");
    let _ = esc.push().await;
    let srv = pg.filas(&format!("SELECT precio_venta::float8, stock_actual::float8 FROM productos WHERE id = {}", pa.id));
    let (srv_precio, srv_stock): (f64, f64) = (srv[0][0].parse().unwrap(), srv[0][1].parse().unwrap());
    chk!(inf, "p", cerca(srv_precio, precio_p) && cerca(srv_stock, s_loc_p),
         "servidor tras el push: precio de la web ({srv_precio} = {precio_p}) y stock del escritorio ({srv_stock} = {s_loc_p})");
    let merge = pg.uno(&format!(
        "SELECT origen_device FROM sync_cursor WHERE tabla = 'productos' AND uuid = '{}' ORDER BY id DESC LIMIT 1", pa.uuid
    ));
    chk!(inf, "p", merge.as_deref() == Some("merge"), "cursor 'merge' para que el escritorio baje la fila combinada ({merge:?})");
    chk!(inf, "p", esc.pendientes_outbox() == 0, "outbox vacío tras el merge ({:?})", esc.errores_outbox());
    let _ = esc.pull().await;
    let ri = inbox::reintentar(&db, &esc.client).await;
    let p_loc = d_f64(&db, "SELECT precio_venta FROM productos WHERE id = 1");
    let s_loc = d_f64(&db, "SELECT stock_actual FROM productos WHERE id = 1");
    chk!(inf, "p", cerca(p_loc, precio_p) && cerca(s_loc, s_loc_p),
         "escritorio converge: precio {p_loc} (web {precio_p}), stock {s_loc} (local {s_loc_p}); bandeja {ri:?}");
    let sigue = d_i64(&db, &format!("SELECT count(*) FROM sync_inbox WHERE uuid = '{}'", pa.uuid));
    chk!(inf, "p", sigue == 0, "la bandeja ya no tiene P_A ({sigue})");
    chk!(inf, "p", esc.pendientes_outbox() == 0, "lo aplicado no regresa al outbox ({})", esc.pendientes_outbox());

    // ═══ q) producto nuevo de la web vendido en espejo en el mismo ciclo ═══
    println!("\n-- q) producto creado en la web y vendido en espejo antes del pull --");
    let frenos_srv = pg.i64("SELECT id FROM categorias WHERE nombre = 'Frenos' AND deleted_at IS NULL ORDER BY id LIMIT 1");
    let frenos_uuid = pg.uno(&format!("SELECT uuid FROM categorias WHERE id = {frenos_srv}")).unwrap();
    let frenos_local = d_opt_i64(&db, &format!("SELECT id FROM categorias WHERE uuid = '{frenos_uuid}'"));
    let leon_srv = pg.i64("SELECT id FROM proveedores WHERE nombre = 'LEON' ORDER BY id LIMIT 1");
    let codigo_q = format!("E2E-WEB-{run}");
    let np = web
        .rpc(&tok_admin, "crear_producto", json!({ "usuario_id": 0, "producto": {
            "codigo": codigo_q, "codigo_tipo": null, "nombre": format!("Producto web E2E {run}"), "descripcion": null,
            "categoria_id": frenos_srv, "precio_costo": 40.0, "precio_venta": 88.0, "stock_actual": 6.0,
            "stock_minimo": 0.0, "proveedor_id": leon_srv, "foto_url": null } }))
        .await;
    chk!(inf, "q", np.is_ok(), "crear_producto web (categoría Frenos id servidor {frenos_srv}): {np:?}");
    let (q_id, q_uuid) = {
        let f = pg.filas(&format!("SELECT id, uuid FROM productos WHERE codigo = '{codigo_q}'"));
        (f[0][0].parse::<i64>().unwrap(), f[0][1].clone())
    };
    let rv = venta_web(&web, &tok_espejo, &[(q_id, 2.0, 88.0)]).await;
    chk!(inf, "q", rv.is_ok(), "venta espejo del producto nuevo: {rv:?}");
    let wq_uuid = pg.uno(&format!("SELECT uuid FROM ventas WHERE id = {}", rv.as_ref().map(|v| v["id"].as_i64().unwrap_or(0)).unwrap_or(0))).unwrap_or_default();
    let antes_q = esc.periodo(&mas_segundos(&cortes::ahora_local(), 2));
    let _ = esc.pull().await;
    let pq = d_foto(&db, &format!("SELECT id, categoria_id, proveedor_id, stock_actual FROM productos WHERE uuid = '{q_uuid}'"));
    chk!(inf, "q", pq.len() == 1, "el producto web llegó ({pq:?}; bandeja {:?})", esc.inbox());
    let q_local = d_opt_i64(&db, &format!("SELECT id FROM productos WHERE uuid = '{q_uuid}'"));
    let q_cat = d_opt_i64(&db, &format!("SELECT categoria_id FROM productos WHERE uuid = '{q_uuid}'"));
    let q_prov = d_opt_i64(&db, &format!("SELECT proveedor_id FROM productos WHERE uuid = '{q_uuid}'"));
    let leon_local = d_opt_i64(&db, &format!(
        "SELECT id FROM proveedores WHERE uuid = '{}'", pg.uno(&format!("SELECT uuid FROM proveedores WHERE id = {leon_srv}")).unwrap()
    ));
    chk!(inf, "q", q_cat.is_some() && q_cat == frenos_local && q_prov == leon_local,
         "categoría/proveedor LOCALES (categoria {q_cat:?} = Frenos local {frenos_local:?}, no {frenos_srv}; proveedor {q_prov:?} = {leon_local:?})");
    chk!(inf, "q", cerca(d_f64(&db, &format!("SELECT COALESCE((SELECT stock_actual FROM productos WHERE uuid = '{q_uuid}'), -1)")), 4.0),
         "stock del producto web tras la venta: 4 en los dos lados (servidor {})",
         pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {q_id}")));
    let det = d_foto(&db, &format!(
        "SELECT vd.producto_id FROM venta_detalle vd JOIN ventas v ON v.id = vd.venta_id WHERE v.uuid = '{wq_uuid}'"
    ));
    chk!(inf, "q", det.len() == 1 && q_local.map(|l| det[0] == format!("Integer({l})|")).unwrap_or(false),
         "la venta llegó y su renglón apunta al id LOCAL del producto nuevo ({det:?} vs {q_local:?}; servidor {q_id})");
    let despues_q = esc.periodo(&mas_segundos(&cortes::ahora_local(), 2));
    chk!(inf, "q", cerca(despues_q.total_ventas_efectivo - antes_q.total_ventas_efectivo, 176.0),
         "la venta espejo cuenta en el período abierto (+{})", despues_q.total_ventas_efectivo - antes_q.total_ventas_efectivo);

    // ═══ q2) producto creado en la web, vendido en espejo y BORRADO en la web
    //     antes de que el escritorio jale (p. ej. durante un corte de internet) ═══
    println!("\n-- q2) producto web vendido y luego borrado antes del pull --");
    let codigo_q2 = format!("E2E-BORR-{run}");
    let np = web
        .rpc(&tok_admin, "crear_producto", json!({ "usuario_id": 0, "producto": {
            "codigo": codigo_q2, "codigo_tipo": null, "nombre": format!("Producto web borrado E2E {run}"), "descripcion": null,
            "categoria_id": null, "precio_costo": 20.0, "precio_venta": 55.0, "stock_actual": 2.0,
            "stock_minimo": 0.0, "proveedor_id": null, "foto_url": null } }))
        .await;
    chk!(inf, "q2", np.is_ok(), "crear_producto web: {np:?}");
    let q2 = pg.filas(&format!("SELECT id, uuid FROM productos WHERE codigo = '{codigo_q2}'"));
    if let Some(q2) = q2.first() {
        let (q2_id, q2_uuid) = (q2[0].parse::<i64>().unwrap(), q2[1].clone());
        let rv = venta_web(&web, &tok_espejo, &[(q2_id, 1.0, 55.0)]).await;
        chk!(inf, "q2", rv.is_ok(), "venta espejo: {rv:?}");
        let wq2_uuid = pg.uno(&format!("SELECT uuid FROM ventas WHERE id = {}", rv.as_ref().map(|v| v["id"].as_i64().unwrap_or(0)).unwrap_or(0))).unwrap_or_default();
        let el = web.rpc(&tok_admin, "eliminar_producto", json!({ "productoId": q2_id, "usuarioId": 0 })).await;
        chk!(inf, "q2", el.is_ok(), "eliminar_producto web: {el:?}");
        let antes_q2 = esc.periodo(&mas_segundos(&cortes::ahora_local(), 2));
        let _ = esc.pull().await;
        let llego = d_i64(&db, &format!("SELECT count(*) FROM ventas WHERE uuid = '{wq2_uuid}'"));
        let motivo = d_txt(&db, &format!("SELECT motivo || ': ' || COALESCE(error, '') FROM sync_inbox WHERE uuid = '{wq2_uuid}'"));
        let despues_q2 = esc.periodo(&mas_segundos(&cortes::ahora_local(), 2));
        chk!(inf, "q2", llego == 1 && cerca(despues_q2.total_ventas_efectivo - antes_q2.total_ventas_efectivo, 55.0),
             "la venta espejo de un producto que la web borró después llega y cuenta (llegó {llego}, +{}; bandeja {motivo:?}; producto local {:?})",
             despues_q2.total_ventas_efectivo - antes_q2.total_ventas_efectivo,
             d_txt(&db, &format!("SELECT COALESCE(deleted_at, 'vivo') FROM productos WHERE uuid = '{q2_uuid}'")));
        // SPEC3 D4: el producto web que llegó ya borrado se crea borrado.
        let q2_local = d_foto(&db, &format!("SELECT deleted_at IS NOT NULL, activo FROM productos WHERE uuid = '{q2_uuid}'"));
        chk!(inf, "q2", q2_local == vec!["Integer(1)|Integer(0)|".to_string()],
             "el producto web borrado se crea aquí ya borrado ({q2_local:?})");
    }

    // ═══ i3) mismo código creado en el escritorio (sin subir) y en la web ═══
    //     (carrera real: el servidor no exige código único y la web no ve el
    //     producto del escritorio hasta el push). SPEC2 R3.2: el producto web
    //     baja como '<código>-W<8 del uuid>' y su venta espejo cuenta.
    println!("\n-- i3) mismo código en el escritorio y en la web antes de sincronizar --");
    let codigo_c = format!("E2E-DUP-{run}");
    let pd_uuid = hex32();
    {
        let c = db.lock().unwrap();
        c.execute(
            "INSERT INTO productos (uuid, codigo, nombre, precio_venta, precio_costo, stock_actual) VALUES (?1, ?2, ?3, 60, 30, 5)",
            rusqlite::params![pd_uuid, codigo_c, format!("Escritorio con código repetido E2E {run}")],
        )
        .unwrap();
    }
    let np = web
        .rpc(&tok_admin, "crear_producto", json!({ "usuario_id": 0, "producto": {
            "codigo": codigo_c, "codigo_tipo": null, "nombre": format!("Web con código repetido E2E {run}"), "descripcion": null,
            "categoria_id": null, "precio_costo": 40.0, "precio_venta": 75.0, "stock_actual": 3.0,
            "stock_minimo": 0.0, "proveedor_id": null, "foto_url": null } }))
        .await;
    chk!(inf, "i3", np.is_ok(), "crear_producto web con el código que el escritorio aún no sube: {np:?}");
    let pw = pg.filas(&format!("SELECT id, uuid FROM productos WHERE codigo = '{codigo_c}' AND uuid !~ {HEX32}"));
    if let Some(pw) = pw.first() {
        let (pw_id, pw_uuid) = (pw[0].parse::<i64>().unwrap(), pw[1].clone());
        let rv = venta_web(&web, &tok_espejo, &[(pw_id, 1.0, 75.0)]).await;
        chk!(inf, "i3", rv.is_ok(), "venta espejo del producto web: {rv:?}");
        let wi3_uuid = pg.uno(&format!("SELECT uuid FROM ventas WHERE id = {}", rv.as_ref().map(|v| v["id"].as_i64().unwrap_or(0)).unwrap_or(0))).unwrap_or_default();
        let antes_i3 = esc.periodo(&mas_segundos(&cortes::ahora_local(), 2));
        // Ciclo del worker: push (sube el producto del escritorio) → pull.
        let _ = esc.push().await;
        let _ = esc.pull().await;
        let n_srv = pg.i64(&format!("SELECT count(*) FROM productos WHERE codigo = '{codigo_c}'"));
        println!("  servidor: {n_srv} productos con código {codigo_c}");
        let esperado = format!("{codigo_c}-W{}", &pw_uuid[..8]);
        let cod_web = d_txt(&db, &format!("SELECT codigo FROM productos WHERE uuid = '{pw_uuid}'"));
        chk!(inf, "i3", cod_web.as_deref() == Some(esperado.as_str()),
             "el producto web baja con código '{esperado}' ({cod_web:?}; bandeja {:?})", esc.inbox());
        let cod_esc = d_txt(&db, &format!("SELECT codigo FROM productos WHERE uuid = '{pd_uuid}'"));
        chk!(inf, "i3", cod_esc.as_deref() == Some(codigo_c.as_str()), "el producto del escritorio conserva su código ({cod_esc:?})");
        let pw_local = d_opt_i64(&db, &format!("SELECT id FROM productos WHERE uuid = '{pw_uuid}'"));
        let det = d_foto(&db, &format!(
            "SELECT vd.producto_id FROM venta_detalle vd JOIN ventas v ON v.id = vd.venta_id WHERE v.uuid = '{wi3_uuid}'"
        ));
        chk!(inf, "i3", det.len() == 1 && pw_local.map(|l| det[0] == format!("Integer({l})|")).unwrap_or(false),
             "la venta espejo llegó con el producto web LOCAL ({det:?} vs {pw_local:?})");
        let despues_i3 = esc.periodo(&mas_segundos(&cortes::ahora_local(), 2));
        chk!(inf, "i3", despues_i3.num_transacciones == antes_i3.num_transacciones + 1
                       && cerca(despues_i3.total_ventas_efectivo - antes_i3.total_ventas_efectivo, 75.0),
             "y cuenta en el período abierto (+{} en {} venta(s))",
             despues_i3.total_ventas_efectivo - antes_i3.total_ventas_efectivo, despues_i3.num_transacciones - antes_i3.num_transacciones);
        let band = d_i64(&db, &format!("SELECT count(*) FROM sync_inbox WHERE uuid IN ('{pw_uuid}', '{pd_uuid}', '{wi3_uuid}')"));
        chk!(inf, "i3", band == 0 && esc.pendientes_outbox() == 0, "nada atorado (bandeja {band}, outbox {:?})", esc.errores_outbox());
    } else {
        chk!(inf, "i3", false, "el producto web no quedó en el servidor");
    }

    // ═══ g4) push legado (v0.1.38, otro equipo) de filas hechas en la web ═══
    //     El escritorio viejo trae FK con SUS ids; el servidor debe conservar
    //     las suyas (SPEC2 R1.1). Y el escritorio nuevo, al bajar esa fila, no
    //     borra el registrado_at local de una venta re-acomodada (SPEC2 R3.3).
    println!("\n-- g4) push legado de filas web (producto y venta espejo tardía) --");
    let dev_leg = hex32();
    let (mut dq, _) = fila_legada(&db, "productos", &q_uuid);
    let nombre_leg = format!("Producto web E2E {run} (legado)");
    dq["nombre"] = json!(nombre_leg);
    let (cat_q_local, prov_q_local) = (dq["categoria_id"].clone(), dq["proveedor_id"].clone());
    let (dv, hv) = fila_legada(&db, "ventas", &wd_uuid);
    let (u_wd_local, reg_wd) = (dv["usuario_id"].clone(), d_txt(&db, &format!("SELECT registrado_at FROM ventas WHERE uuid = '{wd_uuid}'")));
    let pids_wd_local: Vec<Value> = hv.as_ref().and_then(|h| h["venta_detalle"].as_array()).map(|a| a.iter().map(|x| x["producto_id"].clone()).collect()).unwrap_or_default();
    let srv_wd_antes = pg.filas(&format!("SELECT v.usuario_id, vd.producto_id, v.caja, v.origen FROM ventas v JOIN venta_detalle vd ON vd.venta_id = v.id WHERE v.uuid = '{wd_uuid}'"));
    println!("  locales: categoría {cat_q_local} / proveedor {prov_q_local}; venta usuario {u_wd_local}, productos {pids_wd_local:?}; registrado_at {reg_wd:?}");
    let periodo_g4 = esc.periodo(&mas_segundos(&cortes::ahora_local(), 2));
    let (st, resp) = push_legado(&web, &token_sync, &dev_leg, vec![
        json!({ "tabla": "productos", "operacion": "UPDATE", "data": dq }),
        json!({ "tabla": "ventas", "operacion": "UPDATE", "data": dv, "children": hv }),
    ])
    .await;
    chk!(inf, "g4", st == 200 && aceptado(&resp, &q_uuid) && aceptado(&resp, &wd_uuid), "push legado aceptado: {st} {resp}");
    let sq = pg.filas(&format!(
        "SELECT nombre, COALESCE(categoria_id::text, 'NULL'), COALESCE(proveedor_id::text, 'NULL') FROM productos WHERE uuid = '{q_uuid}'"
    ));
    chk!(inf, "g4", sq.first().map(|f| f[0] == nombre_leg && f[1] == frenos_srv.to_string() && f[2] == leon_srv.to_string()).unwrap_or(false),
         "producto web: se aplica el cambio pero conserva categoría {frenos_srv} y proveedor {leon_srv} del servidor \
          (el escritorio viejo mandó {cat_q_local} / {prov_q_local}): {sq:?}");
    let srv_wd = pg.filas(&format!(
        "SELECT v.usuario_id, vd.producto_id, v.caja, v.origen FROM ventas v JOIN venta_detalle vd ON vd.venta_id = v.id WHERE v.uuid = '{wd_uuid}'"
    ));
    chk!(inf, "g4", !srv_wd.is_empty() && srv_wd == srv_wd_antes && srv_wd[0][0] == vend_srv.to_string() && srv_wd[0][1] == pa.id.to_string(),
         "venta web: usuario {vend_srv} y producto {} del servidor intactos, renglón colgado de su padre ({srv_wd:?})", pa.id);
    let pend_g4 = pg.i64(&format!(
        "SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid = '{q_uuid}' OR fila_uuid = '{wd_uuid}' \
            OR fila_uuid IN (SELECT vd.uuid FROM venta_detalle vd JOIN ventas v ON v.id = vd.venta_id WHERE v.uuid = '{wd_uuid}')"
    ));
    chk!(inf, "g4", pend_g4 == 0, "sin celdas pendientes ({pend_g4})");
    let _ = esc.pull().await;
    let lq = d_foto(&db, &format!("SELECT nombre, categoria_id, proveedor_id FROM productos WHERE uuid = '{q_uuid}'"));
    chk!(inf, "g4", lq.len() == 1 && lq[0].starts_with(&format!("Text({nombre_leg:?})|Integer({cat_q_local})|Integer({prov_q_local})|")),
         "el escritorio nuevo baja el cambio con sus FK LOCALES ({lq:?})");
    let reg_wd2 = d_txt(&db, &format!("SELECT registrado_at FROM ventas WHERE uuid = '{wd_uuid}'"));
    let u_wd2 = d_i64(&db, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{wd_uuid}'"));
    chk!(inf, "g4", reg_wd2 == reg_wd && reg_wd.is_some() && json!(u_wd2) == u_wd_local,
         "la venta tardía conserva su registrado_at local ({reg_wd:?} → {reg_wd2:?}) y su vendedor local ({u_wd_local} → {u_wd2})");
    let periodo_g4b = esc.periodo(&mas_segundos(&cortes::ahora_local(), 2));
    chk!(inf, "g4", periodo_g4b.num_transacciones == periodo_g4.num_transacciones && cerca(periodo_g4b.efectivo_esperado, periodo_g4.efectivo_esperado),
         "el período abierto no cambia ({} → {})", periodo_g4.efectivo_esperado, periodo_g4b.efectivo_esperado);

    // ═══ f) tareas únicas (equipo nuevo, clon A) ═════════
    println!("\n-- f1) tareas únicas con un equipo nuevo --");
    let foto_escritorio = |db: &Db| {
        let mut v = d_foto(db, "SELECT uuid, total, usuario_id, cliente_id, caja, origen, registrado_at, anulada FROM ventas ORDER BY id");
        v.extend(d_foto(db, "SELECT uuid, venta_id, producto_id, cantidad, subtotal FROM venta_detalle ORDER BY id"));
        v.extend(d_foto(db, "SELECT uuid, monto, corte_id, usuario_id, caja FROM movimientos_caja ORDER BY id"));
        v.extend(d_foto(db, "SELECT * FROM cortes ORDER BY id"));
        v
    };
    let antes_f = foto_escritorio(&db);
    let rep = tareas::subir_mapa_ids(&db, &esc.client, &device).await;
    chk!(inf, "f1", rep.is_ok(), "subida del mapa de ids: {:?}", rep.as_ref().err().map(|e| e.mensaje()));
    if let Ok(rep) = &rep {
        let u = rep.tablas.iter().find(|t| t.tabla == "usuarios");
        chk!(inf, "f1", u.map(|t| t.enviados >= 5 && t.recibidos == t.enviados).unwrap_or(false), "usuarios en el mapa: {u:?}");
    }
    let legada = || pg.filas(&format!(
        "SELECT v.usuario_id, vd.producto_id, COALESCE(e.estado,''), COALESCE(v.cliente_id::text, 'NULL'), \
                COALESCE(vd.autorizado_por::text, 'NULL') FROM ventas v JOIN venta_detalle vd ON vd.venta_id = v.id \
           LEFT JOIN sync_fk_estado e ON e.tabla = 'ventas' AND e.uuid = v.uuid \
          WHERE v.uuid = '{vg_uuid}'"
    ));
    let g = legada();
    chk!(inf, "f1", g.first().map(|g| g[0] == "2" && g[3] == "1" && g[2].is_empty()).unwrap_or(false),
         "subir el mapa no toca la venta legada (cruda, sin estado) hasta la reparación ({g:?})");
    let mariana_srv = srv_usuario(&usuarios[2].1);
    let cli_local = d_opt_i64(&db, &format!("SELECT id FROM clientes WHERE uuid = '{cli_uuid}'"));
    // En la clon A hay ventas del escritorio de OTROS equipos (la tienda real)
    // sin reparar: la verificación debe limitarse a las de ESTE equipo (SPEC2 R1.5b).
    let otras = pg.i64(&format!(
        "SELECT count(*) FROM ventas v WHERE v.uuid ~ {HEX32} \
            AND NOT EXISTS (SELECT 1 FROM sync_fk_estado e WHERE e.tabla = 'ventas' AND e.uuid = v.uuid)"
    ));
    let rs = tareas::reparar_servidor(&esc.client, &device).await;
    match &rs {
        Ok(v) => {
            chk!(inf, "f1", v["ok"] == json!(true),
                 "reparación del servidor ok para un segundo equipo aunque haya {otras} ventas de otro equipo sin reparar: {}", v["verificacion"]);
            // Lo único de este equipo que no llegó por v2 es la venta legada:
            // usuario_id 2→Nancy, cliente_id 1→cliente de h2, producto_id 1→P_A,
            // autorizado_por 3→Mariana.
            chk!(inf, "f1", v["celdas_cambiadas"].as_i64() == Some(4), "4 celdas (solo la venta legada): {}", v["celdas_cambiadas"]);
            let g = legada();
            chk!(inf, "f1", g.first().map(|g| g[0] == nancy_srv.to_string() && g[1] == pa.id.to_string() && g[2] == "reparado"
                                             && g[3] == cli_srv_id.to_string() && g[4] == mariana_srv.to_string()).unwrap_or(false),
                 "la reparación traduce la venta legada: Nancy ({nancy_srv}), cliente {cli_srv_id} (local {cli_local:?}), producto {}, \
                  autorizó Mariana ({mariana_srv}) y estado 'reparado' ({g:?})", pa.id);
        }
        Err(tareas::ErrorTarea::Rechazado(v)) => {
            chk!(inf, "f1", false,
                 "POST /sync/reparar de un segundo equipo rechazado (¿verificación global?): {}", v["verificacion"]["fallas"]);
        }
        Err(e) => chk!(inf, "f1", false, "reparar: {}", e.mensaje()),
    }
    let espejo = tareas::arreglo_espejo(&db, &esc.client).await;
    chk!(inf, "f1", espejo.as_ref().map(|r| r.celdas_cambiadas == 0 && r.errores.is_empty()).unwrap_or(false),
         "arreglo espejo: 0 celdas (las filas web llegaron con __refs): {:?}", espejo.as_ref().map(|r| (r.filas, r.celdas_cambiadas, r.sin_resolver, r.errores.clone())).map_err(|e| e.mensaje()));
    let espejo2 = tareas::arreglo_espejo(&db, &esc.client).await;
    chk!(inf, "f1", espejo2.as_ref().map(|r| r.celdas_cambiadas == 0).unwrap_or(false), "segunda corrida del espejo: 0 celdas");
    chk!(inf, "f1", foto_escritorio(&db) == antes_f, "filas del escritorio intactas (montos, corte_id, FK)");
    if var("E2E_REPLAY_COMPLETO").is_some() {
        println!("  re-descarga completa (equipo nuevo, desde el cursor 0)…");
        {
            let c = db.lock().unwrap();
            state::marcar_referencias_reparadas(&c).unwrap();
        }
        let periodo_antes = esc.periodo(&cortes::ahora_local());
        let t0 = std::time::Instant::now();
        for _ in 0..2000 {
            let cfg = esc.cfg();
            if let Err(e) = tareas::correr_replay(&db, &esc.client, &cfg).await {
                chk!(inf, "f1", false, "replay: {e}");
                break;
            }
            if !state::leer_tareas(&db.lock().unwrap()).unwrap().replay_pendiente() {
                break;
            }
        }
        println!("  re-descarga: {:?}", t0.elapsed());
        let periodo_despues = esc.periodo(&cortes::ahora_local());
        chk!(inf, "f1", periodo_despues.num_transacciones == periodo_antes.num_transacciones
                       && cerca(periodo_despues.efectivo_esperado, periodo_antes.efectivo_esperado),
             "la re-descarga no mete historia al período abierto ({} → {})", periodo_antes.efectivo_esperado, periodo_despues.efectivo_esperado);
        let motivos = d_foto(&db, "SELECT motivo, tabla, count(*) FROM sync_inbox GROUP BY 1, 2 ORDER BY 1, 2");
        println!("  bandeja tras la re-descarga completa: {motivos:?}");
        // SPEC3 D4/D5: ni los productos de escritorio con código repetido en
        // el servidor ni las ventas de productos web borrados se atoran.
        let atorados = d_foto(&db, "SELECT tabla, uuid, COALESCE(error, '') FROM sync_inbox WHERE motivo IN ('error', 'sin_fk') ORDER BY id");
        chk!(inf, "f1", atorados.is_empty(), "re-descarga completa en un equipo nuevo: nada atorado en la bandeja: {atorados:?}");
        // Pares vivos del servidor con el mismo código: uno guarda el código y
        // el otro queda como '<código>-D<8 del uuid>'.
        let pares = pg.filas(&format!(
            "SELECT p.uuid, p.codigo FROM productos p WHERE p.uuid ~ {HEX32} AND p.deleted_at IS NULL \
               AND EXISTS (SELECT 1 FROM productos q WHERE q.codigo = p.codigo AND q.id <> p.id \
                             AND q.uuid ~ {HEX32} AND q.deleted_at IS NULL) ORDER BY p.codigo, p.id"
        ));
        let mut con_d = 0usize;
        let mut mal_pares = Vec::new();
        for f in &pares {
            let local = d_txt(&db, &format!("SELECT codigo FROM productos WHERE uuid = '{}'", f[0]));
            let con_sufijo = format!("{}-D{}", f[1], &f[0][..8]);
            match local {
                Some(c) if c == con_sufijo => con_d += 1,
                Some(c) if c == f[1] => {}
                otro => mal_pares.push(format!("{} {} → {otro:?}", f[0], f[1])),
            }
        }
        chk!(inf, "f1", mal_pares.is_empty() && (pares.is_empty() || con_d > 0),
             "{} productos de escritorio con código repetido en el servidor: {con_d} guardados como '<código>-D<8>'; mal: {mal_pares:?}",
             pares.len());
    }

    // ═══ g5) push legado de una venta del ESCRITORIO que ya está traducida ═══
    //     (SPEC2 R1.1: FK por el mapa del equipo; sin entrada → se conserva
    //     lo guardado, nunca NULL/0 ni pendiente).
    println!("\n-- g5) push legado de una venta del escritorio ya traducida --");
    let (mut dva, hva) = fila_legada(&db, "ventas", &va_uuid);
    dva["usuario_id"] = json!(2); // en el mapa (tras f1): Nancy
    dva["cliente_id"] = json!(424_242); // sin entrada en el mapa
    let mut hva = hva.unwrap_or(json!({}));
    if let Some(arr) = hva["venta_detalle"].as_array_mut() {
        for h in arr {
            h["autorizado_por"] = json!(99); // sin entrada en el mapa
        }
    }
    let est_va = || pg.uno(&format!("SELECT estado FROM sync_fk_estado WHERE tabla = 'ventas' AND uuid = '{va_uuid}'"));
    let est_antes = est_va();
    let (st, resp) = push_legado(&web, &token_sync, &device, vec![
        json!({ "tabla": "ventas", "operacion": "UPDATE", "data": dva, "children": hva }),
    ])
    .await;
    chk!(inf, "g5", st == 200 && aceptado(&resp, &va_uuid), "push legado aceptado: {st} {resp}");
    let sv = pg.filas(&format!(
        "SELECT v.usuario_id, COALESCE(v.cliente_id::text, 'NULL') FROM ventas v WHERE v.uuid = '{va_uuid}'"
    ));
    chk!(inf, "g5", sv.first().map(|f| f[0] == nancy_srv.to_string() && f[1] == "NULL").unwrap_or(false),
         "usuario_id 2 → Nancy del servidor ({nancy_srv}) por el mapa; cliente_id sin mapa conserva lo guardado (NULL): {sv:?}");
    let sd: BTreeSet<String> = pg
        .filas(&format!(
            "SELECT vd.producto_id || '/' || COALESCE(vd.autorizado_por::text, 'NULL') FROM venta_detalle vd \
               JOIN ventas v ON v.id = vd.venta_id WHERE v.uuid = '{va_uuid}'"
        ))
        .into_iter()
        .map(|f| f[0].clone())
        .collect();
    chk!(inf, "g5", sd == BTreeSet::from([format!("{}/NULL", pa.id), format!("{p3_srv_id}/NULL")]),
         "renglones: productos del servidor por el mapa ({} y {p3_srv_id}), autorizado_por sin mapa conserva NULL: {sd:?}", pa.id);
    let pend_g5 = pg.i64(&format!(
        "SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid = '{va_uuid}' \
            OR fila_uuid IN (SELECT vd.uuid FROM venta_detalle vd JOIN ventas v ON v.id = vd.venta_id WHERE v.uuid = '{va_uuid}')"
    ));
    chk!(inf, "g5", pend_g5 == 0 && est_va() == est_antes, "sin pendientes ({pend_g5}) y estado igual ({est_antes:?} → {:?})", est_va());

    // ═══ r) push v2 con una referencia que el servidor aún no conoce, en una
    //     fila que YA existe (SPEC2 R1.2): se conserva lo guardado; el
    //     reintento corrige la celda solo si nadie la cambió (compara-e-intercambia).
    println!("\n-- r) celda pendiente en una fila existente --");
    let frenos_l = frenos_local.unwrap_or(1);
    let p5_uuid = hex32();
    let p5_local: i64 = {
        let c = db.lock().unwrap();
        c.execute(
            "INSERT INTO productos (uuid, codigo, nombre, precio_venta, precio_costo, stock_actual, categoria_id) \
             VALUES (?1, ?2, ?3, 50, 20, 5, ?4)",
            rusqlite::params![p5_uuid, format!("E2E-R-{run}"), format!("Producto pendiente E2E {run}"), frenos_l],
        )
        .unwrap();
        c.last_insert_rowid()
    };
    let _ = esc.push().await;
    let srv_cat = || pg.uno(&format!("SELECT COALESCE(categoria_id::text, 'NULL') FROM productos WHERE uuid = '{p5_uuid}'"));
    let est_p5 = || pg.uno(&format!("SELECT estado FROM sync_fk_estado WHERE tabla = 'productos' AND uuid = '{p5_uuid}'"));
    chk!(inf, "r", srv_cat().as_deref() == Some(frenos_srv.to_string().as_str()), "P5 sube con Frenos del servidor ({:?})", srv_cat());
    // Categoría local que no subió (renglón del outbox que falló, p. ej.).
    let nueva_cat = |nombre: String| -> (String, i64) {
        let u = hex32();
        let c = db.lock().unwrap();
        c.execute("INSERT INTO categorias (nombre, uuid) VALUES (?1, ?2)", rusqlite::params![nombre, u]).unwrap();
        let id = c.last_insert_rowid();
        limpiar_outbox(&c);
        (u, id)
    };
    let (cat_uuid, cat_local) = nueva_cat(format!("E2E cat {run}"));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    db.lock().unwrap().execute("UPDATE productos SET categoria_id = ?1 WHERE id = ?2", [cat_local, p5_local]).unwrap();
    let _ = esc.push().await;
    let pend = pg.filas(&format!(
        "SELECT ref_uuid, COALESCE(valor_puesto::text, 'NULL') FROM sync_fk_pendiente WHERE fila_uuid = '{p5_uuid}' AND columna = 'categoria_id'"
    ));
    chk!(inf, "r", srv_cat().as_deref() == Some(frenos_srv.to_string().as_str())
                   && pend == vec![vec![cat_uuid.clone(), frenos_srv.to_string()]]
                   && est_p5().as_deref() == Some("pendiente"),
         "referencia desconocida en fila existente: conserva {frenos_srv} (no NULL), pendiente con valor_puesto ({:?}, {pend:?}, estado {:?})",
         srv_cat(), est_p5());
    // La categoría sube después: el reintento corrige la celda.
    db.lock().unwrap().execute("UPDATE categorias SET descripcion = 'sube' WHERE uuid = ?1", [&cat_uuid]).unwrap();
    let _ = esc.push().await;
    let cat_srv = pg.uno(&format!("SELECT id FROM categorias WHERE uuid = '{cat_uuid}'"));
    let pend_n = pg.i64(&format!("SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid = '{p5_uuid}'"));
    chk!(inf, "r", cat_srv.is_some() && srv_cat() == cat_srv && pend_n == 0 && est_p5().as_deref() == Some("traducido"),
         "al llegar la categoría ({cat_srv:?}) la celda se corrige ({:?}), sin pendientes ({pend_n}), estado {:?}", srv_cat(), est_p5());
    // Compara-e-intercambia: la web cambia la celda mientras está pendiente.
    let (cat2_uuid, cat2_local) = nueva_cat(format!("E2E cat2 {run}"));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    db.lock().unwrap().execute("UPDATE productos SET categoria_id = ?1 WHERE id = ?2", [cat2_local, p5_local]).unwrap();
    let _ = esc.push().await;
    let puesto = pg.uno(&format!("SELECT COALESCE(valor_puesto::text, 'NULL') FROM sync_fk_pendiente WHERE fila_uuid = '{p5_uuid}'"));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let p5_srv_id = pg.i64(&format!("SELECT id FROM productos WHERE uuid = '{p5_uuid}'"));
    let ed = web
        .rpc(&tok_admin, "actualizar_producto", json!({ "usuario_id": 0, "producto": {
            "id": p5_srv_id, "codigo": format!("E2E-R-{run}"), "nombre": format!("Producto pendiente E2E {run}"), "descripcion": null,
            "categoria_id": frenos_srv, "precio_costo": 20.0, "precio_venta": 50.0, "stock_minimo": 0.0,
            "proveedor_id": null, "foto_url": null } }))
        .await;
    chk!(inf, "r", ed.is_ok() && puesto == cat_srv, "la web pone Frenos mientras la celda espera (valor_puesto {puesto:?}): {ed:?}");
    db.lock().unwrap().execute("UPDATE categorias SET descripcion = 'sube' WHERE uuid = ?1", [&cat2_uuid]).unwrap();
    let _ = esc.push().await;
    let pend_n = pg.i64(&format!("SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid = '{p5_uuid}'"));
    chk!(inf, "r", srv_cat().as_deref() == Some(frenos_srv.to_string().as_str()) && pend_n == 0,
         "el reintento NO pisa la edición web (servidor {:?} = Frenos {frenos_srv}); el pendiente viejo se borra ({pend_n})", srv_cat());
    let _ = esc.pull().await;
    let _ = inbox::reintentar(&db, &esc.client).await;
    let cat_l = d_opt_i64(&db, &format!("SELECT categoria_id FROM productos WHERE uuid = '{p5_uuid}'"));
    if cat_l != Some(frenos_l) {
        inf.nota("r", format!(
            "tras el compara-e-intercambia el escritorio sigue con su categoría ({cat_l:?}; Frenos local {frenos_l}): la edición web \
             llegó mientras la fila estaba 'pendiente' (sin __refs) y nada vuelve a publicarla"
        ));
    }

    // ═══ r1) la respuesta de un push se pierde (SPEC3 S1) ═══
    //     El servidor aplica el push pero el escritorio no se entera (504,
    //     tiempo de espera, internet caído): el renglón sigue en el outbox, la
    //     base no avanza y el siguiente ciclo reenvía EXACTAMENTE lo mismo. La
    //     existencia viaja como diferencia: aplicarlo otra vez restaría la
    //     misma venta dos veces.
    println!("\n-- r1) respuesta del push perdida: el reenvío idéntico no descuenta dos veces --");
    let proxy = Proxy::arrancar(&web.url).await;
    let esc_p = Escritorio { db: db.clone(), client: RemoteClient::new(&proxy.url, &token_sync).unwrap() };
    let stock_srv = |id: i64| pg.f64(&format!("SELECT stock_actual::float8 FROM productos WHERE id = {id}"));
    let precio_srv = |id: i64| pg.f64(&format!("SELECT precio_venta::float8 FROM productos WHERE id = {id}"));
    let stock_loc = |id: i64| d_f64(&db, &format!("SELECT stock_actual FROM productos WHERE id = {id}"));
    let precio_loc = |id: i64| d_f64(&db, &format!("SELECT precio_venta FROM productos WHERE id = {id}"));
    let cursores = |u: &str| pg.i64(&format!("SELECT count(*) FROM sync_cursor WHERE tabla = 'productos' AND uuid = '{u}'"));
    let ultimo_cursor = |u: &str| pg.uno(&format!(
        "SELECT origen_device FROM sync_cursor WHERE tabla = 'productos' AND uuid = '{u}' ORDER BY id DESC LIMIT 1"
    ));
    let reenvios_srv = |u: &str| lineas_bitacora_srv(&["reenvío idéntico ya aplicado", u]);
    let en_bandeja = |u: &str| d_i64(&db, &format!("SELECT count(*) FROM sync_inbox WHERE uuid = '{u}'"));
    // Existencia de sobra para r1–r3 (la fusión nunca deja el stock < 0 y un
    // tope en 0 escondería una resta de más).
    esc.asentar().await;
    for (id, nombre) in [(pa.id, "P_A"), (pb.id, "P_B")] {
        let nuevo = stock_srv(id) + 20.0;
        let rr = web
            .rpc(&tok_admin, "ajustar_stock", json!({ "productoId": id, "nuevoStock": nuevo, "motivo": "existencia e2e r1-r3", "usuarioId": 0 }))
            .await;
        chk!(inf, "r1", rr.is_ok(), "ajuste web de {nombre} a {nuevo}: {rr:?}");
    }
    tokio::time::sleep(Duration::from_millis(1100)).await;
    esc.asentar().await;

    // r1a) fusión de 3 vías (venta web + venta del escritorio) cuya respuesta se pierde.
    let s0 = stock_srv(pa.id);
    chk!(inf, "r1", cerca(stock_loc(1), s0), "P_A igual en los dos lados al empezar (escritorio {} / servidor {s0})", stock_loc(1));
    let rv = venta_web(&web, &tok_espejo, &[(pa.id, 1.0, precio_srv(pa.id))]).await;
    chk!(inf, "r1", rv.is_ok(), "venta web de P_A: {rv:?}");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    venta_escritorio(&db, 1, 2, &cortes::ahora_local());
    let reenvios0 = reenvios_srv(&pa.uuid);
    proxy.perder_siguiente_push();
    let r = esc_p.push().await;
    let s_fusion = stock_srv(pa.id);
    let cur = ultimo_cursor(&pa.uuid);
    chk!(inf, "r1", r.is_err() && cerca(s_fusion, s0 - 2.0) && cur.as_deref() == Some("merge"),
         "el servidor fusiona (web −1 y escritorio −1: {s0} → {s_fusion}, cursor {cur:?}) pero la respuesta se pierde ({r:?})");
    chk!(inf, "r1", esc.pendientes_outbox() > 0 && !esc.errores_outbox().is_empty(),
         "el escritorio lo da por fallido: el renglón sigue en el outbox con error ({:?})", esc.errores_outbox());
    // El mismo ciclo sigue con el pull: la fila fusionada espera en la bandeja
    // (hay un cambio local sin subir).
    let _ = esc.pull().await;
    chk!(inf, "r1", cerca(stock_loc(1), s0 - 1.0),
         "el pull no pisa el cambio local que no se confirmó (escritorio {}; bandeja {:?})", stock_loc(1), esc.inbox());
    let n_cur = cursores(&pa.uuid);
    // Siguiente ciclo: reenvía la misma fila con la misma base.
    let r = esc.push().await;
    let s_re = stock_srv(pa.id);
    chk!(inf, "r1", r.is_ok() && cerca(s_re, s0 - 2.0) && cursores(&pa.uuid) == n_cur,
         "reenvío idéntico tras la fusión: el servidor NO descuenta otra vez (stock {s_re}, esperado {}; cursores {n_cur} → {}; {r:?})",
         s0 - 2.0, cursores(&pa.uuid));
    match (reenvios0, reenvios_srv(&pa.uuid)) {
        (Some(a), Some(b)) => chk!(inf, "r1", b == a + 1, "el servidor lo reconoció como reenvío ya aplicado (bitácora {a} → {b})"),
        _ => inf.nota("r1", "sin E2E_SRV_LOG: no se revisó la bitácora del servidor".into()),
    }
    let _ = esc.pull().await;
    let ri = inbox::reintentar(&db, &esc.client).await;
    chk!(inf, "r1", cerca(stock_loc(1), s0 - 2.0) && cerca(stock_srv(pa.id), s0 - 2.0)
                   && esc.pendientes_outbox() == 0 && en_bandeja(&pa.uuid) == 0,
         "el escritorio converge con el servidor: {} / {} (esperado {}); outbox {:?}; bandeja {ri:?} {:?}",
         stock_loc(1), stock_srv(pa.id), s0 - 2.0, esc.errores_outbox(), esc.inbox());

    // r1b) push normal (sin cambio del servidor) cuya respuesta se pierde, y
    //      una venta web que entra ANTES del reintento.
    esc.asentar().await;
    let s0 = stock_srv(pa.id);
    chk!(inf, "r1", cerca(stock_loc(1), s0), "P_A igual en los dos lados ({} / {s0})", stock_loc(1));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    venta_escritorio(&db, 1, 2, &cortes::ahora_local());
    let reenvios0 = reenvios_srv(&pa.uuid);
    proxy.perder_siguiente_push();
    let r = esc_p.push().await;
    let s_lww = stock_srv(pa.id);
    let cur = ultimo_cursor(&pa.uuid);
    chk!(inf, "r1", r.is_err() && cerca(s_lww, s0 - 1.0) && cur.as_deref() == Some(device.as_str()),
         "el servidor aplica la venta del escritorio ({s0} → {s_lww}, cursor de este equipo: {}) pero la respuesta se pierde ({r:?})",
         cur.as_deref() == Some(device.as_str()));
    let _ = esc.pull().await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let rv = venta_web(&web, &tok_espejo, &[(pa.id, 1.0, precio_srv(pa.id))]).await;
    chk!(inf, "r1", rv.is_ok() && cerca(stock_srv(pa.id), s0 - 2.0), "venta web antes del reintento ({}): {rv:?}", stock_srv(pa.id));
    let r = esc.push().await;
    let s_re = stock_srv(pa.id);
    chk!(inf, "r1", r.is_ok() && cerca(s_re, s0 - 2.0),
         "reintento tras un cambio web: el servidor NO resta otra vez la venta del escritorio (stock {s_re}, esperado {}; {r:?})", s0 - 2.0);
    match (reenvios0, reenvios_srv(&pa.uuid)) {
        (Some(a), Some(b)) => chk!(inf, "r1", b == a + 1, "reconocido como reenvío ya aplicado (bitácora {a} → {b})"),
        _ => {}
    }
    let _ = esc.pull().await;
    let ri = inbox::reintentar(&db, &esc.client).await;
    chk!(inf, "r1", cerca(stock_loc(1), s0 - 2.0) && esc.pendientes_outbox() == 0 && en_bandeja(&pa.uuid) == 0,
         "el escritorio baja la venta web: {} (esperado {}); outbox {:?}; bandeja {ri:?} {:?}",
         stock_loc(1), s0 - 2.0, esc.errores_outbox(), esc.inbox());
    // Y la base quedó bien: la siguiente venta del escritorio resta exactamente 1.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    venta_escritorio(&db, 1, 2, &cortes::ahora_local());
    esc.asentar().await;
    chk!(inf, "r1", cerca(stock_srv(pa.id), s0 - 3.0) && cerca(stock_loc(1), s0 - 3.0),
         "la venta siguiente resta solo 1 en los dos lados (servidor {}, escritorio {}, esperado {})",
         stock_srv(pa.id), stock_loc(1), s0 - 3.0);

    // r1c/r1d) la respuesta se pierde y, ANTES del reintento, el escritorio
    //      vende otra pieza: el reintento ya no es idéntico (otra existencia)
    //      y sigue llevando la base vieja (la del push que sí se aplicó).
    //      c: el push perdido fue una fusión (venta web antes);
    //      d: el push perdido fue normal y la web vende antes del reintento.
    for (caso, web_antes) in [("r1c", true), ("r1d", false)] {
        esc.asentar().await;
        let s0 = stock_srv(pa.id);
        chk!(inf, caso, cerca(stock_loc(1), s0), "P_A igual en los dos lados ({} / {s0})", stock_loc(1));
        if web_antes {
            let rv = venta_web(&web, &tok_espejo, &[(pa.id, 1.0, precio_srv(pa.id))]).await;
            chk!(inf, caso, rv.is_ok(), "venta web de P_A antes del push: {rv:?}");
        }
        tokio::time::sleep(Duration::from_millis(1100)).await;
        venta_escritorio(&db, 1, 2, &cortes::ahora_local());
        proxy.perder_siguiente_push();
        let r = esc_p.push().await;
        let s_ap = stock_srv(pa.id);
        chk!(inf, caso, r.is_err() && cerca(s_ap, s0 - 2.0 + if web_antes { 0.0 } else { 1.0 }),
             "el servidor aplica la venta del escritorio ({s0} → {s_ap}; cursor {:?}) pero la respuesta se pierde",
             ultimo_cursor(&pa.uuid));
        let _ = esc.pull().await;
        if !web_antes {
            tokio::time::sleep(Duration::from_millis(1100)).await;
            let rv = venta_web(&web, &tok_espejo, &[(pa.id, 1.0, precio_srv(pa.id))]).await;
            chk!(inf, caso, rv.is_ok(), "venta web de P_A antes del reintento: {rv:?}");
        }
        // Otra venta en el escritorio antes del siguiente ciclo.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        venta_escritorio(&db, 1, 2, &cortes::ahora_local());
        esc.asentar().await;
        let (ss, sl) = (stock_srv(pa.id), stock_loc(1));
        chk!(inf, caso, cerca(ss, s0 - 3.0) && cerca(sl, s0 - 3.0),
             "respuesta perdida + otra venta local antes del reintento ({}): 2 ventas del escritorio y 1 web = −3; \
              servidor {ss}, escritorio {sl} (esperado {}; cursor {:?})",
             if web_antes { "push perdido = fusión" } else { "venta web antes del reintento" }, s0 - 3.0, ultimo_cursor(&pa.uuid));
        // Que el siguiente caso arranque parejo aunque este haya fallado.
        if !(cerca(ss, s0 - 3.0) && cerca(sl, s0 - 3.0)) {
            let rr = web
                .rpc(&tok_admin, "ajustar_stock", json!({ "productoId": pa.id, "nuevoStock": s0 - 3.0, "motivo": "e2e r1c/r1d", "usuarioId": 0 }))
                .await;
            tokio::time::sleep(Duration::from_millis(1100)).await;
            esc.asentar().await;
            inf.nota(caso, format!("existencia de P_A re-ajustada en la web a {} para seguir ({rr:?}; escritorio {})", s0 - 3.0, stock_loc(1)));
        }
    }

    // ═══ r2) reloj del escritorio adelantado + venta y precio web entre dos
    //     push (SPEC3 S2/D1): la fusión se decide por contenido. ═══
    for adelanto in [300i64, 30] {
        println!("\n-- r2) reloj del escritorio {adelanto} s adelantado; venta y precio web entre dos push --");
        esc.asentar().await;
        let s0 = stock_srv(pb.id);
        let precio0 = precio_srv(pb.id);
        chk!(inf, "r2", cerca(stock_loc(2), s0) && cerca(precio_loc(2), precio0),
             "[{adelanto} s] P_B igual en los dos lados al empezar (stock {} / {s0}, precio {} / {precio0})", stock_loc(2), precio_loc(2));
        let adelantada = || mas_segundos(&cortes::ahora_local(), adelanto);
        // Ciclo 1: venta del escritorio con la hora adelantada → push → pull.
        let t = adelantada();
        let v = venta_escritorio(&db, 2, 3, &t);
        marcar_hora_local(&db, 2, v, &t);
        let m_pre = d_txt(&db, "SELECT updated_at FROM productos WHERE id = 2").unwrap_or_default();
        let r = esc.push().await;
        let m_srv = pg.uno(&format!("SELECT updated_at FROM productos WHERE id = {}", pb.id)).unwrap_or_default();
        let m_loc = d_txt(&db, "SELECT updated_at FROM productos WHERE id = 2").unwrap_or_default();
        let m_base = d_txt(&db, &format!("SELECT updated_at FROM sync_base WHERE tabla = 'productos' AND uuid = '{}'", pb.uuid));
        chk!(inf, "r2", r.is_ok() && m_pre == t && cerca(stock_srv(pb.id), s0 - 1.0)
                       && m_srv.as_str() <= mas_segundos(&cortes::ahora_local(), 1).as_str()
                       && m_loc == m_srv && m_base.as_deref() == Some(m_srv.as_str()),
             "[{adelanto} s] ciclo 1: el escritorio sube su marca adelantada ({m_pre}), el servidor la recorta a {m_srv}; la fila local \
              ({m_loc}) y la base ({m_base:?}) toman la del servidor");
        let _ = esc.pull().await;
        // Entre los dos push: la web vende 1 y sube el precio.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let rv = venta_web(&web, &tok_espejo, &[(pb.id, 1.0, precio0)]).await;
        let precio_w = r2(precio0 + 10.0);
        let ed = editar_precio_web(&web, &pg, &tok_admin, pb.id, precio_w).await;
        chk!(inf, "r2", rv.is_ok() && ed.is_ok(), "[{adelanto} s] web: venta de P_B y precio {precio0} → {precio_w} ({rv:?} / {ed:?})");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        // Ciclo 2: otra venta del escritorio (aún con el precio viejo) → push → pull.
        let t = adelantada();
        let v = venta_escritorio(&db, 2, 3, &t);
        marcar_hora_local(&db, 2, v, &t);
        let m_pre = d_txt(&db, "SELECT updated_at FROM productos WHERE id = 2").unwrap_or_default();
        chk!(inf, "r2", m_pre == t, "[{adelanto} s] ciclo 2: la fila local lleva la marca adelantada ({m_pre} = {t})");
        let _ = esc.push().await;
        let cur = ultimo_cursor(&pb.uuid);
        let _ = esc.pull().await;
        let ri = inbox::reintentar(&db, &esc.client).await;
        let (ss, ps, sl, pl) = (stock_srv(pb.id), precio_srv(pb.id), stock_loc(2), precio_loc(2));
        chk!(inf, "r2", cerca(ss, s0 - 3.0) && cerca(sl, ss) && cerca(ps, precio_w) && cerca(pl, precio_w),
             "[{adelanto} s] las dos ventas del escritorio y la web cuentan y el precio web se queda: stock servidor {ss} / escritorio {sl} \
              (esperado {}), precio servidor {ps} / escritorio {pl} (web {precio_w}); cursor {cur:?}; bandeja {ri:?} {:?}",
             s0 - 3.0, esc.inbox());
        chk!(inf, "r2", cur.as_deref() == Some("merge"), "[{adelanto} s] el servidor detectó el cambio web por contenido (cursor {cur:?})");
        // Ciclo 3: una venta más (hora adelantada) no descuenta de más ni
        // regresa el precio viejo.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let t = adelantada();
        let v = venta_escritorio(&db, 2, 3, &t);
        marcar_hora_local(&db, 2, v, &t);
        esc.asentar().await;
        let (ss, ps, sl, pl) = (stock_srv(pb.id), precio_srv(pb.id), stock_loc(2), precio_loc(2));
        chk!(inf, "r2", cerca(ss, s0 - 4.0) && cerca(sl, ss) && cerca(ps, precio_w) && cerca(pl, precio_w),
             "[{adelanto} s] la venta siguiente resta 1 y el precio web sigue: stock {ss} / {sl} (esperado {}), precio {ps} / {pl}",
             s0 - 4.0);
        chk!(inf, "r2", esc.pendientes_outbox() == 0 && en_bandeja(&pb.uuid) == 0,
             "[{adelanto} s] outbox y bandeja limpios ({:?} / {:?})", esc.errores_outbox(), esc.inbox());
    }

    // ═══ r3) "Forzar sincronización" mientras corre un ciclo (SPEC3 D2) ═══
    //     Un solo ciclo a la vez: el forzado espera al que está en curso y no
    //     vuelve a empujar lo que ese ya subió.
    println!("\n-- r3) sincronización forzada durante un ciclo en curso --");
    esc.asentar().await;
    // El ciclo completo (worker::ciclo) lee la URL de sync_state: va por el
    // proxy (cuenta y demora los push). Las tareas únicas (mapa, reparación,
    // re-descarga) ya se probaron en f1: aquí se dan por hechas.
    {
        let c = db.lock().unwrap();
        c.execute(
            "UPDATE sync_state SET remote_url = ?1, \
                    referencias_reparadas_at = COALESCE(referencias_reparadas_at, datetime('now','localtime')), \
                    replay_cursor = COALESCE(replay_hasta, '0'), replay_hasta = COALESCE(replay_hasta, '0') \
              WHERE id = 1",
            [&proxy.url],
        )
        .unwrap();
    }
    let s0 = stock_srv(pa.id);
    chk!(inf, "r3", cerca(stock_loc(1), s0), "P_A igual en los dos lados ({} / {s0})", stock_loc(1));
    let rv = venta_web(&web, &tok_espejo, &[(pa.id, 1.0, precio_srv(pa.id))]).await;
    chk!(inf, "r3", rv.is_ok(), "venta web de P_A: {rv:?}");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    venta_escritorio(&db, 1, 2, &cortes::ahora_local());
    let reenvios0 = reenvios_srv(&pa.uuid);
    proxy.reiniciar_cuenta();
    proxy.demorar_push(1500);
    // Ciclo en curso (su push tarda 1.5 s) y, 0.3 s después, el botón.
    let (ra, (rb, espera_b)) = tokio::join!(
        worker::forzar_ciclo_con_espera(db.clone(), Duration::from_secs(30)),
        async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let t = std::time::Instant::now();
            let r = worker::forzar_ciclo_con_espera(db.clone(), Duration::from_secs(30)).await;
            (r, t.elapsed())
        }
    );
    proxy.demorar_push(0);
    let n_pa = proxy.pushes_de("productos", &pa.uuid);
    chk!(inf, "r3", ra.is_ok() && rb.is_ok(), "los dos ciclos terminan bien ({ra:?} / {rb:?})");
    chk!(inf, "r3", espera_b >= Duration::from_millis(1000),
         "el forzado esperó a que terminara el ciclo en curso ({espera_b:?}; el push en vuelo tardaba 1.5 s)");
    chk!(inf, "r3", n_pa == 1, "el cambio de P_A viajó en UN solo push ({n_pa}; push en total {})", proxy.total_pushes());
    if let (Some(a), Some(b)) = (reenvios0, reenvios_srv(&pa.uuid)) {
        chk!(inf, "r3", a == b, "al servidor no llegó ningún reenvío de P_A (bitácora {a} → {b})");
    }
    chk!(inf, "r3", cerca(stock_srv(pa.id), s0 - 2.0) && cerca(stock_loc(1), s0 - 2.0) && esc.pendientes_outbox() == 0,
         "venta web + venta del escritorio: {s0} → servidor {} / escritorio {} (esperado {}); outbox {:?}",
         stock_srv(pa.id), stock_loc(1), s0 - 2.0, esc.errores_outbox());
    // Control (sin el turno): dos push crudos a la vez SÍ mandan el mismo
    // cambio dos veces (la cuenta del proxy lo ve) y la huella del servidor
    // absorbe el segundo: la existencia queda bien de todos modos.
    let s1 = stock_srv(pa.id);
    let rv = venta_web(&web, &tok_espejo, &[(pa.id, 1.0, precio_srv(pa.id))]).await;
    chk!(inf, "r3", rv.is_ok(), "control: venta web de P_A: {rv:?}");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    venta_escritorio(&db, 1, 2, &cortes::ahora_local());
    let reenvios1 = reenvios_srv(&pa.uuid);
    proxy.reiniciar_cuenta();
    proxy.demorar_push(1500);
    let (ra, rb) = tokio::join!(esc_p.push(), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        esc_p.push().await
    });
    proxy.demorar_push(0);
    let n_pa = proxy.pushes_de("productos", &pa.uuid);
    let reenvios2 = reenvios_srv(&pa.uuid);
    esc.asentar().await;
    chk!(inf, "r3", ra.is_ok() && rb.is_ok() && n_pa == 2
                   && reenvios1.zip(reenvios2).map(|(a, b)| b == a + 1).unwrap_or(true)
                   && cerca(stock_srv(pa.id), s1 - 2.0) && cerca(stock_loc(1), s1 - 2.0),
         "control sin turno: P_A viajó {n_pa} veces, el servidor reconoció el reenvío (bitácora {reenvios1:?} → {reenvios2:?}) y la \
          existencia queda {} / {} (esperado {}) ({ra:?} / {rb:?})",
         stock_srv(pa.id), stock_loc(1), s1 - 2.0);
    // Con el turno ocupado más allá de su espera, el forzado se rinde sin
    // empujar nada; al soltarse el turno, sube una sola vez.
    let s2 = stock_srv(pa.id);
    let turno = worker::tomar_turno(Duration::from_secs(5)).await;
    chk!(inf, "r3", turno.is_ok(), "la prueba toma el turno de sincronización");
    venta_escritorio(&db, 1, 2, &cortes::ahora_local());
    proxy.reiniciar_cuenta();
    let r = worker::forzar_ciclo_con_espera(db.clone(), Duration::from_millis(400)).await;
    chk!(inf, "r3", r.as_ref().err().map(|e| e == worker::MSG_EN_CURSO).unwrap_or(false)
                   && proxy.total_pushes() == 0 && esc.pendientes_outbox() > 0 && cerca(stock_srv(pa.id), s2),
         "turno ocupado: el forzado responde '{}' sin empujar ({r:?}; push {}; outbox {})",
         worker::MSG_EN_CURSO, proxy.total_pushes(), esc.pendientes_outbox());
    drop(turno);
    let r = worker::forzar_ciclo_con_espera(db.clone(), Duration::from_secs(30)).await;
    let n_pa = proxy.pushes_de("productos", &pa.uuid);
    chk!(inf, "r3", r.is_ok() && n_pa == 1 && cerca(stock_srv(pa.id), s2 - 1.0) && cerca(stock_loc(1), s2 - 1.0)
                   && esc.pendientes_outbox() == 0,
         "al soltar el turno sube una vez ({r:?}; push con P_A {n_pa}; servidor {} / escritorio {}, esperado {})",
         stock_srv(pa.id), stock_loc(1), s2 - 1.0);
    {
        let c = db.lock().unwrap();
        c.execute("UPDATE sync_state SET remote_url = ?1 WHERE id = 1", [&web.url]).unwrap();
    }

    // ═══ r4) producto con el código renombrado aquí (SPEC4 D1) ═══
    //     El producto web llega con un código que aquí ya es de otro producto
    //     y se guarda como '<código>-W<8 del uuid>'. Sus ventas de aquí suben
    //     con el código del servidor: el servidor nunca recibe el sufijo y,
    //     sin cambios web de por medio, el push es normal (cursor de este
    //     equipo), no un 'merge' en cada venta.
    println!("\n-- r4) producto renombrado aquí ('-W<8>'): venta web + ventas del escritorio --");
    esc.asentar().await;
    let codigo_r4 = format!("E2E-R4-{run}");
    let pd4_uuid = hex32();
    db.lock()
        .unwrap()
        .execute(
            "INSERT INTO productos (uuid, codigo, nombre, precio_venta, precio_costo, stock_actual) VALUES (?1, ?2, ?3, 60, 30, 5)",
            rusqlite::params![pd4_uuid, codigo_r4, format!("Escritorio r4 E2E {run}")],
        )
        .unwrap();
    let np = web
        .rpc(&tok_admin, "crear_producto", json!({ "usuario_id": 0, "producto": {
            "codigo": codigo_r4, "codigo_tipo": null, "nombre": format!("Web r4 E2E {run}"), "descripcion": null,
            "categoria_id": null, "precio_costo": 40.0, "precio_venta": 75.0, "stock_actual": 20.0,
            "stock_minimo": 0.0, "proveedor_id": null, "foto_url": null } }))
        .await;
    chk!(inf, "r4", np.is_ok(), "crear_producto web con el código de un producto del escritorio: {np:?}");
    let pw4 = pg.filas(&format!("SELECT id, uuid FROM productos WHERE codigo = '{codigo_r4}' AND uuid !~ {HEX32}"));
    if let Some(pw4) = pw4.first() {
        let (pw4_id, pw4_uuid) = (pw4[0].parse::<i64>().unwrap(), pw4[1].clone());
        let codigo_srv = || pg.uno(&format!("SELECT codigo FROM productos WHERE id = {pw4_id}")).unwrap_or_default();
        let sufijados_srv = || pg.i64(&format!("SELECT count(*) FROM productos WHERE codigo LIKE '{codigo_r4}-%'"));
        // Ciclo: sube el producto del escritorio y baja el web (renombrado).
        esc.asentar().await;
        let esperado = format!("{codigo_r4}-W{}", &pw4_uuid[..8]);
        let local4 = d_opt_i64(&db, &format!("SELECT id FROM productos WHERE uuid = '{pw4_uuid}'"));
        let cod_local = || d_txt(&db, &format!("SELECT codigo FROM productos WHERE uuid = '{pw4_uuid}'"));
        chk!(inf, "r4", local4.is_some() && cod_local().as_deref() == Some(esperado.as_str()),
             "el producto web baja como '{esperado}' ({:?}; bandeja {:?})", cod_local(), esc.inbox());
        if let Some(l4) = local4 {
            let s0 = stock_srv(pw4_id);
            chk!(inf, "r4", cerca(s0, 20.0) && cerca(stock_loc(l4), s0), "existencia igual en los dos lados ({} / {s0})", stock_loc(l4));
            // Venta web y, después, una del escritorio en el mismo ciclo.
            let rv = venta_web(&web, &tok_espejo, &[(pw4_id, 1.0, 75.0)]).await;
            chk!(inf, "r4", rv.is_ok(), "venta web del producto renombrado: {rv:?}");
            tokio::time::sleep(Duration::from_millis(1100)).await;
            venta_escritorio(&db, l4, 2, &cortes::ahora_local());
            let r = esc.push().await;
            let cur = ultimo_cursor(&pw4_uuid);
            chk!(inf, "r4", r.is_ok() && cerca(stock_srv(pw4_id), s0 - 2.0) && codigo_srv() == codigo_r4 && sufijados_srv() == 0,
                 "venta web + venta del escritorio: servidor {} (esperado {}), código del servidor '{}' sin sufijo (con sufijo: {}); \
                  cursor {cur:?} ({r:?})",
                 stock_srv(pw4_id), s0 - 2.0, codigo_srv(), sufijados_srv());
            let _ = esc.pull().await;
            let _ = inbox::reintentar(&db, &esc.client).await;
            chk!(inf, "r4", cerca(stock_loc(l4), s0 - 2.0) && cod_local().as_deref() == Some(esperado.as_str()),
                 "el escritorio converge ({}; esperado {}) y conserva su código local ({:?})", stock_loc(l4), s0 - 2.0, cod_local());
            // Ventas siguientes del escritorio, sin cambios web: push normal.
            for i in 1..=3 {
                tokio::time::sleep(Duration::from_millis(1100)).await;
                venta_escritorio(&db, l4, 2, &cortes::ahora_local());
                let n_cur = cursores(&pw4_uuid);
                let r = esc.push().await;
                let cur = ultimo_cursor(&pw4_uuid);
                let esperado_s = s0 - 2.0 - i as f64;
                chk!(inf, "r4", r.is_ok() && cur.as_deref() == Some(device.as_str()) && cursores(&pw4_uuid) == n_cur + 1
                               && cerca(stock_srv(pw4_id), esperado_s) && codigo_srv() == codigo_r4,
                     "venta {i} del escritorio: push normal (cursor de este equipo: {}; cursor {cur:?}), servidor {} (esperado {esperado_s}), \
                      código '{}' ({r:?})",
                     cur.as_deref() == Some(device.as_str()), stock_srv(pw4_id), codigo_srv());
                esc.asentar().await;
                chk!(inf, "r4", cerca(stock_loc(l4), esperado_s) && cod_local().as_deref() == Some(esperado.as_str())
                               && esc.pendientes_outbox() == 0 && en_bandeja(&pw4_uuid) == 0,
                     "venta {i}: escritorio {} con código {:?}; outbox {:?}; bandeja {:?}",
                     stock_loc(l4), cod_local(), esc.errores_outbox(), esc.inbox());
            }
        }
    } else {
        chk!(inf, "r4", false, "el producto web de r4 no quedó en el servidor");
    }

    // ═══ r1e) DOS respuestas perdidas seguidas sobre la misma base (SPEC4 S1) ═══
    //     Venta del escritorio (respuesta perdida) → venta web → otra venta del
    //     escritorio (aún con la base vieja; respuesta perdida otra vez) →
    //     otra venta y por fin llega la respuesta. Cada push aplicado es la
    //     base efectiva del siguiente: 3 ventas del escritorio y 1 web = −4.
    println!("\n-- r1e) dos respuestas perdidas seguidas sobre la misma base --");
    {
        let rr = web
            .rpc(&tok_admin, "ajustar_stock", json!({ "productoId": pa.id, "nuevoStock": stock_srv(pa.id) + 20.0,
                                                      "motivo": "existencia e2e r1e/r5", "usuarioId": 0 }))
            .await;
        chk!(inf, "r1e", rr.is_ok(), "ajuste web de P_A (+20): {rr:?}");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        esc.asentar().await;
        let s0 = stock_srv(pa.id);
        chk!(inf, "r1e", cerca(stock_loc(1), s0) && esc.pendientes_outbox() == 0,
             "P_A igual en los dos lados ({} / {s0}); outbox {}", stock_loc(1), esc.pendientes_outbox());
        let misma_base0 = lineas_bitacora_srv(&["misma base", &pa.uuid]);
        // 1) Venta del escritorio; el servidor la aplica y la respuesta se pierde.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        venta_escritorio(&db, 1, 2, &cortes::ahora_local());
        proxy.perder_siguiente_push();
        let r = esc_p.push().await;
        chk!(inf, "r1e", r.is_err() && cerca(stock_srv(pa.id), s0 - 1.0),
             "1ª respuesta perdida: el servidor aplicó la venta ({s0} → {}; cursor {:?})", stock_srv(pa.id), ultimo_cursor(&pa.uuid));
        let _ = esc.pull().await;
        // 2) Venta web.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let rv = venta_web(&web, &tok_espejo, &[(pa.id, 1.0, precio_srv(pa.id))]).await;
        chk!(inf, "r1e", rv.is_ok() && cerca(stock_srv(pa.id), s0 - 2.0), "venta web de P_A (servidor {}): {rv:?}", stock_srv(pa.id));
        // 3) Otra venta del escritorio: el push lleva la base vieja, se aplica
        //    y su respuesta también se pierde.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        venta_escritorio(&db, 1, 2, &cortes::ahora_local());
        proxy.perder_siguiente_push();
        let r = esc_p.push().await;
        let s2 = stock_srv(pa.id);
        chk!(inf, "r1e", r.is_err() && cerca(s2, s0 - 3.0),
             "2ª respuesta perdida (misma base vieja): 2 ventas del escritorio y 1 web = −3; servidor {s2} (esperado {}; cursor {:?})",
             s0 - 3.0, ultimo_cursor(&pa.uuid));
        let _ = esc.pull().await;
        // 4) Una venta más y el reintento sí recibe respuesta.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        venta_escritorio(&db, 1, 2, &cortes::ahora_local());
        esc.asentar().await;
        let (ss, sl) = (stock_srv(pa.id), stock_loc(1));
        chk!(inf, "r1e", cerca(ss, s0 - 4.0) && cerca(sl, s0 - 4.0) && esc.pendientes_outbox() == 0 && en_bandeja(&pa.uuid) == 0,
             "3 ventas del escritorio y 1 web = −4: servidor {ss}, escritorio {sl} (esperado {}); outbox {:?}; bandeja {:?}",
             s0 - 4.0, esc.errores_outbox(), esc.inbox());
        match (misma_base0, lineas_bitacora_srv(&["misma base", &pa.uuid])) {
            (Some(a), Some(b)) => chk!(inf, "r1e", b == a + 2,
                                       "el servidor usó la base efectiva en los dos pushes con la base vieja (bitácora {a} → {b})"),
            _ => inf.nota("r1e", "sin E2E_SRV_LOG: no se revisó la bitácora del servidor".into()),
        }
    }

    // ═══ r5) bandeja 'pendiente_local' sin red (SPEC4 D2) ═══
    //     La fila web de P_A espera en la bandeja porque aquí había ventas sin
    //     subir. Las ventas suben, pero al reintentar la bandeja /sync/fila no
    //     contesta (sin red): el payload guardado (de ANTES de esas ventas) no
    //     se aplica ni se da por resuelto; el siguiente ciclo con red trae la
    //     versión actual.
    println!("\n-- r5) bandeja 'pendiente_local' con /sync/fila sin red --");
    {
        esc.asentar().await;
        let s0 = stock_srv(pa.id);
        chk!(inf, "r5", cerca(stock_loc(1), s0) && en_bandeja(&pa.uuid) == 0, "P_A igual en los dos lados ({} / {s0})", stock_loc(1));
        // Dos ventas del escritorio sin subir y, después, una web.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        venta_escritorio(&db, 1, 2, &cortes::ahora_local());
        venta_escritorio(&db, 1, 2, &cortes::ahora_local());
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let rv = venta_web(&web, &tok_espejo, &[(pa.id, 1.0, precio_srv(pa.id))]).await;
        chk!(inf, "r5", rv.is_ok(), "venta web de P_A: {rv:?}");
        // El pull llega antes que el push: la fila web espera en la bandeja.
        let _ = esc.pull().await;
        let entrada = d_foto(&db, &format!(
            "SELECT motivo, payload IS NOT NULL FROM sync_inbox WHERE tabla = 'productos' AND uuid = '{}'", pa.uuid
        ));
        chk!(inf, "r5", entrada.len() == 1 && entrada[0].contains("pendiente_local") && entrada[0].contains("Integer(1)")
                       && cerca(stock_loc(1), s0 - 2.0),
             "la fila web espera en la bandeja como 'pendiente_local' con su payload ({entrada:?}); escritorio {} (esperado {})",
             stock_loc(1), s0 - 2.0);
        let r = esc.push().await;
        chk!(inf, "r5", r.is_ok() && cerca(stock_srv(pa.id), s0 - 3.0) && esc.pendientes_outbox() == 0,
             "las dos ventas suben y se combinan con la web: servidor {} (esperado {}) ({r:?})", stock_srv(pa.id), s0 - 3.0);
        // /sync/fila sin red: un puerto local que nadie escucha.
        let url_sin_red = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        let sin_red = RemoteClient::new(&url_sin_red, &token_sync).unwrap();
        let ri = inbox::reintentar(&db, &sin_red).await;
        chk!(inf, "r5", en_bandeja(&pa.uuid) == 1 && cerca(stock_loc(1), s0 - 2.0),
             "sin red no se usa el payload guardado: la entrada sigue en la bandeja ({}) y el escritorio sigue en {} ({ri:?})",
             en_bandeja(&pa.uuid), stock_loc(1));
        // Siguiente ciclo con red: baja la combinación y la bandeja se vacía.
        esc.asentar().await;
        let (ss, sl) = (stock_srv(pa.id), stock_loc(1));
        chk!(inf, "r5", cerca(ss, s0 - 3.0) && cerca(sl, s0 - 3.0) && en_bandeja(&pa.uuid) == 0 && esc.pendientes_outbox() == 0,
             "con red converge: servidor {ss}, escritorio {sl} (esperado {}); bandeja {:?}; outbox {:?}",
             s0 - 3.0, esc.inbox(), esc.errores_outbox());
    }

    // ═══ r6) decimales: el servidor guarda 2 (SPEC4 S2) ═══
    //     El costo de 3 decimales del escritorio (33.335) queda aquí como
    //     33.34; la base del escritorio sigue diciendo 33.335 y eso NO es un
    //     cambio del servidor. El escritorio cambia el costo a 40 y DESPUÉS la
    //     web vende (la marca del servidor es la más nueva): el costo del
    //     escritorio se queda y la venta web cuenta.
    println!("\n-- r6) costo con 3 decimales: el redondeo del servidor no es un cambio suyo --");
    {
        let u6 = hex32();
        db.lock()
            .unwrap()
            .execute(
                "INSERT INTO productos (uuid, codigo, nombre, precio_venta, precio_costo, stock_actual, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, 70, 33.335, 10, ?4, ?4)",
                rusqlite::params![u6, format!("E2E-R6-{run}"), format!("Decimales r6 E2E {run}"), cortes::ahora_local()],
            )
            .unwrap();
        esc.asentar().await;
        let l6 = d_i64(&db, &format!("SELECT id FROM productos WHERE uuid = '{u6}'"));
        let id6 = pg.uno(&format!("SELECT id FROM productos WHERE uuid = '{u6}'")).and_then(|s| s.parse::<i64>().ok());
        let costo_loc = || d_f64(&db, &format!("SELECT precio_costo FROM productos WHERE id = {l6}"));
        let costo_base = d_txt(&db, &format!(
            "SELECT CAST(json_extract(datos, '$.precio_costo') AS TEXT) FROM sync_base WHERE tabla = 'productos' AND uuid = '{u6}'"
        ));
        if let Some(id6) = id6 {
            let costo_srv = || pg.f64(&format!("SELECT precio_costo::float8 FROM productos WHERE id = {id6}"));
            chk!(inf, "r6", cerca(costo_srv(), 33.34) && costo_loc() == 33.335 && costo_base.as_deref() == Some("33.335")
                           && cerca(stock_srv(id6), 10.0),
                 "el producto sube: costo servidor {} / escritorio {} / base {costo_base:?}; existencia {}",
                 costo_srv(), costo_loc(), stock_srv(id6));
            // El escritorio cambia el costo (como actualizar_producto: marca local).
            tokio::time::sleep(Duration::from_millis(1100)).await;
            db.lock()
                .unwrap()
                .execute(
                    "UPDATE productos SET precio_costo = 40, updated_at = ?1 WHERE id = ?2",
                    rusqlite::params![cortes::ahora_local(), l6],
                )
                .unwrap();
            // Después, la web vende una pieza (marca del servidor más nueva).
            tokio::time::sleep(Duration::from_millis(1100)).await;
            let rv = venta_web(&web, &tok_espejo, &[(id6, 1.0, 70.0)]).await;
            chk!(inf, "r6", rv.is_ok(), "venta web del producto: {rv:?}");
            let r = esc.push().await;
            let cur = ultimo_cursor(&u6);
            chk!(inf, "r6", r.is_ok() && cerca(costo_srv(), 40.0) && cerca(stock_srv(id6), 9.0) && cur.as_deref() == Some("merge"),
                 "costo del escritorio 40 y venta web: servidor costo {} (esperado 40), existencia {} (esperado 9), cursor {cur:?} ({r:?})",
                 costo_srv(), stock_srv(id6));
            esc.asentar().await;
            chk!(inf, "r6", cerca(costo_loc(), 40.0) && cerca(stock_loc(l6), 9.0) && esc.pendientes_outbox() == 0 && en_bandeja(&u6) == 0,
                 "el escritorio converge: costo {} existencia {}; outbox {:?}; bandeja {:?}",
                 costo_loc(), stock_loc(l6), esc.errores_outbox(), esc.inbox());
            // Sin cambios web de por medio, la siguiente venta es un push normal.
            tokio::time::sleep(Duration::from_millis(1100)).await;
            venta_escritorio(&db, l6, 2, &cortes::ahora_local());
            let r = esc.push().await;
            let cur = ultimo_cursor(&u6);
            chk!(inf, "r6", r.is_ok() && cur.as_deref() == Some(device.as_str()) && cerca(stock_srv(id6), 8.0) && cerca(costo_srv(), 40.0),
                 "venta siguiente: push normal (cursor de este equipo: {}), existencia {} (esperado 8), costo {} ({r:?})",
                 cur.as_deref() == Some(device.as_str()), stock_srv(id6), costo_srv());
            esc.asentar().await;
        } else {
            chk!(inf, "r6", false, "el producto de r6 no llegó al servidor (outbox {:?})", esc.errores_outbox());
        }
    }

    // ═══ r7) la marca guardada nunca va hacia atrás (SPEC4 S3) ═══
    //     La web vuelve a guardar el producto SIN cambios (marca nueva) y el
    //     escritorio, con el reloj atrasado, sube una venta: es un push normal
    //     (el servidor no cambió nada desde la base), el contenido es el del
    //     escritorio y la marca guardada se queda en la de la web.
    println!("\n-- r7) re-guardado web sin cambios + venta del escritorio con el reloj atrasado --");
    {
        let u7 = hex32();
        let (codigo7, nombre7) = (format!("E2E-R7-{run}"), format!("Marca r7 E2E {run}"));
        // search_text como lo arman crear_producto (aquí) y actualizar_producto
        // (web): así el re-guardado web no cambia NADA del contenido.
        let busqueda7 = crate::commands::productos::normalizar_texto(&format!("{codigo7} {nombre7} "));
        db.lock()
            .unwrap()
            .execute(
                "INSERT INTO productos (uuid, codigo, nombre, precio_venta, precio_costo, stock_actual, search_text, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, 50, 20, 10, ?4, ?5, ?5)",
                rusqlite::params![u7, codigo7, nombre7, busqueda7, cortes::ahora_local()],
            )
            .unwrap();
        esc.asentar().await;
        let l7 = d_i64(&db, &format!("SELECT id FROM productos WHERE uuid = '{u7}'"));
        let id7 = pg.uno(&format!("SELECT id FROM productos WHERE uuid = '{u7}'")).and_then(|s| s.parse::<i64>().ok());
        if let Some(id7) = id7 {
            let marca_srv = || pg.uno(&format!("SELECT updated_at FROM productos WHERE id = {id7}")).unwrap_or_default();
            let m0 = marca_srv();
            tokio::time::sleep(Duration::from_millis(1100)).await;
            let ed = editar_precio_web(&web, &pg, &tok_admin, id7, 50.0).await;
            let m_web = marca_srv();
            let cur_web = ultimo_cursor(&u7);
            chk!(inf, "r7", ed.is_ok() && m_web.as_str() > m0.as_str() && cerca(precio_srv(id7), 50.0)
                           && cur_web.as_deref() != Some(device.as_str()),
                 "la web vuelve a guardar sin cambios: marca {m0} → {m_web}, precio {}, cursor {cur_web:?} ({ed:?})", precio_srv(id7));
            // Venta del escritorio con el reloj 20 s atrasado (sin bajar antes lo de la web).
            let t_atras = mas_segundos(&cortes::ahora_local(), -20);
            let v = venta_escritorio(&db, l7, 2, &t_atras);
            marcar_hora_local(&db, l7, v, &t_atras);
            let m_pre = d_txt(&db, &format!("SELECT updated_at FROM productos WHERE id = {l7}")).unwrap_or_default();
            let r = esc.push().await;
            let m_srv = marca_srv();
            let cur = ultimo_cursor(&u7);
            chk!(inf, "r7", r.is_ok() && m_pre == t_atras && cerca(stock_srv(id7), 9.0) && cerca(precio_srv(id7), 50.0)
                           && m_srv.as_str() >= m_web.as_str() && cur.as_deref() == Some(device.as_str()),
                 "push normal con la marca atrasada ({m_pre}): existencia {} (esperado 9), marca guardada {m_srv} (web {m_web}; no va \
                  hacia atrás), cursor de este equipo: {} ({r:?})",
                 stock_srv(id7), cur.as_deref() == Some(device.as_str()));
            let m_loc = d_txt(&db, &format!("SELECT updated_at FROM productos WHERE id = {l7}")).unwrap_or_default();
            let m_base = d_txt(&db, &format!("SELECT updated_at FROM sync_base WHERE tabla = 'productos' AND uuid = '{u7}'"));
            chk!(inf, "r7", m_loc == m_srv && m_base.as_deref() == Some(m_srv.as_str()),
                 "la fila local ({m_loc}) y la base ({m_base:?}) toman la marca que quedó en el servidor ({m_srv})");
            esc.asentar().await;
            chk!(inf, "r7", cerca(stock_loc(l7), 9.0) && esc.pendientes_outbox() == 0 && en_bandeja(&u7) == 0,
                 "el escritorio queda en 9 sin pendientes; outbox {:?}; bandeja {:?}", esc.errores_outbox(), esc.inbox());
        } else {
            chk!(inf, "r7", false, "el producto de r7 no llegó al servidor (outbox {:?})", esc.errores_outbox());
        }
    }

    println!("\nbandeja final: {:?}\noutbox con error: {:?}", esc.inbox(), esc.errores_outbox());
    inf.cerrar("e2e_sync_v2_escenarios");
}

// ═════════════════════════════════════════════════════════
// f2) Tareas únicas con el device_uuid REAL de la tienda (clon B)
// ═════════════════════════════════════════════════════════

#[tokio::test]
async fn e2e_sync_v2_reparacion_tienda_real() {
    let Some(ctx) = preparar("E2E_SERVER_URL_B", "E2E_PG_URL_B", "postgres://postgres@localhost:54329/e2e_srv_b").await else {
        eprintln!("E2E_SERVER_URL_B no definido: se omite la reparación con el equipo real");
        return;
    };
    let Ctx { web, pg, token_sync, run } = ctx;
    let mut inf = Informe::default();
    println!("\n== e2e f2: reparación con el equipo real de la tienda (corrida {run}) contra {} ==", web.url);
    if pg.uno("SELECT valor FROM sync_meta WHERE clave = 'fk_reparado'").is_some() {
        inf.nota("f2", "el clon B ya estaba reparado (corrida previa): las cifras de la primera reparación no aplican".into());
    }

    // ── i) Ventana del despliegue: el servidor ya es nuevo y la tienda sigue en
    //     v0.1.38. Una venta web espejo de ese momento: el pull legado la omite
    //     (y avanza el cursor); la tienda hace su corte; después se actualiza.
    let pin_lau = pin_aleatorio();
    let h = bcrypt::hash(&pin_lau, 4).unwrap();
    pg.exec(&format!("UPDATE usuarios SET pin = '{h}' WHERE nombre_usuario = 'Lau' AND uuid ~ {HEX32}"));
    let tok_lau = web.login_pin(&pin_lau, Some(&uuid::Uuid::new_v4().to_string())).await["token"].as_str().unwrap().to_string();
    let (pv_id, pv_precio) = {
        let f = pg.filas("SELECT id, precio_venta::float8 FROM productos WHERE id <= 5948 AND deleted_at IS NULL AND activo = 1 AND precio_venta > 0 ORDER BY id LIMIT 1");
        (f[0][0].parse::<i64>().unwrap(), f[0][1].parse::<f64>().unwrap())
    };
    let rv = venta_web(&web, &tok_lau, &[(pv_id, 1.0, pv_precio)]).await;
    chk!(inf, "i", rv.is_ok(), "venta web espejo durante la ventana del despliegue: {rv:?}");
    let rv = rv.unwrap_or(json!({}));
    let wi_uuid = pg.uno(&format!("SELECT uuid FROM ventas WHERE id = {}", rv["id"].as_i64().unwrap_or(0))).unwrap_or_default();
    let wi_total = rv["total"].as_f64().unwrap_or(0.0);
    let wi_fecha = rv["fecha"].as_str().unwrap_or("").to_string();
    let (st_leg, txt_leg) = web.get(&format!("/sync/pull?cursor={}&sucursal_id=1", pg.max_cursor() - 5), Some(&token_sync)).await;
    chk!(inf, "i", st_leg == 200 && !txt_leg.contains(&wi_uuid), "el pull legado (v0.1.38) omite la venta web y avanza el cursor");
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // ── f3) antes de que la tienda se actualice, el dueño cambia en la web la
    //     categoría de un producto del escritorio (la web usa ids del servidor).
    let frenos_srv = pg.i64("SELECT id FROM categorias WHERE nombre = 'Frenos'");
    let pc = pg.filas(&format!(
        "SELECT id, codigo, nombre, precio_costo::float8, precio_venta::float8, stock_minimo::float8, COALESCE(proveedor_id::text, '') \
           FROM productos WHERE id <= 5948 AND uuid ~ {HEX32} AND deleted_at IS NULL AND categoria_id IS NULL ORDER BY id OFFSET 20 LIMIT 1"
    ));
    let pc = &pc[0];
    let pc_id: i64 = pc[0].parse().unwrap();
    let ed = web
        .rpc(&tok_lau, "actualizar_producto", json!({ "usuario_id": 0, "producto": {
            "id": pc_id, "codigo": pc[1], "nombre": pc[2], "descripcion": null, "categoria_id": frenos_srv,
            "precio_costo": pc[3].parse::<f64>().unwrap(), "precio_venta": pc[4].parse::<f64>().unwrap(),
            "stock_minimo": pc[5].parse::<f64>().unwrap(), "proveedor_id": pc[6].parse::<i64>().ok(), "foto_url": null } }))
        .await;
    chk!(inf, "f3", ed.is_ok(), "web: categoría 'Frenos' (id servidor {frenos_srv}) al producto {pc_id}: {ed:?}");

    // Distribución cruda ANTES (ids del escritorio: 1 Dueño, 2 Nancy, 3 Mariana, 4 Lau).
    let crudo: BTreeMap<String, i64> = pg
        .filas(&format!(
            "SELECT v.usuario_id, count(*) FROM ventas v \
              WHERE v.uuid ~ {HEX32} AND NOT EXISTS (SELECT 1 FROM sync_fk_estado e WHERE e.tabla = 'ventas' AND e.uuid = v.uuid) \
              GROUP BY 1"
        ))
        .into_iter()
        .map(|f| (f[0].clone(), f[1].parse().unwrap()))
        .collect();
    let colgados_antes = pg.i64("SELECT count(*) FROM venta_detalle vd WHERE NOT EXISTS (SELECT 1 FROM productos p WHERE p.id = vd.producto_id)");
    let web_antes = pg.filas(&format!("SELECT uuid, usuario_id FROM ventas WHERE uuid !~ {HEX32} ORDER BY uuid"));
    println!("  ventas del escritorio por usuario_id crudo: {crudo:?}");

    // ── Escritorio con lo que tiene la tienda ──
    let cursor_max = pg.max_cursor();
    let conn = db_como_la_tienda();
    insertar_usuarios_tienda(&conn, &pg);
    alinear_categorias(&conn, &pg);
    insertar_proveedores_identidad(&conn, &pg);
    for f in pg.filas("SELECT id, uuid, nombre, COALESCE(descuento_porcentaje,0)::float8, updated_at FROM clientes ORDER BY id") {
        conn.execute(
            "INSERT INTO clientes (id, uuid, nombre, descuento_porcentaje, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![f[0].parse::<i64>().unwrap(), f[1], f[2], f[3].parse::<f64>().unwrap(), f[4]],
        )
        .unwrap();
    }
    // i2) Productos web cuyo código chocaba con uno del escritorio: la
    //     migración 011 los renombró '<código>-W<id>' y los volvió a publicar.
    //     La tienda real nunca pudo guardarlos (UNIQUE codigo), así que el
    //     escritorio de prueba tampoco los tiene.
    let renombrados: Vec<Vec<String>> = pg.filas(&format!(
        "SELECT id, uuid, codigo, (deleted_at IS NULL)::text FROM productos WHERE uuid !~ {HEX32} AND codigo ~ ('-W' || id || '$') ORDER BY id"
    ));
    let uuids_renombrados: BTreeSet<String> = renombrados.iter().map(|f| f[1].clone()).collect();
    let choques = pg.i64(&format!(
        "SELECT count(*) FROM productos p WHERE p.uuid !~ {HEX32} \
            AND EXISTS (SELECT 1 FROM productos q WHERE q.codigo = p.codigo AND q.id <> p.id AND q.uuid ~ {HEX32})"
    ));
    let republicados = pg.i64(&format!(
        "SELECT count(DISTINCT p.uuid) FROM productos p JOIN sync_cursor c ON c.tabla = 'productos' AND c.uuid = p.uuid \
          WHERE p.uuid !~ {HEX32} AND p.codigo ~ ('-W' || p.id || '$')"
    ));
    chk!(inf, "i2", !renombrados.is_empty() && choques == 0 && republicados as usize == renombrados.len(),
         "migración 011: {} productos web renombrados '-W<id>' y re-publicados ({republicados}); quedan {choques} choques con el escritorio",
         renombrados.len());
    // Productos: ids ≤ 5948 iguales en los dos lados; los demás con ids propios.
    let mut sig_id = 5948i64;
    let mut omitidos = 0;
    let mut uuids_omitidos: BTreeSet<String> = BTreeSet::new();
    // uuid → (código en el servidor, borrado)
    let mut omitidos_det: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for f in pg.filas(
        "SELECT id, uuid, codigo, nombre, precio_venta::float8, precio_costo::float8, stock_actual::float8, \
                COALESCE(proveedor_id::text, ''), activo, updated_at, COALESCE(deleted_at, '') \
           FROM productos ORDER BY (uuid ~ '^[0-9a-f]{32}$') DESC, id",
    ) {
        if uuids_renombrados.contains(&f[1]) {
            continue;
        }
        let sid: i64 = f[0].parse().unwrap();
        let local = if sid <= 5948 { sid } else { sig_id += 1; sig_id };
        if local > 5948 && conn.query_row("SELECT count(*) FROM productos WHERE id = ?", [local], |r| r.get::<_, i64>(0)).unwrap() > 0 {
            continue;
        }
        let prov: Option<i64> = f[7].parse().ok().filter(|p| *p <= 15);
        let del: Option<&str> = if f[10].is_empty() { None } else { Some(f[10].as_str()) };
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO productos (id, uuid, codigo, nombre, precio_venta, precio_costo, stock_actual, \
                                                  proveedor_id, activo, updated_at, deleted_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![local, f[1], f[2], f[3], f[4].parse::<f64>().unwrap(), f[5].parse::<f64>().unwrap(),
                                  f[6].parse::<f64>().unwrap(), prov, f[8].parse::<i64>().unwrap(), f[9], del],
            )
            .unwrap();
        if n == 0 {
            omitidos += 1;
            uuids_omitidos.insert(f[1].clone());
            omitidos_det.insert(f[1].clone(), (f[2].clone(), !f[10].is_empty()));
        }
    }
    println!("  productos locales: {} (omitidos por código repetido: {omitidos})",
             conn.query_row("SELECT count(*) FROM productos", [], |r| r.get::<_, i64>(0)).unwrap());
    // Una venta web que bajó con la versión vieja: FK con ids del SERVIDOR
    // (usuario 2 = Dueño en el servidor, aquí 2 = Nancy).
    let vw = "019dfaf7-6ab9-7920-9db0-c1e5c15334ed";
    let fv = pg.filas(&format!(
        "SELECT folio, total::float8, metodo_pago, fecha, updated_at, usuario_id, id, COALESCE(cliente_id::text, '') \
           FROM ventas WHERE uuid = '{vw}'"
    ));
    let fv = &fv[0];
    // FK crudas del servidor, como las dejaba el pull viejo (clientes: mismo id).
    conn.execute(
        "INSERT INTO ventas (id, folio, usuario_id, cliente_id, subtotal, total, metodo_pago, monto_recibido, fecha, uuid, updated_at, origen, caja) \
         VALUES (414, ?1, ?2, ?8, ?3, ?3, ?4, ?3, ?5, ?6, ?7, 'web', 'principal')",
        rusqlite::params![fv[0], fv[5].parse::<i64>().unwrap(), fv[1].parse::<f64>().unwrap(), fv[2], fv[3], vw, fv[4],
                          fv[7].parse::<i64>().ok()],
    )
    .unwrap();
    for d in pg.filas(&format!(
        "SELECT uuid, producto_id, cantidad::float8, precio_final::float8, subtotal::float8 FROM venta_detalle WHERE venta_id = {}",
        fv[6]
    )) {
        let pid: i64 = d[1].parse().unwrap();
        let existe: i64 = conn.query_row("SELECT count(*) FROM productos WHERE id = ?", [pid], |r| r.get(0)).unwrap();
        if existe == 1 {
            conn.execute(
                "INSERT INTO venta_detalle (venta_id, producto_id, cantidad, precio_final, subtotal, uuid) VALUES (414, ?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![pid, d[2].parse::<f64>().unwrap(), d[3].parse::<f64>().unwrap(), d[4].parse::<f64>().unwrap(), d[0]],
            )
            .unwrap();
        }
    }
    configurar_sync(&conn, DEVICE_TIENDA, &web.url, &token_sync, cursor_max);
    limpiar_outbox(&conn);
    let db: Db = Arc::new(Mutex::new(conn));
    let esc = Escritorio { db: db.clone(), client: RemoteClient::new(&web.url, &token_sync).unwrap() };
    // La tienda tiene su cadena de cortes: un corte ahora cierra la historia.
    let ahora0 = cortes::ahora_local();
    let (_cid, fin0, _) = esc.corte(&ahora0);
    // f4) antes de reparar, el escritorio toca la venta web local (sus FK son
    //     ids del SERVIDOR que dejó el pull viejo: usuario 2 = Dueño allá,
    //     Nancy aquí). SPEC2 R3.6: el push no manda sus FK hasta que las
    //     referencias estén reparadas, así que el servidor no la cambia de dueño.
    let srv_vw = || pg.filas(&format!(
        "SELECT v.usuario_id::text, COALESCE(string_agg(vd.producto_id::text, ',' ORDER BY vd.id), '') FROM ventas v \
           LEFT JOIN venta_detalle vd ON vd.venta_id = v.id WHERE v.uuid = '{vw}' GROUP BY v.usuario_id"
    ));
    let srv_vw0 = srv_vw();
    let upd_vw = || pg.uno(&format!("SELECT updated_at FROM ventas WHERE uuid = '{vw}'"));
    let upd_vw0 = upd_vw();
    db.lock().unwrap().execute("UPDATE ventas SET updated_at = ?1 WHERE id = 414", [cortes::ahora_local()]).unwrap();
    // El worker empuja su outbox antes de las tareas únicas.
    let r = esc.push().await;
    chk!(inf, "f2", r.is_ok() && esc.pendientes_outbox() == 0, "push del corte de la tienda: {r:?} {:?}", esc.errores_outbox());
    let srv_vw1 = srv_vw();
    chk!(inf, "f4", upd_vw() != upd_vw0, "la venta web local sí se subió ({upd_vw0:?} → {:?})", upd_vw());
    chk!(inf, "f4", srv_vw1 == srv_vw0 && srv_vw0.first().map(|f| f[0] == fv[5]).unwrap_or(false),
         "push de la venta web local antes de reparar: el servidor conserva usuario y productos ({srv_vw0:?} → {srv_vw1:?})");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let periodo0 = esc.periodo(&cortes::ahora_local());
    println!("  corte de la tienda en {fin0}; período abierto: {} ventas", periodo0.num_transacciones);
    let foto = |db: &Db| {
        let mut v = d_foto(db, "SELECT * FROM cortes ORDER BY id");
        v.extend(d_foto(db, "SELECT uuid, total, cliente_id, caja, origen, registrado_at FROM ventas ORDER BY id"));
        v.extend(d_foto(db, "SELECT uuid, monto, corte_id FROM movimientos_caja ORDER BY id"));
        v
    };
    let foto0 = foto(&db);

    // ── a) mapa + reparación del servidor + b) espejo (lo que hace el worker) ──
    let t0 = std::time::Instant::now();
    let rep = tareas::reparar_referencias(&db, &esc.client, DEVICE_TIENDA, false).await;
    println!("  reparar_referencias: {:?}", t0.elapsed());
    match &rep {
        Ok(r) => {
            let s = &r.servidor;
            chk!(inf, "f2", s["ok"] == json!(true) && s["confirmado"] == json!(true), "reporte del servidor ok/confirmado: {}", s["verificacion"]);
            println!("  celdas cambiadas: {} — por columna: {}", s["celdas_cambiadas"], s["por_columna"]);
            println!("  filas: {}", s["filas"]);
            chk!(inf, "f2", s["celdas_cambiadas"].as_i64().unwrap_or(0) > 0, "la reparación cambió celdas ({})", s["celdas_cambiadas"]);
            chk!(inf, "f2", r.espejo.celdas_cambiadas >= 1, "espejo corrigió la venta web local ({:?})", r.espejo);
            for t in &r.mapa.tablas {
                if t.desconocidos > 0 {
                    inf.nota("f2", format!("mapa {}: {} enviados, {} desconocidos en el servidor", t.tabla, t.enviados, t.desconocidos));
                }
            }
        }
        Err(e) => chk!(inf, "f2", false, "reparar_referencias: {}", e.mensaje()),
    }
    chk!(inf, "f2", pg.uno("SELECT valor FROM sync_meta WHERE clave = 'fk_reparado'").as_deref() == Some("1"), "sync_meta fk_reparado = 1");
    let cat = pg.uno(&format!("SELECT c.nombre FROM productos p LEFT JOIN categorias c ON c.id = p.categoria_id WHERE p.id = {pc_id}"));
    chk!(inf, "f3", cat.as_deref() == Some("Frenos"),
         "tras la reparación el producto {pc_id} sigue en 'Frenos' (quedó en {cat:?}): la celda ya estaba en ids del servidor");

    let despues: BTreeMap<String, i64> = pg
        .filas(&format!(
            "SELECT u.nombre_usuario, count(*) FROM ventas v JOIN usuarios u ON u.id = v.usuario_id \
              WHERE v.uuid ~ {HEX32} GROUP BY 1"
        ))
        .into_iter()
        .map(|f| (f[0].clone(), f[1].parse().unwrap()))
        .collect();
    println!("  ventas del escritorio por vendedor tras reparar: {despues:?}");
    for (crudo_id, nu) in [("1", "admin"), ("2", "Nancy"), ("3", "Mariana"), ("4", "Lau")] {
        let esperado = crudo.get(crudo_id).copied().unwrap_or(0);
        let real = despues.get(nu).copied().unwrap_or(0);
        chk!(inf, "f2", real >= esperado && esperado > 0, "{nu}: {real} ventas (crudas con id {crudo_id}: {esperado})");
    }
    chk!(inf, "f2", !despues.keys().any(|k| k == "andrew"), "ninguna venta del escritorio a nombre de 'Andrew Web'");
    let colgados = pg.i64("SELECT count(*) FROM venta_detalle vd WHERE NOT EXISTS (SELECT 1 FROM productos p WHERE p.id = vd.producto_id)");
    chk!(inf, "f2", colgados <= colgados_antes, "renglones con producto inexistente no aumentan ({colgados_antes} → {colgados})");
    let web_despues = pg.filas(&format!("SELECT uuid, usuario_id FROM ventas WHERE uuid !~ {HEX32} ORDER BY uuid"));
    chk!(inf, "f2", web_despues == web_antes, "ventas web intactas en el servidor");
    chk!(inf, "f2", d_i64(&db, "SELECT usuario_id FROM ventas WHERE id = 414") == 1, "espejo: la venta web local quedó con el Dueño LOCAL (1)");
    let foto1 = foto(&db);
    chk!(inf, "f2", foto1 == foto0, "escritorio: montos, cortes y corte_id intactos tras reparar {:?}", diferencias(&foto0, &foto1));

    // ── segunda corrida (botón manual): no-op ──
    let rep2 = tareas::reparar_referencias(&db, &esc.client, DEVICE_TIENDA, true).await;
    match &rep2 {
        Ok(r) => {
            chk!(inf, "f2", r.servidor["ok"] == json!(true) && r.servidor["celdas_cambiadas"].as_i64() == Some(0),
                 "segunda reparación: 0 celdas ({})", r.servidor["celdas_cambiadas"]);
            chk!(inf, "f2", r.espejo.celdas_cambiadas == 0, "segundo espejo: 0 celdas ({})", r.espejo.celdas_cambiadas);
        }
        Err(e) => chk!(inf, "f2", false, "segunda reparación: {}", e.mensaje()),
    }
    let foto2 = foto(&db);
    chk!(inf, "f2", foto2 == foto1, "escritorio intacto tras la segunda corrida {:?}", diferencias(&foto1, &foto2));

    // ── c) re-descarga desde el cursor 0 (solo inserta) ──
    let t = state::leer_tareas(&db.lock().unwrap()).unwrap();
    chk!(inf, "f2", t.replay_pendiente() && t.replay_hasta.as_deref() == Some(cursor_max.to_string().as_str()),
         "re-descarga programada de 0 a {cursor_max} ({:?} → {:?})", t.replay_cursor, t.replay_hasta);
    let t0 = std::time::Instant::now();
    for _ in 0..500 {
        let cfg = esc.cfg();
        if let Err(e) = tareas::correr_replay(&db, &esc.client, &cfg).await {
            chk!(inf, "f2", false, "replay: {e}");
            break;
        }
        if !state::leer_tareas(&db.lock().unwrap()).unwrap().replay_pendiente() {
            break;
        }
    }
    println!("  re-descarga: {:?}", t0.elapsed());
    chk!(inf, "f2", !state::leer_tareas(&db.lock().unwrap()).unwrap().replay_pendiente(), "re-descarga terminada");
    chk!(inf, "f2", esc.cfg().last_pull_cursor.as_deref() == Some(cursor_max.to_string().as_str()), "la re-descarga no mueve el cursor normal");
    let web_srv: Vec<Vec<String>> = pg.filas(&format!(
        "SELECT v.uuid, u.uuid, v.total::float8 FROM ventas v LEFT JOIN usuarios u ON u.id = v.usuario_id WHERE v.uuid !~ {HEX32} ORDER BY v.uuid"
    ));
    let mut faltan = Vec::new();
    let mut mal = Vec::new();
    for w in &web_srv {
        let l = d_foto(&db, &format!(
            "SELECT u.uuid, v.total, v.caja, v.origen, v.registrado_at FROM ventas v LEFT JOIN usuarios u ON u.id = v.usuario_id WHERE v.uuid = '{}'",
            w[0]
        ));
        // La venta de la ventana del despliegue (i) sí se re-acomoda (SPEC2
        // R3.4); la historia anterior nunca.
        let sin_refechar = w[0] == wi_uuid || l.first().map(|f| f.contains("Text(\"principal\")|Text(\"web\")|Null|")).unwrap_or(false);
        if l.is_empty() {
            faltan.push(w[0].clone());
        } else if !l[0].starts_with(&format!("Text({:?})", w[1])) || !l[0].contains("Text(\"principal\")|Text(\"web\")|") || !sin_refechar {
            mal.push(format!("{} → {}", w[0], l[0]));
        }
    }
    chk!(inf, "f2", faltan.is_empty(), "las {} ventas web de producción llegan al escritorio; faltan {faltan:?} (bandeja {:?})", web_srv.len(), esc.inbox());
    chk!(inf, "f2", mal.is_empty(), "con vendedor LOCAL correcto, caja principal, origen web, sin re-fechar (salvo la de la ventana): {mal:?}");
    let det_mal = d_i64(&db,
        "SELECT count(*) FROM venta_detalle vd JOIN ventas v ON v.id = vd.venta_id \
          WHERE v.origen = 'web' AND NOT EXISTS (SELECT 1 FROM productos p WHERE p.id = vd.producto_id)");
    chk!(inf, "f2", det_mal == 0, "renglones de ventas web con producto local existente ({det_mal} colgados)");
    chk!(inf, "f2", d_i64(&db, "SELECT count(*) FROM ventas WHERE length(uuid) = 32 AND uuid NOT GLOB '*[^0-9a-f]*'") == 0,
         "ninguna venta del escritorio se importó (D4)");
    let periodo1 = esc.periodo(&cortes::ahora_local());
    // Ronda 2 (SPEC2 R3.4): con `caja_v2_desde` (hora en que se actualizó el
    // servidor) la venta de la ventana del despliegue SÍ entra al período
    // abierto (re-acomodada); la historia anterior no.
    let caja_v2 = pg.uno("SELECT valor FROM sync_meta WHERE clave = 'caja_v2_desde'");
    let ventana_cuenta = caja_v2.as_deref().map(|c| c <= wi_fecha.as_str()).unwrap_or(false);
    let (extra_n, extra_ef) = if ventana_cuenta { (1, wi_total) } else { (0, 0.0) };
    chk!(inf, "f2", periodo1.num_transacciones == periodo0.num_transacciones + extra_n
                    && cerca(periodo1.efectivo_esperado, periodo0.efectivo_esperado + extra_ef)
                    && cerca(periodo1.total_entradas_efectivo, periodo0.total_entradas_efectivo)
                    && cerca(periodo1.total_retiros_efectivo, periodo0.total_retiros_efectivo),
         "la historia web re-descargada no entra al período abierto, salvo la venta de la ventana ({} ventas, esperado {} → {}; caja_v2_desde {caja_v2:?})",
         periodo1.num_transacciones, periodo0.efectivo_esperado, periodo1.efectivo_esperado);
    chk!(inf, "f2", d_i64(&db, "SELECT count(*) FROM cortes") == 1 && d_i64(&db, "SELECT count(*) FROM aperturas_caja") == 0,
         "la re-descarga no trajo cortes ni aperturas");
    chk!(inf, "f2", d_foto(&db, "SELECT * FROM cortes ORDER BY id") == d_foto(&db, "SELECT * FROM cortes ORDER BY id") && foto(&db)[..1] == foto0[..1],
         "el corte de la tienda sigue igual");
    // i) la venta de la ventana llega por la re-descarga… ¿y cuenta?
    let vi = d_foto(&db, &format!("SELECT registrado_at, fecha, total, caja FROM ventas WHERE uuid = '{wi_uuid}'"));
    chk!(inf, "i", vi.len() == 1, "la venta web de la ventana llega con la re-descarga ({vi:?})");
    let desde = state::leer_tareas(&db.lock().unwrap()).unwrap().acepta_web_desde.unwrap_or_default();
    let en_corte = d_f64(&db, "SELECT COALESCE(SUM(total_ventas_efectivo), 0) FROM cortes");
    let cuenta_abierto = periodo1.vendedores.iter().any(|v| v.usuario_id == 4);
    if let Some(c) = &caja_v2 {
        chk!(inf, "i", desde.as_str() <= c.as_str(), "acepta_web_desde ({desde}) bajó a caja_v2_desde ({c})");
        let reg = d_txt(&db, &format!("SELECT registrado_at FROM ventas WHERE uuid = '{wi_uuid}'"));
        chk!(inf, "i", !ventana_cuenta || (cuenta_abierto && reg.as_deref().map(|r| r > fin0.as_str()).unwrap_or(false)),
             "la venta de la ventana (fecha {wi_fecha}) se re-acomoda después del corte ({fin0}) y cuenta (registrado_at {reg:?})");
    }
    if vi.len() == 1 && !cuenta_abierto {
        inf.nota("i", format!(
            "venta web ESPEJO (caja principal, ${wi_total}, fecha {wi_fecha}) hecha con el servidor nuevo y la tienda aún en \
             v0.1.38: llega solo por la re-descarga, con fecha < acepta_web_desde ({desde}) no se re-fecha y NO cuenta en \
             ningún corte del escritorio (cortes: ${en_corte}; período abierto: {} ventas). El efectivo está en el cajón.",
            periodo1.num_transacciones
        ));
    }

    let motivos = d_foto(&db, "SELECT motivo, tabla, count(*) FROM sync_inbox GROUP BY 1, 2 ORDER BY 1, 2");
    println!("  bandeja tras la re-descarga: {motivos:?}");
    let errores = d_foto(&db, "SELECT tabla, uuid, error FROM sync_inbox WHERE motivo = 'error' ORDER BY id LIMIT 20");
    if !errores.is_empty() {
        inf.nota("f2", format!("cambios con motivo 'error' en la bandeja tras la re-descarga: {errores:?}"));
    }
    // Productos que el sembrado de prueba no pudo guardar por código repetido
    // (el servidor tiene pares viejos de escritorio con el mismo código: lo
    // mismo le pasa a una PC nueva). SPEC3 D5: la re-descarga ya no los atora
    // en la bandeja; los guarda como '<código>-D<8 del uuid>' (o '-W' si los
    // hizo la web). Cualquier error en la bandeja es una falla.
    let errores_reales: Vec<Vec<String>> = {
        let c = db.lock().unwrap();
        let mut st = c.prepare("SELECT tabla, uuid, COALESCE(error, '') FROM sync_inbox WHERE motivo IN ('error', 'sin_fk')").unwrap();
        let v = st
            .query_map([], |r| Ok(vec![r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?]))
            .unwrap()
            .map(|x| x.unwrap())
            .collect();
        v
    };
    chk!(inf, "f2", errores_reales.is_empty(),
         "la bandeja no tiene errores ni FK sin resolver ({} productos omitidos por código repetido): {errores_reales:?}",
         uuids_omitidos.len());
    let (mut con_sufijo, mut mal_sufijo, mut no_llegaron) = (0usize, Vec::new(), Vec::new());
    for (u, (codigo, borrado)) in &omitidos_det {
        let es_escritorio = u.len() == 32 && u.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase());
        let esperado = format!("{codigo}-{}{}", if es_escritorio { 'D' } else { 'W' }, &u[..8.min(u.len())]);
        match d_txt(&db, &format!("SELECT codigo FROM productos WHERE uuid = '{u}'")) {
            Some(c) if c == esperado => con_sufijo += 1,
            Some(c) => mal_sufijo.push(format!("{u} {codigo} → {c}")),
            // Borrados del escritorio: no se crean (regla de apply.rs).
            None if *borrado && es_escritorio => {}
            None => no_llegaron.push(format!("{u} {codigo}")),
        }
    }
    chk!(inf, "f2", mal_sufijo.is_empty(),
         "{con_sufijo} productos con código repetido llegaron como '<código>-D/-W<8 del uuid>'; con otro código: {mal_sufijo:?}");
    // Los que subió este mismo equipo no regresan (el pull v2 no devuelve
    // los ecos del equipo que pide): la tienda real ya los tiene.
    let ajenos: Vec<&String> = no_llegaron
        .iter()
        .filter(|f| {
            let u = f.split(' ').next().unwrap_or_default();
            pg.i64(&format!(
                "SELECT count(*) FROM sync_cursor WHERE tabla = 'productos' AND uuid = '{u}'                    AND origen_device IS DISTINCT FROM '{DEVICE_TIENDA}'"
            )) > 0
        })
        .collect();
    chk!(inf, "f2", ajenos.is_empty(),
         "los productos con código repetido que no llegaron son ecos de la propia tienda ({} de {}); de otro origen: {ajenos:?}",
         no_llegaron.len(), omitidos_det.len());
    // i2) los productos renombrados por la 011 llegan con la re-descarga; los
    //     borrados en la web llegan ya borrados (SPEC3 D4).
    let vivos: Vec<&Vec<String>> = renombrados.iter().filter(|f| f[3] == "true").collect();
    let faltan_ren: Vec<String> = vivos
        .iter()
        .filter(|f| d_txt(&db, &format!("SELECT codigo FROM productos WHERE uuid = '{}'", f[1])).as_deref() != Some(f[2].as_str()))
        .map(|f| format!("{} {}", f[0], f[2]))
        .collect();
    chk!(inf, "i2", faltan_ren.is_empty(),
         "los {} productos renombrados por la 011 y no borrados están en el escritorio con su código nuevo; faltan {faltan_ren:?}",
         vivos.len());
    let borrados_ren = renombrados.len() - vivos.len();
    let borrados_en_bandeja = renombrados
        .iter()
        .filter(|f| f[3] != "true" && d_i64(&db, &format!("SELECT count(*) FROM sync_inbox WHERE uuid = '{}'", f[1])) > 0)
        .count();
    chk!(inf, "i2", borrados_en_bandeja == 0, "los {borrados_ren} renombrados que ya estaban borrados no se atoran en la bandeja ({borrados_en_bandeja})");
    let borrados_mal: Vec<String> = renombrados
        .iter()
        .filter(|f| f[3] != "true")
        .filter(|f| {
            d_foto(&db, &format!("SELECT codigo, deleted_at IS NOT NULL, activo FROM productos WHERE uuid = '{}'", f[1]))
                != vec![format!("Text({:?})|Integer(1)|Integer(0)|", f[2])]
        })
        .map(|f| format!("{} {}", f[0], f[2]))
        .collect();
    chk!(inf, "i2", borrados_mal.is_empty(),
         "los {borrados_ren} renombrados ya borrados en la web están aquí borrados con su código nuevo; mal: {borrados_mal:?}");
    let movs = d_foto(&db, "SELECT uuid, monto, caja, origen, registrado_at, usuario_id FROM movimientos_caja ORDER BY id");
    println!("  movimientos en el escritorio: {movs:?}");
    let devs = d_foto(&db, "SELECT uuid, venta_id, total_devuelto FROM devoluciones ORDER BY id");
    println!("  devoluciones en el escritorio: {devs:?}");

    // ── pull normal posterior: nada nuevo ──
    let antes = d_i64(&db, "SELECT (SELECT count(*) FROM ventas) + (SELECT count(*) FROM productos) + (SELECT count(*) FROM movimientos_caja)");
    let _ = esc.pull().await;
    let despues_pull = d_i64(&db, "SELECT (SELECT count(*) FROM ventas) + (SELECT count(*) FROM productos) + (SELECT count(*) FROM movimientos_caja)");
    chk!(inf, "f2", antes == despues_pull, "pull normal después: nada nuevo ({antes} → {despues_pull})");
    chk!(inf, "f2", esc.pendientes_outbox() == 0, "nada de lo recibido regresa al outbox: {:?}",
         d_foto(&db, "SELECT tabla, uuid, operacion, created_at FROM sync_outbox WHERE synced_at IS NULL"));

    // i2) venta web espejo de un producto WEB cuyo código chocaba con uno del
    //     escritorio (renombrado por la 011; en producción hay varios activos
    //     con stock): llega y cuenta en el período abierto.
    let dup = pg.filas(&format!(
        "SELECT p.id, p.uuid, p.precio_venta::float8, p.codigo FROM productos p \
          WHERE p.uuid !~ {HEX32} AND p.deleted_at IS NULL AND p.activo = 1 AND p.precio_venta > 0 AND p.stock_actual >= 1 \
            AND p.codigo ~ ('-W' || p.id || '$') \
          ORDER BY p.stock_actual DESC LIMIT 1"
    ));
    if let Some(dp) = dup.first() {
        let local_dup = d_opt_i64(&db, &format!("SELECT id FROM productos WHERE uuid = '{}'", dp[1]));
        println!("  producto web {} ({}) renombrado por la 011: local {local_dup:?}", dp[0], dp[3]);
        let antes_i2 = esc.periodo(&mas_segundos(&cortes::ahora_local(), 2));
        let rv = venta_web(&web, &tok_lau, &[(dp[0].parse().unwrap(), 1.0, dp[2].parse().unwrap())]).await;
        chk!(inf, "i2", rv.is_ok(), "venta web espejo del producto {} (código {}): {rv:?}", dp[0], dp[3]);
        let total_i2 = rv.as_ref().map(|v| v["total"].as_f64().unwrap_or(0.0)).unwrap_or(0.0);
        let wu = pg.uno(&format!("SELECT uuid FROM ventas WHERE id = {}", rv.as_ref().map(|v| v["id"].as_i64().unwrap_or(0)).unwrap_or(0))).unwrap_or_default();
        let _ = esc.pull().await;
        let llego = d_i64(&db, &format!("SELECT count(*) FROM ventas WHERE uuid = '{wu}'"));
        let motivo = d_txt(&db, &format!("SELECT motivo || ': ' || COALESCE(error, '') FROM sync_inbox WHERE uuid = '{wu}'"));
        chk!(inf, "i2", llego == 1,
             "venta espejo de un producto web renombrado llega al escritorio (llegó {llego}; bandeja: {motivo:?})");
        let det = d_foto(&db, &format!(
            "SELECT vd.producto_id FROM venta_detalle vd JOIN ventas v ON v.id = vd.venta_id WHERE v.uuid = '{wu}'"
        ));
        chk!(inf, "i2", local_dup.map(|l| det == vec![format!("Integer({l})|")]).unwrap_or(false),
             "su renglón apunta al producto LOCAL ({det:?} vs {local_dup:?})");
        let despues_i2 = esc.periodo(&mas_segundos(&cortes::ahora_local(), 2));
        chk!(inf, "i2", despues_i2.num_transacciones == antes_i2.num_transacciones + 1
                       && cerca(despues_i2.total_ventas_efectivo - antes_i2.total_ventas_efectivo, total_i2),
             "y cuenta en el período abierto (+{} de {total_i2}, {} → {} ventas)",
             despues_i2.total_ventas_efectivo - antes_i2.total_ventas_efectivo, antes_i2.num_transacciones, despues_i2.num_transacciones);
    } else {
        chk!(inf, "i2", false, "no hay producto web renombrado activo con stock para vender");
    }

    inf.cerrar("e2e_sync_v2_reparacion_tienda_real");
}
