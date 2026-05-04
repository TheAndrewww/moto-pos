// rpc.rs — Dispatcher para el POS en modo web.
//
// El frontend (mismo bundle que Tauri) llama a `invoke(cmd, args)` vía
// `invokeCompat.ts`. En el navegador, esto se traduce a:
//     POST /rpc/{cmd}    body = JSON.stringify(args)
//
// Aquí dispatch-amos por nombre de comando y devolvemos la shape que espera
// el frontend (idéntica a la del comando Tauri equivalente, ver `commands/`
// en `src-tauri`). Así el mismo código React funciona en escritorio y web.
//
// Nota de alcance: implementamos el MVP — lectura + `crear_venta`. Comandos
// no soportados devuelven 501. Se van agregando bajo demanda.

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::api::row_to_json;
use crate::auth::{autenticar, emitir_token};
use crate::error::{ApiError, ApiResult};
use crate::AppState;

const WEB_ORIGIN: &str = "web-pos";

/// Etiqueta de `origen` para registros creados desde el POS web.
/// Las filas con `origen='desktop'` provienen del cliente Tauri (vía sync).
const ORIGEN_WEB: &str = "web";

/// Expresión SQL para timestamp TEXT igual al POS.
const NOW_TEXT: &str = "to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS')";

// -----------------------------------------------------------------------------
// Dispatcher principal
// -----------------------------------------------------------------------------

pub async fn dispatch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(cmd): Path<String>,
    Json(args): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Comandos de login son los únicos sin auth previa: emiten el token.
    if cmd == "login_pin" {
        return Ok(Json(login_pin(&state, &args).await?));
    }
    if cmd == "login_password" {
        return Ok(Json(login_password(&state, &args).await?));
    }
    if cmd == "logout" {
        // Stateless en web: el frontend simplemente borra el token.
        return Ok(Json(Value::Null));
    }

    // Toda RPC restante requiere Bearer token válido.
    let _claims = autenticar(&headers, &state.jwt_secret)?;

    let result = match cmd.as_str() {
        // ─── Catálogos (read) ────────────────────────────────
        "listar_productos"              => listar_productos(&state).await?,
        "listar_productos_stock_bajo"   => listar_productos_stock_bajo(&state).await?,
        "obtener_producto_por_codigo"   => obtener_producto_por_codigo(&state, &args).await?,
        "listar_categorias"             => listar_categorias(&state).await?,
        "listar_proveedores"            => listar_proveedores(&state).await?,
        "listar_clientes"               => listar_clientes(&state).await?,
        "crear_cliente"                 => crear_cliente(&state, args).await?,
        "actualizar_cliente"            => actualizar_cliente(&state, args).await?,
        "toggle_cliente_activo"         => toggle_cliente_activo(&state, &args).await?,
        "listar_usuarios"               => listar_usuarios(&state).await?,
        "listar_roles"                  => listar_roles(),

        // ─── Productos (mutaciones) ──────────────────────────
        "crear_producto"                => crear_producto(&state, args).await?,
        "actualizar_producto"           => actualizar_producto(&state, args).await?,
        "eliminar_producto"             => eliminar_producto(&state, &args).await?,
        "ajustar_stock"                 => ajustar_stock(&state, &args).await?,
        "generar_codigo_interno"        => generar_codigo_interno(&state).await?,
        "historial_precios_producto"    => historial_precios_producto(&state, &args).await?,

        // ─── Config / sistema ────────────────────────────────
        "obtener_config_negocio"        => obtener_config_negocio(),
        "obtener_config_descuentos"     => obtener_config_descuentos(),
        "listar_impresoras"             => json!([]),
        "obtener_info_bd"               => json!(0),
        "obtener_info_servidor"         => obtener_info_servidor(&state).await?,
        "listar_dispositivos"           => listar_dispositivos(&state).await?,

        // El web NO empuja al servidor (sus cambios viven directamente en
        // Postgres vía RPC). Devolvemos un estado "siempre conectado, 0
        // pendientes" para que la página /sincronizacion no se quede vacía.
        "obtener_estado_sync"           => json!({
            "activo": true,
            "remote_url": null,
            "device_uuid": "web-pos",
            "sucursal_id": 1,
            "last_push_at": null,
            "last_pull_at": null,
            "pendientes": 0
        }),
        "configurar_sync"               => json!({
            "activo": true, "remote_url": null, "device_uuid": "web-pos",
            "sucursal_id": 1, "last_push_at": null, "last_pull_at": null, "pendientes": 0
        }),
        "desactivar_sync"               => Value::Null,
        "probar_conexion_remota"        => json!(true),
        "reenviar_pendientes"           => json!(0),

        // ─── Respaldos (no aplican en modo web — el server vive en Postgres
        //     y se respalda con el motor de Railway, no desde el frontend) ──
        "respaldo_auto_si_necesario"    => Value::Null,
        "listar_respaldos"              => json!([]),
        "crear_respaldo"                => Value::Null,

        // ─── Cortes / caja ───────────────────────────────────
        "obtener_apertura_hoy"          => obtener_apertura_hoy(&state).await?,
        "crear_apertura_caja"           => crear_apertura_caja(&state, &args).await?,
        "verificar_corte_dia_pendiente" => verificar_corte_dia_pendiente(&state).await?,
        "listar_movimientos_sin_corte"  => listar_movimientos_sin_corte(&state).await?,
        "listar_cortes"                 => listar_cortes(&state, &args).await?,
        "obtener_detalle_corte"         => obtener_detalle_corte(&state, &args).await?,
        "obtener_fondo_sugerido"        => obtener_fondo_sugerido(&state).await?,
        "crear_movimiento_caja"         => crear_movimiento_caja(&state, args).await?,
        "calcular_datos_corte"          => calcular_datos_corte(&state, &args).await?,
        "crear_corte"                   => crear_corte(&state, args).await?,

        // ─── Ventas ──────────────────────────────────────────
        "buscar_ventas"                 => buscar_ventas(&state, &args).await?,
        "obtener_detalle_venta"         => obtener_detalle_venta(&state, &args).await?,
        "crear_venta"                   => crear_venta(&state, args).await?,
        "listar_ventas_dia"             => listar_ventas_dia(&state).await?,
        "anular_venta"                  => anular_venta(&state, &args).await?,

        // ─── Estadísticas ────────────────────────────────────
        "obtener_estadisticas_dia"      => obtener_estadisticas_dia(&state, &args).await?,

        // ─── Bitácora (read) ─────────────────────────────────
        "listar_bitacora"               => listar_bitacora(&state, &args).await?,

        // ─── Presupuestos (stub) ─────────────────────────────
        "listar_presupuestos"           => json!([]),

        // ─── Devoluciones ────────────────────────────────────
        "listar_devoluciones"           => listar_devoluciones(&state, &args).await?,
        "obtener_detalle_devolucion"    => obtener_detalle_devolucion(&state, &args).await?,
        "crear_devolucion"              => crear_devolucion(&state, args).await?,

        // ─── Recepciones ─────────────────────────────────────
        "listar_recepciones"            => listar_recepciones(&state).await?,
        "obtener_detalle_recepcion"     => obtener_detalle_recepcion(&state, &args).await?,
        "crear_recepcion"               => crear_recepcion(&state, args).await?,

        // ─── Pedidos a proveedor ─────────────────────────────
        "listar_ordenes_pedido"         => listar_ordenes_pedido(&state, &args).await?,
        "obtener_detalle_orden"         => obtener_detalle_orden(&state, &args).await?,
        "crear_orden_pedido"            => crear_orden_pedido(&state, args).await?,
        "cambiar_estado_orden"          => cambiar_estado_orden(&state, &args).await?,

        // ─── PIN dueño (autorizaciones) ──────────────────────
        "verificar_pin_dueno"           => verificar_pin_dueno(&state, &args).await?,
        "resolver_dueno_por_pin"        => resolver_dueno_por_pin(&state, &args).await?,

        // ─── No soportado ────────────────────────────────────
        _ => {
            tracing::warn!("RPC no implementado: {}", cmd);
            return Err(ApiError::BadRequest(format!(
                "RPC '{}' no disponible en modo web",
                cmd
            )));
        }
    };
    Ok(Json(result))
}

// =============================================================================
// PRODUCTOS
// =============================================================================

async fn listar_productos(state: &AppState) -> Result<Value, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT p.id, p.codigo, p.codigo_tipo, p.nombre, p.descripcion,
               p.categoria_id, c.nombre AS categoria_nombre,
               p.precio_costo, p.precio_venta, p.stock_actual, p.stock_minimo,
               p.proveedor_id, pr.nombre AS proveedor_nombre,
               p.foto_url, p.activo
        FROM productos p
        LEFT JOIN categorias  c  ON c.id  = p.categoria_id
        LEFT JOIN proveedores pr ON pr.id = p.proveedor_id
        WHERE p.deleted_at IS NULL AND p.activo = 1
        ORDER BY p.nombre
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(json!(rows.iter().map(producto_row_to_json).collect::<Vec<_>>()))
}

async fn listar_productos_stock_bajo(state: &AppState) -> Result<Value, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT p.id, p.codigo, p.codigo_tipo, p.nombre, p.descripcion,
               p.categoria_id, c.nombre AS categoria_nombre,
               p.precio_costo, p.precio_venta, p.stock_actual, p.stock_minimo,
               p.proveedor_id, pr.nombre AS proveedor_nombre,
               p.foto_url, p.activo
        FROM productos p
        LEFT JOIN categorias  c  ON c.id  = p.categoria_id
        LEFT JOIN proveedores pr ON pr.id = p.proveedor_id
        WHERE p.deleted_at IS NULL AND p.activo = 1
          AND p.stock_actual <= p.stock_minimo
        ORDER BY p.stock_actual ASC
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(json!(rows.iter().map(producto_row_to_json).collect::<Vec<_>>()))
}

#[derive(Deserialize)]
struct CodigoArg { codigo: String }

async fn obtener_producto_por_codigo(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let a: CodigoArg = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let row_opt = sqlx::query(
        r#"
        SELECT p.id, p.codigo, p.codigo_tipo, p.nombre, p.descripcion,
               p.categoria_id, c.nombre AS categoria_nombre,
               p.precio_costo, p.precio_venta, p.stock_actual, p.stock_minimo,
               p.proveedor_id, pr.nombre AS proveedor_nombre,
               p.foto_url, p.activo
        FROM productos p
        LEFT JOIN categorias  c  ON c.id  = p.categoria_id
        LEFT JOIN proveedores pr ON pr.id = p.proveedor_id
        WHERE p.deleted_at IS NULL AND p.codigo = $1
        LIMIT 1
        "#,
    )
    .bind(&a.codigo)
    .fetch_optional(&state.pool)
    .await?;

    Ok(match row_opt {
        Some(r) => producto_row_to_json(&r),
        None    => Value::Null,
    })
}

/// Convierte row de productos en la shape exacta del struct `Producto` (Tauri).
/// Diferencias clave: `precio_*` y `stock_*` son NUMERIC → f64; `activo` es int→bool.
fn producto_row_to_json(row: &sqlx::postgres::PgRow) -> Value {
    use rust_decimal::prelude::ToPrimitive;
    let dec = |name: &str| -> f64 {
        row.try_get::<rust_decimal::Decimal, _>(name).ok()
            .and_then(|d| d.to_f64()).unwrap_or(0.0)
    };
    json!({
        "id":                row.get::<i64, _>("id"),
        "codigo":            row.get::<String, _>("codigo"),
        "codigo_tipo":       row.get::<String, _>("codigo_tipo"),
        "nombre":            row.get::<String, _>("nombre"),
        "descripcion":       row.try_get::<Option<String>, _>("descripcion").ok().flatten(),
        "categoria_id":      row.try_get::<Option<i64>, _>("categoria_id").ok().flatten(),
        "categoria_nombre":  row.try_get::<Option<String>, _>("categoria_nombre").ok().flatten(),
        "precio_costo":      dec("precio_costo"),
        "precio_venta":      dec("precio_venta"),
        "stock_actual":      dec("stock_actual"),
        "stock_minimo":      dec("stock_minimo"),
        "proveedor_id":      row.try_get::<Option<i64>, _>("proveedor_id").ok().flatten(),
        "proveedor_nombre":  row.try_get::<Option<String>, _>("proveedor_nombre").ok().flatten(),
        "foto_url":          row.try_get::<Option<String>, _>("foto_url").ok().flatten(),
        "activo":            row.get::<i32, _>("activo") != 0,
    })
}

// =============================================================================
// PRODUCTOS — MUTACIONES (web puede crear/editar/eliminar/ajustar stock)
// =============================================================================
//
// Toda mutación:
//   1. Escribe en Postgres con updated_at = NOW_TEXT
//   2. Inserta entrada en sync_cursor (origen 'web-pos') para que el desktop
//      la jale en su próximo pull
//   3. Registra en audit_log (web-side; el desktop tiene su propio audit local)

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NuevoProductoArgs {
    producto: NuevoProductoIn,
    #[serde(alias = "usuario_id")]
    usuario_id: i64,
}

#[derive(Deserialize)]
struct NuevoProductoIn {
    codigo: Option<String>,
    codigo_tipo: Option<String>,
    nombre: String,
    descripcion: Option<String>,
    categoria_id: Option<i64>,
    precio_costo: f64,
    precio_venta: f64,
    stock_actual: f64,
    stock_minimo: f64,
    proveedor_id: Option<i64>,
    foto_url: Option<String>,
}

async fn crear_producto(state: &AppState, args: Value) -> Result<Value, ApiError> {
    let a: NuevoProductoArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let p = a.producto;

    let mut tx = state.pool.begin().await?;

    // Generar código si no se proporcionó (atómico vía codigo_secuencia)
    let codigo = match p.codigo.filter(|c| !c.is_empty()) {
        Some(c) => c,
        None => {
            let nuevo: i64 = sqlx::query_scalar(
                "UPDATE codigo_secuencia SET ultimo_valor = ultimo_valor + 1 \
                 WHERE id = 1 RETURNING ultimo_valor",
            )
            .fetch_one(&mut *tx)
            .await?;
            format!("MR-{:05}", nuevo)
        }
    };

    let codigo_tipo = p.codigo_tipo.clone().unwrap_or_else(|| "INTERNO".to_string());
    let search_text = format!(
        "{} {} {}",
        codigo.to_lowercase(),
        p.nombre.to_lowercase(),
        p.descripcion.as_deref().unwrap_or("").to_lowercase(),
    );
    let uuid = uuid::Uuid::now_v7().to_string();

    let ins_sql = format!(
        r#"
        INSERT INTO productos
            (uuid, codigo, codigo_tipo, nombre, descripcion, categoria_id,
             precio_costo, precio_venta, stock_actual, stock_minimo,
             proveedor_id, foto_url, search_text, activo,
             created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 1,
                {NOW_TEXT}, {NOW_TEXT})
        RETURNING id, uuid, codigo, codigo_tipo, nombre, descripcion,
                  categoria_id, precio_costo, precio_venta, stock_actual,
                  stock_minimo, proveedor_id, foto_url, activo
        "#
    );
    let row = sqlx::query(&ins_sql)
        .bind(&uuid)
        .bind(&codigo)
        .bind(&codigo_tipo)
        .bind(&p.nombre)
        .bind(p.descripcion.as_deref())
        .bind(p.categoria_id)
        .bind(p.precio_costo)
        .bind(p.precio_venta)
        .bind(p.stock_actual)
        .bind(p.stock_minimo)
        .bind(p.proveedor_id)
        .bind(p.foto_url.as_deref())
        .bind(&search_text)
        .fetch_one(&mut *tx)
        .await?;
    let id: i64 = row.get("id");

    // sync_cursor → desktop pull
    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('productos', $1, 1, $2)",
    )
    .bind(&uuid)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    // Bitácora web
    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            descripcion_legible, origen, fecha)
           VALUES ($1, 'PRODUCTO_CREADO', 'productos', $2, $3, 'WEB', {NOW_TEXT})"#
    );
    let _ = sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(id)
        .bind(format!("Producto creado: {} ({})", p.nombre, codigo))
        .execute(&mut *tx)
        .await;

    tx.commit().await?;

    // Devolver shape `Producto` (compatible con el store del frontend)
    Ok(producto_row_to_json(&row))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActualizarProductoArgs {
    producto: ActualizarProductoIn,
    #[serde(alias = "usuario_id")]
    usuario_id: i64,
}

