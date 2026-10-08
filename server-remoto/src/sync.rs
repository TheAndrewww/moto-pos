// sync.rs — Endpoints del sync escritorio ↔ servidor (protocolo v2).
//
//   POST /sync/push   { device_uuid, sucursal_id, proto?, cambios: [{tabla, operacion, data,
//                       children?, refs?, base?}] }
//                     → { aceptados: [uuid], rechazados: [{uuid, motivo}], faltantes?: [uuid],
//                         marcas: {uuid: updated_at guardado aquí} }
//   GET  /sync/pull?cursor=&sucursal_id=&device_uuid=&proto=
//                     → { cambios: [{__tabla, __operacion, __data, __children?, __refs?,
//                         __origen, __cursor}], next_cursor, hay_mas, caja_v2_desde }
//   POST /sync/id-map { device_uuid, tabla, pares: [[id_local, uuid]] } → { recibidos, desconocidos }
//   POST /sync/reparar { device_uuid, simulacion?, revertir_lote? } → reporte
//   POST /sync/refs   { tabla, uuids } → { uuid: {columna: uuid | null} }
//   GET  /sync/fila?tabla=&uuid= → un cambio con la forma del pull v2
//
// IDENTIDAD: cada base numera sus filas por su cuenta; las FK enteras solo
// significan algo del lado que las escribió. En el push v2 se traducen a ids
// de aquí (por los uuids de `refs` o por el mapa `sync_id_map`); si la
// referencia aún no llega, una fila nueva queda con centinela (NULL/0) y una
// que ya existe conserva lo guardado; en ambos casos con un pendiente que
// resolver_pendientes corrige solo si la celda no cambió. En el pull v2 viajan
// como uuids en `__refs`, solo cuando la fila ya está en "idioma del
// servidor" y nunca las celdas pendientes (ver sync_fk.rs).
//
// ESCRITORIO VIEJO (push sin `proto`): sus filas sin estado se guardan crudas
// como antes de v2 (la reparación las traduce); las que ya tienen estado se
// traducen por el mapa (sin entrada → se conserva lo guardado); a las filas
// web no se les tocan las FK (el escritorio viejo trae ids del servidor).
//
// LWW: marcas `updated_at` normalizadas (19 caracteres, 'T' → ' '), con tope
// en la hora local del servidor; se rechaza solo si lo guardado es
// estrictamente más nuevo Y lo escribió otro dispositivo (si fue este mismo
// escritorio, su fila actual se aplica). La respuesta trae en `marcas` la
// marca con la que quedó guardada cada fila aceptada (el reloj de aquí), para
// que el escritorio compare con UN solo reloj.
//
// productos con `base` (lo último en que escritorio y servidor coincidieron),
// si lo último de aquí no lo escribió este mismo escritorio: combinación a
// tres bandas decidida por CONTENIDO (stock por diferencia, cada columna gana
// de quien la cambió), nunca comparando marcas de dos relojes. Si el servidor
// no tuvo cambios propios desde la base, es un push normal; si sí, se guarda
// la combinación con la hora de aquí y cursor 'merge' (en el push normal la
// marca guardada nunca retrocede). Estos pushes son idempotentes: la huella
// (fila + base) se anota en sync_push_aplicado y un reenvío idéntico
// (respuesta perdida, ciclos encimados) se acepta sin escribir, para no
// descontar la misma venta dos veces. Si el reintento trae OTRO contenido
// sobre la misma base (respuesta perdida y otra venta antes del reintento),
// la base efectiva es la fila que ya se aplicó (anotada junto a la huella).
//
// Seguridad: solo el token del escritorio (/auth/login: rol admin, sin claim
// de dispositivo, de un usuario activo del panel) entra a /sync/*. Tabla
// validada contra la lista blanca y columnas validadas contra el esquema real
// de Postgres antes de armar SQL.

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::auth::{autenticar, Claims};
use crate::error::{ApiError, ApiResult};
use crate::sync_fk::{self, Lww};
use crate::sync_fk_map::{hijos_de, orden_de};
use crate::AppState;

const PULL_BATCH: i64 = 200;
const MAX_PARES_ID_MAP: usize = 5000;
const MAX_UUIDS_REFS: usize = 500;

// -----------------------------------------------------------------------------
// Whitelist de tablas sincronizables.
// -----------------------------------------------------------------------------

const TABLAS_SYNC: &[&str] = &[
    // Catálogo
    "sucursales", "categorias", "proveedores", "productos", "clientes", "usuarios",
    // Ya no se sincroniza (nadie la lee): el push la acepta sin hacer nada y
    // el pull nunca la manda. Sigue aquí para drenar outboxes viejos.
    "stock_sucursal",
    // Operacionales (padres)
    "ventas", "recepciones", "ordenes_pedido", "presupuestos", "cortes",
    "devoluciones", "transferencias",
    "movimientos_caja", "aperturas_caja",
    // Bitácora bidireccional (migración 006). El desktop la empuja con
    // origen='POS', el web genera entradas con origen='WEB'.
    "audit_log",
    // Hijos (llegan bajo `children` de su padre)
    "venta_detalle", "recepcion_detalle", "orden_pedido_detalle",
    "presupuesto_detalle", "corte_denominaciones", "corte_vendedores",
    "devolucion_detalle", "transferencia_detalle",
];

fn tabla_valida(t: &str) -> bool {
    TABLAS_SYNC.contains(&t)
}

/// Tablas que tienen columna `sucursal_id NOT NULL` en Postgres pero que el POS
/// SQLite puede no incluir en la fila (esquema legacy de sucursal única).
/// El push inyecta `sucursal_id` desde el body del dispositivo cuando falta.
const TABLAS_CON_SUCURSAL: &[&str] = &[
    "ventas", "recepciones", "ordenes_pedido", "presupuestos",
    "cortes", "movimientos_caja", "aperturas_caja", "devoluciones",
];

/// Usuario de la web que nunca baja al escritorio ('Andrew Web').
fn es_usuario_web_admin(tabla: &str, uuid: &str) -> bool {
    tabla == "usuarios" && uuid.starts_with("web-admin-")
}

// -----------------------------------------------------------------------------
// Autorización
// -----------------------------------------------------------------------------

/// Solo el token del POS de escritorio, emitido por /auth/login: rol 'admin',
/// sin claim de dispositivo, y `sub`/`email` de un usuario ACTIVO del panel
/// (admin_users). Los tokens de sesión web (rol 'device', admin con
/// dispositivo, o un login por PIN sin dispositivo: su sub/email son de
/// `usuarios`) reciben 403. El token de 100 años que ya tiene la tienda pasa.
pub async fn autenticar_sync(headers: &HeaderMap, state: &AppState) -> ApiResult<Claims> {
    let claims = autenticar(headers, &state.jwt_secret)?;
    if !token_de_sync(&claims) || !es_usuario_del_panel(&state.pool, &claims).await? {
        return Err(ApiError::Forbidden);
    }
    Ok(claims)
}

pub fn token_de_sync(claims: &Claims) -> bool {
    claims.role == "admin" && claims.device.is_none()
}

/// ¿El token lo emitió /auth/login? (sub = admin_users.id y email suyo, activo)
pub async fn es_usuario_del_panel(pool: &PgPool, claims: &Claims) -> ApiResult<bool> {
    let ok: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM admin_users \
                         WHERE id = $1 AND lower(email) = lower($2) AND activo)",
    )
    .bind(claims.sub)
    .bind(&claims.email)
    .fetch_one(pool)
    .await?;
    Ok(ok)
}

// -----------------------------------------------------------------------------
// PUSH
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PushBody {
    pub device_uuid: String,
    pub sucursal_id: i64,
    /// Versión del protocolo (2 = refs por uuid). Ausente en escritorios viejos.
    #[serde(default)]
    pub proto: Option<i32>,
    pub cambios: Vec<CambioIn>,
}

