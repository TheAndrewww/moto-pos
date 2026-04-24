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
        "listar_usuarios"               => listar_usuarios(&state).await?,
        "listar_roles"                  => listar_roles(),

        // ─── Config / sistema ────────────────────────────────
        "obtener_config_negocio"        => obtener_config_negocio(),
        "obtener_config_descuentos"     => obtener_config_descuentos(),
        "listar_impresoras"             => json!([]),
        "obtener_info_bd"               => json!(0),
        "obtener_info_servidor"         => obtener_info_servidor(&state).await?,
        "listar_dispositivos"           => listar_dispositivos(&state).await?,

        // ─── Cortes / caja ───────────────────────────────────
        "obtener_apertura_hoy"          => obtener_apertura_hoy(&state).await?,
        "crear_apertura_caja"           => crear_apertura_caja(&state, &args).await?,
        "verificar_corte_dia_pendiente" => Value::Null,
        "listar_movimientos_sin_corte"  => json!([]),
        "listar_cortes"                 => json!([]),
        "obtener_fondo_sugerido"        => json!(2000.0),

        // ─── Ventas ──────────────────────────────────────────
        "buscar_ventas"                 => buscar_ventas(&state, &args).await?,
        "obtener_detalle_venta"         => obtener_detalle_venta(&state, &args).await?,
        "crear_venta"                   => crear_venta(&state, args).await?,

        // ─── Estadísticas ────────────────────────────────────
        "obtener_estadisticas_dia"      => obtener_estadisticas_dia(&state, &args).await?,

        // ─── Bitácora (read) ─────────────────────────────────
        "listar_bitacora"               => json!([]),

        // ─── Presupuestos (stub) ─────────────────────────────
        "listar_presupuestos"           => json!([]),
        "listar_devoluciones"           => json!([]),

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

async fn obtener_info_servidor(state: &AppState) -> Result<Value, ApiError> {
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM pos_devices",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    Ok(json!({
        "host": "servidor-remoto",
        "dispositivos_conectados": total,
        "tipo": "postgres",
    }))
}

async fn listar_dispositivos(state: &AppState) -> Result<Value, ApiError> {
    let rows = sqlx::query(
        "SELECT id, device_uuid, nombre, sucursal_id, last_push_at, last_pull_at \
         FROM pos_devices ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(json!(rows.iter().map(row_to_json).collect::<Vec<_>>()))
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
struct VentaIdArg { id: i64 }

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
    datos: NuevaVentaWeb,
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
    let d = a.datos;

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
    let row = sqlx::query(
        r#"
        SELECT
          COALESCE(SUM(total), 0)::numeric AS total_dia,
          COUNT(*)::bigint                 AS num_ventas,
          COALESCE(SUM(descuento), 0)::numeric AS descuento_dia
        FROM ventas
        WHERE deleted_at IS NULL AND anulada = 0
          AND substr(fecha, 1, 10)
              = to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD')
        "#,
    )
    .fetch_one(&state.pool)
    .await?;

    let total: f64 = row.try_get::<rust_decimal::Decimal, _>("total_dia")
        .ok().and_then(|d| d.to_f64()).unwrap_or(0.0);
    let desc: f64 = row.try_get::<rust_decimal::Decimal, _>("descuento_dia")
        .ok().and_then(|d| d.to_f64()).unwrap_or(0.0);
    let num: i64 = row.try_get("num_ventas").unwrap_or(0);

    Ok(json!({
        "total_ventas_dia": total,
        "num_ventas_dia":   num,
        "total_descuentos_dia": desc,
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

        // Sumar al stock + actualizar precio_costo
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
    let row = sqlx::query(
        r#"
        SELECT a.id, a.usuario_id, u.nombre_completo AS usuario_nombre,
               a.fondo_declarado, a.nota, a.fecha
        FROM aperturas_caja a
        JOIN usuarios u ON u.id = a.usuario_id
        WHERE a.deleted_at IS NULL
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

    // Validar que no exista ya una apertura para hoy.
    let existe: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM aperturas_caja \
         WHERE deleted_at IS NULL \
           AND substr(fecha, 1, 10) \
               = to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD')",
    )
    .fetch_one(&state.pool)
    .await?;
    if existe > 0 {
        return Err(ApiError::BadRequest("Ya existe una apertura de caja para hoy".into()));
    }

    let sucursal_id: i64 = 1;
    let new_uuid = uuid::Uuid::now_v7().to_string();
    let sql = format!(
        r#"
        INSERT INTO aperturas_caja
          (uuid, sucursal_id, usuario_id, fondo_declarado, nota, fecha, updated_at)
        VALUES ($1, $2, $3, $4, $5, {NOW_TEXT}, {NOW_TEXT})
        RETURNING id, fecha
        "#
    );
    let row = sqlx::query(&sql)
        .bind(&new_uuid)
        .bind(sucursal_id)
        .bind(d.usuario_id)
        .bind(d.fondo_declarado)
        .bind(d.nota.as_deref())
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
