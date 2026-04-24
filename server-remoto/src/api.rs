// api.rs — Endpoints REST para el panel web admin.
//
// Toda escritura desde la web debe:
//   1. Actualizar `updated_at = now()`
//   2. Registrar en `sync_cursor` con origen_device = 'web-admin'
// Así el POS recibe los cambios en su siguiente pull y el sync es simétrico.

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::auth::autenticar;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

const WEB_ORIGIN: &str = "web-admin";

/// Expresión SQL que genera el timestamp TEXT en el formato esperado por el POS
/// (YYYY-MM-DD HH24:MI:SS, TZ Mexico City). Usar en vez de `now()` para columnas
/// TEXT sincronizadas.
const NOW_TEXT: &str = "to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS')";

// -----------------------------------------------------------------------------
// Filtros comunes
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub sucursal_id: Option<i64>,
}

impl ListQuery {
    fn limit(&self) -> i64 { self.limit.unwrap_or(100).clamp(1, 500) }
    fn offset(&self) -> i64 { self.offset.unwrap_or(0).max(0) }
    fn q_like(&self) -> Option<String> {
        self.q.as_ref().map(|s| format!("%{}%", s.to_lowercase()))
    }
}

// -----------------------------------------------------------------------------
// Helper: registrar cambio en sync_cursor
// -----------------------------------------------------------------------------

async fn registrar_cambio(
    ex: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    tabla: &str,
    uuid: &str,
    sucursal_id: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ($1, $2, $3, $4)"
    )
    .bind(tabla)
    .bind(uuid)
    .bind(sucursal_id)
    .bind(WEB_ORIGIN)
    .execute(ex)
    .await?;
    Ok(())
}

// =============================================================================
// PRODUCTOS
// =============================================================================

pub async fn productos_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    autenticar(&headers, &state.jwt_secret)?;
    let sql = r#"
        SELECT id, uuid, codigo, codigo_tipo, nombre, descripcion, categoria_id,
               precio_costo, precio_venta, stock_actual, stock_minimo,
               proveedor_id, foto_url, activo, created_at, updated_at
        FROM productos
        WHERE deleted_at IS NULL
          AND ($1::text IS NULL
               OR lower(nombre) LIKE $1
               OR lower(codigo) LIKE $1)
        ORDER BY nombre
        LIMIT $2 OFFSET $3
    "#;
    let rows = sqlx::query(sql)
        .bind(q.q_like())
        .bind(q.limit())
        .bind(q.offset())
        .fetch_all(&state.pool)
        .await?;
    let items: Vec<Value> = rows.iter().map(row_to_json).collect();
    Ok(Json(json!({ "items": items, "count": items.len() })))
}

#[derive(Debug, Deserialize)]
pub struct ProductoInput {
    pub codigo: String,
    pub codigo_tipo: Option<String>,
    pub nombre: String,
    pub descripcion: Option<String>,
    pub categoria_id: Option<i64>,
    pub precio_costo: f64,
    pub precio_venta: f64,
    pub stock_minimo: Option<f64>,
    pub proveedor_id: Option<i64>,
    pub foto_url: Option<String>,
    pub activo: Option<bool>,
}

pub async fn productos_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<ProductoInput>,
) -> ApiResult<Json<Value>> {
    autenticar(&headers, &state.jwt_secret)?;
    let uuid = uuid::Uuid::now_v7().to_string();
    let sql = format!(r#"
        INSERT INTO productos
            (uuid, codigo, codigo_tipo, nombre, descripcion, categoria_id,
             precio_costo, precio_venta, stock_minimo, proveedor_id, foto_url, activo,
             created_at, updated_at)
        VALUES ($1, $2, COALESCE($3,'INTERNO'), $4, $5, $6, $7, $8, COALESCE($9,0),
                $10, $11, CASE WHEN COALESCE($12, TRUE) THEN 1 ELSE 0 END,
                {NOW_TEXT}, {NOW_TEXT})
        RETURNING id, uuid, nombre, codigo, precio_venta, stock_actual, updated_at
    "#);
    let row = sqlx::query(&sql)
    .bind(&uuid)
    .bind(&input.codigo)
    .bind(input.codigo_tipo.as_deref())
    .bind(&input.nombre)
    .bind(input.descripcion.as_deref())
    .bind(input.categoria_id)
    .bind(input.precio_costo)
    .bind(input.precio_venta)
    .bind(input.stock_minimo)
    .bind(input.proveedor_id)
    .bind(input.foto_url.as_deref())
    .bind(input.activo)
    .fetch_one(&state.pool)
    .await?;

    registrar_cambio(&state.pool, "productos", &uuid, None).await?;
    Ok(Json(row_to_json(&row)))
}

pub async fn productos_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
    Json(input): Json<ProductoInput>,
) -> ApiResult<Json<Value>> {
    autenticar(&headers, &state.jwt_secret)?;
    let sql = format!(r#"
        UPDATE productos SET
            codigo = $2,
            codigo_tipo = COALESCE($3, codigo_tipo),
            nombre = $4,
            descripcion = $5,
            categoria_id = $6,
            precio_costo = $7,
            precio_venta = $8,
            stock_minimo = COALESCE($9, stock_minimo),
            proveedor_id = $10,
            foto_url = $11,
            activo = CASE WHEN COALESCE($12, TRUE) THEN 1 ELSE 0 END,
            updated_at = {NOW_TEXT}
        WHERE uuid = $1 AND deleted_at IS NULL
        RETURNING id, uuid, nombre, codigo, precio_venta, stock_actual, updated_at
    "#);
    let row = sqlx::query(&sql)
    .bind(&uuid)
    .bind(&input.codigo)
    .bind(input.codigo_tipo.as_deref())
    .bind(&input.nombre)
    .bind(input.descripcion.as_deref())
    .bind(input.categoria_id)
    .bind(input.precio_costo)
    .bind(input.precio_venta)
    .bind(input.stock_minimo)
    .bind(input.proveedor_id)
    .bind(input.foto_url.as_deref())
    .bind(input.activo)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::NotFound)?;

    registrar_cambio(&state.pool, "productos", &uuid, None).await?;
    Ok(Json(row_to_json(&row)))
}