#[derive(Debug, Deserialize)]
pub struct CambioIn {
    pub tabla: String,
    pub operacion: String,
    pub data: Value,
    #[serde(default)]
    pub children: Option<Value>,
    /// {uuid_fila: {columna_fk: uuid_referido | null}} del padre y sus hijos.
    #[serde(default)]
    pub refs: Option<Value>,
    /// Solo productos (v2): la fila como estaba la última vez que el
    /// escritorio y el servidor coincidieron (sync_base del escritorio).
    #[serde(default)]
    pub base: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct PushResponse {
    pub aceptados: Vec<String>,
    pub rechazados: Vec<Rechazado>,
    /// Informativo: uuids referidos que todavía no existen aquí.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub faltantes: Vec<String>,
    /// uuid de cada fila padre aceptada (también las aceptadas sin escribir)
    /// → `updated_at` con el que quedó guardada aquí, 'YYYY-MM-DD HH:MM:SS'.
    /// Los escritorios viejos lo ignoran.
    pub marcas: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct Rechazado {
    pub uuid: String,
    pub motivo: String,
}

pub async fn push(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PushBody>,
) -> ApiResult<Json<PushResponse>> {
    autenticar_sync(&headers, &state).await?;
    procesar_push(&state.pool, &body).await.map(Json)
}

/// Lógica del push (separada del handler para poder probarla).
pub async fn procesar_push(pool: &PgPool, body: &PushBody) -> ApiResult<PushResponse> {
    registrar_device(pool, &body.device_uuid, body.sucursal_id, body.proto).await?;
    let esquema = sync_fk::esquema(pool).await?;

    // Orden estable por dependencias: lo referido se aplica antes.
    let mut cambios: Vec<&CambioIn> = body.cambios.iter().collect();
    cambios.sort_by_key(|c| orden_de(&c.tabla));

    // Pase 1: registrar id local → uuid de todo el lote (padres e hijos).
    if let Err(e) = registrar_ids_lote(pool, &body.device_uuid, &cambios).await {
        tracing::warn!("push: no se pudo registrar el mapa de ids: {:?}", e);
    }

    let mut aceptados: Vec<String> = Vec::new();
    let mut rechazados: Vec<Rechazado> = Vec::new();
    let mut faltantes: Vec<String> = Vec::new();
    let mut marcas: BTreeMap<String, String> = BTreeMap::new();

    for cambio in cambios {
        if !tabla_valida(&cambio.tabla) {
            rechazados.push(Rechazado {
                uuid: extraer_uuid(&cambio.data).unwrap_or_default(),
                motivo: format!("tabla desconocida: {}", cambio.tabla),
            });
            continue;
        }
        let uuid = match extraer_uuid(&cambio.data) {
            Some(u) => u,
            None => {
                rechazados.push(Rechazado { uuid: String::new(), motivo: "falta uuid".into() });
                continue;
            }
        };

        match aplicar_cambio(pool, esquema, cambio, &body.device_uuid, body.sucursal_id, body.proto).await {
            Ok(Resultado::Aceptado { faltantes: f, marca }) => {
                if let Some(m) = marca {
                    marcas.insert(uuid.clone(), m);
                }
                aceptados.push(uuid);
                for u in f {
                    if !faltantes.contains(&u) {
                        faltantes.push(u);
                    }
                }
            }
            Ok(Resultado::Rechazado(motivo)) => rechazados.push(Rechazado { uuid, motivo }),
            Err(e) => {
                tracing::warn!("push error tabla={} uuid={}: {:?}", cambio.tabla, uuid, e);
                rechazados.push(Rechazado { uuid, motivo: format!("error: {}", e) });
            }
        }
    }

    // Celdas que esperaban una fila de este mismo lote (o de lotes previos).
    if let Err(e) = sync_fk::resolver_pendientes(pool).await {
        tracing::warn!("push: resolver_pendientes falló: {:?}", e);
    }

    // Huellas de más de 30 días: un reenvío tan tardío ya no llega.
    if let Err(e) = podar_huellas(pool).await {
        tracing::warn!("push: no se pudieron podar las huellas viejas: {:?}", e);
    }

    sqlx::query("UPDATE pos_devices SET last_push_at = now() WHERE device_uuid = $1")
        .bind(&body.device_uuid)
        .execute(pool)
        .await?;

    Ok(PushResponse { aceptados, rechazados, faltantes, marcas })
}

#[derive(Debug, PartialEq)]
enum Resultado {
    /// Aceptado; lleva los uuids referidos que faltan aquí (informativo) y la
    /// marca con la que quedó guardada la fila (None si la tabla no tiene
    /// updated_at o la fila no existe).
    Aceptado { faltantes: Vec<String>, marca: Option<String> },
    Rechazado(String),
}

impl Resultado {
    fn aceptado(marca: Option<String>) -> Self {
        Resultado::Aceptado { faltantes: vec![], marca }
    }
}

/// Días que se guarda la huella de un push ya aplicado.
const DIAS_HUELLA: i32 = 30;

async fn podar_huellas(pool: &PgPool) -> ApiResult<()> {
    sqlx::query("DELETE FROM sync_push_aplicado WHERE created_at < now() - make_interval(days => $1)")
        .bind(DIAS_HUELLA)
        .execute(pool)
        .await?;
    Ok(())
}

/// Huella de un push de productos con `base`: md5 de la fila entrante sin
/// id/synced_at || md5 de la base, en JSON canónico. Guarda también los dos
/// textos: al anotarla se recuerda QUÉ se aplicó (la fila) y sobre qué base.
struct HuellaPush {
    huella: String,
    /// Fila entrante (sin id/synced_at) en JSON canónico.
    t_data: String,
    /// Base que mandó el escritorio, en JSON canónico.
    t_base: String,
}

/// Huella del push y si este dispositivo ya lo aplicó.
async fn huella_del_push(
    tx: &mut Transaction<'_, Postgres>,
    device_uuid: &str,
    tabla: &str,
    uuid: &str,
    data: &Value,
    base: &Map<String, Value>,
) -> ApiResult<(HuellaPush, bool)> {
    let (t_data, t_base) = sync_fk::textos_huella(data, base);
    let r = sqlx::query(
        "WITH h AS (SELECT md5($4) || md5($5) AS huella) \
         SELECT h.huella, EXISTS (SELECT 1 FROM sync_push_aplicado a \
                                   WHERE a.device_uuid = $1 AND a.tabla = $2 AND a.uuid = $3 \
                                     AND a.huella = h.huella) \
           FROM h",
    )
    .bind(device_uuid)
    .bind(tabla)
    .bind(uuid)
    .bind(&t_data)
    .bind(&t_base)
    .fetch_one(&mut **tx)
    .await?;
    Ok((HuellaPush { huella: r.get::<String, _>(0), t_data, t_base }, r.get::<bool, _>(1)))
}

/// Anota el push aplicado: su huella, el md5 de la base sobre la que llegó y
/// la fila entrante (`datos`), que sirve de base efectiva si la respuesta se
/// pierde y el escritorio reintenta OTRO contenido con la misma base vieja.
async fn anotar_huella(
    tx: &mut Transaction<'_, Postgres>,
    device_uuid: &str,
    tabla: &str,
    uuid: &str,
    h: &HuellaPush,
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO sync_push_aplicado (device_uuid, tabla, uuid, huella, base_md5, datos) \
         VALUES ($1, $2, $3, $4, md5($5), $6::jsonb) \
         ON CONFLICT (device_uuid, tabla, uuid, huella) DO NOTHING",
    )
    .bind(device_uuid)
    .bind(tabla)
    .bind(uuid)
    .bind(&h.huella)
    .bind(&h.t_base)
    .bind(&h.t_data)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Base efectiva de un push de productos (SPEC4 S1). Si lo ÚLTIMO que se
/// aplicó de este dispositivo para esta fila llegó sobre la MISMA base que
/// trae este push, el escritorio no recibió esa respuesta (si la hubiera
/// recibido, su base ya sería otra): el servidor ya incorporó esa fila, así
/// que la diferencia de este push se mide contra ella y no contra la base
/// vieja (si no, la venta anterior se restaría dos veces). En cadenas de
/// respuestas perdidas cada push aplicado es la base del siguiente.
///
/// Se mira solo el último push del dispositivo (no "el último sobre esa
/// base"): si después ya aplicó uno sobre otra base, el escritorio avanzó, y
/// una base vieja que vuelve (restauró un respaldo) se usa tal cual llega.
/// Las huellas anteriores a la migración 013 no tienen `datos` → base tal cual.
async fn base_efectiva(
    tx: &mut Transaction<'_, Postgres>,
    device_uuid: &str,
    tabla: &str,
    uuid: &str,
    h: &HuellaPush,
) -> ApiResult<Option<Map<String, Value>>> {
    let texto: Option<Option<String>> = sqlx::query_scalar(
        "SELECT CASE WHEN u.base_md5 = md5($4) THEN u.datos::text END \
           FROM (SELECT base_md5, datos FROM sync_push_aplicado \
                  WHERE device_uuid = $1 AND tabla = $2 AND uuid = $3 \
                  ORDER BY id DESC LIMIT 1) u",
    )
    .bind(device_uuid)
    .bind(tabla)
    .bind(uuid)
    .bind(&h.t_base)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(t) = texto.flatten() else { return Ok(None) };
    match serde_json::from_str::<Value>(&t) {
        Ok(Value::Object(m)) => Ok(Some(m)),
        _ => {
            tracing::warn!("push {tabla} {uuid}: datos del push anterior ilegibles; se usa la base que llegó");
            Ok(None)
        }
    }
}

/// `updated_at` con el que está guardada la fila aquí, normalizado.
async fn marca_guardada(
    tx: &mut Transaction<'_, Postgres>,
    tabla: &str,
    cols: &sync_fk::ColumnasTabla,
    uuid: &str,
) -> ApiResult<Option<String>> {
    if !cols.contains_key("updated_at") {
        return Ok(None);
    }
    let t: Option<Option<String>> =
        sqlx::query_scalar(&format!("SELECT updated_at::text FROM {tabla} WHERE uuid = $1"))
            .bind(uuid)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(t.flatten().map(|s| sync_fk::normalizar_ts(&s)).filter(|s| !s.is_empty()))
}

fn marca_de(fila: Option<&Map<String, Value>>) -> Option<String> {
    fila.and_then(|g| g.get("updated_at"))
        .and_then(|v| v.as_str())
        .map(sync_fk::normalizar_ts)
        .filter(|s| !s.is_empty())
}

/// refs del cambio: campo `refs`, o `data.__refs` si el cliente no lo movió.
fn refs_del_cambio(cambio: &CambioIn) -> Option<Map<String, Value>> {
    cambio
        .refs
        .as_ref()
        .and_then(|v| v.as_object())
        .or_else(|| cambio.data.get("__refs").and_then(|v| v.as_object()))
        .cloned()
}

fn refs_de_fila<'a>(refs: Option<&'a Map<String, Value>>, uuid: &str) -> Option<&'a Map<String, Value>> {
    refs?.get(uuid)?.as_object()
}

