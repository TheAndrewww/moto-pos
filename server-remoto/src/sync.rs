// sync.rs — Endpoints /sync/push y /sync/pull.
//
// Protocolo (alineado con pos/src-tauri/src/sync/client.rs):
//   POST /sync/push  { device_uuid, sucursal_id, cambios: [{tabla, operacion, data, children?}] }
//      → { aceptados: [uuid...], rechazados: [{uuid, motivo}] }
//   GET  /sync/pull?cursor=X&sucursal_id=Y
//      → { cambios: [{__tabla, __operacion, __data, __children?}], next_cursor, hay_mas }
//
// LWW: comparación por `updated_at` como TEXTO (mismo formato que SQLite).
// Seguridad: tabla validada contra whitelist antes de construir SQL dinámico.

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Postgres, Row, Transaction};

use crate::auth::autenticar;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

const PULL_BATCH: i64 = 200;

// -----------------------------------------------------------------------------
// Whitelist de tablas sincronizables + definición de agregados.
// -----------------------------------------------------------------------------

const TABLAS_SYNC: &[&str] = &[
    // Catálogo
    "sucursales", "categorias", "proveedores", "productos", "clientes", "usuarios",
    "stock_sucursal",
    // Operacionales (padres)
    "ventas", "recepciones", "ordenes_pedido", "presupuestos", "cortes",
    "devoluciones", "transferencias",
    "movimientos_caja", "aperturas_caja",
    // Hijos (no vienen como "cambio" independiente; llegan bajo __children,
    // pero se listan para que pull pueda reconstruirlos si hiciera falta)
    "venta_detalle", "recepcion_detalle", "orden_pedido_detalle",
    "presupuesto_detalle", "corte_denominaciones", "corte_vendedores",
    "devolucion_detalle", "transferencia_detalle",
];

/// (padre, [(hijo, fk_columna)])
const AGGREGATES: &[(&str, &[(&str, &str)])] = &[
    ("ventas",         &[("venta_detalle",        "venta_id")]),
    ("presupuestos",   &[("presupuesto_detalle",  "presupuesto_id")]),
    ("ordenes_pedido", &[("orden_pedido_detalle", "orden_id")]),
    ("recepciones",    &[("recepcion_detalle",    "recepcion_id")]),
    ("cortes",         &[("corte_denominaciones", "corte_id"),
                         ("corte_vendedores",     "corte_id")]),
    ("devoluciones",   &[("devolucion_detalle",   "devolucion_id")]),
    ("transferencias", &[("transferencia_detalle","transferencia_id")]),
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

fn hijos_de(tabla: &str) -> &'static [(&'static str, &'static str)] {
    AGGREGATES.iter().find(|(p, _)| *p == tabla).map(|(_, h)| *h).unwrap_or(&[])
}

// -----------------------------------------------------------------------------
// PUSH
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PushBody {
    pub device_uuid: String,
    pub sucursal_id: i64,
    pub cambios: Vec<CambioIn>,
}

#[derive(Debug, Deserialize)]
pub struct CambioIn {
    pub tabla: String,
    pub operacion: String,
    pub data: Value,
    #[serde(default)]
    pub children: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct PushResponse {
    pub aceptados: Vec<String>,
    pub rechazados: Vec<Rechazado>,
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
    let _claims = autenticar(&headers, &state.jwt_secret)?;

    registrar_device(&state.pool, &body.device_uuid, body.sucursal_id).await?;

    let mut aceptados: Vec<String> = Vec::new();
    let mut rechazados: Vec<Rechazado> = Vec::new();

    for cambio in &body.cambios {
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
                rechazados.push(Rechazado {
                    uuid: String::new(),
                    motivo: "falta uuid".into(),
                });
                continue;
            }
        };