#[derive(Deserialize)]
struct ActualizarProductoIn {
    id: i64,
    codigo: String,
    nombre: String,
    descripcion: Option<String>,
    categoria_id: Option<i64>,
    precio_costo: f64,
    precio_venta: f64,
    stock_minimo: f64,
    proveedor_id: Option<i64>,
    foto_url: Option<String>,
}

async fn actualizar_producto(state: &AppState, args: Value) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    let a: ActualizarProductoArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let p = a.producto;

    let mut tx = state.pool.begin().await?;

    // Capturar precio anterior + datos para bitácora antes del UPDATE
    let prev = sqlx::query(
        "SELECT uuid, nombre, precio_costo, precio_venta \
         FROM productos WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(p.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound)?;

    let uuid: String = prev.get("uuid");
    let nombre_ant: String = prev.get("nombre");
    let precio_costo_ant = prev
        .try_get::<rust_decimal::Decimal, _>("precio_costo")
        .ok()
        .and_then(|d| d.to_f64())
        .unwrap_or(0.0);
    let precio_venta_ant = prev
        .try_get::<rust_decimal::Decimal, _>("precio_venta")
        .ok()
        .and_then(|d| d.to_f64())
        .unwrap_or(0.0);

    let search_text = format!(
        "{} {} {}",
        p.codigo.to_lowercase(),
        p.nombre.to_lowercase(),
        p.descripcion.as_deref().unwrap_or("").to_lowercase(),
    );

    let upd_sql = format!(
        r#"
        UPDATE productos SET
            codigo = $1, nombre = $2, descripcion = $3, categoria_id = $4,
            precio_costo = $5, precio_venta = $6,
            stock_minimo = $7, proveedor_id = $8, foto_url = $9,
            search_text = $10, updated_at = {NOW_TEXT}
        WHERE id = $11 AND deleted_at IS NULL
        "#
    );
    sqlx::query(&upd_sql)
        .bind(&p.codigo)
        .bind(&p.nombre)
        .bind(p.descripcion.as_deref())
        .bind(p.categoria_id)
        .bind(p.precio_costo)
        .bind(p.precio_venta)
        .bind(p.stock_minimo)
        .bind(p.proveedor_id)
        .bind(p.foto_url.as_deref())
        .bind(&search_text)
        .bind(p.id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('productos', $1, 1, $2)",
    )
    .bind(&uuid)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    let datos_ant_str = format!(
        "{} | costo:{:.2} | venta:{:.2}",
        nombre_ant, precio_costo_ant, precio_venta_ant
    );
    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            datos_anteriores, descripcion_legible, origen, fecha)
           VALUES ($1, 'PRODUCTO_EDITADO', 'productos', $2, $3, $4, 'WEB', {NOW_TEXT})"#
    );
    let _ = sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(p.id)
        .bind(&datos_ant_str)
        .bind(format!("Producto editado: {}", p.nombre))
        .execute(&mut *tx)
        .await;

    // Si cambió el precio de venta, dejar registro dedicado para historial_precios
    if (p.precio_venta - precio_venta_ant).abs() > 0.001 {
        let json_ant = format!("{{\"precio_venta\":{:.2}}}", precio_venta_ant);
        let json_new = format!("{{\"precio_venta\":{:.2}}}", p.precio_venta);
        let descr = format!(
            "Precio de '{}' cambió de ${:.2} a ${:.2}",
            p.nombre, precio_venta_ant, p.precio_venta
        );
        let pa_sql = format!(
            r#"INSERT INTO audit_log
               (usuario_id, accion, tabla_afectada, registro_id,
                datos_anteriores, datos_nuevos, descripcion_legible, origen, fecha)
               VALUES ($1, 'PRECIO_ACTUALIZADO', 'productos', $2, $3, $4, $5, 'WEB', {NOW_TEXT})"#
        );
        let _ = sqlx::query(&pa_sql)
            .bind(a.usuario_id)
            .bind(p.id)
            .bind(&json_ant)
            .bind(&json_new)
            .bind(&descr)
            .execute(&mut *tx)
            .await;
    }

    tx.commit().await?;
    Ok(json!(true))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EliminarProductoArgs {
    #[serde(alias = "producto_id")]
    producto_id: i64,
    #[serde(alias = "usuario_id")]
    usuario_id: i64,
}

async fn eliminar_producto(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let a: EliminarProductoArgs = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let mut tx = state.pool.begin().await?;

    // Capturar uuid + nombre antes del soft-delete
    let prev = sqlx::query(
        "SELECT uuid, nombre FROM productos \
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(a.producto_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound)?;
    let uuid: String = prev.get("uuid");
    let nombre: String = prev.get("nombre");

    let upd_sql = format!(
        "UPDATE productos SET activo = 0, deleted_at = {NOW_TEXT}, \
         updated_at = {NOW_TEXT} WHERE id = $1"
    );
    sqlx::query(&upd_sql)
        .bind(a.producto_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('productos', $1, 1, $2)",
    )
    .bind(&uuid)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            descripcion_legible, origen, fecha)
           VALUES ($1, 'PRODUCTO_ELIMINADO', 'productos', $2, $3, 'WEB', {NOW_TEXT})"#
    );
    let _ = sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(a.producto_id)
        .bind(format!("Producto eliminado: {}", nombre))
        .execute(&mut *tx)
        .await;

    tx.commit().await?;
    Ok(json!(true))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AjustarStockArgs {
    #[serde(alias = "producto_id")]
    producto_id: i64,
    #[serde(alias = "nuevo_stock")]
    nuevo_stock: f64,
    motivo: String,
    #[serde(alias = "usuario_id")]
    usuario_id: i64,
}

async fn ajustar_stock(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    let a: AjustarStockArgs = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    if a.motivo.trim().is_empty() {
        return Err(ApiError::BadRequest("El motivo es obligatorio".into()));
    }

    let mut tx = state.pool.begin().await?;

    let prev = sqlx::query(
        "SELECT uuid, nombre, stock_actual FROM productos \
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(a.producto_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound)?;
    let uuid: String = prev.get("uuid");
    let nombre: String = prev.get("nombre");
    let stock_anterior = prev
        .try_get::<rust_decimal::Decimal, _>("stock_actual")
        .ok()
        .and_then(|d| d.to_f64())
        .unwrap_or(0.0);

    let upd_sql = format!(
        "UPDATE productos SET stock_actual = $1, updated_at = {NOW_TEXT} WHERE id = $2"
    );
    sqlx::query(&upd_sql)
        .bind(a.nuevo_stock)
        .bind(a.producto_id)
        .execute(&mut *tx)
        .await?;

    let upd_suc_sql = format!(
        "UPDATE stock_sucursal SET stock_actual = $1, updated_at = {NOW_TEXT} \
         WHERE producto_id = $2 AND sucursal_id = 1"
    );
    let _ = sqlx::query(&upd_suc_sql)
        .bind(a.nuevo_stock)
        .bind(a.producto_id)
        .execute(&mut *tx)
        .await;

    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('productos', $1, 1, $2)",
    )
    .bind(&uuid)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    let diff = a.nuevo_stock - stock_anterior;
    let signo = if diff >= 0.0 { "+" } else { "" };
    let json_ant = format!("{{\"stock_actual\":{}}}", stock_anterior);
    let motivo_san = a.motivo.replace('\\', "\\\\").replace('"', "\\\"");
    let json_new = format!(
        "{{\"stock_actual\":{},\"motivo\":\"{}\"}}",
        a.nuevo_stock, motivo_san
    );
    let descr = format!(
        "Stock ajustado: {} ({}{}) — {}",
        nombre, signo, diff, a.motivo.trim()
    );
    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            datos_anteriores, datos_nuevos, descripcion_legible, origen, fecha)
           VALUES ($1, 'STOCK_AJUSTADO', 'productos', $2, $3, $4, $5, 'WEB', {NOW_TEXT})"#
    );
    let _ = sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(a.producto_id)
        .bind(&json_ant)
        .bind(&json_new)
        .bind(&descr)
        .execute(&mut *tx)
        .await;

    tx.commit().await?;
    Ok(json!(true))
}

async fn generar_codigo_interno(state: &AppState) -> Result<Value, ApiError> {
    // Solo previsualiza el siguiente sin consumirlo (no incrementa).
    let next: i64 = sqlx::query_scalar(
        "SELECT ultimo_valor + 1 FROM codigo_secuencia WHERE id = 1",
    )
    .fetch_one(&state.pool)
    .await?;
    Ok(json!(format!("MR-{:05}", next)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistorialArgs {
    #[serde(alias = "producto_id")]
    producto_id: i64,
}

async fn historial_precios_producto(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let a: HistorialArgs = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let rows = sqlx::query(
        r#"
        SELECT a.fecha, a.datos_anteriores, a.datos_nuevos,
               COALESCE(u.nombre_completo, 'Desconocido') AS usuario
        FROM audit_log a
        LEFT JOIN usuarios u ON u.id = a.usuario_id
        WHERE a.accion = 'PRECIO_ACTUALIZADO'
          AND a.tabla_afectada = 'productos'
          AND a.registro_id = $1
        ORDER BY a.fecha DESC
        "#,
    )
    .bind(a.producto_id)
    .fetch_all(&state.pool)
    .await?;

    let extraer = |s: &str| -> f64 {
        let key = "\"precio_venta\":";
        if let Some(i) = s.find(key) {
            let rest = &s[i + key.len()..];
            let end = rest.find(|c: char| c == ',' || c == '}').unwrap_or(rest.len());
            rest[..end].trim().parse::<f64>().unwrap_or(0.0)
        } else {
            0.0
        }
    };

    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let ant: Option<String> = r.try_get("datos_anteriores").ok().flatten();
            let new_: Option<String> = r.try_get("datos_nuevos").ok().flatten();
            json!({
                "fecha":           r.get::<String, _>("fecha"),
                "precio_anterior": extraer(&ant.unwrap_or_default()),
                "precio_nuevo":    extraer(&new_.unwrap_or_default()),
                "usuario_nombre":  r.get::<String, _>("usuario"),
            })
        })
        .collect();
    Ok(json!(items))
}

// =============================================================================
// CATÁLOGOS LIGEROS
// =============================================================================

async fn listar_categorias(state: &AppState) -> Result<Value, ApiError> {
    let rows = sqlx::query(
        "SELECT id, nombre, descripcion FROM categorias \
         WHERE deleted_at IS NULL ORDER BY nombre",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(json!(rows.iter().map(|r| json!({
        "id":          r.get::<i64, _>("id"),
        "nombre":      r.get::<String, _>("nombre"),
        "descripcion": r.try_get::<Option<String>, _>("descripcion").ok().flatten(),
    })).collect::<Vec<_>>()))
}

async fn listar_proveedores(state: &AppState) -> Result<Value, ApiError> {
    let rows = sqlx::query(
        "SELECT id, nombre, contacto, telefono, email, notas \
         FROM proveedores WHERE deleted_at IS NULL ORDER BY nombre",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(json!(rows.iter().map(|r| json!({
        "id":       r.get::<i64, _>("id"),
        "nombre":   r.get::<String, _>("nombre"),
        "contacto": r.try_get::<Option<String>, _>("contacto").ok().flatten(),
        "telefono": r.try_get::<Option<String>, _>("telefono").ok().flatten(),
        "email":    r.try_get::<Option<String>, _>("email").ok().flatten(),
        "notas":    r.try_get::<Option<String>, _>("notas").ok().flatten(),
    })).collect::<Vec<_>>()))
}

async fn listar_clientes(state: &AppState) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    let rows = sqlx::query(
        "SELECT id, nombre, telefono, email, descuento_porcentaje, notas, activo \
         FROM clientes WHERE deleted_at IS NULL ORDER BY nombre",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(json!(rows.iter().map(|r| json!({
        "id":       r.get::<i64, _>("id"),
        "nombre":   r.get::<String, _>("nombre"),
        "telefono": r.try_get::<Option<String>, _>("telefono").ok().flatten(),
        "email":    r.try_get::<Option<String>, _>("email").ok().flatten(),
        "descuento_porcentaje": r.try_get::<rust_decimal::Decimal, _>("descuento_porcentaje")
            .ok().and_then(|d| d.to_f64()).unwrap_or(0.0),
        "notas":    r.try_get::<Option<String>, _>("notas").ok().flatten(),
        "activo":   r.get::<i32, _>("activo") != 0,
    })).collect::<Vec<_>>()))
}

// =============================================================================
// USUARIOS / ROLES
// =============================================================================

async fn listar_usuarios(state: &AppState) -> Result<Value, ApiError> {
    let rows = sqlx::query(
        "SELECT id, nombre_completo, nombre_usuario, rol_id, activo, ultimo_login \
         FROM usuarios WHERE deleted_at IS NULL ORDER BY nombre_completo",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(json!(rows.iter().map(|r| json!({
        "id":              r.get::<i64, _>("id"),
        "nombre_completo": r.get::<String, _>("nombre_completo"),
        "nombre_usuario":  r.get::<String, _>("nombre_usuario"),
        "rol_id":          r.get::<i64, _>("rol_id"),
        "rol_nombre":      rol_nombre_por_id(r.get::<i64, _>("rol_id")),
        "es_admin":        rol_es_admin(r.get::<i64, _>("rol_id")),
        "activo":          r.get::<i32, _>("activo") != 0,
        "ultimo_login":    r.try_get::<Option<String>, _>("ultimo_login").ok().flatten(),
    })).collect::<Vec<_>>()))
}

/// En el POS local hay tabla `roles` con 3 filas seed. En web se hardcodea.
fn listar_roles() -> Value {
    json!([
        { "id": 1, "nombre": "dueño",    "es_admin": true  },
        { "id": 2, "nombre": "gerente",  "es_admin": true  },
        { "id": 3, "nombre": "vendedor", "es_admin": false },
    ])
}

fn rol_nombre_por_id(id: i64) -> &'static str {
    match id { 1 => "dueño", 2 => "gerente", _ => "vendedor" }
}
fn rol_es_admin(id: i64) -> bool { id <= 2 }

// =============================================================================
// CONFIG NEGOCIO / DESCUENTOS (web POS usa defaults)
// =============================================================================

fn obtener_config_negocio() -> Value {
    json!({
        "nombre": "Moto Refaccionaria",
        "direccion": "",
        "telefono": "",
        "rfc": "",
        "mensaje_pie": "¡Gracias por su compra!",
        "respaldo_auto_activo": false,
        "respaldo_auto_hora": "23:00",
        "impresora_termica": "",
    })
}

fn obtener_config_descuentos() -> Value {
    json!({
        "descuento_max_vendedor_pct": 15.0,
        "descuento_max_total_pct":    10.0,
        "precio_minimo_global_margen": 5.0,
    })
}

async fn obtener_info_servidor(_state: &AppState) -> Result<Value, ApiError> {
    // El servidor LAN para PWA móvil solo existe en el desktop. En el web
    // devolvemos `activo: false` con la misma forma que ServerInfo en el
    // frontend para que la página no truene al renderizar.
    Ok(json!({
        "activo": false,
        "port": 0,
        "ips": []
    }))
}

async fn listar_dispositivos(_state: &AppState) -> Result<Value, ApiError> {
    // Igual: la lista de dispositivos PWA pertenece al desktop. En web,
    // arreglo vacío con la forma que el frontend espera.
    Ok(json!([]))
}

// =============================================================================
// VENTAS — lectura
// =============================================================================

#[derive(Deserialize, Default)]
struct BuscarVentasArgs {
    #[serde(default)] limite: Option<i64>,
    #[serde(default)] texto:  Option<String>,
}

async fn buscar_ventas(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    let a: BuscarVentasArgs = serde_json::from_value(args.clone()).unwrap_or_default();
    let limite  = a.limite.unwrap_or(100).clamp(1, 500);
    let q_like  = a.texto.as_ref().map(|s| format!("%{}%", s.to_lowercase()));

    let rows = sqlx::query(
        r#"
        SELECT v.id, v.folio, v.total, v.metodo_pago, v.anulada, v.fecha,
               u.nombre_completo AS usuario_nombre,
               c.nombre AS cliente_nombre,
               COALESCE((SELECT COUNT(*) FROM venta_detalle vd
                         WHERE vd.venta_id = v.id AND vd.deleted_at IS NULL), 0)
                 AS num_productos
        FROM ventas v
        LEFT JOIN usuarios u ON u.id = v.usuario_id
        LEFT JOIN clientes c ON c.id = v.cliente_id
        WHERE v.deleted_at IS NULL
          AND ($1::text IS NULL
               OR lower(v.folio) LIKE $1
               OR lower(COALESCE(c.nombre,'')) LIKE $1)
        ORDER BY v.fecha DESC
        LIMIT $2
        "#,
    )
    .bind(&q_like)
    .bind(limite)
    .fetch_all(&state.pool)
    .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "id":              r.get::<i64, _>("id"),
        "folio":           r.get::<String, _>("folio"),
        "usuario_nombre":  r.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
        "cliente_nombre":  r.try_get::<Option<String>, _>("cliente_nombre").ok().flatten(),
        "total":           r.try_get::<rust_decimal::Decimal, _>("total").ok()
                              .and_then(|d| d.to_f64()).unwrap_or(0.0),
        "metodo_pago":     r.get::<String, _>("metodo_pago"),
        "anulada":         r.get::<i32, _>("anulada") != 0,
        "fecha":           r.get::<String, _>("fecha"),
        "num_productos":   r.get::<i64, _>("num_productos"),
    })).collect::<Vec<_>>()))
}