/// base del cambio (solo productos): campo `base`, o `data.__base`.
fn base_del_cambio(cambio: &CambioIn) -> Option<Map<String, Value>> {
    cambio
        .base
        .as_ref()
        .and_then(|v| v.as_object())
        .or_else(|| cambio.data.get("__base").and_then(|v| v.as_object()))
        .cloned()
}

/// La fila como está aquí, bloqueada hasta el final de la transacción (la
/// combinación de productos y el LWW deciden con ella; nadie la cambia en
/// medio).
async fn leer_guardada(
    tx: &mut Transaction<'_, Postgres>,
    tabla: &str,
    uuid: &str,
) -> ApiResult<Option<Map<String, Value>>> {
    let texto: Option<String> =
        sqlx::query_scalar(&format!("SELECT row_to_json(t)::text FROM {tabla} t WHERE uuid = $1 FOR UPDATE"))
            .bind(uuid)
            .fetch_optional(&mut **tx)
            .await?;
    match texto {
        None => Ok(None),
        Some(t) => serde_json::from_str(&t)
            .map(Some)
            .map_err(|e| ApiError::Other(anyhow::anyhow!("row_to_json de {tabla} {uuid}: {e}"))),
    }
}

/// Hijos que ya existen aquí, por uuid (de cualquier padre).
async fn leer_hijos_guardados(
    tx: &mut Transaction<'_, Postgres>,
    tabla_hijo: &str,
    uuids: &[String],
) -> ApiResult<HashMap<String, Map<String, Value>>> {
    let mut out = HashMap::new();
    if uuids.is_empty() {
        return Ok(out);
    }
    let textos: Vec<String> =
        sqlx::query_scalar(&format!("SELECT row_to_json(t)::text FROM {tabla_hijo} t WHERE uuid = ANY($1)"))
            .bind(uuids)
            .fetch_all(&mut **tx)
            .await?;
    for t in textos {
        let m: Map<String, Value> = serde_json::from_str(&t)
            .map_err(|e| ApiError::Other(anyhow::anyhow!("row_to_json de {tabla_hijo}: {e}")))?;
        if let Some(u) = m.get("uuid").and_then(|v| v.as_str()) {
            out.insert(u.to_string(), m.clone());
        }
    }
    Ok(out)
}