pub async fn productos_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> ApiResult<Json<Value>> {
    autenticar(&headers, &state.jwt_secret)?;
    let sql = format!(
        "UPDATE productos SET deleted_at = {NOW_TEXT}, updated_at = {NOW_TEXT} \
         WHERE uuid = $1 AND deleted_at IS NULL"
    );
    let affected = sqlx::query(&sql)
    .bind(&uuid)
    .execute(&state.pool)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(ApiError::NotFound);
    }
    registrar_cambio(&state.pool, "productos", &uuid, None).await?;
    Ok(Json(json!({ "ok": true })))
}

// =============================================================================
// CATÁLOGOS LIGEROS (categorias, proveedores, clientes)
// =============================================================================

pub async fn categorias_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    autenticar(&headers, &state.jwt_secret)?;
    let rows = sqlx::query(
        "SELECT id, uuid, nombre, descripcion FROM categorias \
         WHERE deleted_at IS NULL ORDER BY nombre",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "items": rows.iter().map(row_to_json).collect::<Vec<_>>() })))
}

pub async fn proveedores_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    autenticar(&headers, &state.jwt_secret)?;
    let rows = sqlx::query(
        "SELECT id, uuid, nombre, contacto, telefono, email \
         FROM proveedores WHERE deleted_at IS NULL ORDER BY nombre",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "items": rows.iter().map(row_to_json).collect::<Vec<_>>() })))
}

pub async fn clientes_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    autenticar(&headers, &state.jwt_secret)?;
    let rows = sqlx::query(
        r#"
        SELECT id, uuid, nombre, telefono, email, descuento_porcentaje, activo
        FROM clientes
        WHERE deleted_at IS NULL
          AND ($1::text IS NULL OR lower(nombre) LIKE $1 OR lower(COALESCE(telefono,'')) LIKE $1)
        ORDER BY nombre
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(q.q_like())
    .bind(q.limit())
    .bind(q.offset())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "items": rows.iter().map(row_to_json).collect::<Vec<_>>() })))
}

// =============================================================================
// VENTAS (read-only desde la web)
// =============================================================================

pub async fn ventas_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    autenticar(&headers, &state.jwt_secret)?;
    let rows = sqlx::query(
        r#"
        SELECT v.id, v.uuid, v.sucursal_id, v.folio, v.total, v.metodo_pago,
               v.anulada, v.fecha,
               u.nombre_completo AS usuario, c.nombre AS cliente
        FROM ventas v
        LEFT JOIN usuarios u ON u.id = v.usuario_id
        LEFT JOIN clientes c ON c.id = v.cliente_id
        WHERE v.deleted_at IS NULL
          AND ($1::bigint IS NULL OR v.sucursal_id = $1)
        ORDER BY v.fecha DESC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(q.sucursal_id)
    .bind(q.limit())
    .bind(q.offset())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "items": rows.iter().map(row_to_json).collect::<Vec<_>>() })))
}

pub async fn ventas_detalle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> ApiResult<Json<Value>> {
    autenticar(&headers, &state.jwt_secret)?;
    let venta = sqlx::query(
        "SELECT * FROM ventas WHERE uuid = $1 AND deleted_at IS NULL",
    )
    .bind(&uuid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::NotFound)?;
    let venta_id: i64 = venta.get("id");
    let detalle = sqlx::query(
        r#"
        SELECT vd.*, p.nombre AS producto_nombre, p.codigo AS producto_codigo
        FROM venta_detalle vd
        LEFT JOIN productos p ON p.id = vd.producto_id
        WHERE vd.venta_id = $1
        ORDER BY vd.id
        "#,
    )
    .bind(venta_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({
        "venta": row_to_json(&venta),
        "detalle": detalle.iter().map(row_to_json).collect::<Vec<_>>(),
    })))
}