#[derive(Deserialize)]
struct VentaIdArg {
    // Aceptamos las tres variantes: el frontend manda `ventaId` (camelCase
    // que Tauri genera a partir de `venta_id`), pero algunas pantallas
    // mandan `id` directo o `venta_id` snake.
    #[serde(alias = "ventaId", alias = "venta_id")]
    id: i64,
}

async fn obtener_detalle_venta(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    let a: VentaIdArg = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let v = sqlx::query(
        r#"
        SELECT v.id, v.folio, v.usuario_id, u.nombre_completo AS usuario_nombre,
               v.cliente_id, c.nombre AS cliente_nombre,
               v.subtotal, v.descuento, v.total, v.metodo_pago,
               v.monto_recibido, v.cambio, v.anulada, v.motivo_anulacion, v.fecha
        FROM ventas v
        LEFT JOIN usuarios u ON u.id = v.usuario_id
        LEFT JOIN clientes c ON c.id = v.cliente_id
        WHERE v.id = $1 AND v.deleted_at IS NULL
        "#,
    )
    .bind(a.id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::NotFound)?;

    let dec = |r: &sqlx::postgres::PgRow, name: &str| -> f64 {
        r.try_get::<rust_decimal::Decimal, _>(name).ok()
            .and_then(|d| d.to_f64()).unwrap_or(0.0)
    };

    let items = sqlx::query(
        r#"
        SELECT vd.id, vd.producto_id, p.codigo, p.nombre,
               vd.cantidad, vd.precio_original, vd.descuento_porcentaje,
               vd.descuento_monto, vd.precio_final, vd.subtotal,
               COALESCE((SELECT SUM(dd.cantidad)
                         FROM devolucion_detalle dd
                         WHERE dd.venta_detalle_id = vd.id), 0) AS cantidad_devuelta
        FROM venta_detalle vd
        LEFT JOIN productos p ON p.id = vd.producto_id
        WHERE vd.venta_id = $1 AND vd.deleted_at IS NULL
        ORDER BY vd.id
        "#,
    )
    .bind(a.id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    let items_json: Vec<Value> = items.iter().map(|r| {
        let cant      = dec(r, "cantidad");
        let devuelta  = dec(r, "cantidad_devuelta");
        json!({
            "id":                   r.get::<i64, _>("id"),
            "producto_id":          r.get::<i64, _>("producto_id"),
            "codigo":               r.try_get::<Option<String>, _>("codigo").ok().flatten().unwrap_or_default(),
            "nombre":               r.try_get::<Option<String>, _>("nombre").ok().flatten().unwrap_or_default(),
            "cantidad":             cant,
            "cantidad_devuelta":    devuelta,
            "cantidad_disponible":  (cant - devuelta).max(0.0),
            "precio_original":      dec(r, "precio_original"),
            "descuento_porcentaje": dec(r, "descuento_porcentaje"),
            "descuento_monto":      dec(r, "descuento_monto"),
            "precio_final":         dec(r, "precio_final"),
            "subtotal":             dec(r, "subtotal"),
        })
    }).collect();

    Ok(json!({
        "id":               v.get::<i64, _>("id"),
        "folio":            v.get::<String, _>("folio"),
        "usuario_id":       v.get::<i64, _>("usuario_id"),
        "usuario_nombre":   v.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
        "cliente_id":       v.try_get::<Option<i64>, _>("cliente_id").ok().flatten(),
        "cliente_nombre":   v.try_get::<Option<String>, _>("cliente_nombre").ok().flatten(),
        "subtotal":         dec(&v, "subtotal"),
        "descuento":        dec(&v, "descuento"),
        "total":            dec(&v, "total"),
        "metodo_pago":      v.get::<String, _>("metodo_pago"),
        "monto_recibido":   dec(&v, "monto_recibido"),
        "cambio":           dec(&v, "cambio"),
        "anulada":          v.get::<i32, _>("anulada") != 0,
        "motivo_anulacion": v.try_get::<Option<String>, _>("motivo_anulacion").ok().flatten(),
        "fecha":            v.get::<String, _>("fecha"),
        "items":            items_json,
    }))
}

// =============================================================================
// VENTAS — crear (write crítico)
// =============================================================================

#[derive(Deserialize)]
struct CrearVentaArgs {
    // El cliente desktop manda `{ venta: {...} }` (param de Tauri).
    // Mantenemos `datos` como alias por compatibilidad con clientes viejos.
    #[serde(alias = "datos")]
    venta: NuevaVentaWeb,
}

#[derive(Deserialize)]
struct NuevaVentaWeb {
    usuario_id: i64,
    cliente_id: Option<i64>,
    subtotal: f64,
    descuento: f64,
    total: f64,
    metodo_pago: String,
    monto_recibido: f64,
    cambio: f64,
    items: Vec<ItemVentaWeb>,
    #[serde(default)] presupuesto_origen_id: Option<i64>,
}

#[derive(Deserialize)]
struct ItemVentaWeb {
    producto_id: i64,
    cantidad: f64,
    precio_original: f64,
    descuento_porcentaje: f64,
    descuento_monto: f64,
    precio_final: f64,
    subtotal: f64,
    #[serde(default)] autorizado_por: Option<i64>,
}

async fn crear_venta(state: &AppState, args: Value) -> Result<Value, ApiError> {
    let a: CrearVentaArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let d = a.venta;

    if d.items.is_empty() {
        return Err(ApiError::BadRequest("La venta no tiene items".into()));
    }

    // Sucursal: por ahora, web POS usa sucursal 1 (principal). Cuando haya
    // multi-sucursal per-user, se toma del claims del JWT.
    let sucursal_id: i64 = 1;

    let mut tx = state.pool.begin().await?;

    // Validación de stock (lock FOR UPDATE)
    for it in &d.items {
        let stock: Option<rust_decimal::Decimal> = sqlx::query_scalar(
            "SELECT stock_actual FROM productos \
             WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(it.producto_id)
        .fetch_optional(&mut *tx)
        .await?;

        let disponible = match stock {
            Some(d) => {
                use rust_decimal::prelude::ToPrimitive;
                d.to_f64().unwrap_or(0.0)
            }
            None => return Err(ApiError::BadRequest(format!(
                "Producto {} no existe", it.producto_id))),
        };
        if disponible < it.cantidad {
            return Err(ApiError::BadRequest(format!(
                "Stock insuficiente para producto {} (disponible: {}, pedido: {})",
                it.producto_id, disponible, it.cantidad)));
        }
    }

    // Generar folio consecutivo: V-YYYYMMDD-NNNN
    let hoy: String = sqlx::query_scalar(
        "SELECT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYYMMDD')",
    )
    .fetch_one(&mut *tx)
    .await?;
    let count_hoy: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM ventas
           WHERE folio LIKE $1 AND deleted_at IS NULL"#,
    )
    .bind(format!("V-{}-%", hoy))
    .fetch_one(&mut *tx)
    .await?;
    let folio = format!("V-{}-{:04}", hoy, count_hoy + 1);

    let venta_uuid = uuid::Uuid::now_v7().to_string();

    // Insertar venta
    let insert_sql = format!(
        r#"
        INSERT INTO ventas
          (uuid, sucursal_id, folio, usuario_id, cliente_id,
           subtotal, descuento, total, metodo_pago,
           monto_recibido, cambio, anulada, fecha, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 0, {NOW_TEXT}, {NOW_TEXT})
        RETURNING id, fecha
        "#
    );
    let vrow = sqlx::query(&insert_sql)
        .bind(&venta_uuid)
        .bind(sucursal_id)
        .bind(&folio)
        .bind(d.usuario_id)
        .bind(d.cliente_id)
        .bind(d.subtotal)
        .bind(d.descuento)
        .bind(d.total)
        .bind(&d.metodo_pago)
        .bind(d.monto_recibido)
        .bind(d.cambio)
        .fetch_one(&mut *tx)
        .await?;
    let venta_id: i64 = vrow.get("id");
    let fecha: String = vrow.get("fecha");

    // Insertar detalle + descontar stock
    for it in &d.items {
        let det_uuid = uuid::Uuid::now_v7().to_string();
        let det_sql = format!(
            r#"
            INSERT INTO venta_detalle
              (uuid, venta_id, producto_id, cantidad,
               precio_original, descuento_porcentaje, descuento_monto,
               precio_final, subtotal, autorizado_por, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, {NOW_TEXT})
            "#
        );
        sqlx::query(&det_sql)
            .bind(&det_uuid)
            .bind(venta_id)
            .bind(it.producto_id)
            .bind(it.cantidad)
            .bind(it.precio_original)
            .bind(it.descuento_porcentaje)
            .bind(it.descuento_monto)
            .bind(it.precio_final)
            .bind(it.subtotal)
            .bind(it.autorizado_por)
            .execute(&mut *tx)
            .await?;

        let upd_sql = format!(
            "UPDATE productos SET stock_actual = stock_actual - $1, \
             updated_at = {NOW_TEXT} WHERE id = $2"
        );
        sqlx::query(&upd_sql)
            .bind(it.cantidad)
            .bind(it.producto_id)
            .execute(&mut *tx)
            .await?;

        // sync_cursor para productos (stock cambió)
        sqlx::query(
            "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
             SELECT 'productos', p.uuid, $1, $2 FROM productos p WHERE p.id = $3",
        )
        .bind(sucursal_id)
        .bind(WEB_ORIGIN)
        .bind(it.producto_id)
        .execute(&mut *tx)
        .await?;
    }

    // Marcar presupuesto como convertido si aplica
    if let Some(pid) = d.presupuesto_origen_id {
        let upd_pres = format!(
            "UPDATE presupuestos SET estado = 'convertido', venta_id = $1, \
             updated_at = {NOW_TEXT} WHERE id = $2"
        );
        let _ = sqlx::query(&upd_pres)
            .bind(venta_id)
            .bind(pid)
            .execute(&mut *tx)
            .await;
    }

    // sync_cursor para la venta
    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind("ventas")
    .bind(&venta_uuid)
    .bind(sucursal_id)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(json!({
        "id":     venta_id,
        "folio":  folio,
        "total":  d.total,
        "cambio": d.cambio,
        "fecha":  fecha,
    }))
}

// =============================================================================
// ESTADÍSTICAS DÍA
// =============================================================================

async fn obtener_estadisticas_dia(state: &AppState, _args: &Value) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    // Misma shape que el comando Tauri `obtener_estadisticas_dia`:
    //   { total_ventas, num_transacciones, efectivo, tarjeta, transferencia,
    //     producto_top_nombre, producto_top_cantidad }
    // El frontend hace `stats.total_ventas.toFixed(2)` etc., así que
    // CUALQUIER campo faltante revienta el render del Dashboard.
    let dec = |row: &sqlx::postgres::PgRow, name: &str| -> f64 {
        row.try_get::<rust_decimal::Decimal, _>(name).ok()
            .and_then(|d| d.to_f64()).unwrap_or(0.0)
    };

    let row = sqlx::query(
        r#"
        SELECT
          COALESCE(SUM(total), 0)::numeric AS total_ventas,
          COUNT(*)::bigint                 AS num_transacciones,
          COALESCE(SUM(CASE WHEN metodo_pago = 'efectivo'      THEN total ELSE 0 END), 0)::numeric AS efectivo,
          COALESCE(SUM(CASE WHEN metodo_pago = 'tarjeta'       THEN total ELSE 0 END), 0)::numeric AS tarjeta,
          COALESCE(SUM(CASE WHEN metodo_pago = 'transferencia' THEN total ELSE 0 END), 0)::numeric AS transferencia
        FROM ventas
        WHERE deleted_at IS NULL AND anulada = 0
          AND substr(fecha, 1, 10)
              = to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD')
        "#,
    )
    .fetch_one(&state.pool)
    .await?;

    let num: i64 = row.try_get("num_transacciones").unwrap_or(0);

    // Producto top del día
    let top = sqlx::query(
        r#"
        SELECT p.nombre, SUM(vd.cantidad)::numeric AS qty
          FROM venta_detalle vd
          JOIN ventas v   ON v.id = vd.venta_id
          JOIN productos p ON p.id = vd.producto_id
         WHERE v.deleted_at IS NULL AND v.anulada = 0
           AND substr(v.fecha, 1, 10)
               = to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD')
         GROUP BY vd.producto_id, p.nombre
         ORDER BY qty DESC
         LIMIT 1
        "#,
    )
    .fetch_optional(&state.pool)
    .await?;

    let (top_nombre, top_cant): (Option<String>, f64) = match top {
        Some(r) => (
            r.try_get::<String, _>("nombre").ok(),
            r.try_get::<rust_decimal::Decimal, _>("qty").ok()
                .and_then(|d| d.to_f64()).unwrap_or(0.0),
        ),
        None => (None, 0.0),
    };

    Ok(json!({
        "total_ventas":           dec(&row, "total_ventas"),
        "num_transacciones":      num,
        "efectivo":               dec(&row, "efectivo"),
        "tarjeta":                dec(&row, "tarjeta"),
        "transferencia":          dec(&row, "transferencia"),
        "producto_top_nombre":    top_nombre,
        "producto_top_cantidad":  top_cant,
    }))
}

// =============================================================================
// PIN DUEÑO (autorizaciones)
// =============================================================================

#[derive(Deserialize)]
struct PinArg { pin: String }

async fn verificar_pin_dueno(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let a: PinArg = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    Ok(json!(buscar_dueno_por_pin(state, &a.pin).await?.is_some()))
}

// =============================================================================
// LOGIN (web POS)
// =============================================================================
//
// Devuelve la shape exacta que espera authStore.ts:
//   { ok: bool, usuario?: UsuarioSesion, error?: string, token?: string }
// El campo `token` extra (no existe en Tauri) lo persiste invokeCompat en
// localStorage para autenticar las llamadas RPC subsecuentes.

async fn login_pin(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { pin: String }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    // El campo `pin` se almacena como bcrypt hash (igual que en el POS local).
    // No podemos buscar por igualdad — hay que iterar usuarios activos y
    // verificar el hash. La cantidad de usuarios es chica (decenas), así que ok.
    let candidatos = sqlx::query(
        "SELECT id, nombre_completo, nombre_usuario, rol_id, pin \
         FROM usuarios \
         WHERE activo = 1 AND deleted_at IS NULL",
    )
    .fetch_all(&state.pool)
    .await?;

    for r in &candidatos {
        let hash: String = r.get("pin");
        if bcrypt::verify(&a.pin, &hash).unwrap_or(false) {
            return usuario_sesion_response(&state, r).await;
        }
    }
    Ok(json!({ "ok": false, "error": "PIN incorrecto" }))
}