        match aplicar_cambio(&state.pool, cambio, &body.device_uuid, body.sucursal_id).await {
            Ok(true) => aceptados.push(uuid),
            Ok(false) => rechazados.push(Rechazado {
                uuid,
                motivo: "remoto_mas_nuevo".into(),
            }),
            Err(e) => {
                tracing::warn!("push error tabla={} uuid={}: {:?}", cambio.tabla, uuid, e);
                rechazados.push(Rechazado { uuid, motivo: format!("error: {}", e) });
            }
        }
    }

    sqlx::query("UPDATE pos_devices SET last_push_at = now() WHERE device_uuid = $1")
        .bind(&body.device_uuid)
        .execute(&state.pool)
        .await?;

    Ok(Json(PushResponse { aceptados, rechazados }))
}

async fn aplicar_cambio(
    pool: &sqlx::PgPool,
    cambio: &CambioIn,
    device_uuid: &str,
    sucursal_id: i64,
) -> ApiResult<bool> {
    let uuid = extraer_uuid(&cambio.data).ok_or_else(|| ApiError::BadRequest("falta uuid".into()))?;
    let entrante_updated_at = cambio.data
        .get("updated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut tx = pool.begin().await?;

    // LWW: updated_at ya es TEXT con el mismo formato que SQLite
    // ("YYYY-MM-DD HH24:MI:SS"), la comparación es byte-por-byte directa.
    let sql_check = format!(
        "SELECT updated_at AS ts FROM {} WHERE uuid = $1 LIMIT 1",
        cambio.tabla
    );
    let local_ts: Option<String> = sqlx::query_scalar(&sql_check)
        .bind(&uuid)
        .fetch_optional(&mut *tx)
        .await
        .ok()
        .flatten();

    if let Some(local) = &local_ts {
        if local.as_str() >= entrante_updated_at.as_str() && !entrante_updated_at.is_empty() {
            tx.rollback().await.ok();
            return Ok(false);
        }
    }

    // Parchar sucursal_id si la tabla lo requiere y el POS no lo envió.
    // El esquema SQLite del POS puede ser de sucursal única sin esa columna.
    let data_patched: Value = {
        let falta = TABLAS_CON_SUCURSAL.contains(&cambio.tabla.as_str())
            && cambio.data.get("sucursal_id").map(|v| v.is_null()).unwrap_or(true);
        if falta {
            let mut d = cambio.data.clone();
            if let Some(obj) = d.as_object_mut() {
                obj.insert("sucursal_id".to_string(), json!(sucursal_id));
            }
            d
        } else {
            cambio.data.clone()
        }
    };

    match cambio.operacion.as_str() {
        "DELETE" => {
            // deleted_at y updated_at son TEXT — usamos to_char para mantener
            // el mismo formato que SQLite.
            let sql = format!(
                "UPDATE {} SET \
                   deleted_at = to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'), \
                   updated_at = to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS') \
                 WHERE uuid = $1",
                cambio.tabla
            );
            sqlx::query(&sql).bind(&uuid).execute(&mut *tx).await?;
        }
        _ => {
            upsert_dinamico(&mut tx, &cambio.tabla, &data_patched).await?;

            // Aplicar hijos si es agregado
            let hijos = hijos_de(&cambio.tabla);
            if !hijos.is_empty() {
                if let Some(children) = cambio.children.as_ref().and_then(|v| v.as_object()) {
                    // parent.id local
                    let parent_id: Option<i64> = sqlx::query_scalar(&format!(
                        "SELECT id FROM {} WHERE uuid = $1",
                        cambio.tabla
                    ))
                    .bind(&uuid)
                    .fetch_optional(&mut *tx)
                    .await?;
                    if let Some(pid) = parent_id {
                        for (tabla_hijo, fk) in hijos {
                            if let Some(arr) = children.get(*tabla_hijo).and_then(|v| v.as_array()) {
                                // Reemplazar
                                let sql_del = format!(
                                    "DELETE FROM {} WHERE {} = $1",
                                    tabla_hijo, fk
                                );
                                sqlx::query(&sql_del).bind(pid).execute(&mut *tx).await?;
                                for hijo in arr {
                                    let mut h = hijo.clone();
                                    if let Some(obj) = h.as_object_mut() {
                                        obj.insert(fk.to_string(), json!(pid));
                                        // Propagar sucursal_id a hijos transaccionales si falta
                                        if TABLAS_CON_SUCURSAL.contains(tabla_hijo)
                                            && obj.get("sucursal_id").map(|v| v.is_null()).unwrap_or(true)
                                        {
                                            obj.insert("sucursal_id".to_string(), json!(sucursal_id));
                                        }
                                    }
                                    upsert_dinamico(&mut tx, tabla_hijo, &h).await?;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Registrar en sync_cursor (para que otros dispositivos hagan pull)
    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ($1, $2, $3, $4)"
    )
    .bind(&cambio.tabla)
    .bind(&uuid)
    .bind(sucursal_id)
    .bind(device_uuid)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(true)
}

/// INSERT ... ON CONFLICT(uuid) DO UPDATE SET ... usando columnas del JSON.
async fn upsert_dinamico(
    tx: &mut Transaction<'_, Postgres>,
    tabla: &str,
    data: &Value,
) -> ApiResult<()> {
    let obj = data.as_object().ok_or_else(|| ApiError::BadRequest("data no es objeto".into()))?;
    // Ignorar columnas específicas del POS que no existen en el servidor.
    const COLUMNAS_IGNORADAS: &[&str] = &["id", "synced_at"];
    let cols: Vec<&String> = obj.keys()
        .filter(|k| !COLUMNAS_IGNORADAS.contains(&k.as_str()))
        .collect();
    if cols.is_empty() {
        return Ok(());
    }

    let col_list = cols.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(",");
    let placeholders = (1..=cols.len())
        .map(|i| format!("${}", i))
        .collect::<Vec<_>>()
        .join(",");
    let updates = cols.iter()
        .filter(|c| c.as_str() != "uuid")
        .map(|c| format!("{c}=excluded.{c}"))
        .collect::<Vec<_>>()
        .join(",");

    let sql = if updates.is_empty() {
        format!("INSERT INTO {tabla} ({col_list}) VALUES ({placeholders}) ON CONFLICT(uuid) DO NOTHING")
    } else {
        format!("INSERT INTO {tabla} ({col_list}) VALUES ({placeholders}) ON CONFLICT(uuid) DO UPDATE SET {updates}")
    };

    let mut q = sqlx::query(&sql);
    for c in &cols {
        q = bind_json(q, &obj[c.as_str()]);
    }
    q.execute(&mut **tx).await?;
    Ok(())
}

/// Bind JSON value al query. Inferimos el tipo apropiado de Rust y lo bindeamos.
/// Para strings vacíos y null → Option::<i64>::None (asumiendo columnas numéricas opcionales).
fn bind_json<'q>(
    q: sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments>,
    v: &'q Value,
) -> sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments> {
    match v {
        Value::Null => q.bind(Option::<i64>::None),
        Value::Bool(b) => q.bind(if *b { 1i32 } else { 0i32 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                q.bind(i)
            } else if let Some(f) = n.as_f64() {
                q.bind(f)
            } else {
                q.bind(Option::<i64>::None)
            }
        }
        Value::String(s) => {
            // Strings vacíos → NULL (para FKs opcionales que SQLite envía como "")
            if s.is_empty() {
                q.bind(Option::<String>::None)
            } else if let Ok(n) = s.parse::<i64>() {
                // SQLite puede serializar INTEGER como string "1"/"0" en algunos
                // contextos (backfill manual, afinidad TEXT, activo, anulada...).
                // Bind como i64 para que Postgres acepte columnas INTEGER/BIGINT.
                q.bind(n)
            } else {
                q.bind(s.as_str())
            }
        }
        Value::Array(_) | Value::Object(_) => q.bind(v.to_string()),
    }
}

fn extraer_uuid(data: &Value) -> Option<String> {
    data.get("uuid").and_then(|v| v.as_str()).map(|s| s.to_string())
}

async fn registrar_device(
    pool: &sqlx::PgPool,
    device_uuid: &str,
    sucursal_id: i64,
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO pos_devices (device_uuid, sucursal_id) VALUES ($1, $2) \
         ON CONFLICT (device_uuid) DO UPDATE SET sucursal_id = EXCLUDED.sucursal_id"
    )
    .bind(device_uuid)
    .bind(sucursal_id)
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
}

#[derive(Debug, Serialize)]
pub struct PullResponse {
    pub cambios: Vec<Value>,
    pub next_cursor: String,
    pub hay_mas: bool,
}

pub async fn pull(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PullQuery>,
) -> ApiResult<Json<PullResponse>> {
    let _claims = autenticar(&headers, &state.jwt_secret)?;

    let cursor_id: i64 = q.cursor.parse().unwrap_or(0);

    // Traer hasta PULL_BATCH+1 entradas del cursor para saber si hay_mas.
    // Excluimos los que originó el propio dispositivo (evita echo).
    let rows = sqlx::query(
        "SELECT id, tabla, uuid, origen_device \
         FROM sync_cursor \
         WHERE id > $1 \
           AND (sucursal_id IS NULL OR sucursal_id = $2) \
           AND ($3::text IS NULL OR origen_device IS DISTINCT FROM $3) \
         ORDER BY id ASC \
         LIMIT $4"
    )
    .bind(cursor_id)
    .bind(q.sucursal_id)
    .bind(q.device_uuid.as_deref())
    .bind(PULL_BATCH + 1)
    .fetch_all(&state.pool)
    .await?;

    let hay_mas = rows.len() as i64 > PULL_BATCH;
    let slice = if hay_mas { &rows[..PULL_BATCH as usize] } else { &rows[..] };

    let mut cambios: Vec<Value> = Vec::with_capacity(slice.len());
    let mut max_id: i64 = cursor_id;

    for row in slice {
        let id: i64 = row.get("id");
        let tabla: String = row.get("tabla");
        let uuid: String = row.get("uuid");
        max_id = max_id.max(id);

        if !tabla_valida(&tabla) {
            continue;
        }

        // Obtener fila completa como JSON.
        let sql = format!("SELECT row_to_json(t)::text AS j FROM {} t WHERE uuid = $1", tabla);
        let fila_text: Option<String> = sqlx::query_scalar(&sql)
            .bind(&uuid)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();

        let (operacion, data) = match fila_text {
            Some(j) => {
                let v: Value = serde_json::from_str(&j).unwrap_or(Value::Null);
                let op = if v.get("deleted_at").map(|x| !x.is_null()).unwrap_or(false) {
                    "DELETE"
                } else {
                    "UPSERT"
                };
                (op, v)
            }
            None => ("DELETE", json!({ "uuid": uuid })),
        };

        let mut cambio = json!({
            "__tabla": tabla,
            "__operacion": operacion,
            "__data": data,
        });

        // Si es agregado, cargar hijos.
        let hijos = hijos_de(&tabla);
        if !hijos.is_empty() && operacion != "DELETE" {
            let parent_id = cambio["__data"].get("id").and_then(|v| v.as_i64());
            if let Some(pid) = parent_id {
                let mut children_map = serde_json::Map::new();
                for (tabla_hijo, fk) in hijos {
                    let sql_h = format!(
                        "SELECT COALESCE(json_agg(row_to_json(t)), '[]'::json)::text AS j \
                         FROM {} t WHERE {} = $1",
                        tabla_hijo, fk
                    );
                    let arr_text: Option<String> = sqlx::query_scalar(&sql_h)
                        .bind(pid)
                        .fetch_optional(&state.pool)
                        .await
                        .ok()
                        .flatten();
                    let arr: Value = arr_text
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or(Value::Array(vec![]));
                    children_map.insert(tabla_hijo.to_string(), arr);
                }
                cambio["__children"] = Value::Object(children_map);
            }
        }

        cambios.push(cambio);
    }

    // Actualizar last_pull_at si viene device_uuid.
    if let Some(dev) = &q.device_uuid {
        sqlx::query("UPDATE pos_devices SET last_pull_at = now() WHERE device_uuid = $1")
            .bind(dev)
            .execute(&state.pool)
            .await
            .ok();
    }

    Ok(Json(PullResponse {
        cambios,
        next_cursor: max_id.to_string(),
        hay_mas,
    }))
}