/// Pase 1 del push: (device, tabla, id local) → uuid para padres e hijos.
async fn registrar_ids_lote(pool: &PgPool, device_uuid: &str, cambios: &[&CambioIn]) -> ApiResult<()> {
    let mut por_tabla: HashMap<&str, Vec<(i64, String)>> = HashMap::new();
    for c in cambios {
        if !tabla_valida(&c.tabla) || c.tabla == "stock_sucursal" || c.operacion == "DELETE" {
            continue;
        }
        if let (Some(id), Some(u)) = (c.data.get("id").and_then(sync_fk::valor_entero), extraer_uuid(&c.data)) {
            por_tabla.entry(c.tabla.as_str()).or_default().push((id, u));
        }
        let Some(children) = c.children.as_ref().and_then(|v| v.as_object()) else { continue };
        for (h, _) in hijos_de(&c.tabla) {
            let Some(arr) = children.get(*h).and_then(|v| v.as_array()) else { continue };
            for hijo in arr {
                if let (Some(id), Some(u)) = (hijo.get("id").and_then(sync_fk::valor_entero), extraer_uuid(hijo)) {
                    por_tabla.entry(h).or_default().push((id, u));
                }
            }
        }
    }
    if por_tabla.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(sync_fk::LLAVE_REPARACION)
        .execute(&mut *tx)
        .await?;
    for (tabla, pares) in por_tabla {
        sync_fk::registrar_pares(&mut tx, device_uuid, tabla, &pares, "push").await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn aplicar_cambio(
    pool: &PgPool,
    esquema: &sync_fk::EsquemaPg,
    cambio: &CambioIn,
    device_uuid: &str,
    sucursal_id: i64,
    proto: Option<i32>,
) -> ApiResult<Resultado> {
    let tabla = cambio.tabla.as_str();
    let uuid = extraer_uuid(&cambio.data).ok_or_else(|| ApiError::BadRequest("falta uuid".into()))?;
    let v2 = proto.unwrap_or(0) >= 2;

    // stock_sucursal salió del sync (nadie la lee): se acepta sin hacer nada
    // para que los escritorios viejos vacíen su cola.
    if tabla == "stock_sucursal" {
        return Ok(Resultado::aceptado(None));
    }
    let cols = esquema
        .columnas(tabla)
        .ok_or_else(|| ApiError::BadRequest(format!("tabla sin esquema en Postgres: {tabla}")))?;

    let mut tx = pool.begin().await?;
    // Compartido contra la reparación de FK (que toma el exclusivo).
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(sync_fk::LLAVE_REPARACION)
        .execute(&mut *tx)
        .await?;
    // `sync.origin` = device_uuid para que el trigger de audit_log (migración
    // 006) registre la fila en sync_cursor con el origen correcto.
    sqlx::query("SELECT set_config('sync.origin', $1, true)")
        .bind(device_uuid)
        .execute(&mut *tx)
        .await?;
    let ahora: String = sqlx::query_scalar(sync_fk::SQL_AHORA_LOCAL).fetch_one(&mut *tx).await?;

    if cambio.operacion == "DELETE" {
        // Borrado lógico con la hora local. Tabla sin deleted_at → nada.
        if !cols.contains_key("deleted_at") {
            let marca = marca_guardada(&mut tx, tabla, cols, &uuid).await?;
            tx.rollback().await.ok();
            return Ok(Resultado::aceptado(marca));
        }
        let sql = if cols.contains_key("updated_at") {
            format!("UPDATE {tabla} SET deleted_at = $2, updated_at = $2 WHERE uuid = $1")
        } else {
            format!("UPDATE {tabla} SET deleted_at = $2 WHERE uuid = $1")
        };
        sqlx::query(&sql).bind(&uuid).bind(&ahora).execute(&mut *tx).await?;
        insertar_cursor(&mut tx, tabla, &uuid, sucursal_id, device_uuid).await?;
        let marca = marca_guardada(&mut tx, tabla, cols, &uuid).await?;
        tx.commit().await?;
        return Ok(Resultado::aceptado(marca));
    }

    let mut data: Map<String, Value> = cambio
        .data
        .as_object()
        .cloned()
        .ok_or_else(|| ApiError::BadRequest("data no es objeto".into()))?;

    // ── La fila como está aquí y quién la escribió por última vez ──
    let guardada = leer_guardada(&mut tx, tabla, &uuid).await?;
    let ultimo_origen: Option<String> = if guardada.is_some() {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT origen_device FROM sync_cursor WHERE tabla = $1 AND uuid = $2 ORDER BY id DESC LIMIT 1",
        )
        .bind(tabla)
        .bind(&uuid)
        .fetch_optional(&mut *tx)
        .await?
        .flatten()
    } else {
        None
    };
    let mismo_dispositivo = ultimo_origen.as_deref() == Some(device_uuid);
    let entrante = data.get("updated_at").and_then(|v| v.as_str()).map(|s| s.to_string());

    // ── productos con `base` (v2): idempotente. Un reenvío idéntico (la
    //    respuesta se perdió, o dos ciclos empujaron lo mismo) se acepta sin
    //    escribir: aplicarlo otra vez restaría de nuevo la diferencia de stock.
    //    (La fila ya está bloqueada: dos envíos iguales a la vez se forman.) ──
    let mut base = if tabla == "productos" && v2 { base_del_cambio(cambio) } else { None };
    let huella = match &base {
        Some(b) => {
            let (h, ya_aplicado) = huella_del_push(&mut tx, device_uuid, tabla, &uuid, &cambio.data, b).await?;
            if ya_aplicado {
                tx.rollback().await.ok();
                tracing::info!("push {tabla} {uuid}: reenvío idéntico ya aplicado; se acepta sin escribir");
                return Ok(Resultado::aceptado(marca_de(guardada.as_ref())));
            }
            Some(h)
        }
        None => None,
    };

    // ── ¿Combinación a tres bandas? Con `base`, si lo último de aquí no lo
    //    escribió este mismo escritorio. Si hubo cambio del servidor desde la
    //    base se decide por CONTENIDO más abajo (nunca con marcas de dos
    //    relojes); aquí no se rechaza por LWW. ──
    let tres_vias = guardada.is_some() && base.is_some() && !mismo_dispositivo;

    // ── Base efectiva: si ya se aplicó un push de este escritorio sobre esta
    //    misma base y su respuesta se perdió, la base real es lo que se aplicó
    //    (el reintento trae otra venta encima; la anterior ya está aquí). ──
    if tres_vias {
        if let Some(h) = &huella {
            if let Some(efectiva) = base_efectiva(&mut tx, device_uuid, tabla, &uuid, h).await? {
                tracing::info!(
                    "push {tabla} {uuid}: ya se aplicó un push de este equipo sobre la misma base (respuesta perdida); \
                     se combina contra lo aplicado"
                );
                base = Some(efectiva);
            }
        }
    }
    let mut origen_cursor = device_uuid;
    if !tres_vias {
        // ── LWW ──
        let guardado_ts = if cols.contains_key("updated_at") {
            guardada.as_ref().and_then(|g| g.get("updated_at")).and_then(|v| v.as_str())
        } else {
            None
        };
        match sync_fk::decidir_lww(guardado_ts, entrante.as_deref(), &ahora) {
            Lww::Aplicar(Some(ts)) => {
                data.insert("updated_at".into(), Value::String(ts));
            }
            Lww::Aplicar(None) => {}
            Lww::Rechazar if mismo_dispositivo => {
                // Lo más nuevo de aquí lo escribió este mismo escritorio: él
                // siempre manda su fila actual, así que no es contenido más
                // viejo (su reloj se atrasó, o una marca UTC que se recortó).
                if let Some(ts) = sync_fk::marca_efectiva(entrante.as_deref(), &ahora) {
                    data.insert("updated_at".into(), Value::String(ts));
                }
            }
            Lww::Rechazar => {
                if tabla == "productos"
                    && merge_productos(&mut tx, cols, &uuid, &data, sucursal_id, &ahora).await?
                {
                    if let Some(h) = &huella {
                        anotar_huella(&mut tx, device_uuid, tabla, &uuid, h).await?;
                    }
                    let marca = marca_guardada(&mut tx, tabla, cols, &uuid).await?;
                    tx.commit().await?;
                    return Ok(Resultado::aceptado(marca));
                }
                tx.rollback().await.ok();
                return Ok(Resultado::Rechazado("remoto_mas_nuevo".into()));
            }
        }
    }

    // Parchar sucursal_id si la tabla lo requiere y el POS no lo envió.
    if TABLAS_CON_SUCURSAL.contains(&tabla) && data.get("sucursal_id").map(|v| v.is_null()).unwrap_or(true) {
        data.insert("sucursal_id".into(), json!(sucursal_id));
    }

    // ── Quién hizo la fila y en qué idioma están sus FK aquí ──
    let es_esc = sync_fk::es_fila_escritorio(tabla, &uuid, guardada.as_ref().unwrap_or(&data));
    let traducibles = sync_fk::tiene_fks_traducibles(tabla);
    let estado = if traducibles && guardada.is_some() {
        sync_fk::estado_de_fila(&mut tx, tabla, &uuid).await?
    } else {
        None
    };
    let mut politica = sync_fk::politica_de(v2, es_esc, estado.as_deref());

    // ── Hijos que llegan, y los que ya existen aquí ──
    let refs = refs_del_cambio(cambio);
    let children = cambio.children.as_ref().and_then(|v| v.as_object());
    let mut hijos_entrantes: Vec<(&'static str, &'static str, Vec<Map<String, Value>>)> = Vec::new();
    for (h, fk) in hijos_de(tabla) {
        let Some(arr) = children.and_then(|c| c.get(*h)).and_then(|v| v.as_array()) else { continue };
        let mut filas = Vec::with_capacity(arr.len());
        for x in arr {
            match x.as_object() {
                Some(o) if o.get("uuid").and_then(|v| v.as_str()).is_some() => filas.push(o.clone()),
                _ => tracing::warn!("push {tabla} {uuid}: hijo de {h} sin uuid; se ignora"),
            }
        }
        hijos_entrantes.push((h, fk, filas));
    }
    let mut hijos_guardados: HashMap<&'static str, HashMap<String, Map<String, Value>>> = HashMap::new();
    let mut uuids_hijos: Vec<String> = Vec::new();
    for (h, _, filas) in &hijos_entrantes {
        let us: Vec<String> =
            filas.iter().filter_map(|f| f.get("uuid").and_then(|v| v.as_str()).map(|s| s.to_string())).collect();
        hijos_guardados.insert(h, leer_hijos_guardados(&mut tx, h, &us).await?);
        uuids_hijos.extend(us);
    }

    // Fila 'sin_mapa' de un escritorio viejo: celdas que la reparación ya tradujo.
    let mut reparadas: HashMap<(String, String), HashSet<String>> = HashMap::new();
    if let sync_fk::Politica::SinMapa { traducidas } = &mut politica {
        let mut us = uuids_hijos.clone();
        us.push(uuid.clone());
        reparadas = sync_fk::celdas_reparadas(&mut tx, &us).await?;
        *traducidas = reparadas.get(&(tabla.to_string(), uuid.clone())).cloned().unwrap_or_default();
    }

    // Celdas que escribió la web en esta fila del escritorio (o en sus hijos)
    // mientras no estaba reparada (sync_fk_celda_web).
    let celdas_web = if traducibles && es_esc && guardada.is_some() {
        let mut us: Vec<String> = hijos_guardados.values().flat_map(|m| m.keys().cloned()).collect();
        us.push(uuid.clone());
        sync_fk::cargar_celdas_web(&mut *tx, &us).await?
    } else {
        HashMap::new()
    };
    let marcadas = |t: &str, u: &str| celdas_web.get(&(t.to_string(), u.to_string()));

    // ── Búsquedas de FK (padre + hijos [+ base]) en lote ──
    let mut pedidos = sync_fk::recolectar_pedidos(tabla, &data, refs_de_fila(refs.as_ref(), &uuid));
    for (h, _, filas) in &hijos_entrantes {
        for f in filas {
            let hu = f.get("uuid").and_then(|v| v.as_str()).unwrap_or_default();
            pedidos.extend(sync_fk::recolectar_pedidos(h, f, refs_de_fila(refs.as_ref(), hu)));
        }
    }
    if tres_vias {
        if let Some(b) = &base {
            // Las FK de la base son ids del escritorio: por el mapa.
            pedidos.extend(sync_fk::recolectar_pedidos(tabla, b, None));
        }
    }
    let busq = sync_fk::cargar_busquedas(&mut tx, device_uuid, &pedidos).await?;

    // ── Combinación a tres bandas (productos), por contenido ──
    let mut omitir: HashSet<String> = HashSet::new();
    if let (true, Some(g), Some(b)) = (tres_vias, guardada.as_ref(), base.as_ref()) {
        let en_idioma = sync_fk::en_significado_servidor(es_esc, estado.as_deref());
        let sin_marcas = HashSet::new();
        let celdas_web_fila = if en_idioma { &sin_marcas } else { marcadas(tabla, &uuid).unwrap_or(&sin_marcas) };
        let entrante_ts = sync_fk::marca_efectiva(entrante.as_deref(), &ahora).unwrap_or_default();
        let mut combinada = data.clone();
        let r = sync_fk::combinar_tres_vias(
            tabla,
            &mut combinada,
            &sync_fk::Combinacion {
                guardada: g,
                base: b,
                cols,
                busq: &busq,
                guardada_en_idioma_servidor: en_idioma,
                celdas_web: celdas_web_fila,
                entrante_ts: &entrante_ts,
            },
        );
        if r.servidor_aporta {
            // El servidor cambió algo desde la base: se guarda la combinación
            // con la hora de aquí y el escritorio la baja (cursor 'merge').
            data = combinada;
            omitir = r.omitir;
            data.insert("updated_at".into(), Value::String(ahora.clone()));
            origen_cursor = "merge";
        } else if !entrante_ts.is_empty() {
            // Sin cambios propios del servidor desde la base (aunque lo último
            // lo haya escrito otro): push normal con la marca entrante (tope
            // en "ahora"), pero la marca guardada nunca va hacia atrás
            // (SPEC4 S3): si la web volvió a guardar sin cambios con una marca
            // más nueva que la del escritorio (o el reloj del escritorio va
            // atrasado), se queda la guardada. El contenido es el del
            // escritorio; `marcas` le devuelve la que quedó.
            let guardada_ts = marca_de(Some(g)).unwrap_or_default();
            data.insert("updated_at".into(), Value::String(entrante_ts.max(guardada_ts)));
        }
    }

    // ── Padre ──
    let r_padre = sync_fk::traducir_fila_con(
        tabla,
        &uuid,
        &mut data,
        refs_de_fila(refs.as_ref(), &uuid),
        &busq,
        Some(cols),
        None,
        guardada.as_ref(),
        &politica,
        &omitir,
    );
    let (columnas, descartadas) =
        sync_fk::preparar_columnas(cols, &data).map_err(|e| ApiError::BadRequest(format!("{tabla}: {e}")))?;
    sync_fk::avisar_descartadas(tabla, &descartadas);
    let pid = sync_fk::escribir_fila(&mut tx, tabla, &columnas, cols, guardada.is_some()).await?;
    sync_fk::borrar_pendientes_celdas(&mut tx, tabla, &uuid, &r_padre.escritas).await?;
    let reescritas = sync_fk::celdas_web_reescritas(marcadas(tabla, &uuid), &columnas, guardada.as_ref());
    sync_fk::borrar_celdas_web(&mut tx, tabla, &uuid, &reescritas).await?;
    let mut pendientes = r_padre.pendientes;

    // ── Hijos: fk = id del padre, traducir, upsert por uuid, borrar los que ya no vienen ──
    let sin_omitir = HashSet::new();
    for (h, fk, filas) in hijos_entrantes {
        let cols_h = esquema
            .columnas(h)
            .ok_or_else(|| ApiError::BadRequest(format!("tabla sin esquema en Postgres: {h}")))?;
        let guardados_h = hijos_guardados.remove(h).unwrap_or_default();
        let uuids_entrantes: Vec<String> = filas
            .iter()
            .filter_map(|f| f.get("uuid").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect();
        for mut f in filas {
            let hu = f.get("uuid").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let g_h = guardados_h.get(&hu);
            // Un hijo que hoy pertenece a OTRO padre no se mueve.
            if let Some(g) = g_h {
                if g.get(fk).and_then(sync_fk::valor_entero) != Some(pid) {
                    tracing::warn!("push {tabla} {uuid}: el hijo {h} {hu} pertenece a otro padre; se ignora");
                    continue;
                }
            }
            let traducidas_h = reparadas.get(&(h.to_string(), hu.clone())).cloned().unwrap_or_default();
            let pol_h = sync_fk::politica_hijo(&politica, g_h.is_some(), traducidas_h);
            let r = sync_fk::traducir_fila_con(
                h,
                &hu,
                &mut f,
                refs_de_fila(refs.as_ref(), &hu),
                &busq,
                Some(cols_h),
                Some(pid),
                g_h,
                &pol_h,
                &sin_omitir,
            );
            let (columnas_h, descartadas_h) =
                sync_fk::preparar_columnas(cols_h, &f).map_err(|e| ApiError::BadRequest(format!("{h}: {e}")))?;
            sync_fk::avisar_descartadas(h, &descartadas_h);
            sync_fk::escribir_fila(&mut tx, h, &columnas_h, cols_h, g_h.is_some()).await?;
            sync_fk::borrar_pendientes_celdas(&mut tx, h, &hu, &r.escritas).await?;
            let reescritas_h = sync_fk::celdas_web_reescritas(marcadas(h, &hu), &columnas_h, g_h);
            sync_fk::borrar_celdas_web(&mut tx, h, &hu, &reescritas_h).await?;
            pendientes.extend(r.pendientes);
        }
        let borrados: Vec<String> =
            sqlx::query_scalar(&format!("DELETE FROM {h} WHERE {fk} = $1 AND NOT (uuid = ANY($2)) RETURNING uuid"))
                .bind(pid)
                .bind(&uuids_entrantes)
                .fetch_all(&mut *tx)
                .await?;
        sync_fk::borrar_pendientes(&mut tx, h, &borrados).await?;
    }

    // ── Celdas pendientes y estado de la fila ──
    sync_fk::guardar_pendientes(&mut tx, device_uuid, &pendientes).await?;
    // Solo cuando este cambio dejó sus FK en ids de aquí (v2, o escritorio
    // viejo sobre una fila ya traducida). Crudo / web viejo / sin_mapa: el
    // estado se queda como estaba.
    if traducibles && matches!(politica, sync_fk::Politica::Traducir { .. }) {
        let hay = sync_fk::fila_con_pendientes(&mut tx, tabla, &uuid, pid).await?;
        sync_fk::escribir_estado(&mut tx, tabla, &uuid, if hay { "pendiente" } else { "traducido" }).await?;
    }

    insertar_cursor(&mut tx, tabla, &uuid, sucursal_id, origen_cursor).await?;
    if let Some(h) = &huella {
        anotar_huella(&mut tx, device_uuid, tabla, &uuid, h).await?;
    }
    let marca = marca_guardada(&mut tx, tabla, cols, &uuid).await?;
    tx.commit().await?;

    let mut faltantes: Vec<String> = pendientes.into_iter().filter_map(|p| p.ref_uuid).collect();
    faltantes.sort();
    faltantes.dedup();
    Ok(Resultado::Aceptado { faltantes, marca })
}

/// productos sin `base` (escritorio sin sync_base) con conflicto contra una
/// versión más nueva que escribió OTRO dispositivo (la web): el existente gana
/// todo menos el stock, que es del escritorio. Se marca con un cursor 'merge'
/// para que el escritorio baje la fila combinada. Devuelve false si no aplica
/// (→ remoto_mas_nuevo). El que llama ya descartó al mismo dispositivo.
async fn merge_productos(
    tx: &mut Transaction<'_, Postgres>,
    cols: &sync_fk::ColumnasTabla,
    uuid: &str,
    data: &Map<String, Value>,
    sucursal_id: i64,
    ahora: &str,
) -> ApiResult<bool> {
    let (Some(info), Some(v)) = (cols.get("stock_actual"), data.get("stock_actual")) else {
        return Ok(false);
    };
    let stock = sync_fk::convertir_valor(info, v).map_err(|e| ApiError::BadRequest(format!("stock_actual: {e}")))?;
    if stock.es_nulo() {
        return Ok(false);
    }
    let q = sqlx::query("UPDATE productos SET stock_actual = $1, updated_at = $2 WHERE uuid = $3");
    sync_fk::bind_valor(q, &stock).bind(ahora).bind(uuid).execute(&mut **tx).await?;
    insertar_cursor(tx, "productos", uuid, sucursal_id, "merge").await?;
    Ok(true)
}

async fn insertar_cursor(
    tx: &mut Transaction<'_, Postgres>,
    tabla: &str,
    uuid: &str,
    sucursal_id: i64,
    origen: &str,
) -> ApiResult<()> {
    sqlx::query("INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) VALUES ($1, $2, $3, $4)")
        .bind(tabla)
        .bind(uuid)
        .bind(sucursal_id)
        .bind(origen)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn extraer_uuid(data: &Value) -> Option<String> {
    data.get("uuid").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string())
}

async fn registrar_device(
    pool: &PgPool,
    device_uuid: &str,
    sucursal_id: i64,
    proto: Option<i32>,
) -> ApiResult<()> {
    // `protocolo` (migración 010): versión de sync que reporta el escritorio
    // en cada push; NULL = anterior a v2.
    sqlx::query(
        "INSERT INTO pos_devices (device_uuid, sucursal_id, protocolo) VALUES ($1, $2, $3) \
         ON CONFLICT (device_uuid) DO UPDATE SET sucursal_id = EXCLUDED.sucursal_id, \
                                                 protocolo = EXCLUDED.protocolo",
    )
    .bind(device_uuid)
    .bind(sucursal_id)
    .bind(proto)
    .execute(pool)
    .await?;
    Ok(())
}

// -----------------------------------------------------------------------------
// PULL
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PullQuery {
    #[serde(default)]
    pub cursor: String,
    pub sucursal_id: i64,
    #[serde(default)]
    pub device_uuid: Option<String>,
    /// 2 = protocolo v2; ausente = escritorio viejo.
    #[serde(default)]
    pub proto: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct PullResponse {
    pub cambios: Vec<Value>,
    pub next_cursor: String,
    pub hay_mas: bool,
    /// Desde cuándo este servidor separa el dinero por caja (sync_meta,
    /// migración 011), 'YYYY-MM-DD HH:MM:SS' o null. El escritorio lo usa como
    /// frontera para acomodar ventas web (espejo) que bajen tarde.
    pub caja_v2_desde: Option<String>,
}

/// Una fila leída para el pull: (datos, hijos por tabla) o None si ya no existe.
type FilaLeida = Option<(Map<String, Value>, Option<Map<String, Value>>)>;

struct FilaPull {
    cursor: Option<i64>,
    tabla: String,
    uuid: String,
    origen: Option<String>,
    fila: FilaLeida,
}

pub async fn pull(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PullQuery>,
) -> ApiResult<Json<PullResponse>> {
    autenticar_sync(&headers, &state).await?;
    procesar_pull(&state.pool, &q).await.map(Json)
}

pub async fn procesar_pull(pool: &PgPool, q: &PullQuery) -> ApiResult<PullResponse> {
    let v2 = q.proto.unwrap_or(0) >= 2;
    let cursor_id: i64 = q.cursor.parse().unwrap_or(0);
    let device = q.device_uuid.as_deref().filter(|d| !d.is_empty());

    // Traer hasta PULL_BATCH+1 entradas del cursor para saber si hay_mas.
    // Excluimos los que originó el propio dispositivo (evita echo).
    let rows = sqlx::query(
        "SELECT id, tabla, uuid, origen_device \
         FROM sync_cursor \
         WHERE id > $1 \
           AND (sucursal_id IS NULL OR sucursal_id = $2) \
           AND ($3::text IS NULL OR origen_device IS DISTINCT FROM $3) \
         ORDER BY id ASC \
         LIMIT $4",
    )
    .bind(cursor_id)
    .bind(q.sucursal_id)
    .bind(device)
    .bind(PULL_BATCH + 1)
    .fetch_all(pool)
    .await?;

    let mut hay_mas = rows.len() as i64 > PULL_BATCH;
    let slice = if hay_mas { &rows[..PULL_BATCH as usize] } else { &rows[..] };

    let mut filas: Vec<FilaPull> = Vec::with_capacity(slice.len());
    let mut max_id: i64 = cursor_id;

    for row in slice {
        let id: i64 = row.get("id");
        let tabla: String = row.get("tabla");
        let uuid: String = row.get("uuid");
        let origen: Option<String> = row.get("origen_device");

        let omitir = !tabla_valida(&tabla)
            || tabla == "stock_sucursal"
            || es_usuario_web_admin(&tabla, &uuid)
            || (!v2 && sync_fk::es_caja_web_legacy(&tabla, &uuid));
        if omitir {
            max_id = max_id.max(id);
            continue;
        }

        match leer_fila(pool, &tabla, &uuid).await {
            Ok(fila) => {
                max_id = max_id.max(id);
                filas.push(FilaPull { cursor: Some(id), tabla, uuid, origen, fila });
            }
            Err(e) => {
                // Sin DELETE falso: se corta el lote aquí y se reintenta.
                tracing::warn!("pull: no se pudo leer {tabla} {uuid} (cursor {id}): {:?}", e);
                max_id = max_id.max(id - 1);
                hay_mas = true;
                break;
            }
        }
    }

    let cambios = construir_cambios(pool, filas, v2).await?;

    // Actualizar last_pull_at si viene device_uuid.
    if let Some(dev) = device {
        sqlx::query("UPDATE pos_devices SET last_pull_at = now() WHERE device_uuid = $1")
            .bind(dev)
            .execute(pool)
            .await
            .ok();
    }

    let caja_v2_desde = sync_fk::meta(pool, "caja_v2_desde").await?;
    Ok(PullResponse { cambios, next_cursor: max_id.to_string(), hay_mas, caja_v2_desde })
}

/// Lee una fila (y sus hijos si es agregado). Ok(None) = no existe. Un error
/// de lectura se propaga (el pull corta el lote; nunca manda un DELETE falso).
async fn leer_fila(pool: &PgPool, tabla: &str, uuid: &str) -> ApiResult<FilaLeida> {
    let texto: Option<String> =
        sqlx::query_scalar(&format!("SELECT row_to_json(t)::text AS j FROM {tabla} t WHERE uuid = $1"))
            .bind(uuid)
            .fetch_optional(pool)
            .await?;
    let Some(texto) = texto else { return Ok(None) };
    let data: Map<String, Value> = serde_json::from_str(&texto)
        .map_err(|e| ApiError::Other(anyhow::anyhow!("row_to_json de {tabla} {uuid}: {e}")))?;

    let hijos = hijos_de(tabla);
    if hijos.is_empty() {
        return Ok(Some((data, None)));
    }
    let Some(pid) = data.get("id").and_then(|v| v.as_i64()) else {
        return Ok(Some((data, None)));
    };
    let mut children = Map::new();
    for (h, fk) in hijos {
        let arr_text: String = sqlx::query_scalar(&format!(
            "SELECT COALESCE(json_agg(row_to_json(t) ORDER BY t.id), '[]'::json)::text \
               FROM {h} t WHERE {fk} = $1"
        ))
        .bind(pid)
        .fetch_one(pool)
        .await?;
        let arr: Value = serde_json::from_str(&arr_text)
            .map_err(|e| ApiError::Other(anyhow::anyhow!("hijos {h} de {tabla} {uuid}: {e}")))?;
        children.insert(h.to_string(), arr);
    }
    Ok(Some((data, Some(children))))
}

/// Arma los cambios del pull (y de /sync/fila).
///   v2: `__operacion` UPSERT si la fila existe (deleted_at viaja en __data),
///       DELETE solo si ya no existe; `__refs` solo para filas en idioma del
///       servidor (web, o del escritorio ya traducidas/reparadas).
///   legacy: como antes (DELETE si deleted_at), pero a las filas del
///       escritorio con estado se les quitan las columnas FK (padre e hijos)
///       para que el escritorio viejo conserve sus valores.
async fn construir_cambios(pool: &PgPool, filas: Vec<FilaPull>, v2: bool) -> ApiResult<Vec<Value>> {
    // Estados de las filas del escritorio.
    let pares: Vec<(String, String)> = filas
        .iter()
        .filter_map(|f| {
            let (d, _) = f.fila.as_ref()?;
            (sync_fk::tiene_fks_traducibles(&f.tabla) && sync_fk::es_fila_escritorio(&f.tabla, &f.uuid, d))
                .then(|| (f.tabla.clone(), f.uuid.clone()))
        })
        .collect();
    let estados = sync_fk::cargar_estados(pool, &pares).await?;
    let estado_de = |f: &FilaPull| estados.get(&(f.tabla.clone(), f.uuid.clone())).map(|s| s.as_str());

    // Celdas pendientes aquí (padres e hijos): su valor es un centinela o lo
    // que había antes, así que nunca viajan ("no sé": el escritorio conserva
    // el suyo).
    let mut uuids_lote: Vec<String> = Vec::new();
    for f in &filas {
        let Some((_, hijos)) = f.fila.as_ref() else { continue };
        uuids_lote.push(f.uuid.clone());
        for (_, arr) in hijos.iter().flatten() {
            for x in arr.as_array().into_iter().flatten() {
                if let Some(u) = x.get("uuid").and_then(|v| v.as_str()) {
                    uuids_lote.push(u.to_string());
                }
            }
        }
    }
    let pendientes = sync_fk::cargar_pendientes_de(pool, &uuids_lote).await?;
    let pend = |t: &str, u: &str| pendientes.get(&(t.to_string(), u.to_string()));
    // Pull viejo: celdas que la web reescribió (en ids de aquí) en filas del
    // escritorio sin reparar. Un escritorio viejo las guardaría como si fueran
    // ids suyos, así que no viajan (se queda con el suyo).
    let celdas_web = if v2 { HashMap::new() } else { sync_fk::cargar_celdas_web(pool, &uuids_lote).await? };
    let web = |t: &str, u: &str| celdas_web.get(&(t.to_string(), u.to_string()));

    // v2: ids → uuid de todas las refs del lote, en una consulta por tabla.
    let mut uuids_por_id = HashMap::new();
    if v2 {
        let mut pedidos = Vec::new();
        for f in &filas {
            let Some((d, hijos)) = f.fila.as_ref() else { continue };
            if !sync_fk::tiene_fks_traducibles(&f.tabla) {
                continue;
            }
            let esc = sync_fk::es_fila_escritorio(&f.tabla, &f.uuid, d);
            if !sync_fk::en_significado_servidor(esc, estado_de(f)) {
                continue;
            }
            pedidos.extend(sync_fk::pedidos_refs(&f.tabla, d));
            for (h, arr) in hijos.iter().flatten() {
                for x in arr.as_array().into_iter().flatten() {
                    if let Some(o) = x.as_object() {
                        pedidos.extend(sync_fk::pedidos_refs(h, o));
                    }
                }
            }
        }
        uuids_por_id = sync_fk::cargar_uuids_por_id(pool, &pedidos).await?;
    }

    let mut cambios = Vec::with_capacity(filas.len());
    for f in &filas {
        let estado = estado_de(f);
        let mut cambio = Map::new();
        cambio.insert("__tabla".into(), json!(f.tabla));
        match &f.fila {
            None => {
                cambio.insert("__operacion".into(), json!("DELETE"));
                cambio.insert("__data".into(), json!({ "uuid": f.uuid }));
            }
            Some((data, hijos)) => {
                let mut data = data.clone();
                let mut hijos = hijos.clone();
                let esc = sync_fk::es_fila_escritorio(&f.tabla, &f.uuid, &data);
                let borrada = data.get("deleted_at").map(|x| !x.is_null()).unwrap_or(false);
                if v2 {
                    cambio.insert("__operacion".into(), json!("UPSERT"));
                    if sync_fk::tiene_fks_traducibles(&f.tabla) && sync_fk::en_significado_servidor(esc, estado) {
                        let mut refs = Map::new();
                        refs.insert(
                            f.uuid.clone(),
                            Value::Object(sync_fk::refs_de_fila(&f.tabla, &data, &uuids_por_id, pend(&f.tabla, &f.uuid))),
                        );
                        for (h, arr) in hijos.iter().flatten() {
                            for x in arr.as_array().into_iter().flatten() {
                                let (Some(o), Some(hu)) = (x.as_object(), x.get("uuid").and_then(|v| v.as_str())) else {
                                    continue;
                                };
                                refs.insert(
                                    hu.to_string(),
                                    Value::Object(sync_fk::refs_de_fila(h, o, &uuids_por_id, pend(h, hu))),
                                );
                            }
                        }
                        cambio.insert("__refs".into(), Value::Object(refs));
                    }
                } else {
                    cambio.insert("__operacion".into(), json!(if borrada { "DELETE" } else { "UPSERT" }));
                    if borrada {
                        hijos = None;
                    }
                    if sync_fk::quitar_fks_para_legacy(esc, estado) {
                        sync_fk::quitar_fks(&f.tabla, &mut data);
                        if let Some(hm) = hijos.as_mut() {
                            for (h, arr) in hm.iter_mut() {
                                if let Some(a) = arr.as_array_mut() {
                                    for x in a.iter_mut() {
                                        if let Some(o) = x.as_object_mut() {
                                            sync_fk::quitar_fks(h, o);
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        // Filas web (y del escritorio sin estado): sin las
                        // celdas pendientes, que el escritorio viejo copiaría
                        // tal cual (NULL o 0), ni las que la web reescribió
                        // en una fila del escritorio (id de aquí, no suyo).
                        sync_fk::quitar_columnas(&mut data, pend(&f.tabla, &f.uuid));
                        sync_fk::quitar_columnas(&mut data, web(&f.tabla, &f.uuid));
                        if let Some(hm) = hijos.as_mut() {
                            for (h, arr) in hm.iter_mut() {
                                for x in arr.as_array_mut().into_iter().flatten() {
                                    let hu = x.get("uuid").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                                    if let Some(o) = x.as_object_mut() {
                                        sync_fk::quitar_columnas(o, pend(h, &hu));
                                        sync_fk::quitar_columnas(o, web(h, &hu));
                                    }
                                }
                            }
                        }
                    }
                }
                cambio.insert("__data".into(), Value::Object(data));
                if let Some(hm) = hijos {
                    cambio.insert("__children".into(), Value::Object(hm));
                }
            }
        }
        cambio.insert("__origen".into(), json!(f.origen));
        cambio.insert("__cursor".into(), json!(f.cursor));
        cambios.push(Value::Object(cambio));
    }
    Ok(cambios)
}

// -----------------------------------------------------------------------------
// GET /sync/fila — un cambio con la forma del pull v2
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct FilaQuery {
    pub tabla: String,
    pub uuid: String,
}

pub async fn fila(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FilaQuery>,
) -> ApiResult<Json<Value>> {
    autenticar_sync(&headers, &state).await?;
    procesar_fila(&state.pool, &q).await.map(Json)
}

/// Devuelve el cambio directo; `{"cambio": null}` si la fila nunca baja al
/// escritorio (stock_sucursal, usuario web-admin). Si la fila ya no existe
/// devuelve un DELETE (igual que el pull v2).
pub async fn procesar_fila(pool: &PgPool, q: &FilaQuery) -> ApiResult<Value> {
    if !tabla_valida(&q.tabla) {
        return Err(ApiError::BadRequest(format!("tabla desconocida: {}", q.tabla)));
    }
    if q.tabla == "stock_sucursal" || es_usuario_web_admin(&q.tabla, &q.uuid) {
        return Ok(json!({ "cambio": null }));
    }
    let cursor = sqlx::query(
        "SELECT id, origen_device FROM sync_cursor WHERE tabla = $1 AND uuid = $2 ORDER BY id DESC LIMIT 1",
    )
    .bind(&q.tabla)
    .bind(&q.uuid)
    .fetch_optional(pool)
    .await?;
    let (cursor_id, origen) = match cursor {
        Some(r) => (Some(r.get::<i64, _>(0)), r.get::<Option<String>, _>(1)),
        None => (None, None),
    };
    let fila = leer_fila(pool, &q.tabla, &q.uuid).await?;
    let mut cambios = construir_cambios(
        pool,
        vec![FilaPull { cursor: cursor_id, tabla: q.tabla.clone(), uuid: q.uuid.clone(), origen, fila }],
        true,
    )
    .await?;
    Ok(cambios.pop().unwrap_or(json!({ "cambio": null })))
}

// -----------------------------------------------------------------------------
// POST /sync/refs — referencias (por uuid) de filas en idioma del servidor
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RefsBody {
    pub tabla: String,
    pub uuids: Vec<String>,
}

pub async fn refs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RefsBody>,
) -> ApiResult<Json<Value>> {
    autenticar_sync(&headers, &state).await?;
    procesar_refs(&state.pool, &body).await.map(Json)
}

/// `{uuid: {columna: uuid | null}}` de las filas pedidas (y de sus hijos si
/// es agregado), solo de las que están en idioma del servidor. Solo lectura.
pub async fn procesar_refs(pool: &PgPool, body: &RefsBody) -> ApiResult<Value> {
    let tabla = body.tabla.as_str();
    if !tabla_valida(tabla) || tabla == "stock_sucursal" {
        return Err(ApiError::BadRequest(format!("tabla no sincronizable: {tabla}")));
    }
    if body.uuids.len() > MAX_UUIDS_REFS {
        return Err(ApiError::BadRequest(format!("máximo {MAX_UUIDS_REFS} uuids por llamada")));
    }
    let mut out = Map::new();
    if body.uuids.is_empty() {
        return Ok(Value::Object(out));
    }

    let filas: Vec<Map<String, Value>> = leer_filas_json(
        pool,
        &format!("SELECT row_to_json(t)::text FROM {tabla} t WHERE uuid = ANY($1)"),
        &body.uuids,
    )
    .await?;

    // ¿Quién decide si la fila está en idioma del servidor? La propia fila, o
    // su padre si es hija de un agregado.
    let padre = sync_fk::padre_de(tabla);
    // (tabla dueña, uuid dueño) por uuid de fila.
    let mut dueno: HashMap<String, (String, String, Map<String, Value>)> = HashMap::new();
    if let Some((p, fk)) = padre {
        let pids: Vec<i64> = filas.iter().filter_map(|f| f.get(fk).and_then(|v| v.as_i64())).collect();
        let padres = leer_filas_json_ids(pool, &format!("SELECT row_to_json(t)::text FROM {p} t WHERE id = ANY($1)"), &pids).await?;
        let por_id: HashMap<i64, Map<String, Value>> = padres
            .into_iter()
            .filter_map(|d| Some((d.get("id")?.as_i64()?, d)))
            .collect();
        for f in &filas {
            let Some(u) = f.get("uuid").and_then(|v| v.as_str()) else { continue };
            if let Some(pd) = f.get(fk).and_then(|v| v.as_i64()).and_then(|i| por_id.get(&i)) {
                let pu = pd.get("uuid").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                dueno.insert(u.to_string(), (p.to_string(), pu, pd.clone()));
            }
        }
    }
    for f in &filas {
        let Some(u) = f.get("uuid").and_then(|v| v.as_str()) else { continue };
        dueno.entry(u.to_string()).or_insert_with(|| (tabla.to_string(), u.to_string(), f.clone()));
    }
    let pares: Vec<(String, String)> = dueno
        .values()
        .filter(|(t, u, d)| sync_fk::es_fila_escritorio(t, u, d))
        .map(|(t, u, _)| (t.clone(), u.clone()))
        .collect();
    let estados = sync_fk::cargar_estados(pool, &pares).await?;
    let en_servidor = |u: &str| -> bool {
        match dueno.get(u) {
            Some((t, du, d)) => sync_fk::en_significado_servidor(
                sync_fk::es_fila_escritorio(t, du, d),
                estados.get(&(t.clone(), du.clone())).map(|s| s.as_str()),
            ),
            None => false,
        }
    };

    let filas: Vec<&Map<String, Value>> = filas
        .iter()
        .filter(|f| f.get("uuid").and_then(|v| v.as_str()).map(en_servidor).unwrap_or(false))
        .collect();

    // Hijos de los agregados pedidos.
    let mut hijos_por_padre: Vec<(&'static str, Vec<Map<String, Value>>)> = Vec::new();
    let pids: Vec<i64> = filas.iter().filter_map(|f| f.get("id").and_then(|v| v.as_i64())).collect();
    for (h, fk) in hijos_de(tabla) {
        let hs = leer_filas_json_ids(pool, &format!("SELECT row_to_json(t)::text FROM {h} t WHERE {fk} = ANY($1)"), &pids).await?;
        hijos_por_padre.push((h, hs));
    }

    let mut pedidos = Vec::new();
    for f in &filas {
        pedidos.extend(sync_fk::pedidos_refs(tabla, f));
    }
    for (h, hs) in &hijos_por_padre {
        for x in hs {
            pedidos.extend(sync_fk::pedidos_refs(h, x));
        }
    }
    let uuids_por_id = sync_fk::cargar_uuids_por_id(pool, &pedidos).await?;
    // Celdas pendientes aquí: se omiten ("no sé").
    let mut uuids_lote: Vec<String> =
        filas.iter().filter_map(|f| f.get("uuid").and_then(|v| v.as_str()).map(|s| s.to_string())).collect();
    for (_, hs) in &hijos_por_padre {
        uuids_lote.extend(hs.iter().filter_map(|x| x.get("uuid").and_then(|v| v.as_str()).map(|s| s.to_string())));
    }
    let pendientes = sync_fk::cargar_pendientes_de(pool, &uuids_lote).await?;
    let pend = |t: &str, u: &str| pendientes.get(&(t.to_string(), u.to_string()));
    for f in &filas {
        if let Some(u) = f.get("uuid").and_then(|v| v.as_str()) {
            out.insert(u.to_string(), Value::Object(sync_fk::refs_de_fila(tabla, f, &uuids_por_id, pend(tabla, u))));
        }
    }
    for (h, hs) in &hijos_por_padre {
        for x in hs {
            if let Some(u) = x.get("uuid").and_then(|v| v.as_str()) {
                out.insert(u.to_string(), Value::Object(sync_fk::refs_de_fila(h, x, &uuids_por_id, pend(h, u))));
            }
        }
    }
    Ok(Value::Object(out))
}

async fn leer_filas_json(pool: &PgPool, sql: &str, uuids: &[String]) -> ApiResult<Vec<Map<String, Value>>> {
    let textos: Vec<String> = sqlx::query_scalar(sql).bind(uuids).fetch_all(pool).await?;
    Ok(textos.iter().filter_map(|t| serde_json::from_str(t).ok()).collect())
}

async fn leer_filas_json_ids(pool: &PgPool, sql: &str, ids: &[i64]) -> ApiResult<Vec<Map<String, Value>>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let textos: Vec<String> = sqlx::query_scalar(sql).bind(ids).fetch_all(pool).await?;
    Ok(textos.iter().filter_map(|t| serde_json::from_str(t).ok()).collect())
}

// -----------------------------------------------------------------------------
// POST /sync/id-map — subida del mapa id local → uuid del escritorio
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct IdMapBody {
    pub device_uuid: String,
    pub tabla: String,
    pub pares: Vec<Value>,
}

pub async fn id_map(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<IdMapBody>,
) -> ApiResult<Json<Value>> {
    autenticar_sync(&headers, &state).await?;
    procesar_id_map(&state.pool, &body).await.map(Json)
}

pub async fn procesar_id_map(pool: &PgPool, body: &IdMapBody) -> ApiResult<Value> {
    if body.device_uuid.trim().is_empty() {
        return Err(ApiError::BadRequest("falta device_uuid".into()));
    }
    if !tabla_valida(&body.tabla) || body.tabla == "stock_sucursal" {
        return Err(ApiError::BadRequest(format!("tabla no sincronizable: {}", body.tabla)));
    }
    if body.pares.len() > MAX_PARES_ID_MAP {
        return Err(ApiError::BadRequest(format!("máximo {MAX_PARES_ID_MAP} pares por llamada")));
    }
    let mut pares: Vec<(i64, String)> = Vec::with_capacity(body.pares.len());
    let mut invalidos = 0usize;
    for p in &body.pares {
        let par = p.as_array().and_then(|a| {
            let id = a.first().and_then(sync_fk::valor_entero)?;
            let u = a.get(1)?.as_str().filter(|s| !s.is_empty())?;
            Some((id, u.to_string()))
        });
        match par {
            Some(x) => pares.push(x),
            None => invalidos += 1,
        }
    }

    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(sync_fk::LLAVE_REPARACION)
        .execute(&mut *tx)
        .await?;
    let reg = sync_fk::registrar_pares(&mut tx, &body.device_uuid, &body.tabla, &pares, "bootstrap").await?;
    let uuids: Vec<String> = pares.iter().map(|(_, u)| u.clone()).collect();
    let desconocidos: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT DISTINCT u FROM unnest($1::text[]) AS u \
          WHERE NOT EXISTS (SELECT 1 FROM {} t WHERE t.uuid = u) ORDER BY u",
        body.tabla
    ))
    .bind(&uuids)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    // Celdas pendientes que esperaban justo este mapa.
    if let Err(e) = sync_fk::resolver_pendientes(pool).await {
        tracing::warn!("id-map: resolver_pendientes falló: {:?}", e);
    }

    Ok(json!({
        "recibidos": reg.recibidos,
        "remapeados": reg.remapeados,
        "invalidos": invalidos,
        "desconocidos": desconocidos,
    }))
}

// -----------------------------------------------------------------------------
// POST /sync/reparar — reparación de FK de filas viejas del escritorio
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RepararBody {
    pub device_uuid: String,
    /// true = corre todo y revierte (solo reporte).
    #[serde(default, alias = "dry_run")]
    pub simulacion: bool,
    /// Si viene, en vez de reparar revierte ese lote.
    #[serde(default)]
    pub revertir_lote: Option<String>,
}

pub async fn reparar(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RepararBody>,
) -> ApiResult<Response> {
    autenticar_sync(&headers, &state).await?;
    if let Some(lote) = body.revertir_lote.as_deref().filter(|l| !l.is_empty()) {
        let r = sync_fk::revertir_lote(&state.pool, lote).await?;
        return Ok(Json(r).into_response());
    }
    if body.device_uuid.trim().is_empty() {
        return Err(ApiError::BadRequest("falta device_uuid".into()));
    }
    let r = sync_fk::reparar(&state.pool, &body.device_uuid, body.simulacion).await?;
    // Verificación fallida → 409 con el reporte (no se confirmó nada).
    let status = if r.ok { StatusCode::OK } else { StatusCode::CONFLICT };
    Ok((status, Json(r.reporte)).into_response())
}

#[cfg(test)]
#[path = "../tests/pg/sync_v2_pg.rs"]
mod pruebas_pg;