async fn login_password(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A {
        #[serde(rename = "nombreUsuario")] nombre_usuario: String,
        password: String,
    }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let row_opt = sqlx::query(
        "SELECT id, nombre_completo, nombre_usuario, rol_id, password_hash \
         FROM usuarios \
         WHERE lower(nombre_usuario) = lower($1) \
           AND activo = 1 AND deleted_at IS NULL \
         LIMIT 1",
    )
    .bind(&a.nombre_usuario)
    .fetch_optional(&state.pool)
    .await?;

    let r = match row_opt {
        Some(r) => r,
        None    => return Ok(json!({ "ok": false, "error": "Credenciales incorrectas" })),
    };
    let hash: String = r.get("password_hash");
    let ok = bcrypt::verify(&a.password, &hash).unwrap_or(false);
    if !ok {
        return Ok(json!({ "ok": false, "error": "Credenciales incorrectas" }));
    }
    usuario_sesion_response(&state, &r).await
}

/// Construye la respuesta `{ ok: true, usuario, token }` a partir de un row de usuarios.
async fn usuario_sesion_response(
    state: &AppState,
    r: &sqlx::postgres::PgRow,
) -> Result<Value, ApiError> {
    let id: i64               = r.get("id");
    let nombre_completo: String = r.get("nombre_completo");
    let nombre_usuario: String  = r.get("nombre_usuario");
    let rol_id: i64           = r.get("rol_id");
    let es_admin              = rol_es_admin(rol_id);

    // JWT que el frontend usa para todas las RPC siguientes.
    let token = emitir_token(
        &state.jwt_secret,
        id,
        &nombre_usuario,
        if es_admin { "admin" } else { "device" },
        1,
        chrono::Duration::days(7),
    )?;

    Ok(json!({
        "ok": true,
        "token": token,
        "usuario": {
            "id":              id,
            "nombre_completo": nombre_completo,
            "nombre_usuario":  nombre_usuario,
            "rol_id":          rol_id,
            "rol_nombre":      rol_nombre_por_id(rol_id),
            "es_admin":        es_admin,
            "sesion_id":       chrono::Utc::now().timestamp(), // stateless: timestamp
            "permisos":        [],
        },
    }))
}

async fn resolver_dueno_por_pin(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let a: PinArg = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    Ok(match buscar_dueno_por_pin(state, &a.pin).await? {
        Some(id) => json!(id),
        None     => Value::Null,
    })
}

// =============================================================================
// RECEPCIONES (entrada de mercancía)
// =============================================================================

async fn listar_recepciones(state: &AppState) -> Result<Value, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT r.id, u.nombre_completo AS usuario_nombre,
               p.nombre AS proveedor_nombre, r.fecha, r.notas,
               COALESCE((SELECT COUNT(*) FROM recepcion_detalle rd
                         WHERE rd.recepcion_id = r.id AND rd.deleted_at IS NULL), 0)
                 AS total_items
        FROM recepciones r
        LEFT JOIN usuarios u ON u.id = r.usuario_id
        LEFT JOIN proveedores p ON p.id = r.proveedor_id
        WHERE r.deleted_at IS NULL
        ORDER BY r.fecha DESC
        LIMIT 200
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "id":               r.get::<i64, _>("id"),
        "usuario_nombre":   r.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
        "proveedor_nombre": r.try_get::<Option<String>, _>("proveedor_nombre").ok().flatten(),
        "fecha":            r.get::<String, _>("fecha"),
        "notas":            r.try_get::<Option<String>, _>("notas").ok().flatten(),
        "total_items":      r.get::<i64, _>("total_items"),
    })).collect::<Vec<_>>()))
}

async fn obtener_detalle_recepcion(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    #[derive(Deserialize)]
    struct A {
        #[serde(rename = "recepcionId")] recepcion_id: i64,
    }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let rows = sqlx::query(
        r#"
        SELECT rd.id, rd.producto_id, p.nombre AS producto_nombre,
               p.codigo AS producto_codigo,
               rd.cantidad, rd.precio_costo
        FROM recepcion_detalle rd
        LEFT JOIN productos p ON p.id = rd.producto_id
        WHERE rd.recepcion_id = $1 AND rd.deleted_at IS NULL
        ORDER BY rd.id
        "#,
    )
    .bind(a.recepcion_id)
    .fetch_all(&state.pool)
    .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "id":               r.get::<i64, _>("id"),
        "producto_id":      r.get::<i64, _>("producto_id"),
        "producto_nombre":  r.try_get::<Option<String>, _>("producto_nombre").ok().flatten().unwrap_or_default(),
        "producto_codigo":  r.try_get::<Option<String>, _>("producto_codigo").ok().flatten().unwrap_or_default(),
        "cantidad":         r.try_get::<rust_decimal::Decimal, _>("cantidad").ok()
                              .and_then(|d| d.to_f64()).unwrap_or(0.0),
        "precio_costo":     r.try_get::<rust_decimal::Decimal, _>("precio_costo").ok()
                              .and_then(|d| d.to_f64()).unwrap_or(0.0),
    })).collect::<Vec<_>>()))
}

async fn crear_recepcion(state: &AppState, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { recepcion: DatosRecepcionWeb }
    #[derive(Deserialize)]
    struct DatosRecepcionWeb {
        usuario_id: i64,
        #[serde(default)] proveedor_id: Option<i64>,
        #[serde(default)] orden_id: Option<i64>,
        #[serde(default)] notas: Option<String>,
        items: Vec<ItemRecepcionWeb>,
    }
    #[derive(Deserialize)]
    struct ItemRecepcionWeb {
        producto_id: i64,
        cantidad: f64,
        precio_costo: f64,
        /// Nuevo precio de venta opcional (multiplicadores 1.4/1.5/1.7).
        /// Si viene Some(v) con v > 0, se actualiza productos.precio_venta.
        #[serde(default)]
        precio_venta: Option<f64>,
    }

    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let r = a.recepcion;

    if r.items.is_empty() {
        return Err(ApiError::BadRequest("La recepción no tiene items".into()));
    }

    let sucursal_id: i64 = 1;
    let mut tx = state.pool.begin().await?;

    // Cabecera
    let recep_uuid = uuid::Uuid::now_v7().to_string();
    let cab_sql = format!(
        r#"
        INSERT INTO recepciones
          (uuid, sucursal_id, orden_id, usuario_id, proveedor_id, fecha, notas, updated_at)
        VALUES ($1, $2, $3, $4, $5, {NOW_TEXT}, $6, {NOW_TEXT})
        RETURNING id, fecha
        "#
    );
    let crow = sqlx::query(&cab_sql)
        .bind(&recep_uuid)
        .bind(sucursal_id)
        .bind(r.orden_id)
        .bind(r.usuario_id)
        .bind(r.proveedor_id)
        .bind(r.notas.as_deref())
        .fetch_one(&mut *tx)
        .await?;
    let recep_id: i64 = crow.get("id");
    let fecha: String = crow.get("fecha");
    let total_items = r.items.len() as i64;

    // Detalle + actualización de stock + (si aplica) cantidad_recibida en orden
    for it in &r.items {
        let det_uuid = uuid::Uuid::now_v7().to_string();
        let det_sql = format!(
            r#"
            INSERT INTO recepcion_detalle
              (uuid, recepcion_id, producto_id, cantidad, precio_costo, updated_at)
            VALUES ($1, $2, $3, $4, $5, {NOW_TEXT})
            "#
        );
        sqlx::query(&det_sql)
            .bind(&det_uuid)
            .bind(recep_id)
            .bind(it.producto_id)
            .bind(it.cantidad)
            .bind(it.precio_costo)
            .execute(&mut *tx)
            .await?;

        // Sumar al stock + actualizar precio_costo (y precio_venta si vino > 0).
        // Si precio_venta viene None / 0, dejamos el existente intacto.
        let nuevo_pv = it.precio_venta.filter(|v| *v > 0.0);
        if let Some(pv) = nuevo_pv {
            let upd_sql = format!(
                "UPDATE productos SET stock_actual = stock_actual + $1, \
                 precio_costo = $2, precio_venta = $3, updated_at = {NOW_TEXT} WHERE id = $4"
            );
            sqlx::query(&upd_sql)
                .bind(it.cantidad)
                .bind(it.precio_costo)
                .bind(pv)
                .bind(it.producto_id)
                .execute(&mut *tx)
                .await?;
        } else {
            let upd_sql = format!(
                "UPDATE productos SET stock_actual = stock_actual + $1, \
                 precio_costo = $2, updated_at = {NOW_TEXT} WHERE id = $3"
            );
            sqlx::query(&upd_sql)
                .bind(it.cantidad)
                .bind(it.precio_costo)
                .bind(it.producto_id)
                .execute(&mut *tx)
                .await?;
        }

        // sync_cursor productos
        sqlx::query(
            "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
             SELECT 'productos', p.uuid, $1, $2 FROM productos p WHERE p.id = $3",
        )
        .bind(sucursal_id)
        .bind(WEB_ORIGIN)
        .bind(it.producto_id)
        .execute(&mut *tx)
        .await?;

        // Si es contra una orden, sumar a cantidad_recibida en el detalle
        if let Some(oid) = r.orden_id {
            let upd_pd_sql = format!(
                "UPDATE orden_pedido_detalle \
                 SET cantidad_recibida = cantidad_recibida + $1, updated_at = {NOW_TEXT} \
                 WHERE orden_id = $2 AND producto_id = $3"
            );
            let _ = sqlx::query(&upd_pd_sql)
                .bind(it.cantidad)
                .bind(oid)
                .bind(it.producto_id)
                .execute(&mut *tx)
                .await;
        }
    }

    // Si hay orden, recalcular su estado (completa / parcial)
    if let Some(oid) = r.orden_id {
        use rust_decimal::prelude::ToPrimitive;
        let faltante: rust_decimal::Decimal = sqlx::query_scalar(
            "SELECT COALESCE(SUM(CASE WHEN cantidad_pedida > cantidad_recibida \
                                      THEN cantidad_pedida - cantidad_recibida ELSE 0 END), 0) \
             FROM orden_pedido_detalle WHERE orden_id = $1",
        )
        .bind(oid)
        .fetch_one(&mut *tx)
        .await
        .unwrap_or_default();
        let f64_falt = faltante.to_f64().unwrap_or(0.0);
        let nuevo_estado = if f64_falt <= 0.0 { "recibida_completa" } else { "recibida_parcial" };
        let upd_orden_sql = format!(
            "UPDATE ordenes_pedido SET estado = $1, fecha_recepcion = {NOW_TEXT}, \
             updated_at = {NOW_TEXT} WHERE id = $2"
        );
        let _ = sqlx::query(&upd_orden_sql)
            .bind(nuevo_estado)
            .bind(oid)
            .execute(&mut *tx)
            .await;
    }

    // sync_cursor para la recepción
    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('recepciones', $1, $2, $3)",
    )
    .bind(&recep_uuid)
    .bind(sucursal_id)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // Resolver nombres para el response
    let usuario_nombre: String = sqlx::query_scalar(
        "SELECT nombre_completo FROM usuarios WHERE id = $1",
    )
    .bind(r.usuario_id)
    .fetch_optional(&state.pool)
    .await?
    .unwrap_or_else(|| "—".into());

    let proveedor_nombre: Option<String> = match r.proveedor_id {
        Some(pid) => sqlx::query_scalar("SELECT nombre FROM proveedores WHERE id = $1")
            .bind(pid)
            .fetch_optional(&state.pool)
            .await?,
        None => None,
    };

    Ok(json!({
        "id":               recep_id,
        "usuario_nombre":   usuario_nombre,
        "proveedor_nombre": proveedor_nombre,
        "fecha":            fecha,
        "notas":            r.notas,
        "total_items":      total_items,
    }))
}

// =============================================================================
// DEVOLUCIONES
// =============================================================================

async fn listar_devoluciones(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;

    #[derive(Deserialize, Default)]
    struct A { #[serde(default)] limite: Option<i64> }
    let a: A = serde_json::from_value(args.clone()).unwrap_or_default();
    let limite = a.limite.unwrap_or(100);

    let rows = sqlx::query(
        r#"
        SELECT d.id, d.folio, d.venta_id, v.folio AS venta_folio,
               u.nombre_completo AS usuario_nombre,
               ua.nombre_completo AS autorizado_por_nombre,
               d.motivo, d.total_devuelto,
               (SELECT COUNT(*) FROM devolucion_detalle dd
                  WHERE dd.devolucion_id = d.id AND dd.deleted_at IS NULL) AS num_items,
               d.fecha
        FROM devoluciones d
        JOIN ventas v       ON v.id = d.venta_id
        JOIN usuarios u     ON u.id = d.usuario_id
        LEFT JOIN usuarios ua ON ua.id = d.autorizado_por
        WHERE d.deleted_at IS NULL
        ORDER BY d.fecha DESC
        LIMIT $1
        "#,
    )
    .bind(limite)
    .fetch_all(&state.pool)
    .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "id":                    r.get::<i64, _>("id"),
        "folio":                 r.get::<String, _>("folio"),
        "venta_id":              r.get::<i64, _>("venta_id"),
        "venta_folio":           r.get::<String, _>("venta_folio"),
        "usuario_nombre":        r.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
        "autorizado_por_nombre": r.try_get::<Option<String>, _>("autorizado_por_nombre").ok().flatten(),
        "motivo":                r.get::<String, _>("motivo"),
        "total_devuelto":        r.try_get::<rust_decimal::Decimal, _>("total_devuelto").ok()
                                    .and_then(|d| d.to_f64()).unwrap_or(0.0),
        "num_items":             r.get::<i64, _>("num_items"),
        "fecha":                 r.get::<String, _>("fecha"),
    })).collect::<Vec<_>>()))
}