// =============================================================================
// DASHBOARD RESUMEN
// =============================================================================

pub async fn dashboard_resumen(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    autenticar(&headers, &state.jwt_secret)?;
    let suc = q.sucursal_id;

    // Ventas de hoy (TZ Mexico City)
    let hoy = sqlx::query(
        r#"
        SELECT
          COALESCE(SUM(total), 0) AS total_dia,
          COUNT(*)::bigint        AS num_ventas
        FROM ventas
        WHERE deleted_at IS NULL AND anulada = 0
          AND substr(fecha, 1, 10)
              = to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD')
          AND ($1::bigint IS NULL OR sucursal_id = $1)
        "#,
    )
    .bind(suc)
    .fetch_one(&state.pool)
    .await?;

    // Productos con stock bajo
    let bajo: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM productos \
         WHERE deleted_at IS NULL AND activo = 1 AND stock_actual <= stock_minimo",
    )
    .fetch_one(&state.pool)
    .await?;

    // Dispositivos POS conectados
    let dispositivos: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM pos_devices",
    )
    .fetch_one(&state.pool)
    .await?;

    // Últimas 5 ventas
    let ultimas = sqlx::query(
        r#"
        SELECT v.uuid, v.folio, v.total, v.fecha,
               u.nombre_completo AS usuario
        FROM ventas v
        LEFT JOIN usuarios u ON u.id = v.usuario_id
        WHERE v.deleted_at IS NULL AND v.anulada = 0
          AND ($1::bigint IS NULL OR v.sucursal_id = $1)
        ORDER BY v.fecha DESC
        LIMIT 5
        "#,
    )
    .bind(suc)
    .fetch_all(&state.pool)
    .await?;

    let total_dia: rust_decimal::Decimal = hoy.try_get("total_dia").unwrap_or_default();
    let num_ventas: i64 = hoy.try_get("num_ventas").unwrap_or(0);

    Ok(Json(json!({
        "ventas_hoy": {
            "total":  total_dia.to_string(),
            "cuenta": num_ventas,
        },
        "stock_bajo": bajo,
        "dispositivos": dispositivos,
        "ultimas_ventas": ultimas.iter().map(row_to_json).collect::<Vec<_>>(),
    })))
}

// =============================================================================
// SUCURSALES
// =============================================================================

pub async fn sucursales_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    autenticar(&headers, &state.jwt_secret)?;
    let rows = sqlx::query(
        "SELECT id, uuid, nombre, direccion, telefono, activa \
         FROM sucursales WHERE deleted_at IS NULL ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "items": rows.iter().map(row_to_json).collect::<Vec<_>>() })))
}

// =============================================================================
// Conversión PgRow → serde_json::Value
// =============================================================================

pub fn row_to_json(row: &sqlx::postgres::PgRow) -> Value {
    use sqlx::Column;
    use sqlx::TypeInfo;

    let mut map = serde_json::Map::new();
    for col in row.columns() {
        let name = col.name();
        let type_name = col.type_info().name().to_string();
        let v = pg_value_to_json(row, col.ordinal(), &type_name);
        map.insert(name.to_string(), v);
    }
    Value::Object(map)
}

fn pg_value_to_json(
    row: &sqlx::postgres::PgRow,
    idx: usize,
    type_name: &str,
) -> Value {
    match type_name {
        "INT2" | "INT4" | "INT8" | "BIGINT" | "INTEGER" | "SMALLINT" => {
            row.try_get::<Option<i64>, _>(idx).ok().flatten()
                .map(|v| json!(v)).unwrap_or(Value::Null)
        }
        "FLOAT4" | "FLOAT8" | "REAL" | "DOUBLE PRECISION" => {
            row.try_get::<Option<f64>, _>(idx).ok().flatten()
                .map(|v| json!(v)).unwrap_or(Value::Null)
        }
        "NUMERIC" => {
            row.try_get::<Option<rust_decimal::Decimal>, _>(idx).ok().flatten()
                .map(|d| json!(d.to_string())).unwrap_or(Value::Null)
        }
        "BOOL" | "BOOLEAN" => {
            row.try_get::<Option<bool>, _>(idx).ok().flatten()
                .map(|v| json!(v)).unwrap_or(Value::Null)
        }
        "TIMESTAMPTZ" | "TIMESTAMP" | "DATE" | "TIME" => {
            row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(idx).ok().flatten()
                .map(|v| json!(v.to_rfc3339())).unwrap_or(Value::Null)
        }
        "JSON" | "JSONB" => {
            row.try_get::<Option<Value>, _>(idx).ok().flatten().unwrap_or(Value::Null)
        }
        "UUID" => {
            row.try_get::<Option<uuid::Uuid>, _>(idx).ok().flatten()
                .map(|v| json!(v.to_string())).unwrap_or(Value::Null)
        }
        _ => {
            // TEXT, VARCHAR, CHAR, y cualquier otro → string
            row.try_get::<Option<String>, _>(idx).ok().flatten()
                .map(|v| json!(v)).unwrap_or(Value::Null)
        }
    }
}