async fn obtener_detalle_devolucion(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;

    #[derive(Deserialize)]
    struct A { id: i64 }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let cab = sqlx::query(
        r#"
        SELECT d.id, d.folio, d.venta_id, v.folio AS venta_folio,
               u.nombre_completo AS usuario_nombre,
               ua.nombre_completo AS autorizado_por_nombre,
               d.motivo, d.total_devuelto,
               (SELECT COUNT(*) FROM devolucion_detalle dd
                  WHERE dd.devolucion_id = d.id AND dd.deleted_at IS NULL) AS num_items,
               d.fecha
        FROM devoluciones d
        JOIN ventas v       ON v.id = d.venta_id
        JOIN usuarios u     ON u.id = d.usuario_id
        LEFT JOIN usuarios ua ON ua.id = d.autorizado_por
        WHERE d.id = $1 AND d.deleted_at IS NULL
        "#,
    )
    .bind(a.id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::NotFound)?;

    let items = sqlx::query(
        r#"
        SELECT dd.producto_id, p.codigo, p.nombre,
               dd.cantidad, dd.precio_unitario, dd.subtotal
        FROM devolucion_detalle dd
        JOIN productos p ON p.id = dd.producto_id
        WHERE dd.devolucion_id = $1 AND dd.deleted_at IS NULL
        ORDER BY dd.id
        "#,
    )
    .bind(a.id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    let dec = |r: &sqlx::postgres::PgRow, name: &str| -> f64 {
        r.try_get::<rust_decimal::Decimal, _>(name).ok()
            .and_then(|d| d.to_f64()).unwrap_or(0.0)
    };

    Ok(json!({
        "devolucion": {
            "id":                    cab.get::<i64, _>("id"),
            "folio":                 cab.get::<String, _>("folio"),
            "venta_id":              cab.get::<i64, _>("venta_id"),
            "venta_folio":           cab.get::<String, _>("venta_folio"),
            "usuario_nombre":        cab.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
            "autorizado_por_nombre": cab.try_get::<Option<String>, _>("autorizado_por_nombre").ok().flatten(),
            "motivo":                cab.get::<String, _>("motivo"),
            "total_devuelto":        dec(&cab, "total_devuelto"),
            "num_items":             cab.get::<i64, _>("num_items"),
            "fecha":                 cab.get::<String, _>("fecha"),
        },
        "items": items.iter().map(|r| json!({
            "producto_id":     r.get::<i64, _>("producto_id"),
            "codigo":          r.try_get::<Option<String>, _>("codigo").ok().flatten().unwrap_or_default(),
            "nombre":          r.try_get::<Option<String>, _>("nombre").ok().flatten().unwrap_or_default(),
            "cantidad":        dec(r, "cantidad"),
            "precio_unitario": dec(r, "precio_unitario"),
            "subtotal":        dec(r, "subtotal"),
        })).collect::<Vec<_>>(),
    }))
}

async fn crear_devolucion(state: &AppState, args: Value) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;

    #[derive(Deserialize)]
    struct A {
        // El cliente desktop manda `{ datos: {...} }` (param de Tauri).
        datos: NuevaDevolucionWeb,
    }
    #[derive(Deserialize)]
    struct NuevaDevolucionWeb {
        venta_id: i64,
        usuario_id: i64,
        #[serde(default)] autorizado_por: Option<i64>,
        motivo: String,
        items: Vec<ItemDevolucionWeb>,
    }
    #[derive(Deserialize)]
    struct ItemDevolucionWeb {
        venta_detalle_id: i64,
        cantidad: f64,
    }

    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let d = a.datos;

    if d.motivo.trim().is_empty() {
        return Err(ApiError::BadRequest("El motivo es obligatorio".into()));
    }
    if d.items.is_empty() {
        return Err(ApiError::BadRequest("Debe incluir al menos un producto a devolver".into()));
    }
    for it in &d.items {
        if it.cantidad <= 0.0 {
            return Err(ApiError::BadRequest("Las cantidades deben ser mayores a 0".into()));
        }
    }

    let sucursal_id: i64 = 1;
    let mut tx = state.pool.begin().await?;

    // Verificar venta no anulada y obtener su folio
    let venta_folio: String = sqlx::query_scalar(
        "SELECT folio FROM ventas WHERE id = $1 AND anulada = 0 AND deleted_at IS NULL",
    )
    .bind(d.venta_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::BadRequest("Venta no encontrada o está anulada".into()))?;

    // Si el usuario no es admin/dueño, requiere autorizado_por.
    // Postgres no tiene tabla `roles`; usamos la convención del POS:
    // rol_id 1 = dueño, 2 = gerente (admin), 3 = vendedor.
    let rol_id: i64 = sqlx::query_scalar(
        "SELECT rol_id FROM usuarios WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(d.usuario_id)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(3);
    let es_admin = rol_es_admin(rol_id);

    if !es_admin && d.autorizado_por.is_none() {
        return Err(ApiError::BadRequest(
            "Se requiere autorización del dueño para registrar devoluciones".into(),
        ));
    }

    // Validar cantidades contra cada venta_detalle
    let mut total_devuelto: f64 = 0.0;
    // (venta_detalle_id, producto_id, cantidad, precio_unitario, subtotal)
    let mut items_validados: Vec<(i64, i64, f64, f64, f64)> = Vec::new();

    for item in &d.items {
        let row = sqlx::query(
            "SELECT venta_id, producto_id, cantidad, precio_final \
             FROM venta_detalle \
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(item.venta_detalle_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::BadRequest(
            format!("Partida de venta no encontrada: id={}", item.venta_detalle_id)
        ))?;

        let vd_venta_id: i64 = row.get("venta_id");
        let prod_id: i64 = row.get("producto_id");
        let cantidad_orig: f64 = row.try_get::<rust_decimal::Decimal, _>("cantidad")
            .ok().and_then(|d| d.to_f64()).unwrap_or(0.0);
        let precio_final: f64 = row.try_get::<rust_decimal::Decimal, _>("precio_final")
            .ok().and_then(|d| d.to_f64()).unwrap_or(0.0);

        if vd_venta_id != d.venta_id {
            return Err(ApiError::BadRequest(
                "Una de las partidas no pertenece a la venta".into(),
            ));
        }

        let ya_devuelto: f64 = sqlx::query_scalar::<_, rust_decimal::Decimal>(
            "SELECT COALESCE(SUM(cantidad), 0)::numeric \
             FROM devolucion_detalle \
             WHERE venta_detalle_id = $1 AND deleted_at IS NULL",
        )
        .bind(item.venta_detalle_id)
        .fetch_one(&mut *tx)
        .await
        .ok()
        .and_then(|d| d.to_f64())
        .unwrap_or(0.0);

        let disponible = cantidad_orig - ya_devuelto;
        if item.cantidad > disponible + 0.0001 {
            return Err(ApiError::BadRequest(format!(
                "Cantidad excede lo disponible (vendido {}, ya devuelto {}, queda {})",
                cantidad_orig, ya_devuelto, disponible
            )));
        }

        let subtotal = item.cantidad * precio_final;
        total_devuelto += subtotal;
        items_validados.push((
            item.venta_detalle_id, prod_id, item.cantidad, precio_final, subtotal,
        ));
    }

    // Generar folio D-YYYYMMDD-NNNN (mismo patrón que ventas para evitar colisión
    // con folios del desktop, que usan D-NNNNNN sin fecha).
    let hoy: String = sqlx::query_scalar(
        "SELECT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYYMMDD')",
    )
    .fetch_one(&mut *tx)
    .await?;
    let count_hoy: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM devoluciones
           WHERE folio LIKE $1 AND deleted_at IS NULL"#,
    )
    .bind(format!("D-{}-%", hoy))
    .fetch_one(&mut *tx)
    .await?;
    let folio = format!("D-{}-{:04}", hoy, count_hoy + 1);

    // Insertar devolución
    let dev_uuid = uuid::Uuid::now_v7().to_string();
    let ins_sql = format!(
        r#"
        INSERT INTO devoluciones
          (uuid, sucursal_id, folio, venta_id, usuario_id, autorizado_por,
           motivo, total_devuelto, fecha, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, {NOW_TEXT}, {NOW_TEXT})
        RETURNING id, fecha
        "#
    );
    let drow = sqlx::query(&ins_sql)
        .bind(&dev_uuid)
        .bind(sucursal_id)
        .bind(&folio)
        .bind(d.venta_id)
        .bind(d.usuario_id)
        .bind(d.autorizado_por)
        .bind(&d.motivo)
        .bind(total_devuelto)
        .fetch_one(&mut *tx)
        .await?;
    let devolucion_id: i64 = drow.get("id");
    let fecha: String = drow.get("fecha");

    // Insertar detalle + restaurar stock
    for (vd_id, prod_id, cantidad, precio, subtotal) in &items_validados {
        let det_uuid = uuid::Uuid::now_v7().to_string();
        let det_sql = format!(
            r#"
            INSERT INTO devolucion_detalle
              (uuid, devolucion_id, venta_detalle_id, producto_id,
               cantidad, precio_unitario, subtotal, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, {NOW_TEXT})
            "#
        );
        sqlx::query(&det_sql)
            .bind(&det_uuid)
            .bind(devolucion_id)
            .bind(vd_id)
            .bind(prod_id)
            .bind(cantidad)
            .bind(precio)
            .bind(subtotal)
            .execute(&mut *tx)
            .await?;

        let upd_sql = format!(
            "UPDATE productos SET stock_actual = stock_actual + $1, \
             updated_at = {NOW_TEXT} WHERE id = $2"
        );
        sqlx::query(&upd_sql)
            .bind(cantidad)
            .bind(prod_id)
            .execute(&mut *tx)
            .await?;

        // sync_cursor productos (stock cambió)
        sqlx::query(
            "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
             SELECT 'productos', p.uuid, $1, $2 FROM productos p WHERE p.id = $3",
        )
        .bind(sucursal_id)
        .bind(WEB_ORIGIN)
        .bind(*prod_id)
        .execute(&mut *tx)
        .await?;
    }

    // Movimiento de caja (RETIRO) por el monto devuelto
    let concepto = format!(
        "Devolución {} de venta {} — {}",
        folio, venta_folio, d.motivo
    );
    let mov_uuid = uuid::Uuid::now_v7().to_string();
    let mov_sql = format!(
        r#"
        INSERT INTO movimientos_caja
          (uuid, sucursal_id, tipo, usuario_id, monto, concepto,
           autorizado_por, fecha, updated_at)
        VALUES ($1, $2, 'RETIRO', $3, $4, $5, $6, {NOW_TEXT}, {NOW_TEXT})
        RETURNING id
        "#
    );
    let mrow = sqlx::query(&mov_sql)
        .bind(&mov_uuid)
        .bind(sucursal_id)
        .bind(d.usuario_id)
        .bind(total_devuelto)
        .bind(&concepto)
        .bind(d.autorizado_por)
        .fetch_one(&mut *tx)
        .await?;
    let mov_id: i64 = mrow.get("id");

    // Vincular movimiento a la devolución
    sqlx::query(
        "UPDATE devoluciones SET movimiento_caja_id = $1 WHERE id = $2",
    )
    .bind(mov_id)
    .bind(devolucion_id)
    .execute(&mut *tx)
    .await?;

    // sync_cursor para devolución y movimiento
    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('devoluciones', $1, $2, $3), \
                ('movimientos_caja', $4, $2, $3)",
    )
    .bind(&dev_uuid)
    .bind(sucursal_id)
    .bind(WEB_ORIGIN)
    .bind(&mov_uuid)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(json!({
        "id":             devolucion_id,
        "folio":          folio,
        "total_devuelto": total_devuelto,
        "fecha":          fecha,
    }))
}

// =============================================================================
// PEDIDOS A PROVEEDOR (órdenes)
// =============================================================================

async fn listar_ordenes_pedido(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize, Default)]
    struct A {
        #[serde(default, rename = "estadoFiltro")] estado_filtro: Option<String>,
    }
    let a: A = serde_json::from_value(args.clone()).unwrap_or_default();

    let rows = sqlx::query(
        r#"
        SELECT o.id, p.nombre AS proveedor_nombre,
               u.nombre_completo AS usuario_nombre,
               o.estado, o.notas, o.fecha_pedido AS fecha,
               COALESCE((SELECT COUNT(*) FROM orden_pedido_detalle d
                         WHERE d.orden_id = o.id AND d.deleted_at IS NULL), 0)
                 AS total_items
        FROM ordenes_pedido o
        LEFT JOIN proveedores p ON p.id = o.proveedor_id
        LEFT JOIN usuarios u    ON u.id = o.usuario_id
        WHERE o.deleted_at IS NULL
          AND ($1::text IS NULL OR o.estado = $1)
        ORDER BY o.fecha_pedido DESC
        LIMIT 200
        "#,
    )
    .bind(&a.estado_filtro)
    .fetch_all(&state.pool)
    .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "id":               r.get::<i64, _>("id"),
        "proveedor_nombre": r.try_get::<Option<String>, _>("proveedor_nombre").ok().flatten(),
        "usuario_nombre":   r.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
        "estado":           r.get::<String, _>("estado"),
        "notas":            r.try_get::<Option<String>, _>("notas").ok().flatten(),
        "fecha":            r.get::<String, _>("fecha"),
        "total_items":      r.get::<i64, _>("total_items"),
    })).collect::<Vec<_>>()))
}

async fn obtener_detalle_orden(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    #[derive(Deserialize)]
    struct A { #[serde(rename = "ordenId")] orden_id: i64 }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let rows = sqlx::query(
        r#"
        SELECT od.id, od.producto_id, p.nombre AS producto_nombre,
               p.codigo AS producto_codigo,
               od.cantidad_pedida, od.cantidad_recibida, od.precio_costo
        FROM orden_pedido_detalle od
        LEFT JOIN productos p ON p.id = od.producto_id
        WHERE od.orden_id = $1 AND od.deleted_at IS NULL
        ORDER BY od.id
        "#,
    )
    .bind(a.orden_id)
    .fetch_all(&state.pool)
    .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "id":                r.get::<i64, _>("id"),
        "producto_id":       r.get::<i64, _>("producto_id"),
        "producto_nombre":   r.try_get::<Option<String>, _>("producto_nombre").ok().flatten().unwrap_or_default(),
        "producto_codigo":   r.try_get::<Option<String>, _>("producto_codigo").ok().flatten().unwrap_or_default(),
        "cantidad_pedida":   r.try_get::<rust_decimal::Decimal, _>("cantidad_pedida").ok()
                                .and_then(|d| d.to_f64()).unwrap_or(0.0),
        "cantidad_recibida": r.try_get::<rust_decimal::Decimal, _>("cantidad_recibida").ok()
                                .and_then(|d| d.to_f64()).unwrap_or(0.0),
        "precio_costo":      r.try_get::<rust_decimal::Decimal, _>("precio_costo").ok()
                                .and_then(|d| d.to_f64()).unwrap_or(0.0),
    })).collect::<Vec<_>>()))
}

async fn crear_orden_pedido(state: &AppState, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { orden: DatosOrdenWeb }
    #[derive(Deserialize)]
    struct DatosOrdenWeb {
        usuario_id: i64,
        #[serde(default)] proveedor_id: Option<i64>,
        #[serde(default)] notas: Option<String>,
        items: Vec<ItemOrdenWeb>,
    }
    #[derive(Deserialize)]
    struct ItemOrdenWeb {
        producto_id: i64,
        cantidad_pedida: f64,
        precio_costo: f64,
    }

    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let o = a.orden;

    if o.items.is_empty() {
        return Err(ApiError::BadRequest("El pedido no tiene items".into()));
    }

    let sucursal_id: i64 = 1;
    let mut tx = state.pool.begin().await?;

    // Folio consecutivo
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM ordenes_pedido WHERE deleted_at IS NULL",
    )
    .fetch_one(&mut *tx)
    .await?;
    let folio = format!("P-{:06}", count + 1);

    let orden_uuid = uuid::Uuid::now_v7().to_string();
    let cab_sql = format!(
        r#"
        INSERT INTO ordenes_pedido
          (uuid, sucursal_id, folio, usuario_id, origen, proveedor_id,
           estado, notas, fecha_pedido, updated_at)
        VALUES ($1, $2, $3, $4, 'WEB', $5, 'borrador', $6, {NOW_TEXT}, {NOW_TEXT})
        RETURNING id, fecha_pedido
        "#
    );
    let crow = sqlx::query(&cab_sql)
        .bind(&orden_uuid)
        .bind(sucursal_id)
        .bind(&folio)
        .bind(o.usuario_id)
        .bind(o.proveedor_id)
        .bind(o.notas.as_deref())
        .fetch_one(&mut *tx)
        .await?;
    let orden_id: i64 = crow.get("id");
    let fecha: String = crow.get("fecha_pedido");
    let total_items = o.items.len() as i64;

    for it in &o.items {
        let det_uuid = uuid::Uuid::now_v7().to_string();
        let det_sql = format!(
            r#"
            INSERT INTO orden_pedido_detalle
              (uuid, orden_id, producto_id, cantidad_pedida,
               cantidad_recibida, precio_costo, updated_at)
            VALUES ($1, $2, $3, $4, 0, $5, {NOW_TEXT})
            "#
        );
        sqlx::query(&det_sql)
            .bind(&det_uuid)
            .bind(orden_id)
            .bind(it.producto_id)
            .bind(it.cantidad_pedida)
            .bind(it.precio_costo)
            .execute(&mut *tx)
            .await?;
    }

    // sync_cursor para la orden
    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('ordenes_pedido', $1, $2, $3)",
    )
    .bind(&orden_uuid)
    .bind(sucursal_id)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let usuario_nombre: String = sqlx::query_scalar(
        "SELECT nombre_completo FROM usuarios WHERE id = $1",
    )
    .bind(o.usuario_id)
    .fetch_optional(&state.pool)
    .await?
    .unwrap_or_else(|| "—".into());

    let proveedor_nombre: Option<String> = match o.proveedor_id {
        Some(pid) => sqlx::query_scalar("SELECT nombre FROM proveedores WHERE id = $1")
            .bind(pid)
            .fetch_optional(&state.pool)
            .await?,
        None => None,
    };

    Ok(json!({
        "id":               orden_id,
        "proveedor_nombre": proveedor_nombre,
        "usuario_nombre":   usuario_nombre,
        "estado":           "borrador",
        "notas":            o.notas,
        "fecha":            fecha,
        "total_items":      total_items,
    }))
}

async fn cambiar_estado_orden(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A {
        #[serde(rename = "ordenId")] orden_id: i64,
        #[serde(rename = "nuevoEstado")] nuevo_estado: String,
    }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let upd_sql = format!(
        "UPDATE ordenes_pedido SET estado = $1, updated_at = {NOW_TEXT} \
         WHERE id = $2 AND deleted_at IS NULL"
    );
    let affected = sqlx::query(&upd_sql)
        .bind(&a.nuevo_estado)
        .bind(a.orden_id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(ApiError::NotFound);
    }

    // Propagar al sync_cursor
    let _ = sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         SELECT 'ordenes_pedido', uuid, 1, $1 FROM ordenes_pedido WHERE id = $2",
    )
    .bind(WEB_ORIGIN)
    .bind(a.orden_id)
    .execute(&state.pool)
    .await;

    Ok(json!(true))
}

// =============================================================================
// APERTURA DE CAJA
// =============================================================================

async fn obtener_apertura_hoy(state: &AppState) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    // Filtramos por origen='web' para que la "caja web" tenga su propio ciclo
    // independiente del desktop. Si el desktop ya hizo su apertura del día
    // (origen='desktop'), eso no impide que el web abra su propia caja.
    let row = sqlx::query(
        r#"
        SELECT a.id, a.usuario_id, u.nombre_completo AS usuario_nombre,
               a.fondo_declarado, a.nota, a.fecha
        FROM aperturas_caja a
        JOIN usuarios u ON u.id = a.usuario_id
        WHERE a.deleted_at IS NULL
          AND a.origen = 'web'
          AND substr(a.fecha, 1, 10)
              = to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD')
        ORDER BY a.id DESC LIMIT 1
        "#,
    )
    .fetch_optional(&state.pool)
    .await?;

    Ok(match row {
        None => Value::Null,
        Some(r) => json!({
            "id":              r.get::<i64, _>("id"),
            "usuario_id":      r.get::<i64, _>("usuario_id"),
            "usuario_nombre":  r.get::<String, _>("usuario_nombre"),
            "fondo_declarado": r.try_get::<rust_decimal::Decimal, _>("fondo_declarado")
                                  .ok().and_then(|d| d.to_f64()).unwrap_or(0.0),
            "nota":            r.try_get::<Option<String>, _>("nota").ok().flatten(),
            "fecha":           r.get::<String, _>("fecha"),
        }),
    })
}

async fn crear_apertura_caja(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A {
        datos: NuevaAperturaWeb,
    }
    #[derive(Deserialize)]
    struct NuevaAperturaWeb {
        usuario_id: i64,
        fondo_declarado: f64,
        #[serde(default)] nota: Option<String>,
    }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let d = a.datos;

    if d.fondo_declarado < 0.0 {
        return Err(ApiError::BadRequest("El fondo no puede ser negativo".into()));
    }

    // Solo bloqueamos si YA hay una apertura de la caja web hoy. La apertura
    // del desktop (origen='desktop') es de otra caja y no debe bloquear ésta.
    let existe: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM aperturas_caja \
         WHERE deleted_at IS NULL AND origen = 'web' \
           AND substr(fecha, 1, 10) \
               = to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD')",
    )
    .fetch_one(&state.pool)
    .await?;
    if existe > 0 {
        return Err(ApiError::BadRequest("Ya existe una apertura de caja web para hoy".into()));
    }

    let sucursal_id: i64 = 1;
    let new_uuid = uuid::Uuid::now_v7().to_string();
    let sql = format!(
        r#"
        INSERT INTO aperturas_caja
          (uuid, sucursal_id, usuario_id, fondo_declarado, nota, origen, fecha, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, {NOW_TEXT}, {NOW_TEXT})
        RETURNING id, fecha
        "#
    );
    let row = sqlx::query(&sql)
        .bind(&new_uuid)
        .bind(sucursal_id)
        .bind(d.usuario_id)
        .bind(d.fondo_declarado)
        .bind(d.nota.as_deref())
        .bind(ORIGEN_WEB)
        .fetch_one(&state.pool)
        .await?;

    // sync_cursor para que el desktop la jale
    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('aperturas_caja', $1, $2, $3)",
    )
    .bind(&new_uuid)
    .bind(sucursal_id)
    .bind(WEB_ORIGIN)
    .execute(&state.pool)
    .await?;

    let id: i64       = row.get("id");
    let fecha: String = row.get("fecha");

    // Nombre del usuario (para devolver shape completa)
    let nombre: String = sqlx::query_scalar(
        "SELECT nombre_completo FROM usuarios WHERE id = $1",
    )
    .bind(d.usuario_id)
    .fetch_optional(&state.pool)
    .await?
    .unwrap_or_default();

    Ok(json!({
        "id":              id,
        "usuario_id":      d.usuario_id,
        "usuario_nombre":  nombre,
        "fondo_declarado": d.fondo_declarado,
        "nota":            d.nota,
        "fecha":           fecha,
    }))
}

// =============================================================================
// CORTES DE CAJA — módulo completo (web only, origen='web')
// =============================================================================
//
// Filtros: TODOS los queries de cortes/movimientos en este módulo agregan
//   `WHERE origen = 'web'`
// para mantener la caja web independiente del flujo desktop. El desktop ya
// hizo sus cortes en SQLite y los pushea a postgres con origen='desktop' —
// no los queremos contar ni mezclar aquí.

/// Helper local: f64 desde columna NUMERIC.
fn pg_dec(row: &sqlx::postgres::PgRow, name: &str) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    row.try_get::<rust_decimal::Decimal, _>(name).ok()
        .and_then(|d| d.to_f64()).unwrap_or(0.0)
}

const RETIRO_LIMITE_SIN_PIN: f64 = 500.0;

async fn crear_movimiento_caja(state: &AppState, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { datos: NuevoMov }
    #[derive(Deserialize)]
    struct NuevoMov {
        tipo: String,
        usuario_id: i64,
        monto: f64,
        concepto: String,
        #[serde(default)] autorizado_por: Option<i64>,
        #[serde(default)] pin_autorizacion: Option<String>,
    }

    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let d = a.datos;

    if d.monto <= 0.0 {
        return Err(ApiError::BadRequest("El monto debe ser mayor a cero".into()));
    }
    if d.concepto.trim().is_empty() {
        return Err(ApiError::BadRequest("El concepto es obligatorio".into()));
    }
    if d.tipo != "ENTRADA" && d.tipo != "RETIRO" {
        return Err(ApiError::BadRequest("Tipo inválido (debe ser ENTRADA o RETIRO)".into()));
    }

    let sucursal_id: i64 = 1;

    // Si es RETIRO grande y el solicitante NO es admin, exigir PIN del dueño
    let rol_id: i64 = sqlx::query_scalar(
        "SELECT rol_id FROM usuarios WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(d.usuario_id)
    .fetch_optional(&state.pool)
    .await?
    .unwrap_or(3);
    let solicitante_es_admin = rol_es_admin(rol_id);

    let autorizado_por_validado: Option<i64> =
        if d.tipo == "RETIRO" && d.monto > RETIRO_LIMITE_SIN_PIN && !solicitante_es_admin {
            let pin = d.pin_autorizacion.as_deref().unwrap_or("").trim();
            if pin.is_empty() {
                return Err(ApiError::BadRequest(format!(
                    "Retiros mayores a ${:.0} requieren PIN del dueño",
                    RETIRO_LIMITE_SIN_PIN
                )));
            }
            match buscar_dueno_por_pin(state, pin).await? {
                Some(id) => Some(id),
                None => return Err(ApiError::BadRequest("PIN del dueño incorrecto".into())),
            }
        } else if d.tipo == "RETIRO" && d.monto > RETIRO_LIMITE_SIN_PIN && solicitante_es_admin {
            Some(d.usuario_id)
        } else {
            d.autorizado_por
        };

    let new_uuid = uuid::Uuid::now_v7().to_string();
    let sql = format!(
        r#"
        INSERT INTO movimientos_caja
          (uuid, sucursal_id, tipo, usuario_id, monto, concepto,
           autorizado_por, origen, fecha, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, 'web', {NOW_TEXT}, {NOW_TEXT})
        RETURNING id, fecha
        "#
    );
    let row = sqlx::query(&sql)
        .bind(&new_uuid)
        .bind(sucursal_id)
        .bind(&d.tipo)
        .bind(d.usuario_id)
        .bind(d.monto)
        .bind(&d.concepto)
        .bind(autorizado_por_validado)
        .fetch_one(&state.pool)
        .await?;

    let id: i64 = row.get("id");
    let fecha: String = row.get("fecha");

    let usuario_nombre: String = sqlx::query_scalar(
        "SELECT nombre_completo FROM usuarios WHERE id = $1",
    )
    .bind(d.usuario_id)
    .fetch_optional(&state.pool)
    .await?
    .unwrap_or_else(|| "Desconocido".into());

    // sync_cursor para que el desktop lo jale
    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('movimientos_caja', $1, $2, $3)",
    )
    .bind(&new_uuid)
    .bind(sucursal_id)
    .bind(WEB_ORIGIN)
    .execute(&state.pool)
    .await?;

    Ok(json!({
        "id":              id,
        "tipo":            d.tipo,
        "usuario_id":      d.usuario_id,
        "usuario_nombre":  usuario_nombre,
        "monto":           d.monto,
        "concepto":        d.concepto,
        "autorizado_por":  autorizado_por_validado,
        "corte_id":        Value::Null,
        "fecha":           fecha,
    }))
}

async fn listar_movimientos_sin_corte(state: &AppState) -> Result<Value, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT m.id, m.tipo, m.usuario_id, u.nombre_completo AS usuario_nombre,
               m.monto, m.concepto, m.autorizado_por, m.corte_id, m.fecha
        FROM movimientos_caja m
        JOIN usuarios u ON u.id = m.usuario_id
        WHERE m.corte_id IS NULL
          AND m.deleted_at IS NULL
          AND m.origen = 'web'
        ORDER BY m.fecha DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "id":             r.get::<i64, _>("id"),
        "tipo":           r.get::<String, _>("tipo"),
        "usuario_id":     r.get::<i64, _>("usuario_id"),
        "usuario_nombre": r.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
        "monto":          pg_dec(r, "monto"),
        "concepto":       r.get::<String, _>("concepto"),
        "autorizado_por": r.try_get::<Option<i64>, _>("autorizado_por").ok().flatten(),
        "corte_id":       r.try_get::<Option<i64>, _>("corte_id").ok().flatten(),
        "fecha":          r.get::<String, _>("fecha"),
    })).collect::<Vec<_>>()))
}

async fn calcular_datos_corte(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A {
        // Frontend manda camelCase via invokeCompat (sin conversion en web).
        #[serde(rename = "fechaInicio")] fecha_inicio: String,
        #[serde(rename = "fechaFin")] fecha_fin: String,
    }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    // Importante: las VENTAS no tienen columna `origen` (no quisimos migrar la
    // tabla más grande). Las ventas hechas desde web se insertan en la misma
    // tabla que las del desktop. Para el corte web, sumamos TODAS las ventas
    // del rango, asumiendo que el web lleva su propio rango de fechas que no
    // se traslapa con el corte parcial del desktop.
    //
    // En la práctica: el primer corte del día web considera ventas desde la
    // apertura de la caja web (o, si no hay apertura web, desde el último
    // corte web). El frontend ya pasa fecha_inicio = última apertura/corte.

    let efectivo: f64 = sqlx::query_scalar::<_, rust_decimal::Decimal>(
        "SELECT COALESCE(SUM(total), 0)::numeric FROM ventas \
         WHERE fecha BETWEEN $1 AND $2 AND anulada = 0 \
           AND metodo_pago = 'efectivo' AND deleted_at IS NULL",
    )
    .bind(&a.fecha_inicio).bind(&a.fecha_fin)
    .fetch_one(&state.pool).await
    .ok().and_then(|d| { use rust_decimal::prelude::ToPrimitive; d.to_f64() })
    .unwrap_or(0.0);

    let tarjeta: f64 = sqlx::query_scalar::<_, rust_decimal::Decimal>(
        "SELECT COALESCE(SUM(total), 0)::numeric FROM ventas \
         WHERE fecha BETWEEN $1 AND $2 AND anulada = 0 \
           AND metodo_pago = 'tarjeta' AND deleted_at IS NULL",
    )
    .bind(&a.fecha_inicio).bind(&a.fecha_fin)
    .fetch_one(&state.pool).await
    .ok().and_then(|d| { use rust_decimal::prelude::ToPrimitive; d.to_f64() })
    .unwrap_or(0.0);

    let transferencia: f64 = sqlx::query_scalar::<_, rust_decimal::Decimal>(
        "SELECT COALESCE(SUM(total), 0)::numeric FROM ventas \
         WHERE fecha BETWEEN $1 AND $2 AND anulada = 0 \
           AND metodo_pago = 'transferencia' AND deleted_at IS NULL",
    )
    .bind(&a.fecha_inicio).bind(&a.fecha_fin)
    .fetch_one(&state.pool).await
    .ok().and_then(|d| { use rust_decimal::prelude::ToPrimitive; d.to_f64() })
    .unwrap_or(0.0);

    let agg = sqlx::query(
        "SELECT COUNT(*)::bigint AS n, \
                COALESCE(SUM(total), 0)::numeric AS total, \
                COALESCE(SUM(descuento), 0)::numeric AS desc \
         FROM ventas \
         WHERE fecha BETWEEN $1 AND $2 AND anulada = 0 AND deleted_at IS NULL",
    )
    .bind(&a.fecha_inicio).bind(&a.fecha_fin)
    .fetch_one(&state.pool).await?;
    let num_transacciones: i64 = agg.try_get("n").unwrap_or(0);
    let total_ventas: f64 = pg_dec(&agg, "total");
    let total_descuentos: f64 = pg_dec(&agg, "desc");

    let total_anulaciones: f64 = sqlx::query_scalar::<_, rust_decimal::Decimal>(
        "SELECT COALESCE(SUM(total), 0)::numeric FROM ventas \
         WHERE fecha BETWEEN $1 AND $2 AND anulada = 1 AND deleted_at IS NULL",
    )
    .bind(&a.fecha_inicio).bind(&a.fecha_fin)
    .fetch_one(&state.pool).await
    .ok().and_then(|d| { use rust_decimal::prelude::ToPrimitive; d.to_f64() })
    .unwrap_or(0.0);

    // Movimientos sin corte (origen=web)
    let mov_rows = sqlx::query(
        r#"
        SELECT m.id, m.tipo, m.usuario_id, u.nombre_completo AS usuario_nombre,
               m.monto, m.concepto, m.autorizado_por, m.corte_id, m.fecha
        FROM movimientos_caja m
        JOIN usuarios u ON u.id = m.usuario_id
        WHERE m.corte_id IS NULL
          AND m.deleted_at IS NULL
          AND m.origen = 'web'
        ORDER BY m.fecha ASC
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let movimientos: Vec<Value> = mov_rows.iter().map(|r| json!({
        "id":             r.get::<i64, _>("id"),
        "tipo":           r.get::<String, _>("tipo"),
        "usuario_id":     r.get::<i64, _>("usuario_id"),
        "usuario_nombre": r.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
        "monto":          pg_dec(r, "monto"),
        "concepto":       r.get::<String, _>("concepto"),
        "autorizado_por": r.try_get::<Option<i64>, _>("autorizado_por").ok().flatten(),
        "corte_id":       r.try_get::<Option<i64>, _>("corte_id").ok().flatten(),
        "fecha":          r.get::<String, _>("fecha"),
    })).collect();

    let total_entradas: f64 = mov_rows.iter()
        .filter(|r| r.get::<String, _>("tipo") == "ENTRADA")
        .map(|r| pg_dec(r, "monto")).sum();
    let total_retiros: f64 = mov_rows.iter()
        .filter(|r| r.get::<String, _>("tipo") == "RETIRO")
        .map(|r| pg_dec(r, "monto")).sum();

    // Fondo inicial: apertura web del día > último fondo_siguiente del último
    // corte web > 0.
    let dia: &str = if a.fecha_inicio.len() >= 10 { &a.fecha_inicio[..10] } else { "" };
    let fondo_apertura: Option<f64> = sqlx::query_scalar::<_, rust_decimal::Decimal>(
        "SELECT fondo_declarado::numeric FROM aperturas_caja \
         WHERE substr(fecha, 1, 10) = $1 \
           AND origen = 'web' AND deleted_at IS NULL \
         LIMIT 1",
    )
    .bind(dia)
    .fetch_optional(&state.pool).await?
    .and_then(|d| { use rust_decimal::prelude::ToPrimitive; d.to_f64() });

    let fondo_inicial: f64 = match fondo_apertura {
        Some(f) => f,
        None => sqlx::query_scalar::<_, rust_decimal::Decimal>(
            "SELECT fondo_siguiente::numeric FROM cortes \
             WHERE origen = 'web' AND deleted_at IS NULL \
             ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(&state.pool).await?
        .and_then(|d| { use rust_decimal::prelude::ToPrimitive; d.to_f64() })
        .unwrap_or(0.0),
    };

    let efectivo_esperado = fondo_inicial + efectivo + total_entradas - total_retiros;

    // Vendedores
    let vrows = sqlx::query(
        r#"
        SELECT v.usuario_id, u.nombre_completo AS usuario_nombre,
               COUNT(*)::bigint AS num_ventas,
               COALESCE(SUM(v.total), 0)::numeric AS total,
               MIN(v.fecha) AS hora_inicio,
               MAX(v.fecha) AS hora_fin
        FROM ventas v
        JOIN usuarios u ON u.id = v.usuario_id
        WHERE v.fecha BETWEEN $1 AND $2 AND v.anulada = 0 AND v.deleted_at IS NULL
        GROUP BY v.usuario_id, u.nombre_completo
        ORDER BY total DESC
        "#,
    )
    .bind(&a.fecha_inicio).bind(&a.fecha_fin)
    .fetch_all(&state.pool)
    .await?;

    let vendedores: Vec<Value> = vrows.iter().map(|r| json!({
        "usuario_id":     r.get::<i64, _>("usuario_id"),
        "usuario_nombre": r.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
        "num_ventas":     r.try_get::<i64, _>("num_ventas").unwrap_or(0),
        "total_vendido":  pg_dec(r, "total"),
        "hora_inicio":    r.try_get::<Option<String>, _>("hora_inicio").ok().flatten().unwrap_or_default(),
        "hora_fin":       r.try_get::<Option<String>, _>("hora_fin").ok().flatten().unwrap_or_default(),
    })).collect();

    Ok(json!({
        "fecha_inicio":               a.fecha_inicio,
        "fecha_fin":                  a.fecha_fin,
        "fondo_inicial":              fondo_inicial,
        "total_ventas_efectivo":      efectivo,
        "total_ventas_tarjeta":       tarjeta,
        "total_ventas_transferencia": transferencia,
        "total_ventas":               total_ventas,
        "num_transacciones":          num_transacciones,
        "total_descuentos":           total_descuentos,
        "total_anulaciones":          total_anulaciones,
        "total_entradas_efectivo":    total_entradas,
        "total_retiros_efectivo":     total_retiros,
        "efectivo_esperado":          efectivo_esperado,
        "movimientos":                movimientos,
        "vendedores":                 vendedores,
    }))
}

async fn crear_corte(state: &AppState, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { datos: NuevoCorteWeb }
    #[derive(Deserialize)]
    struct NuevoCorteWeb {
        tipo: String,
        usuario_id: i64,
        fecha_inicio: String,
        fecha_fin: String,
        datos: DatosCorteWeb,
        efectivo_contado: f64,
        #[serde(default)] nota_diferencia: Option<String>,
        fondo_siguiente: f64,
        #[serde(default)] denominaciones: Option<Vec<DenomInput>>,
    }
    #[derive(Deserialize)]
    struct DatosCorteWeb {
        fondo_inicial: f64,
        total_ventas_efectivo: f64,
        total_ventas_tarjeta: f64,
        total_ventas_transferencia: f64,
        total_ventas: f64,
        num_transacciones: i64,
        total_descuentos: f64,
        total_anulaciones: f64,
        total_entradas_efectivo: f64,
        total_retiros_efectivo: f64,
        efectivo_esperado: f64,
        #[serde(default)] vendedores: Vec<VendedorInput>,
    }
    #[derive(Deserialize)]
    struct VendedorInput {
        usuario_id: i64,
        num_ventas: i64,
        total_vendido: f64,
        hora_inicio: String,
        hora_fin: String,
    }
    #[derive(Deserialize)]
    struct DenomInput {
        denominacion: f64,
        tipo: String,
        cantidad: i64,
    }

    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let c = a.datos;

    if c.tipo != "PARCIAL" && c.tipo != "DIA" {
        return Err(ApiError::BadRequest("Tipo inválido (PARCIAL o DIA)".into()));
    }

    let sucursal_id: i64 = 1;
    let mut tx = state.pool.begin().await?;

    // Solo un corte DIA web por día
    if c.tipo == "DIA" {
        let dia = if c.fecha_inicio.len() >= 10 { &c.fecha_inicio[..10] } else { "" };
        let existe: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM cortes \
             WHERE tipo = 'DIA' AND origen = 'web' AND deleted_at IS NULL \
               AND substr(created_at, 1, 10) = $1",
        )
        .bind(dia)
        .fetch_one(&mut *tx).await?;
        if existe > 0 {
            return Err(ApiError::BadRequest(
                "Ya existe un corte del día (web) para esta fecha".into(),
            ));
        }
    }

    let diferencia = c.efectivo_contado - c.datos.efectivo_esperado;
    let new_uuid = uuid::Uuid::now_v7().to_string();

    let ins_sql = format!(
        r#"
        INSERT INTO cortes
          (uuid, sucursal_id, tipo, usuario_id, fecha_inicio, fecha_fin,
           fondo_inicial, total_ventas_efectivo, total_ventas_tarjeta,
           total_ventas_transferencia, total_ventas, num_transacciones,
           total_descuentos, total_anulaciones, total_entradas_efectivo,
           total_retiros_efectivo, efectivo_esperado, efectivo_contado,
           diferencia, nota_diferencia, fondo_siguiente, origen,
           created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10, $11, $12,
                $13, $14, $15, $16, $17, $18,
                $19, $20, $21, 'web',
                {NOW_TEXT}, {NOW_TEXT})
        RETURNING id, created_at
        "#
    );
    let row = sqlx::query(&ins_sql)
        .bind(&new_uuid)
        .bind(sucursal_id)
        .bind(&c.tipo)
        .bind(c.usuario_id)
        .bind(&c.fecha_inicio)
        .bind(&c.fecha_fin)
        .bind(c.datos.fondo_inicial)
        .bind(c.datos.total_ventas_efectivo)
        .bind(c.datos.total_ventas_tarjeta)
        .bind(c.datos.total_ventas_transferencia)
        .bind(c.datos.total_ventas)
        .bind(c.datos.num_transacciones)
        .bind(c.datos.total_descuentos)
        .bind(c.datos.total_anulaciones)
        .bind(c.datos.total_entradas_efectivo)
        .bind(c.datos.total_retiros_efectivo)
        .bind(c.datos.efectivo_esperado)
        .bind(c.efectivo_contado)
        .bind(diferencia)
        .bind(c.nota_diferencia.as_deref())
        .bind(c.fondo_siguiente)
        .fetch_one(&mut *tx)
        .await?;
    let corte_id: i64 = row.get("id");
    let created_at: String = row.get("created_at");

    // Asociar movimientos pendientes web a este corte
    sqlx::query(
        "UPDATE movimientos_caja \
         SET corte_id = $1, updated_at = $2 \
         WHERE corte_id IS NULL AND origen = 'web' AND deleted_at IS NULL",
    )
    .bind(corte_id)
    .bind(&created_at)
    .execute(&mut *tx)
    .await?;

    // Denominaciones
    if let Some(denoms) = &c.denominaciones {
        for d in denoms {
            if d.cantidad > 0 {
                let subtotal = d.denominacion * d.cantidad as f64;
                let den_uuid = uuid::Uuid::now_v7().to_string();
                let dsql = format!(
                    r#"
                    INSERT INTO corte_denominaciones
                      (uuid, corte_id, denominacion, tipo, cantidad, subtotal, updated_at)
                    VALUES ($1, $2, $3, $4, $5, $6, {NOW_TEXT})
                    "#
                );
                sqlx::query(&dsql)
                    .bind(&den_uuid)
                    .bind(corte_id)
                    .bind(d.denominacion)
                    .bind(&d.tipo)
                    .bind(d.cantidad)
                    .bind(subtotal)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }

    // Vendedores (solo en corte DIA)
    if c.tipo == "DIA" {
        for v in &c.datos.vendedores {
            let vu_uuid = uuid::Uuid::now_v7().to_string();
            let vsql = format!(
                r#"
                INSERT INTO corte_vendedores
                  (uuid, corte_id, usuario_id, num_ventas, total_vendido,
                   hora_inicio, hora_fin, updated_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, {NOW_TEXT})
                "#
            );
            sqlx::query(&vsql)
                .bind(&vu_uuid)
                .bind(corte_id)
                .bind(v.usuario_id)
                .bind(v.num_ventas)
                .bind(v.total_vendido)
                .bind(&v.hora_inicio)
                .bind(&v.hora_fin)
                .execute(&mut *tx)
                .await?;
        }
    }

    // sync_cursor para que el desktop jale el corte
    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('cortes', $1, $2, $3)",
    )
    .bind(&new_uuid)
    .bind(sucursal_id)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(json!({
        "id":                corte_id,
        "tipo":              c.tipo,
        "diferencia":        diferencia,
        "efectivo_esperado": c.datos.efectivo_esperado,
        "efectivo_contado":  c.efectivo_contado,
        "fondo_siguiente":   c.fondo_siguiente,
        "created_at":        created_at,
    }))
}

async fn listar_cortes(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize, Default)]
    struct A { #[serde(default)] limite: Option<i64> }
    let a: A = serde_json::from_value(args.clone()).unwrap_or_default();
    let limite = a.limite.unwrap_or(50);

    let rows = sqlx::query(
        r#"
        SELECT c.id, c.tipo, u.nombre_completo AS usuario_nombre, c.created_at,
               c.efectivo_esperado, c.efectivo_contado, c.diferencia, c.fondo_siguiente
        FROM cortes c
        JOIN usuarios u ON u.id = c.usuario_id
        WHERE c.origen = 'web' AND c.deleted_at IS NULL
        ORDER BY c.created_at DESC
        LIMIT $1
        "#,
    )
    .bind(limite)
    .fetch_all(&state.pool)
    .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "id":                r.get::<i64, _>("id"),
        "tipo":              r.get::<String, _>("tipo"),
        "usuario_nombre":    r.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
        "created_at":        r.get::<String, _>("created_at"),
        "efectivo_esperado": pg_dec(r, "efectivo_esperado"),
        "efectivo_contado":  pg_dec(r, "efectivo_contado"),
        "diferencia":        pg_dec(r, "diferencia"),
        "fondo_siguiente":   pg_dec(r, "fondo_siguiente"),
    })).collect::<Vec<_>>()))
}

async fn obtener_detalle_corte(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { id: i64 }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let cab = sqlx::query(
        r#"
        SELECT c.id, c.tipo, u.nombre_completo AS usuario_nombre, c.created_at,
               c.efectivo_esperado, c.efectivo_contado, c.diferencia, c.fondo_siguiente
        FROM cortes c
        JOIN usuarios u ON u.id = c.usuario_id
        WHERE c.id = $1 AND c.deleted_at IS NULL
        "#,
    )
    .bind(a.id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::NotFound)?;

    let denoms = sqlx::query(
        "SELECT denominacion, tipo, cantidad, subtotal \
         FROM corte_denominaciones \
         WHERE corte_id = $1 AND deleted_at IS NULL \
         ORDER BY denominacion DESC",
    )
    .bind(a.id)
    .fetch_all(&state.pool).await.unwrap_or_default();

    let movs = sqlx::query(
        r#"
        SELECT m.id, m.tipo, m.usuario_id, u.nombre_completo AS usuario_nombre,
               m.monto, m.concepto, m.autorizado_por, m.corte_id, m.fecha
        FROM movimientos_caja m
        JOIN usuarios u ON u.id = m.usuario_id
        WHERE m.corte_id = $1 AND m.deleted_at IS NULL
        ORDER BY m.fecha ASC
        "#,
    )
    .bind(a.id)
    .fetch_all(&state.pool).await.unwrap_or_default();

    let vends = sqlx::query(
        r#"
        SELECT cv.usuario_id, u.nombre_completo AS usuario_nombre,
               cv.num_ventas, cv.total_vendido, cv.hora_inicio, cv.hora_fin
        FROM corte_vendedores cv
        JOIN usuarios u ON u.id = cv.usuario_id
        WHERE cv.corte_id = $1 AND cv.deleted_at IS NULL
        ORDER BY cv.total_vendido DESC
        "#,
    )
    .bind(a.id)
    .fetch_all(&state.pool).await.unwrap_or_default();

    Ok(json!({
        "corte": {
            "id":                cab.get::<i64, _>("id"),
            "tipo":              cab.get::<String, _>("tipo"),
            "usuario_nombre":    cab.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
            "created_at":        cab.get::<String, _>("created_at"),
            "efectivo_esperado": pg_dec(&cab, "efectivo_esperado"),
            "efectivo_contado":  pg_dec(&cab, "efectivo_contado"),
            "diferencia":        pg_dec(&cab, "diferencia"),
            "fondo_siguiente":   pg_dec(&cab, "fondo_siguiente"),
        },
        "denominaciones": denoms.iter().map(|r| json!({
            "denominacion": pg_dec(r, "denominacion"),
            "tipo":         r.get::<String, _>("tipo"),
            "cantidad":     r.try_get::<i64, _>("cantidad").unwrap_or(0),
            "subtotal":     pg_dec(r, "subtotal"),
        })).collect::<Vec<_>>(),
        "movimientos": movs.iter().map(|r| json!({
            "id":             r.get::<i64, _>("id"),
            "tipo":           r.get::<String, _>("tipo"),
            "usuario_id":     r.get::<i64, _>("usuario_id"),
            "usuario_nombre": r.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
            "monto":          pg_dec(r, "monto"),
            "concepto":       r.get::<String, _>("concepto"),
            "autorizado_por": r.try_get::<Option<i64>, _>("autorizado_por").ok().flatten(),
            "corte_id":       r.try_get::<Option<i64>, _>("corte_id").ok().flatten(),
            "fecha":          r.get::<String, _>("fecha"),
        })).collect::<Vec<_>>(),
        "vendedores": vends.iter().map(|r| json!({
            "usuario_id":     r.get::<i64, _>("usuario_id"),
            "usuario_nombre": r.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
            "num_ventas":     r.try_get::<i64, _>("num_ventas").unwrap_or(0),
            "total_vendido":  pg_dec(r, "total_vendido"),
            "hora_inicio":    r.try_get::<Option<String>, _>("hora_inicio").ok().flatten().unwrap_or_default(),
            "hora_fin":       r.try_get::<Option<String>, _>("hora_fin").ok().flatten().unwrap_or_default(),
        })).collect::<Vec<_>>(),
    }))
}

async fn obtener_fondo_sugerido(state: &AppState) -> Result<Value, ApiError> {
    let fondo: Option<f64> = sqlx::query_scalar::<_, rust_decimal::Decimal>(
        "SELECT fondo_siguiente::numeric FROM cortes \
         WHERE tipo = 'DIA' AND origen = 'web' AND deleted_at IS NULL \
         ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(&state.pool).await?
    .and_then(|d| { use rust_decimal::prelude::ToPrimitive; d.to_f64() });

    Ok(json!(fondo.unwrap_or(2000.0)))
}

async fn verificar_corte_dia_pendiente(state: &AppState) -> Result<Value, ApiError> {
    // Día más reciente con ventas (cualquier origen) anterior a HOY que NO
    // tenga corte DIA web. Igual que el desktop pero filtrando origen='web'
    // en cortes — porque desde la web sólo nos interesa el corte web.
    let pendiente: Option<String> = sqlx::query_scalar(
        r#"
        SELECT substr(v.fecha, 1, 10) AS dia
        FROM ventas v
        WHERE substr(v.fecha, 1, 10)
              < to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD')
          AND v.deleted_at IS NULL
          AND NOT EXISTS (
              SELECT 1 FROM cortes c
              WHERE c.tipo = 'DIA'
                AND c.origen = 'web'
                AND c.deleted_at IS NULL
                AND substr(c.fecha_fin, 1, 10) = substr(v.fecha, 1, 10)
          )
        GROUP BY dia
        ORDER BY dia DESC
        LIMIT 1
        "#,
    )
    .fetch_optional(&state.pool)
    .await?;

    Ok(match pendiente {
        Some(s) => json!(s),
        None => Value::Null,
    })
}

/// Busca un usuario admin/dueño por PIN comparando contra el bcrypt hash.
async fn buscar_dueno_por_pin(state: &AppState, pin: &str) -> Result<Option<i64>, ApiError> {
    let candidatos = sqlx::query(
        "SELECT id, pin FROM usuarios \
         WHERE rol_id IN (1,2) AND activo = 1 AND deleted_at IS NULL",
    )
    .fetch_all(&state.pool)
    .await?;
    for r in &candidatos {
        let hash: String = r.get("pin");
        if bcrypt::verify(pin, &hash).unwrap_or(false) {
            return Ok(Some(r.get::<i64, _>("id")));
        }
    }
    Ok(None)
}

// =============================================================================
// CLIENTES — CRUD (paridad con desktop `commands/productos.rs:647-690`)
// =============================================================================
//
// Forma del frontend (`pages/Clientes.tsx`):
//   - crear_cliente:           { nombre, telefono, email, descuentoPorcentaje, notas }
//   - actualizar_cliente:      { datos: { id, nombre, telefono, email,
//                                         descuento_porcentaje, notas } }
//   - toggle_cliente_activo:   { id }
//
// El frontend espera de vuelta la shape `ClienteInfo` que ya devuelve
// `listar_clientes` (mismo payload por consistencia, aunque la página solo
// recarga toda la lista después de un mutate).

async fn crear_cliente(state: &AppState, args: Value) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct A {
        nombre: String,
        #[serde(default)] telefono: Option<String>,
        #[serde(default)] email: Option<String>,
        #[serde(default)] descuento_porcentaje: f64,
        #[serde(default)] notas: Option<String>,
        // Si el frontend lo pasa, lo usamos para auditar; si no, queda null.
        #[serde(default)] usuario_id: Option<i64>,
    }

    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    if a.nombre.trim().is_empty() {
        return Err(ApiError::BadRequest("El nombre es obligatorio".into()));
    }

    let uuid = uuid::Uuid::now_v7().to_string();
    let mut tx = state.pool.begin().await?;

    let ins_sql = format!(
        r#"INSERT INTO clientes (uuid, nombre, telefono, email,
               descuento_porcentaje, notas, activo, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, 1, {NOW_TEXT}, {NOW_TEXT})
           RETURNING id, nombre, telefono, email, descuento_porcentaje,
                     notas, activo"#
    );
    let row = sqlx::query(&ins_sql)
        .bind(&uuid)
        .bind(&a.nombre)
        .bind(a.telefono.as_deref())
        .bind(a.email.as_deref())
        .bind(a.descuento_porcentaje)
        .bind(a.notas.as_deref())
        .fetch_one(&mut *tx)
        .await?;
    let id: i64 = row.get("id");

    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('clientes', $1, 1, $2)",
    )
    .bind(&uuid)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            descripcion_legible, origen, fecha)
           VALUES ($1, 'CLIENTE_CREADO', 'clientes', $2, $3, 'WEB', {NOW_TEXT})"#
    );
    let _ = sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(id)
        .bind(format!("Cliente creado: {}", a.nombre))
        .execute(&mut *tx)
        .await;

    tx.commit().await?;

    Ok(json!({
        "id":       id,
        "nombre":   row.get::<String, _>("nombre"),
        "telefono": row.try_get::<Option<String>, _>("telefono").ok().flatten(),
        "email":    row.try_get::<Option<String>, _>("email").ok().flatten(),
        "descuento_porcentaje": row.try_get::<rust_decimal::Decimal, _>("descuento_porcentaje")
            .ok().and_then(|d| d.to_f64()).unwrap_or(0.0),
        "notas":    row.try_get::<Option<String>, _>("notas").ok().flatten(),
        "activo":   row.get::<i32, _>("activo") != 0,
    }))
}

async fn actualizar_cliente(state: &AppState, args: Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { datos: ClienteUpd, #[serde(default)] usuario_id: Option<i64> }
    #[derive(Deserialize)]
    struct ClienteUpd {
        id: i64,
        nombre: String,
        #[serde(default)] telefono: Option<String>,
        #[serde(default)] email: Option<String>,
        #[serde(default)] descuento_porcentaje: f64,
        #[serde(default)] notas: Option<String>,
    }

    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let c = a.datos;

    let mut tx = state.pool.begin().await?;

    let uuid: String = sqlx::query_scalar(
        "SELECT uuid FROM clientes WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(c.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;

    let upd_sql = format!(
        r#"UPDATE clientes SET
               nombre = $1, telefono = $2, email = $3,
               descuento_porcentaje = $4, notas = $5, updated_at = {NOW_TEXT}
           WHERE id = $6 AND deleted_at IS NULL"#
    );
    sqlx::query(&upd_sql)
        .bind(&c.nombre)
        .bind(c.telefono.as_deref())
        .bind(c.email.as_deref())
        .bind(c.descuento_porcentaje)
        .bind(c.notas.as_deref())
        .bind(c.id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('clientes', $1, 1, $2)",
    )
    .bind(&uuid)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            descripcion_legible, origen, fecha)
           VALUES ($1, 'CLIENTE_EDITADO', 'clientes', $2, $3, 'WEB', {NOW_TEXT})"#
    );
    let _ = sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(c.id)
        .bind(format!("Cliente editado: {}", c.nombre))
        .execute(&mut *tx)
        .await;

    tx.commit().await?;
    Ok(json!(true))
}

async fn toggle_cliente_activo(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { id: i64, #[serde(default)] usuario_id: Option<i64> }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let mut tx = state.pool.begin().await?;

    // Capturar uuid + nombre + estado actual antes del flip (para auditar y sync)
    let row = sqlx::query(
        "SELECT uuid, nombre, activo FROM clientes WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(a.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    let uuid: String = row.get("uuid");
    let nombre: String = row.get("nombre");
    let nuevo_estado: i32 = if row.get::<i32, _>("activo") == 0 { 1 } else { 0 };

    let upd_sql = format!(
        "UPDATE clientes SET activo = $1, updated_at = {NOW_TEXT} WHERE id = $2"
    );
    sqlx::query(&upd_sql)
        .bind(nuevo_estado)
        .bind(a.id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('clientes', $1, 1, $2)",
    )
    .bind(&uuid)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    let accion = if nuevo_estado == 1 { "CLIENTE_REACTIVADO" } else { "CLIENTE_DESACTIVADO" };
    let descr = if nuevo_estado == 1 {
        format!("Cliente reactivado: {}", nombre)
    } else {
        format!("Cliente desactivado: {}", nombre)
    };
    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            descripcion_legible, origen, fecha)
           VALUES ($1, $2, 'clientes', $3, $4, 'WEB', {NOW_TEXT})"#
    );
    let _ = sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(accion)
        .bind(a.id)
        .bind(descr)
        .execute(&mut *tx)
        .await;

    tx.commit().await?;
    Ok(json!(true))
}

// =============================================================================
// VENTAS — listar del día y anular (paridad con `commands/ventas.rs:224, 319`)
// =============================================================================

/// Ventas del día actual en zona horaria de México.
/// El web NO filtra por `origen` (no existe esa columna en `ventas`); muestra
/// todas las ventas del día — simétrico a desktop.
async fn listar_ventas_dia(state: &AppState) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;

    // `date(v.fecha)` sobre la columna TEXT funciona porque el formato
    // canónico es `YYYY-MM-DD HH:MM:SS` (mismo que SQLite).
    // Comparamos contra "hoy" en zona MX para no traer otros días.
    let rows = sqlx::query(
        r#"
        SELECT v.id, v.folio, v.total, v.metodo_pago, v.anulada, v.fecha,
               u.nombre_completo AS usuario_nombre,
               c.nombre AS cliente_nombre,
               COALESCE((SELECT COUNT(*) FROM venta_detalle vd
                         WHERE vd.venta_id = v.id AND vd.deleted_at IS NULL), 0)
                 AS num_productos
        FROM ventas v
        LEFT JOIN usuarios u ON u.id = v.usuario_id
        LEFT JOIN clientes c ON c.id = v.cliente_id
        WHERE v.deleted_at IS NULL
          AND substr(v.fecha, 1, 10) = to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD')
        ORDER BY v.fecha DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "id":              r.get::<i64, _>("id"),
        "folio":           r.get::<String, _>("folio"),
        "usuario_nombre":  r.try_get::<Option<String>, _>("usuario_nombre").ok().flatten().unwrap_or_default(),
        "cliente_nombre":  r.try_get::<Option<String>, _>("cliente_nombre").ok().flatten(),
        "total":           r.try_get::<rust_decimal::Decimal, _>("total").ok()
                              .and_then(|d| d.to_f64()).unwrap_or(0.0),
        "metodo_pago":     r.get::<String, _>("metodo_pago"),
        "anulada":         r.get::<i32, _>("anulada") != 0,
        "fecha":           r.get::<String, _>("fecha"),
        "num_productos":   r.get::<i64, _>("num_productos"),
    })).collect::<Vec<_>>()))
}

/// Anular una venta. Reglas (mismas que desktop):
///   1. Solo el día en curso — para días anteriores se usa devolución.
///   2. No debe tener devoluciones parciales registradas.
///   3. Restaura stock de cada partida.
///   4. NO crea movimiento de caja: `calcular_datos_corte` ya excluye
///      ventas con `anulada = 1`, así que generar un retiro contaría doble.
async fn anular_venta(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct A {
        venta_id: i64,
        usuario_id: i64,
        motivo: String,
    }

    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    if a.motivo.trim().is_empty() {
        return Err(ApiError::BadRequest("El motivo es obligatorio".into()));
    }

    let mut tx = state.pool.begin().await?;

    // Verificar que existe, no está anulada, y es del día en curso.
    let v = sqlx::query(
        "SELECT folio, fecha FROM ventas \
         WHERE id = $1 AND anulada = 0 AND deleted_at IS NULL",
    )
    .bind(a.venta_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::BadRequest("Venta no encontrada o ya anulada".into()))?;

    let folio: String = v.get("folio");
    let fecha_venta: String = v.get("fecha");

    let hoy: String = sqlx::query_scalar(
        "SELECT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD')",
    )
    .fetch_one(&mut *tx)
    .await?;
    if !fecha_venta.starts_with(&hoy) {
        return Err(ApiError::BadRequest(
            "Solo se puede anular una venta del día en curso. Para ventas anteriores, usa el flujo de devolución.".into(),
        ));
    }

    // ¿Tiene devoluciones parciales?
    let num_dev: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM devoluciones \
         WHERE venta_id = $1 AND deleted_at IS NULL",
    )
    .bind(a.venta_id)
    .fetch_one(&mut *tx)
    .await?;
    if num_dev > 0 {
        return Err(ApiError::BadRequest(
            "La venta tiene devoluciones parciales registradas. No se puede anular completa.".into(),
        ));
    }

    // Restaurar stock de cada partida.
    let items = sqlx::query(
        "SELECT producto_id, cantidad FROM venta_detalle \
         WHERE venta_id = $1 AND deleted_at IS NULL",
    )
    .bind(a.venta_id)
    .fetch_all(&mut *tx)
    .await?;

    let upd_stock_sql = format!(
        "UPDATE productos SET stock_actual = stock_actual + $1, updated_at = {NOW_TEXT} \
         WHERE id = $2"
    );
    for it in &items {
        let prod_id: i64 = it.get("producto_id");
        let cantidad: f64 = it.try_get::<rust_decimal::Decimal, _>("cantidad")
            .ok().and_then(|d| d.to_f64()).unwrap_or(0.0);
        sqlx::query(&upd_stock_sql)
            .bind(cantidad)
            .bind(prod_id)
            .execute(&mut *tx)
            .await?;
    }

    // Marcar como anulada.
    sqlx::query(
        "UPDATE ventas SET anulada = 1, anulada_por = $1, motivo_anulacion = $2 \
         WHERE id = $3",
    )
    .bind(a.usuario_id)
    .bind(&a.motivo)
    .bind(a.venta_id)
    .execute(&mut *tx)
    .await?;

    // Bitácora.
    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            descripcion_legible, origen, fecha)
           VALUES ($1, 'ANULACION', 'ventas', $2, $3, 'WEB', {NOW_TEXT})"#
    );
    let _ = sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(a.venta_id)
        .bind(format!("Venta {} anulada — Motivo: {}", folio, a.motivo))
        .execute(&mut *tx)
        .await;

    tx.commit().await?;
    Ok(json!(true))
}

// =============================================================================
// BITÁCORA — visor de audit_log (paridad con `commands/bitacora.rs:20`)
// =============================================================================
//
// El frontend (`pages/Bitacora.tsx`) manda `{ limite, accionFiltro }`. Si la
// columna `accion` contiene el filtro como substring, se devuelve. La tabla
// `audit_log` postgres se creó en migration 002 y solo tiene entradas con
// `origen='WEB'` (las del desktop viven en su SQLite local; bitácoras
// divergentes hasta que se unifiquen — ver auditoría).

async fn listar_bitacora(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct A {
        #[serde(default)] limite: Option<i64>,
        #[serde(default)] accion_filtro: Option<String>,
    }
    let a: A = serde_json::from_value(args.clone()).unwrap_or_default();
    let lim = a.limite.unwrap_or(200).clamp(1, 1000);
    let filtro_like = a.accion_filtro
        .as_ref()
        .map(|s| format!("%{}%", s));

    let rows = sqlx::query(
        r#"
        SELECT a.id, u.nombre_completo AS usuario_nombre,
               a.accion, a.tabla_afectada, a.registro_id,
               a.descripcion_legible, a.origen, a.fecha
        FROM audit_log a
        LEFT JOIN usuarios u ON u.id = a.usuario_id
        WHERE ($1::text IS NULL OR a.accion LIKE $1)
        ORDER BY a.fecha DESC
        LIMIT $2
        "#,
    )
    .bind(&filtro_like)
    .bind(lim)
    .fetch_all(&state.pool)
    .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "id":                  r.get::<i64, _>("id"),
        "usuario_nombre":      r.try_get::<Option<String>, _>("usuario_nombre").ok().flatten(),
        "accion":              r.get::<String, _>("accion"),
        "tabla_afectada":      r.try_get::<Option<String>, _>("tabla_afectada").ok().flatten(),
        "registro_id":         r.try_get::<Option<i64>, _>("registro_id").ok().flatten(),
        "descripcion_legible": r.try_get::<Option<String>, _>("descripcion_legible").ok().flatten().unwrap_or_default(),
        "origen":              r.get::<String, _>("origen"),
        "fecha":               r.get::<String, _>("fecha"),
    })).collect::<Vec<_>>()))
}
