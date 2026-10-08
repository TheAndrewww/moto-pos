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

use crate::auth::{autenticar, emitir_token_con_device, Claims};
use crate::caja::{self, Caja};
use crate::error::{ApiError, ApiResult};
use crate::AppState;

const WEB_ORIGIN: &str = "web-pos";

/// Expresión SQL para timestamp TEXT igual al POS.
const NOW_TEXT: &str = "to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS')";

/// RPC que mueven dinero de la caja: exigen el header `x-pos-cliente` >= 2.
const RPC_QUE_MUEVEN_CAJA: &[&str] = &[
    "crear_corte",
    "crear_apertura_caja",
    "crear_movimiento_caja",
    "crear_devolucion",
    "anular_venta",
];

/// Versión mínima del bundle web (invokeCompat la manda en cada RPC).
const VERSION_MINIMA_CLIENTE_WEB: i64 = 2;

/// ¿El navegador corre un bundle web actual (header `x-pos-cliente` >= 2)?
fn es_cliente_actual(headers: &HeaderMap) -> bool {
    headers
        .get("x-pos-cliente")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .is_some_and(|v| v >= VERSION_MINIMA_CLIENTE_WEB)
}

fn exigir_cliente_actual(headers: &HeaderMap) -> Result<(), ApiError> {
    if es_cliente_actual(headers) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "Hay una versión nueva del POS web. Recarga la página (F5) antes de continuar.".into(),
        ))
    }
}

/// Aviso para una pestaña vieja cuya venta se rechazó por algo que solo el
/// bundle nuevo sabe resolver: no refresca precios tras PRECIO_CAMBIO ni pide
/// el token de `autorizar_descuento`, así que reintentar no serviría.
const AVISO_RECARGAR_VENTA: &str = " Hay una versión nueva del POS web. Recarga la página (F5).";

/// A un error de `crear_venta` de un cliente viejo le agrega cómo salir de él.
fn aviso_cliente_viejo_venta(e: ApiError, headers: &HeaderMap) -> ApiError {
    match e {
        ApiError::BadRequest(m)
            if !es_cliente_actual(headers)
                && (m.starts_with("DESCUENTO_NO_AUTORIZADO") || m.starts_with("PRECIO_CAMBIO")) =>
        {
            ApiError::BadRequest(format!("{m}{AVISO_RECARGAR_VENTA}"))
        }
        otro => otro,
    }
}

/// Serializa una respuesta (structs de caja.rs con la forma del escritorio).
fn a_json<T: serde::Serialize>(v: T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::Other(e.into()))
}

// -----------------------------------------------------------------------------
// Identidad del cajero
// -----------------------------------------------------------------------------
//
// Las RPC que mueven dinero NO usan el `usuario_id` del body: lo toman del
// JWT. Los tokens de `login_pin`/`login_password` llevan sub = usuarios.id y
// email = nombre_usuario (ver `usuario_sesion_response`). Exigir que ambos
// coincidan con una fila activa descarta tokens de otros flujos (p. ej. el
// token de sync de `/auth/login`, cuyo `sub` es un admin_users.id que puede
// chocar con un usuarios.id) y usuarios desactivados después del login.

struct SesionUsuario {
    id: i64,
    es_admin: bool,
}

async fn usuario_de_sesion(state: &AppState, claims: &Claims) -> Result<SesionUsuario, ApiError> {
    let row = sqlx::query(
        "SELECT u.id, COALESCE(r.es_admin, 0) AS es_admin \
         FROM usuarios u LEFT JOIN roles r ON r.id = u.rol_id \
         WHERE u.id = $1 AND lower(u.nombre_usuario) = lower($2) \
           AND u.activo = 1 AND u.deleted_at IS NULL",
    )
    .bind(claims.sub)
    .bind(&claims.email)
    .fetch_optional(&state.pool)
    .await?
    // 401 → invokeCompat borra la sesión y manda a login.
    .ok_or(ApiError::Unauthorized)?;
    Ok(SesionUsuario {
        id: row.get("id"),
        es_admin: row.get::<i32, _>("es_admin") != 0,
    })
}

/// Avisa (sin fallar) si el navegador mandó un usuario_id distinto al de la
/// sesión. Pasa con pestañas viejas tras cambiar de usuario.
fn avisar_usuario_distinto(rpc: &str, body_usuario_id: i64, sesion: &SesionUsuario) {
    if body_usuario_id != sesion.id {
        tracing::warn!(
            "{}: usuario_id del body ({}) != usuario de la sesión ({}); se usa el de la sesión",
            rpc, body_usuario_id, sesion.id
        );
    }
}

/// Límite de descuento sin PIN para vendedores (config_descuentos id=1).
async fn max_descuento_vendedor(state: &AppState) -> Result<f64, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    let v: Option<rust_decimal::Decimal> = sqlx::query_scalar(
        "SELECT descuento_max_vendedor_pct FROM config_descuentos WHERE id = 1",
    )
    .fetch_optional(&state.pool)
    .await?;
    // Mismo default que obtener_config_descuentos y el SEED del desktop.
    Ok(v.and_then(|d| d.to_f64()).unwrap_or(15.0))
}

fn dec_f64(d: rust_decimal::Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or(0.0)
}

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
    let claims = autenticar(&headers, &state.jwt_secret)?;

    // Las RPC que mueven dinero de la caja exigen un bundle web actual. Una
    // pestaña vieja (sin el header) mandaba el retiro de turno ANTES del
    // corte: con el corte que ahora se recalcula aquí, cada reintento tras
    // DATOS_CAMBIARON dejaba otro retiro. Se rechaza antes de tocar la BD.
    if RPC_QUE_MUEVEN_CAJA.contains(&cmd.as_str()) {
        exigir_cliente_actual(&headers)?;
    }

    let result = match cmd.as_str() {
        // ─── Catálogos (read) ────────────────────────────────
        "listar_productos"              => listar_productos(&state).await?,
        "listar_productos_stock_bajo"   => listar_productos_stock_bajo(&state).await?,
        "obtener_producto_por_codigo"   => obtener_producto_por_codigo(&state, &args).await?,
        "listar_categorias"             => listar_categorias(&state).await?,
        "listar_proveedores"            => listar_proveedores(&state).await?,
        "listar_clientes"               => listar_clientes(&state).await?,
        "crear_cliente"                 => crear_cliente(&state, &claims, args).await?,
        "actualizar_cliente"            => actualizar_cliente(&state, &claims, args).await?,
        "toggle_cliente_activo"         => toggle_cliente_activo(&state, &args).await?,
        "listar_usuarios"               => listar_usuarios(&state).await?,
        "crear_usuario"                 => crear_usuario(&state, &claims, args).await?,
        "actualizar_usuario"            => actualizar_usuario(&state, &claims, args).await?,
        "toggle_usuario_activo"         => toggle_usuario_activo(&state, &claims, &args).await?,
        "listar_roles"                  => listar_roles(&state).await?,

        // ─── Productos (mutaciones) ──────────────────────────
        "crear_producto"                => crear_producto(&state, args).await?,
        "actualizar_producto"           => actualizar_producto(&state, args).await?,
        "eliminar_producto"             => eliminar_producto(&state, &args).await?,
        "ajustar_stock"                 => ajustar_stock(&state, &args).await?,
        "generar_codigo_interno"        => generar_codigo_interno(&state).await?,
        "historial_precios_producto"    => historial_precios_producto(&state, &args).await?,

        // ─── Config / sistema ────────────────────────────────
        "obtener_config_negocio"        => obtener_config_negocio(&state).await?,
        "obtener_config_descuentos"     => obtener_config_descuentos(&state).await?,
        "actualizar_config_negocio"     => actualizar_config_negocio(&state, &claims, args).await?,
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

        // ─── Modo de caja ────────────────────────────────────
        // Estos dos viven aparte porque son los únicos que escriben a
        // pos_devices (los demás solo leen el modo desde claims+BD).
        "obtener_modo_caja"             => obtener_modo_caja(&state, &claims).await?,
        "configurar_modo_caja"          => configurar_modo_caja(&state, &claims, &args).await?,

        // ─── Cortes / caja ───────────────────────────────────
        // Reciben &claims para resolver la caja del dispositivo (caja.rs):
        // modo individual → caja 'web'; espejo o sin dispositivo → caja de
        // la tienda (solo lectura aquí; sus cortes los hace el escritorio).
        "obtener_apertura_hoy"          => obtener_apertura_hoy(&state, &claims).await?,
        "crear_apertura_caja"           => crear_apertura_caja(&state, &claims, &args).await?,
        "obtener_esperado_apertura"     => obtener_esperado_apertura(&state, &claims).await?,
        "verificar_corte_dia_pendiente" => verificar_corte_dia_pendiente(&state, &claims).await?,
        "obtener_inicio_proximo_cierre" => obtener_inicio_proximo_cierre(&state, &claims).await?,
        "listar_movimientos_sin_corte"  => listar_movimientos_sin_corte(&state, &claims).await?,
        "listar_cortes"                 => listar_cortes(&state, &claims, &args).await?,
        "obtener_detalle_corte"         => obtener_detalle_corte(&state, &args).await?,
        "obtener_fondo_sugerido"        => obtener_fondo_sugerido(&state, &claims).await?,
        "crear_movimiento_caja"         => crear_movimiento_caja(&state, &claims, args).await?,
        "calcular_datos_corte"          => calcular_datos_corte(&state, &claims).await?,
        "crear_corte"                   => crear_corte(&state, &claims, args).await?,
        "auditar_caja"                  => auditar_caja(&state, &claims, &args).await?,

        // ─── Reportes (solo lectura: todas las ventas, sin filtro de caja) ──
        "obtener_top_productos"         => obtener_top_productos(&state, &args).await?,
        "obtener_ventas_por_vendedor"   => obtener_ventas_por_vendedor(&state, &args).await?,
        "obtener_ventas_por_metodo"     => obtener_ventas_por_metodo(&state, &args).await?,
        "obtener_ventas_por_dia"        => obtener_ventas_por_dia(&state, &args).await?,

        // ─── Ventas ──────────────────────────────────────────
        "buscar_ventas"                 => buscar_ventas(&state, &args).await?,
        "contar_ventas"                 => contar_ventas(&state, &args).await?,
        "obtener_detalle_venta"         => obtener_detalle_venta(&state, &args).await?,
        "crear_venta"                   => crear_venta(&state, &claims, args)
                                               .await
                                               .map_err(|e| aviso_cliente_viejo_venta(e, &headers))?,
        "listar_ventas_dia"             => listar_ventas_dia(&state).await?,
        "anular_venta"                  => anular_venta(&state, &claims, &args).await?,

        // ─── Estadísticas ────────────────────────────────────
        "obtener_estadisticas_dia"      => obtener_estadisticas_dia(&state, &args).await?,

        // ─── Bitácora (read) ─────────────────────────────────
        "listar_bitacora"               => listar_bitacora(&state, &args).await?,

        // ─── Presupuestos (stub) ─────────────────────────────
        "listar_presupuestos"           => json!([]),

        // ─── Devoluciones ────────────────────────────────────
        "listar_devoluciones"           => listar_devoluciones(&state, &args).await?,
        "obtener_detalle_devolucion"    => obtener_detalle_devolucion(&state, &args).await?,
        "crear_devolucion"              => crear_devolucion(&state, &claims, args).await?,

        // ─── Recepciones ─────────────────────────────────────
        "listar_recepciones"            => listar_recepciones(&state).await?,
        "obtener_detalle_recepcion"     => obtener_detalle_recepcion(&state, &args).await?,
        "crear_recepcion"               => crear_recepcion(&state, &claims, args).await?,

        // ─── Pedidos a proveedor ─────────────────────────────
        "listar_ordenes_pedido"         => listar_ordenes_pedido(&state, &args).await?,
        "obtener_detalle_orden"         => obtener_detalle_orden(&state, &args).await?,
        "crear_orden_pedido"            => crear_orden_pedido(&state, args).await?,
        "cambiar_estado_orden"          => cambiar_estado_orden(&state, &args).await?,

        // ─── PIN dueño (autorizaciones) ──────────────────────
        "verificar_pin_dueno"           => verificar_pin_dueno(&state, &args).await?,
        "resolver_dueno_por_pin"        => resolver_dueno_por_pin(&state, &args).await?,
        // Solo web: verifica el PIN y devuelve un token firmado que
        // `crear_venta` exige para descuentos arriba del límite del vendedor.
        "autorizar_descuento"           => autorizar_descuento(&state, &claims, &args).await?,

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
    validar_numeros_producto(p.precio_costo, p.precio_venta, p.stock_minimo)?;
    validar_stock(p.stock_actual)?;

    let mut tx = state.pool.begin().await?;

    // Generar código si no se proporcionó (atómico vía codigo_secuencia).
    // La secuencia del servidor y la del escritorio avanzan por separado, así
    // que un MR-xxxxx puede estar ya tomado: se salta a la siguiente libre.
    let codigo = match p.codigo.filter(|c| !c.is_empty()) {
        Some(c) => {
            verificar_codigo_libre(&mut tx, &c, None).await?;
            c
        }
        None => {
            let mut libre: Option<String> = None;
            for _ in 0..1000 {
                let nuevo: i64 = sqlx::query_scalar(
                    "UPDATE codigo_secuencia SET ultimo_valor = ultimo_valor + 1 \
                     WHERE id = 1 RETURNING ultimo_valor",
                )
                .fetch_one(&mut *tx)
                .await?;
                let candidato = format!("MR-{:05}", nuevo);
                if producto_con_codigo(&mut tx, &candidato, None).await?.is_none() {
                    libre = Some(candidato);
                    break;
                }
            }
            libre.ok_or_else(|| ApiError::BadRequest(
                "No se encontró un código interno libre; captura el código a mano".into(),
            ))?
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
    sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(id)
        .bind(format!("Producto creado: {} ({})", p.nombre, codigo))
        .execute(&mut *tx)
        .await?;

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
    validar_numeros_producto(p.precio_costo, p.precio_venta, p.stock_minimo)?;
    if p.codigo.trim().is_empty() {
        return Err(ApiError::BadRequest("El código es obligatorio".into()));
    }

    let mut tx = state.pool.begin().await?;

    // Capturar precio anterior + datos para bitácora antes del UPDATE
    let prev = sqlx::query(
        "SELECT uuid, codigo, nombre, precio_costo, precio_venta, categoria_id, proveedor_id \
         FROM productos WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(p.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::NotFound)?;

    let uuid: String = prev.get("uuid");
    let codigo_ant: String = prev.get("codigo");
    let nombre_ant: String = prev.get("nombre");
    let fks_cambiadas = fks_producto_cambiadas(
        prev.get("categoria_id"),
        prev.get("proveedor_id"),
        p.categoria_id,
        p.proveedor_id,
    );
    // Solo se valida si el código cambia: hay duplicados viejos en el
    // servidor y no queremos impedir que se editen sus precios.
    if p.codigo != codigo_ant {
        verificar_codigo_libre(&mut tx, &p.codigo, Some(p.id)).await?;
    }
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
    anotar_celdas_fk_web(&mut tx, "productos", &uuid, &fks_cambiadas).await?;

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
    sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(p.id)
        .bind(&datos_ant_str)
        .bind(format!("Producto editado: {}", p.nombre))
        .execute(&mut *tx)
        .await?;

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
        sqlx::query(&pa_sql)
            .bind(a.usuario_id)
            .bind(p.id)
            .bind(&json_ant)
            .bind(&json_new)
            .bind(&descr)
            .execute(&mut *tx)
            .await?;
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
    sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(a.producto_id)
        .bind(format!("Producto eliminado: {}", nombre))
        .execute(&mut *tx)
        .await?;

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
    validar_stock(a.nuevo_stock)?;

    let mut tx = state.pool.begin().await?;

    let prev = sqlx::query(
        "SELECT uuid, nombre, stock_actual FROM productos \
         WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
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

    // stock_sucursal ya no se sincroniza (no lo lee nadie): se mantiene al
    // día localmente pero NO se escribe sync_cursor para esa tabla.
    let upd_suc_sql = format!(
        "UPDATE stock_sucursal SET stock_actual = $1, updated_at = {NOW_TEXT} \
         WHERE producto_id = $2 AND sucursal_id = 1"
    );
    sqlx::query(&upd_suc_sql)
        .bind(a.nuevo_stock)
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
    sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(a.producto_id)
        .bind(&json_ant)
        .bind(&json_new)
        .bind(&descr)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(json!(true))
}

/// Tope de cordura para existencias (numeric(12,2) desborda arriba de
/// 9,999,999,999.99 y Postgres respondería 500).
pub(crate) const MAX_STOCK: f64 = 100_000_000.0;

/// Existencias: número finito, no negativo (regla de la tienda: el stock nunca
/// queda bajo cero; una venta lo frena en 0).
fn validar_stock(stock: f64) -> Result<(), ApiError> {
    if !stock.is_finite() || stock < 0.0 {
        return Err(ApiError::BadRequest("El stock no puede ser negativo".into()));
    }
    if stock > MAX_STOCK {
        return Err(ApiError::BadRequest("El stock capturado es demasiado grande".into()));
    }
    Ok(())
}

/// Precios y stock mínimo del formulario de producto.
pub(crate) fn validar_numeros_producto(
    precio_costo: f64,
    precio_venta: f64,
    stock_minimo: f64,
) -> Result<(), ApiError> {
    use crate::venta_calc::MAX_IMPORTE;
    for (nombre, v) in [("costo", precio_costo), ("precio de venta", precio_venta)] {
        if !v.is_finite() || v < 0.0 || v > MAX_IMPORTE {
            return Err(ApiError::BadRequest(format!("El {} no es válido", nombre)));
        }
    }
    if !stock_minimo.is_finite() || stock_minimo < 0.0 || stock_minimo > MAX_STOCK {
        return Err(ApiError::BadRequest("El stock mínimo no puede ser negativo".into()));
    }
    Ok(())
}

/// La web acaba de CAMBIAR celdas FK de una fila que ya existía (dentro de la
/// transacción del UPDATE). Si la fila la hizo el escritorio (uuid de 32 hex)
/// y todavía no se repara (sin sync_fk_estado), sus FKs siguen siendo ids del
/// escritorio, pero lo que escribió la web ya es un id de ESTE servidor: se
/// anota en sync_fk_celda_web para que la reparación (sync_fk) no lo traduzca
/// otra vez como si fuera del escritorio (p. ej. 'Frenos' → 'Suspensión').
///
/// Solo cuentan las columnas de tipo Tabla del mapa compartido (las Paso,
/// como usuarios.rol_id, significan lo mismo en los dos lados). Filas hechas
/// en la web o ya traducidas/reparadas no necesitan nada.
pub(crate) async fn anotar_celdas_fk_web(
    conn: &mut sqlx::PgConnection,
    tabla: &str,
    uuid: &str,
    cambiadas: &[&str],
) -> Result<(), ApiError> {
    use crate::sync_fk_map::{es_uuid_escritorio, fks_de, RefKind};
    if !es_uuid_escritorio(uuid) {
        return Ok(());
    }
    let columnas: Vec<String> = cambiadas
        .iter()
        .filter(|c| {
            fks_de(tabla)
                .iter()
                .any(|(col, k)| col == *c && matches!(k, RefKind::Tabla(_)))
        })
        .map(|c| c.to_string())
        .collect();
    if columnas.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO sync_fk_celda_web (tabla, uuid, columna) \
         SELECT $1, $2, c FROM unnest($3::text[]) AS c \
         WHERE NOT EXISTS (SELECT 1 FROM sync_fk_estado e WHERE e.tabla = $1 AND e.uuid = $2) \
         ON CONFLICT (tabla, uuid, columna) DO NOTHING",
    )
    .bind(tabla)
    .bind(uuid)
    .bind(&columnas)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Columnas FK de catálogo de un producto que cambian entre lo guardado y lo
/// que manda el formulario.
pub(crate) fn fks_producto_cambiadas(
    categoria_ant: Option<i64>,
    proveedor_ant: Option<i64>,
    categoria_nueva: Option<i64>,
    proveedor_nuevo: Option<i64>,
) -> Vec<&'static str> {
    let mut v = Vec::new();
    if categoria_ant != categoria_nueva {
        v.push("categoria_id");
    }
    if proveedor_ant != proveedor_nuevo {
        v.push("proveedor_id");
    }
    v
}

/// Producto (incluidos los eliminados) que ya usa `codigo`, distinto de
/// `excepto_id`. Devuelve (nombre, eliminado).
async fn producto_con_codigo(
    conn: &mut sqlx::PgConnection,
    codigo: &str,
    excepto_id: Option<i64>,
) -> Result<Option<(String, bool)>, ApiError> {
    let row = sqlx::query(
        "SELECT nombre, (deleted_at IS NOT NULL) AS eliminado FROM productos \
         WHERE codigo = $1 AND ($2::bigint IS NULL OR id <> $2) \
         ORDER BY deleted_at NULLS FIRST, id LIMIT 1",
    )
    .bind(codigo)
    .bind(excepto_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| (r.get::<String, _>("nombre"), r.get::<bool, _>("eliminado"))))
}

/// Rechaza un código que ya usa OTRO producto, incluso eliminado: en el POS de
/// escritorio `productos.codigo` es UNIQUE también para filas borradas, así
/// que un duplicado creado aquí nunca podría bajar a la caja. El candado
/// asesor (por código) evita que dos altas simultáneas pasen la revisión.
pub(crate) async fn verificar_codigo_libre(
    conn: &mut sqlx::PgConnection,
    codigo: &str,
    excepto_id: Option<i64>,
) -> Result<(), ApiError> {
    sqlx::query("SELECT pg_advisory_xact_lock(7301102, hashtext($1))")
        .bind(codigo)
        .execute(&mut *conn)
        .await?;
    if let Some((nombre, eliminado)) = producto_con_codigo(conn, codigo, excepto_id).await? {
        return Err(ApiError::BadRequest(if eliminado {
            format!(
                "El código '{}' ya lo usa el producto eliminado '{}'. Usa otro código.",
                codigo, nombre
            )
        } else {
            format!("El código '{}' ya lo usa el producto '{}'", codigo, nombre)
        }));
    }
    Ok(())
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
    // JOIN con roles para evitar N+1 (resolvemos rol_nombre y es_admin en
    // una sola query). Antes esto era hardcodeado: rol id<=2 = admin.
    let rows = sqlx::query(
        "SELECT u.id, u.nombre_completo, u.nombre_usuario, u.rol_id, u.activo, \
                u.ultimo_login, r.nombre AS rol_nombre, r.es_admin \
         FROM usuarios u \
         LEFT JOIN roles r ON r.id = u.rol_id \
         WHERE u.deleted_at IS NULL \
         ORDER BY u.nombre_completo",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(json!(rows.iter().map(|r| json!({
        "id":              r.get::<i64, _>("id"),
        "nombre_completo": r.get::<String, _>("nombre_completo"),
        "nombre_usuario":  r.get::<String, _>("nombre_usuario"),
        "rol_id":          r.get::<i64, _>("rol_id"),
        "rol_nombre":      r.try_get::<Option<String>, _>("rol_nombre").ok().flatten().unwrap_or_else(|| "desconocido".into()),
        "es_admin":        r.try_get::<i32, _>("es_admin").map(|v| v != 0).unwrap_or(false),
        "activo":          r.get::<i32, _>("activo") != 0,
        "ultimo_login":    r.try_get::<Option<String>, _>("ultimo_login").ok().flatten(),
    })).collect::<Vec<_>>()))
}

/// Lista los roles desde la tabla `roles` (sembrada por migración 007).
async fn listar_roles(state: &AppState) -> Result<Value, ApiError> {
    let rows = sqlx::query(
        "SELECT id, nombre, COALESCE(descripcion, '') AS descripcion, es_admin \
         FROM roles ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(json!(rows.iter().map(|r| json!({
        "id":          r.get::<i64, _>("id"),
        "nombre":      r.get::<String, _>("nombre"),
        "descripcion": r.try_get::<String, _>("descripcion").unwrap_or_default(),
        "es_admin":    r.get::<i32, _>("es_admin") != 0,
    })).collect::<Vec<_>>()))
}

/// Obtiene el nombre del rol leyendo de la tabla. Devuelve "desconocido" si
/// no existe (caso raro: rol_id inválido en algún registro).
async fn rol_nombre_por_id(state: &AppState, id: i64) -> String {
    sqlx::query_scalar::<_, String>("SELECT nombre FROM roles WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "desconocido".into())
}

/// Verifica si el rol es admin leyendo de la tabla `roles`. Devuelve `false`
/// si no existe el rol (defensa: nunca confunde "rol inválido" con admin).
async fn rol_es_admin(state: &AppState, id: i64) -> bool {
    sqlx::query_scalar::<_, i32>("SELECT es_admin FROM roles WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .map(|v| v != 0)
        .unwrap_or(false)
}

/// Lista los permisos de un rol específico. Usado por usuario_sesion_response
/// para devolver al frontend los permisos del usuario logueado.
async fn permisos_de_rol(state: &AppState, rol_id: i64) -> Vec<Value> {
    let rows = sqlx::query(
        "SELECT modulo, accion, permitido FROM permisos WHERE rol_id = $1",
    )
    .bind(rol_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    rows.iter().map(|r| json!({
        "modulo":    r.get::<String, _>("modulo"),
        "accion":    r.get::<String, _>("accion"),
        "permitido": r.get::<i32, _>("permitido") != 0,
    })).collect()
}

// =============================================================================
// CONFIG NEGOCIO / DESCUENTOS (web POS usa defaults)
// =============================================================================

async fn obtener_config_negocio(state: &AppState) -> Result<Value, ApiError> {
    let row = sqlx::query(
        "SELECT nombre, direccion, telefono, rfc, mensaje_pie, \
                respaldo_auto_activo, respaldo_auto_hora, impresora_termica \
         FROM config_negocio WHERE id = 1",
    )
    .fetch_optional(&state.pool)
    .await?;

    Ok(match row {
        Some(r) => json!({
            "nombre":               r.get::<String, _>("nombre"),
            "direccion":            r.get::<String, _>("direccion"),
            "telefono":             r.get::<String, _>("telefono"),
            "rfc":                  r.get::<String, _>("rfc"),
            "mensaje_pie":          r.get::<String, _>("mensaje_pie"),
            "respaldo_auto_activo": r.get::<i32, _>("respaldo_auto_activo") != 0,
            "respaldo_auto_hora":   r.get::<String, _>("respaldo_auto_hora"),
            "impresora_termica":    r.get::<String, _>("impresora_termica"),
        }),
        // Fallback (la migración 007 inserta la fila id=1, no debería pasar).
        None => json!({
            "nombre": "Moto Refaccionaria",
            "direccion": "", "telefono": "", "rfc": "",
            "mensaje_pie": "¡Gracias por su compra!",
            "respaldo_auto_activo": false, "respaldo_auto_hora": "23:00",
            "impresora_termica": "",
        }),
    })
}

async fn obtener_config_descuentos(state: &AppState) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    let row = sqlx::query(
        "SELECT descuento_max_vendedor_pct, descuento_max_total_pct, \
                precio_minimo_global_margen \
         FROM config_descuentos WHERE id = 1",
    )
    .fetch_optional(&state.pool)
    .await?;

    let dec = |r: &sqlx::postgres::PgRow, name: &str| -> f64 {
        r.try_get::<rust_decimal::Decimal, _>(name).ok()
            .and_then(|d| d.to_f64()).unwrap_or(0.0)
    };

    Ok(match row {
        Some(r) => json!({
            "descuento_max_vendedor_pct":  dec(&r, "descuento_max_vendedor_pct"),
            "descuento_max_total_pct":     dec(&r, "descuento_max_total_pct"),
            "precio_minimo_global_margen": dec(&r, "precio_minimo_global_margen"),
        }),
        None => json!({
            "descuento_max_vendedor_pct": 15.0,
            "descuento_max_total_pct":    10.0,
            "precio_minimo_global_margen": 5.0,
        }),
    })
}

/// Actualiza la fila singleton id=1 de config_negocio. Frontend manda
/// `{ datos: ConfigNegocio }`. Devuelve la config actualizada.
async fn actualizar_config_negocio(
    state: &AppState,
    claims: &Claims,
    args: Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { datos: ConfigIn }
    #[derive(Deserialize)]
    struct ConfigIn {
        nombre: String,
        #[serde(default)] direccion: String,
        #[serde(default)] telefono: String,
        #[serde(default)] rfc: String,
        #[serde(default)] mensaje_pie: String,
        #[serde(default)] respaldo_auto_activo: bool,
        #[serde(default = "default_hora")] respaldo_auto_hora: String,
        #[serde(default)] impresora_termica: String,
    }
    fn default_hora() -> String { "23:00".into() }

    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let c = a.datos;

    if c.nombre.trim().is_empty() {
        return Err(ApiError::BadRequest("El nombre del negocio es obligatorio".into()));
    }

    let upd_sql = format!(
        r#"UPDATE config_negocio SET
               nombre = $1, direccion = $2, telefono = $3, rfc = $4,
               mensaje_pie = $5, respaldo_auto_activo = $6,
               respaldo_auto_hora = $7, impresora_termica = $8,
               updated_at = {NOW_TEXT}
           WHERE id = 1"#
    );
    sqlx::query(&upd_sql)
        .bind(&c.nombre)
        .bind(&c.direccion)
        .bind(&c.telefono)
        .bind(&c.rfc)
        .bind(&c.mensaje_pie)
        .bind(if c.respaldo_auto_activo { 1 } else { 0 })
        .bind(&c.respaldo_auto_hora)
        .bind(&c.impresora_termica)
        .execute(&state.pool)
        .await?;

    // Bitácora
    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            descripcion_legible, origen, fecha)
           VALUES ($1, 'CONFIG_NEGOCIO_EDITADO', 'config_negocio', 1, $2, 'WEB', {NOW_TEXT})"#
    );
    let _ = sqlx::query(&audit_sql)
        .bind(claims.sub)
        .bind(format!("Configuración del negocio actualizada"))
        .execute(&state.pool)
        .await;

    obtener_config_negocio(state).await
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

// Args del frontend (camelCase, idénticos a los que manda HistorialVentas
// y Reportes). Todos los campos son opcionales y filtran solo si vienen.
//
// IMPORTANTE: el bug "Reportes siempre muestra los mismos números aunque
// cambies el rango" venía de que esta struct ignoraba fechaInicio/fechaFin
// (solo leía `texto` y `limite`). Sin filtro, el SQL devolvía las últimas
// 500 ventas sin importar qué rango pidiera el frontend.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BuscarVentasArgs {
    #[serde(default)] folio: Option<String>,
    #[serde(default)] fecha_inicio: Option<String>,
    #[serde(default)] fecha_fin: Option<String>,
    #[serde(default)] cliente_texto: Option<String>,
    #[serde(default)] articulo_texto: Option<String>,
    #[serde(default)] limite: Option<i64>,
    /// Salto inicial para paginación. Combinado con `limite`, permite
    /// navegar páginas grandes (Historial puede tener >10k filas). El
    /// frontend pide primero `contar_ventas` con los mismos filtros para
    /// saber cuántas páginas hay y después este handler con offset.
    #[serde(default)] offset: Option<i64>,
}

async fn buscar_ventas(
    state: &AppState,
    args: &Value,
) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    let a: BuscarVentasArgs = serde_json::from_value(args.clone()).unwrap_or_default();
    // Reportes pide hasta 5000, HistorialVentas hasta 100. Subimos el techo
    // a 10k para no truncar reportes mensuales grandes.
    let limite        = a.limite.unwrap_or(100).clamp(1, 10_000);
    let offset        = a.offset.unwrap_or(0).max(0);
    let folio_like    = a.folio.as_ref().filter(|s| !s.trim().is_empty())
        .map(|s| format!("%{}%", s.to_lowercase()));
    let cliente_like  = a.cliente_texto.as_ref().filter(|s| !s.trim().is_empty())
        .map(|s| format!("%{}%", s.to_lowercase()));
    let articulo_like = a.articulo_texto.as_ref().filter(|s| !s.trim().is_empty())
        .map(|s| format!("%{}%", s.to_lowercase()));
    let fecha_inicio  = a.fecha_inicio.as_ref().filter(|s| !s.trim().is_empty()).cloned();
    let fecha_fin     = a.fecha_fin.as_ref().filter(|s| !s.trim().is_empty()).cloned();

    // El historial es de solo lectura: muestra TODAS las ventas (escritorio y
    // web, de cualquier caja). Cada fila trae `caja` y `origen` para que la
    // pantalla ponga la etiqueta y decida si ofrece "Anular".
    // Subquery para filtrar por nombre/código de producto vendido. Solo se
    // aplica si vino articuloTexto. Mantener `$5::text IS NULL` evita
    // ejecutar la subquery cuando no hay filtro.
    //
    // NOTA sobre el filtro de fecha (substr 1..10 en ambos lados):
    // Antes comparábamos `v.fecha >= $3` directo. Eso falla cuando v.fecha
    // está en formato ISO con 'T' (ej. "2026-05-23T18:30:00.000Z") y el
    // filtro viene en formato con espacio ("2026-05-23 00:00:00"): la 'T'
    // (0x54) es lexicográficamente MAYOR que el espacio (0x20), así que
    // una venta del 23 de mayo termina FUERA del rango [2026-05-23 00:00,
    // 2026-05-23 23:59:59]. Resultado: las filas en formato ISO
    // desaparecen del historial/reportes.
    // Fix: comparar solo la parte YYYY-MM-DD (substr 1..10), que es
    // idéntica en ambos formatos.
    let sql = format!(
        r#"
        SELECT v.id, v.folio, v.total, v.metodo_pago, v.anulada, v.fecha,
               v.caja, v.origen,
               u.nombre_completo AS usuario_nombre,
               c.nombre AS cliente_nombre,
               COALESCE((SELECT COUNT(*) FROM venta_detalle vd
                         WHERE vd.venta_id = v.id AND vd.deleted_at IS NULL), 0)
                 AS num_productos
        FROM ventas v
        LEFT JOIN usuarios u ON u.id = v.usuario_id
        LEFT JOIN clientes c ON c.id = v.cliente_id
        WHERE v.deleted_at IS NULL
          AND ($1::text IS NULL OR lower(v.folio) LIKE $1)
          AND ($2::text IS NULL OR lower(COALESCE(c.nombre, '')) LIKE $2)
          AND ($3::text IS NULL OR substr(v.fecha, 1, 10) >= substr($3, 1, 10))
          AND ($4::text IS NULL OR substr(v.fecha, 1, 10) <= substr($4, 1, 10))
          AND ($5::text IS NULL OR EXISTS (
              SELECT 1 FROM venta_detalle vd2
              JOIN productos p ON p.id = vd2.producto_id
              WHERE vd2.venta_id = v.id AND vd2.deleted_at IS NULL
                AND (lower(p.nombre) LIKE $5 OR lower(p.codigo) LIKE $5)
          ))
        ORDER BY v.fecha DESC
        LIMIT $6 OFFSET $7
        "#
    );
    let rows = sqlx::query(&sql)
    .bind(&folio_like)
    .bind(&cliente_like)
    .bind(&fecha_inicio)
    .bind(&fecha_fin)
    .bind(&articulo_like)
    .bind(limite)
    .bind(offset)
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
        "caja":            r.get::<String, _>("caja"),
        "origen":          r.get::<String, _>("origen"),
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

/// Cuenta total de ventas que coinciden con los filtros, sin traer las
/// filas. Lo usa el Historial paginado para mostrar "Página X de Y" y
/// para saber cuántos botones de página renderizar.
///
/// Acepta los mismos filtros que `buscar_ventas` (folio, fechas, cliente,
/// artículo) pero ignora `limite` y `offset` — siempre devuelve el total.
async fn contar_ventas(
    state: &AppState,
    args: &Value,
) -> Result<Value, ApiError> {
    let a: BuscarVentasArgs = serde_json::from_value(args.clone()).unwrap_or_default();
    let folio_like    = a.folio.as_ref().filter(|s| !s.trim().is_empty())
        .map(|s| format!("%{}%", s.to_lowercase()));
    let cliente_like  = a.cliente_texto.as_ref().filter(|s| !s.trim().is_empty())
        .map(|s| format!("%{}%", s.to_lowercase()));
    let articulo_like = a.articulo_texto.as_ref().filter(|s| !s.trim().is_empty())
        .map(|s| format!("%{}%", s.to_lowercase()));
    let fecha_inicio  = a.fecha_inicio.as_ref().filter(|s| !s.trim().is_empty()).cloned();
    let fecha_fin     = a.fecha_fin.as_ref().filter(|s| !s.trim().is_empty()).cloned();

    let sql = format!(
        r#"
        SELECT COUNT(*)::bigint AS total
        FROM ventas v
        LEFT JOIN clientes c ON c.id = v.cliente_id
        WHERE v.deleted_at IS NULL
          AND ($1::text IS NULL OR lower(v.folio) LIKE $1)
          AND ($2::text IS NULL OR lower(COALESCE(c.nombre, '')) LIKE $2)
          AND ($3::text IS NULL OR substr(v.fecha, 1, 10) >= substr($3, 1, 10))
          AND ($4::text IS NULL OR substr(v.fecha, 1, 10) <= substr($4, 1, 10))
          AND ($5::text IS NULL OR EXISTS (
              SELECT 1 FROM venta_detalle vd2
              JOIN productos p ON p.id = vd2.producto_id
              WHERE vd2.venta_id = v.id AND vd2.deleted_at IS NULL
                AND (lower(p.nombre) LIKE $5 OR lower(p.codigo) LIKE $5)
          ))
        "#
    );
    let total: i64 = sqlx::query_scalar(&sql)
        .bind(&folio_like)
        .bind(&cliente_like)
        .bind(&fecha_inicio)
        .bind(&fecha_fin)
        .bind(&articulo_like)
        .fetch_one(&state.pool)
        .await?;

    Ok(json!({ "total": total }))
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
               v.monto_recibido, v.cambio, v.anulada, v.motivo_anulacion, v.fecha,
               ua.nombre_completo AS anulada_por_nombre, v.caja, v.origen,
               -- Mismo criterio que crear_devolucion: solo devoluciones cuya
               -- venta coincide con la de sus partidas.
               (SELECT COALESCE(SUM(dv.total_devuelto), 0)::float8 FROM devoluciones dv
                 WHERE dv.venta_id = v.id AND dv.deleted_at IS NULL
                   AND EXISTS (SELECT 1 FROM devolucion_detalle dd
                               JOIN venta_detalle vdd ON vdd.id = dd.venta_detalle_id
                               WHERE dd.devolucion_id = dv.id AND vdd.venta_id = dv.venta_id))
                 AS total_devuelto
        FROM ventas v
        LEFT JOIN usuarios u ON u.id = v.usuario_id
        LEFT JOIN usuarios ua ON ua.id = v.anulada_por
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
               -- Mismo filtro que crear_devolucion: solo devoluciones cuya
               -- venta coincide con la de la partida (las del escritorio
               -- todavía traen ids crudos que pueden apuntar a otra venta).
               COALESCE((SELECT SUM(dd.cantidad)
                         FROM devolucion_detalle dd
                         JOIN devoluciones dv ON dv.id = dd.devolucion_id
                         WHERE dd.venta_detalle_id = vd.id
                           AND dv.venta_id = vd.venta_id
                           AND dd.deleted_at IS NULL
                           AND dv.deleted_at IS NULL), 0) AS cantidad_devuelta
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
        "anulada_por_nombre": v.try_get::<Option<String>, _>("anulada_por_nombre").ok().flatten(),
        "fecha":            v.get::<String, _>("fecha"),
        "items":            items_json,
        "total_devuelto":   v.get::<f64, _>("total_devuelto"),
        "caja":             v.get::<String, _>("caja"),
        "origen":           v.get::<String, _>("origen"),
    }))
}

// =============================================================================
// VENTAS — crear (write crítico)
// =============================================================================

/// Llaves de pg_advisory_xact_lock para los folios web (una por tabla).
const LOCK_FOLIO_VENTAS: i64 = 7_301_100;
const LOCK_FOLIO_DEVOLUCIONES: i64 = 7_301_101;

/// Siguiente folio web `X-YYYYMMDD-NNNN` del día (hora de la tienda).
///
/// Antes era COUNT(*)+1: dos ventas simultáneas sacaban el mismo folio y el
/// escritorio (folio UNIQUE) rechazaba la segunda para siempre. Ahora se
/// serializa con un candado asesor de la transacción (se libera en el
/// COMMIT/ROLLBACK) y se toma MAX(consecutivo)+1, contando también filas
/// eliminadas (en el escritorio el UNIQUE las incluye).
///
/// Devuelve (folio, fecha). `fecha` es la de la fila: la que se pasa (caja
/// 'web': `ahora_caja`, tomada bajo el candado de la caja) o, si no, el reloj
/// real leído YA con el candado del folio (no now(), que es el inicio de la
/// transacción). El día del folio sale de esa misma fecha.
async fn siguiente_folio_web(
    conn: &mut sqlx::PgConnection,
    tabla: &str,
    prefijo: char,
    llave_candado: i64,
    fecha: Option<&str>,
) -> Result<(String, String), ApiError> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(llave_candado)
        .execute(&mut *conn)
        .await?;
    let fecha = match fecha {
        Some(f) => f.to_string(),
        None => caja::reloj(&mut *conn).await?,
    };
    let hoy: String = fecha.get(..10).unwrap_or("").replace('-', "");
    if hoy.len() != 8 {
        return Err(ApiError::Other(anyhow::anyhow!("fecha inválida para folio: {fecha}")));
    }
    let base = format!("{}-{}-", prefijo, hoy);
    // `tabla` es una constante del código (ventas/devoluciones), nunca input.
    let sql = format!(
        "SELECT COALESCE(MAX(substr(folio, $2)::bigint), 0)::bigint FROM {tabla} \
         WHERE folio LIKE $1 AND substr(folio, $2) ~ '^[0-9]+$'"
    );
    let ultimo: i64 = sqlx::query_scalar(&sql)
        .bind(format!("{}%", base))
        .bind(base.chars().count() as i32 + 1)
        .fetch_one(&mut *conn)
        .await?;
    Ok((format!("{}{:04}", base, ultimo + 1), fecha))
}

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
    /// Se IGNORA: quién autorizó lo decide el servidor (sesión admin o token
    /// de `autorizar_descuento`). Se sigue aceptando para no romper bundles
    /// viejos que lo mandan.
    #[serde(default)]
    #[allow(dead_code)]
    autorizado_por: Option<i64>,
    /// Token de `autorizar_descuento` cuando el descuento excede el límite
    /// del vendedor. El desktop no lo conoce y lo ignora (serde no rechaza
    /// campos desconocidos).
    #[serde(default)]
    autorizacion_token: Option<String>,
}

async fn crear_venta(
    state: &AppState,
    claims: &Claims,
    args: Value,
) -> Result<Value, ApiError> {
    use crate::venta_calc::{self, LineaCliente, MetodoPago, ReglaLinea, TotalesCliente};
    use std::collections::HashMap;

    let a: CrearVentaArgs = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let d = a.venta;

    if d.items.is_empty() {
        return Err(ApiError::BadRequest("La venta no tiene items".into()));
    }
    if d.items.len() > venta_calc::MAX_PARTIDAS {
        return Err(ApiError::BadRequest(format!(
            "La venta tiene demasiadas partidas (máximo {})", venta_calc::MAX_PARTIDAS)));
    }
    let metodo = MetodoPago::parse(&d.metodo_pago).ok_or_else(|| {
        ApiError::BadRequest(format!("Método de pago inválido: '{}'", d.metodo_pago))
    })?;
    // El POS web no tiene presupuestos (listar_presupuestos es un stub y no
    // existe obtener_detalle_presupuesto), así que ningún flujo legítimo del
    // web manda esto. Además presupuesto_detalle.presupuesto_id de los
    // presupuestos del desktop trae ids crudos de SQLite (no sirven aquí).
    if d.presupuesto_origen_id.is_some() {
        return Err(ApiError::BadRequest(
            "Convertir presupuestos en venta solo está disponible en el POS de escritorio".into(),
        ));
    }

    // Cajero = el de la sesión, no el del body.
    let sesion = usuario_de_sesion(state, claims).await?;
    avisar_usuario_distinto("crear_venta", d.usuario_id, &sesion);

    let max_vendedor = max_descuento_vendedor(state).await?;
    let caja_venta = caja::caja_de(&state.pool, claims).await?;

    // Sucursal: por ahora, web POS usa sucursal 1 (principal). Cuando haya
    // multi-sucursal per-user, se toma del claims del JWT.
    let sucursal_id: i64 = 1;

    let mut tx = state.pool.begin().await?;

    // Caja 'web': su candado va PRIMERO (antes que los de producto y el del
    // folio) y la fecha sale de `ahora_caja`: ningún corte de esa caja puede
    // quedar entre la fecha de la venta y su COMMIT. Caja de la tienda: la
    // fecha es el reloj real al tomar el folio (si llega tarde al
    // escritorio, él la re-fecha al siguiente corte).
    let fecha_caja_web = if caja_venta == Caja::Web {
        caja::bloquear(&mut tx, Caja::Web).await?;
        Some(caja::ahora_caja(&mut tx, Caja::Web).await?)
    } else {
        None
    };

    // Cliente (si hay): debe existir (no eliminado); su descuento configurado
    // por el dueño se acepta sin PIN (así funciona el carrito: al elegir el
    // cliente se aplica su % a todas las partidas — ventaStore.seleccionarCliente).
    // No se exige `activo`: el selector del POS lista también clientes
    // inactivos y el escritorio les vende igual.
    let pct_cliente: f64 = match d.cliente_id {
        Some(cid) => {
            let v: Option<rust_decimal::Decimal> = sqlx::query_scalar(
                "SELECT descuento_porcentaje FROM clientes \
                 WHERE id = $1 AND deleted_at IS NULL",
            )
            .bind(cid)
            .fetch_optional(&mut *tx)
            .await?;
            dec_f64(v.ok_or_else(|| {
                ApiError::BadRequest("El cliente de la venta no existe".into())
            })?)
        }
        None => 0.0,
    };
    let limite_normal = max_vendedor.max(pct_cliente);
    let pct_libre = if sesion.es_admin { 100.0 } else { limite_normal };

    // Productos: lock FOR UPDATE en orden de id (evita deadlocks entre dos
    // ventas concurrentes con los mismos productos en distinto orden) y de
    // paso leemos el precio vigente.
    //
    // NO se valida que haya stock suficiente: el POS permite vender sin
    // existencias a propósito (el cajero tiene la pieza en mostrador aunque el
    // sistema diga 0), igual que el desktop — ver `crear_venta` en
    // src-tauri/src/commands/ventas.rs. Si aquí se rechazara, un producto
    // sobrevendido en caja quedaría invendible desde la web para siempre.
    let mut ids: Vec<i64> = d.items.iter().map(|i| i.producto_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let prod_rows = sqlx::query(
        "SELECT id, nombre, precio_venta FROM productos \
         WHERE id = ANY($1) AND deleted_at IS NULL \
         ORDER BY id FOR UPDATE",
    )
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await?;
    let productos: HashMap<i64, (String, f64)> = prod_rows
        .iter()
        .map(|r| {
            let precio = r
                .try_get::<rust_decimal::Decimal, _>("precio_venta")
                .map(dec_f64)
                .unwrap_or(0.0);
            (r.get::<i64, _>("id"), (r.get::<String, _>("nombre"), precio))
        })
        .collect();

    // Reglas por partida + autorizaciones firmadas.
    let mut lineas = Vec::with_capacity(d.items.len());
    let mut reglas = Vec::with_capacity(d.items.len());
    let mut duenos: Vec<Option<i64>> = Vec::with_capacity(d.items.len());
    for it in &d.items {
        let (nombre, precio_vigente) = productos
            .get(&it.producto_id)
            .cloned()
            .ok_or_else(|| ApiError::BadRequest(format!("Producto {} no existe", it.producto_id)))?;

        let aut = it
            .autorizacion_token
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .and_then(|t| {
                crate::autorizacion::verificar(
                    &state.jwt_secret, t, "descuento", sesion.id, Some(it.producto_id),
                )
            });

        reglas.push(ReglaLinea {
            nombre,
            precios_validos: vec![precio_vigente],
            pct_max_sin_autorizacion: pct_libre,
            pct_autorizado: aut.as_ref().and_then(|c| c.max_pct),
        });
        duenos.push(aut.map(|c| c.dueno));
        lineas.push(LineaCliente {
            cantidad: it.cantidad,
            precio_original: it.precio_original,
            descuento_porcentaje: it.descuento_porcentaje,
            descuento_monto: it.descuento_monto,
            precio_final: it.precio_final,
            subtotal: it.subtotal,
        });
    }

    let calc = venta_calc::calcular(
        &lineas,
        &reglas,
        &TotalesCliente {
            subtotal: d.subtotal,
            descuento: d.descuento,
            total: d.total,
            monto_recibido: d.monto_recibido,
            cambio: d.cambio,
        },
        metodo,
    )
    .map_err(|e| ApiError::BadRequest(e.mensaje()))?;

    // autorizado_por de cada partida:
    //   - descuento aceptado por token → el dueño del token (si sigue activo
    //     y sigue siendo admin al momento de cobrar);
    //   - cajero admin con descuento arriba del límite normal → él mismo;
    //   - en otro caso → NULL.
    let mut autorizados: Vec<Option<i64>> = Vec::with_capacity(calc.lineas.len());
    for (i, l) in calc.lineas.iter().enumerate() {
        let quien = if l.uso_autorizacion {
            let dueno = duenos[i].expect("uso_autorizacion implica token válido");
            let sigue_admin: Option<i32> = sqlx::query_scalar(
                "SELECT r.es_admin FROM usuarios u JOIN roles r ON r.id = u.rol_id \
                 WHERE u.id = $1 AND u.activo = 1 AND u.deleted_at IS NULL",
            )
            .bind(dueno)
            .fetch_optional(&mut *tx)
            .await?;
            if sigue_admin != Some(1) {
                return Err(ApiError::BadRequest(
                    "DESCUENTO_NO_AUTORIZADO: quien autorizó el descuento ya no es dueño activo".into(),
                ));
            }
            Some(dueno)
        } else if sesion.es_admin && l.descuento_porcentaje > limite_normal + 1e-9 {
            Some(sesion.id)
        } else {
            None
        };
        autorizados.push(quien);
    }

    // Generar folio consecutivo: V-YYYYMMDD-NNNN (y la fecha de la venta).
    let (folio, fecha_venta) = siguiente_folio_web(
        &mut tx, "ventas", 'V', LOCK_FOLIO_VENTAS, fecha_caja_web.as_deref(),
    )
    .await?;

    let venta_uuid = uuid::Uuid::now_v7().to_string();

    // Insertar venta con los importes RECALCULADOS. `origen='web'` = la creó
    // el POS web; `caja` = el cajón donde entró el dinero (espejo → la tienda,
    // cuenta en el corte del escritorio; individual → la caja web).
    let insert_sql = format!(
        r#"
        INSERT INTO ventas
          (uuid, sucursal_id, folio, usuario_id, cliente_id,
           subtotal, descuento, total, metodo_pago,
           monto_recibido, cambio, anulada, origen, caja, fecha, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 0, 'web', $12, $13, {NOW_TEXT})
        RETURNING id, fecha
        "#
    );
    let vrow = sqlx::query(&insert_sql)
        .bind(&venta_uuid)
        .bind(sucursal_id)
        .bind(&folio)
        .bind(sesion.id)
        .bind(d.cliente_id)
        .bind(calc.subtotal)
        .bind(calc.descuento)
        .bind(calc.total)
        .bind(metodo.as_str())
        .bind(calc.monto_recibido)
        .bind(calc.cambio)
        .bind(caja_venta.as_str())
        .bind(&fecha_venta)
        .fetch_one(&mut *tx)
        .await?;
    let venta_id: i64 = vrow.get("id");
    let fecha: String = vrow.get("fecha");

    // Insertar detalle + descontar stock
    for ((it, l), autorizado_por) in d.items.iter().zip(&calc.lineas).zip(&autorizados) {
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
            .bind(l.cantidad)
            .bind(l.precio_original)
            .bind(l.descuento_porcentaje)
            .bind(l.descuento_monto)
            .bind(l.precio_final)
            .bind(l.subtotal)
            .bind(*autorizado_por)
            .execute(&mut *tx)
            .await?;

        // GREATEST(0, ...) replica el MAX(0, ...) del desktop: el stock se frena
        // en cero en lugar de quedar negativo. Sin este clamp los dos backends
        // dejarían valores distintos para la misma sobreventa y el sync LWW de
        // `productos` (fila completa) los haría pelearse.
        let upd_sql = format!(
            "UPDATE productos SET stock_actual = GREATEST(0, stock_actual - $1), \
             updated_at = {NOW_TEXT} WHERE id = $2"
        );
        sqlx::query(&upd_sql)
            .bind(l.cantidad)
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

    // Sin fila 'VENTA' en audit_log por ahora: audit_log baja al escritorio
    // con usuario_id/registro_id crudos (ids del servidor) y la bitácora de la
    // caja mostraría otro usuario y otra venta. Se agrega cuando el sync
    // traduzca esas columnas por uuid.

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
        "total":  calc.total,
        "cambio": calc.cambio,
        "fecha":  fecha,
    }))
}

// =============================================================================
// ESTADÍSTICAS DÍA
// =============================================================================

async fn obtener_estadisticas_dia(
    state: &AppState,
    _args: &Value,
) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;
    // Misma shape que el comando Tauri `obtener_estadisticas_dia`:
    //   { total_ventas, num_transacciones, efectivo, tarjeta, transferencia,
    //     producto_top_nombre, producto_top_cantidad }
    // El frontend hace `stats.total_ventas.toFixed(2)` etc., así que
    // CUALQUIER campo faltante revienta el render del Dashboard.
    //
    // Solo lectura: cuenta todas las ventas del día (escritorio y web, de
    // cualquier caja), igual que el escritorio.
    let dec = |row: &sqlx::postgres::PgRow, name: &str| -> f64 {
        row.try_get::<rust_decimal::Decimal, _>(name).ok()
            .and_then(|d| d.to_f64()).unwrap_or(0.0)
    };

    let row = sqlx::query(
        &format!(r#"
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
        "#),
    )
    .fetch_one(&state.pool)
    .await?;

    let num: i64 = row.try_get("num_transacciones").unwrap_or(0);

    // Producto top del día
    let top = sqlx::query(
        &format!(r#"
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
        "#),
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
    #[serde(rename_all = "camelCase")]
    struct A {
        pin: String,
        #[serde(default)] device_uuid: Option<String>,
    }
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
            return usuario_sesion_response(&state, r, a.device_uuid.as_deref()).await;
        }
    }
    Ok(json!({ "ok": false, "error": "PIN incorrecto" }))
}

async fn login_password(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A {
        #[serde(rename = "nombreUsuario")] nombre_usuario: String,
        password: String,
        #[serde(default, rename = "deviceUuid")] device_uuid: Option<String>,
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
    usuario_sesion_response(&state, &r, a.device_uuid.as_deref()).await
}

/// Asegura que exista una fila en `pos_devices` para este `device_uuid`.
/// Devuelve `(modo_caja, configurado)` donde `configurado=true` significa
/// que el device ya había sido visto antes (no es la primera vez que entra).
///
/// La primera vez se inserta con `modo_caja='espejo'` (la web se usa como
/// respaldo del mismo cajón de la tienda) y `configurado=false`, lo que
/// dispara el modal de bienvenida en el frontend. Los equipos que ya
/// existían conservan su modo.
async fn upsert_pos_device(
    state: &AppState,
    device_uuid: &str,
) -> Result<(String, bool), ApiError> {
    let existente = sqlx::query(
        "SELECT modo_caja FROM pos_devices WHERE device_uuid = $1",
    )
    .bind(device_uuid)
    .fetch_optional(&state.pool)
    .await?;

    if let Some(row) = existente {
        let modo: String = row.get("modo_caja");
        return Ok((modo, true));
    }

    sqlx::query(
        "INSERT INTO pos_devices (device_uuid, sucursal_id, nombre, modo_caja) \
         VALUES ($1, 1, $2, 'espejo') \
         ON CONFLICT (device_uuid) DO NOTHING",
    )
    .bind(device_uuid)
    .bind(format!("Web {}", prefijo_dispositivo(device_uuid)))
    .execute(&state.pool)
    .await?;

    // Si otra pestaña lo creó al mismo tiempo, vale lo que quedó guardado.
    let modo: Option<String> =
        sqlx::query_scalar("SELECT modo_caja FROM pos_devices WHERE device_uuid = $1")
            .bind(device_uuid)
            .fetch_optional(&state.pool)
            .await?;
    Ok((modo.unwrap_or_else(|| "espejo".to_string()), false))
}

/// Primeros 8 caracteres del device_uuid (sin partir un carácter UTF-8).
fn prefijo_dispositivo(uuid: &str) -> String {
    uuid.chars().take(8).collect()
}

/// ¿Algún POS de escritorio con sync v2 (que sí aplica las filas web de la
/// caja de la tienda) se reportó en los últimos 2 días? Informativo: si no,
/// la web avisa que sus ventas no se verán en el corte del escritorio
/// hasta que lo actualicen.
async fn escritorio_recibe_web(state: &AppState) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM pos_devices \
                         WHERE protocolo >= 2 AND last_push_at > now() - interval '2 days')",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!("escritorio_recibe_web: {e}");
        false
    })
}

/// Datos de caja que acompañan al modo del dispositivo (login y
/// obtener_modo_caja): con qué caja trabaja y si en este equipo se hacen
/// cortes y aperturas.
async fn info_caja_de_modo(state: &AppState, modo: &str) -> Value {
    let caja = if modo == "individual" { Caja::Web } else { Caja::Principal };
    json!({
        "caja": caja.as_str(),
        "cortes_en_este_equipo": caja == Caja::Web,
        "escritorio_recibe_web": escritorio_recibe_web(state).await,
    })
}

/// Construye la respuesta `{ ok: true, usuario, token }` a partir de un row de usuarios.
/// Si `device_uuid` viene del frontend, lo registra en `pos_devices` y lo
/// embebe en los claims del JWT para que los handlers siguientes sepan
/// qué modo de caja respetar.
///
/// El claim `device` de un token de sesión web NUNCA va vacío: sin
/// dispositivo lleva el centinela `caja::DISPOSITIVO_SIN_EQUIPO`. /sync/*
/// solo acepta tokens sin dispositivo (los de /auth/login del escritorio),
/// así que un PIN de dueño ya no abre el sync (push de filas crudas, mapa de
/// ids, reparación).
async fn usuario_sesion_response(
    state: &AppState,
    r: &sqlx::postgres::PgRow,
    device_uuid: Option<&str>,
) -> Result<Value, ApiError> {
    let id: i64               = r.get("id");
    let nombre_completo: String = r.get("nombre_completo");
    let nombre_usuario: String  = r.get("nombre_usuario");
    let rol_id: i64           = r.get("rol_id");

    // Resolver rol + permisos desde la tabla `roles`/`permisos` (migración 007).
    // Antes esto era hardcoded `id <= 2`; ahora respeta lo que esté en BD.
    let es_admin = rol_es_admin(state, rol_id).await;
    let rol_nombre = rol_nombre_por_id(state, rol_id).await;
    let permisos = permisos_de_rol(state, rol_id).await;

    // Dispositivo real que mandó el frontend (vacío o el centinela = ninguno).
    let dispositivo = device_uuid.filter(|u| caja::es_dispositivo_real(u));

    // Si vino device_uuid, asegurar fila en pos_devices y obtener su modo.
    // Sin dispositivo: caja de la tienda (espejo), igual que `caja_de`.
    let (modo_caja, modo_configurado) = match dispositivo {
        Some(uuid) => upsert_pos_device(state, uuid).await?,
        None => ("espejo".to_string(), false),
    };
    let info = info_caja_de_modo(state, &modo_caja).await;

    // JWT que el frontend usa para todas las RPC siguientes.
    let token = emitir_token_con_device(
        &state.jwt_secret,
        id,
        &nombre_usuario,
        if es_admin { "admin" } else { "device" },
        1,
        Some(dispositivo.unwrap_or(caja::DISPOSITIVO_SIN_EQUIPO).to_string()),
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
            "rol_nombre":      rol_nombre,
            "es_admin":        es_admin,
            "sesion_id":       chrono::Utc::now().timestamp(), // stateless: timestamp
            "permisos":        permisos,
        },
        // El frontend usa esta info para decidir si abrir el modal de bienvenida
        // y qué chip mostrar en el topbar. Si no hay device_uuid, queda
        // 'espejo' + configurado=true (no se muestra modal).
        "modo_caja": modo_caja,
        "modo_configurado": modo_configurado || dispositivo.is_none(),
        "caja": info["caja"],
        "cortes_en_este_equipo": info["cortes_en_este_equipo"],
        "escritorio_recibe_web": info["escritorio_recibe_web"],
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

/// Autoriza un descuento arriba del límite del vendedor (solo POS web).
///
/// Args (camelCase): `{ pin, productoId, porcentaje }`.
/// Respuesta: `{ ok: true, autorizado_por, token }` o `{ ok: false, error }`.
/// El token (ver `autorizacion.rs`) vale TTL_SEGUNDOS para ESTE cajero,
/// ESTE producto y hasta ESTE porcentaje; `crear_venta` lo exige.
async fn autorizar_descuento(
    state: &AppState,
    claims: &Claims,
    args: &Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct A {
        pin: String,
        producto_id: i64,
        porcentaje: f64,
    }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    if !a.porcentaje.is_finite() || a.porcentaje <= 0.0 || a.porcentaje > 100.0 {
        return Err(ApiError::BadRequest("Porcentaje de descuento inválido".into()));
    }
    let sesion = usuario_de_sesion(state, claims).await?;
    let Some(dueno) = buscar_dueno_por_pin(state, a.pin.trim()).await? else {
        return Ok(json!({ "ok": false, "error": "PIN del dueño incorrecto" }));
    };
    let token = crate::autorizacion::emitir(
        &state.jwt_secret,
        "descuento",
        dueno,
        sesion.id,
        Some(a.producto_id),
        Some(a.porcentaje),
        chrono::Utc::now().timestamp(),
    )?;
    Ok(json!({ "ok": true, "autorizado_por": dueno, "token": token }))
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

async fn crear_recepcion(state: &AppState, claims: &Claims, args: Value) -> Result<Value, ApiError> {
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
    // Cordura de importes: la UI manda cantidad >= 1, costo >= 0 y
    // precio_venta entero > 0 o null (Recepcion.tsx). Una cantidad negativa
    // restaba stock y un costo negativo quedaba como costo del producto.
    use crate::venta_calc::{round2, MAX_CANTIDAD, MAX_IMPORTE};
    for it in &r.items {
        if !it.cantidad.is_finite() || it.cantidad <= 0.0 || it.cantidad > MAX_CANTIDAD {
            return Err(ApiError::BadRequest(format!(
                "Cantidad inválida para el producto {}", it.producto_id)));
        }
        if !it.precio_costo.is_finite() || it.precio_costo < 0.0 || it.precio_costo > MAX_IMPORTE {
            return Err(ApiError::BadRequest(format!(
                "Costo inválido para el producto {}", it.producto_id)));
        }
        if let Some(pv) = it.precio_venta {
            if !pv.is_finite() || pv < 0.0 || pv > MAX_IMPORTE {
                return Err(ApiError::BadRequest(format!(
                    "Precio de venta inválido para el producto {}", it.producto_id)));
            }
        }
    }
    let sesion = usuario_de_sesion(state, claims).await?;
    avisar_usuario_distinto("crear_recepcion", r.usuario_id, &sesion);

    let sucursal_id: i64 = 1;
    let mut tx = state.pool.begin().await?;

    // Todos los productos deben existir (antes un id inexistente dejaba una
    // partida huérfana y el UPDATE de stock no hacía nada).
    let mut ids: Vec<i64> = r.items.iter().map(|i| i.producto_id).collect();
    ids.sort_unstable();
    ids.dedup();
    // De paso se lee nombre y precio de venta vigentes para dejar en la
    // bitácora los cambios de precio que haga esta recepción.
    let prod_rows = sqlx::query(
        "SELECT id, nombre, precio_venta FROM productos \
         WHERE id = ANY($1) AND deleted_at IS NULL ORDER BY id FOR UPDATE",
    )
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await?;
    let mut precios: std::collections::HashMap<i64, (String, f64)> = prod_rows
        .iter()
        .map(|row| {
            let pv = row
                .try_get::<rust_decimal::Decimal, _>("precio_venta")
                .map(dec_f64)
                .unwrap_or(0.0);
            (row.get::<i64, _>("id"), (row.get::<String, _>("nombre"), pv))
        })
        .collect();
    if let Some(falta) = ids.iter().find(|id| !precios.contains_key(id)) {
        return Err(ApiError::BadRequest(format!("Producto {} no existe", falta)));
    }

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
        .bind(sesion.id)
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
            .bind(round2(it.precio_costo))
            .execute(&mut *tx)
            .await?;

        // Sumar al stock + actualizar precio_costo (y precio_venta si vino > 0).
        // Si precio_venta viene None / 0, dejamos el existente intacto.
        let nuevo_pv = it.precio_venta.filter(|v| *v > 0.0);
        if let Some(pv) = nuevo_pv {
            let pv = round2(pv);
            let upd_sql = format!(
                "UPDATE productos SET stock_actual = stock_actual + $1, \
                 precio_costo = $2, precio_venta = $3, updated_at = {NOW_TEXT} WHERE id = $4"
            );
            sqlx::query(&upd_sql)
                .bind(it.cantidad)
                .bind(round2(it.precio_costo))
                .bind(pv)
                .bind(it.producto_id)
                .execute(&mut *tx)
                .await?;

            // Cambio de precio de venta → bitácora (historial_precios_producto
            // la lee). Antes la recepción web cambiaba precios sin rastro.
            if let Some((nombre, pv_ant)) = precios.get_mut(&it.producto_id) {
                if (pv - *pv_ant).abs() > crate::venta_calc::EPS {
                    let pa_sql = format!(
                        r#"INSERT INTO audit_log
                           (usuario_id, accion, tabla_afectada, registro_id,
                            datos_anteriores, datos_nuevos, descripcion_legible, origen, fecha)
                           VALUES ($1, 'PRECIO_ACTUALIZADO', 'productos', $2, $3, $4, $5, 'WEB', {NOW_TEXT})"#
                    );
                    sqlx::query(&pa_sql)
                        .bind(sesion.id)
                        .bind(it.producto_id)
                        .bind(format!("{{\"precio_venta\":{:.2}}}", *pv_ant))
                        .bind(format!("{{\"precio_venta\":{:.2}}}", pv))
                        .bind(format!(
                            "Precio de '{}' cambió de ${:.2} a ${:.2} (recepción)",
                            nombre, *pv_ant, pv
                        ))
                        .execute(&mut *tx)
                        .await?;
                    *pv_ant = pv;
                }
            }
        } else {
            let upd_sql = format!(
                "UPDATE productos SET stock_actual = stock_actual + $1, \
                 precio_costo = $2, updated_at = {NOW_TEXT} WHERE id = $3"
            );
            sqlx::query(&upd_sql)
                .bind(it.cantidad)
                .bind(round2(it.precio_costo))
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
            sqlx::query(&upd_pd_sql)
                .bind(it.cantidad)
                .bind(oid)
                .bind(it.producto_id)
                .execute(&mut *tx)
                .await?;
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
        .await?;
        let f64_falt = faltante.to_f64().unwrap_or(0.0);
        let nuevo_estado = if f64_falt <= 0.0 { "recibida_completa" } else { "recibida_parcial" };
        let upd_orden_sql = format!(
            "UPDATE ordenes_pedido SET estado = $1, fecha_recepcion = {NOW_TEXT}, \
             updated_at = {NOW_TEXT} WHERE id = $2"
        );
        sqlx::query(&upd_orden_sql)
            .bind(nuevo_estado)
            .bind(oid)
            .execute(&mut *tx)
            .await?;

        // La orden (con sus partidas, que viajan como hijos) cambió: avisar
        // al escritorio. Antes no se escribía cursor y el cambio no bajaba.
        sqlx::query(
            "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
             SELECT 'ordenes_pedido', uuid, $1, $2 FROM ordenes_pedido WHERE id = $3",
        )
        .bind(sucursal_id)
        .bind(WEB_ORIGIN)
        .bind(oid)
        .execute(&mut *tx)
        .await?;
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
    .bind(sesion.id)
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

/// Devolución recibida del frontend (`{ datos: {...} }`).
#[derive(Deserialize)]
struct NuevaDevolucionWeb {
    venta_id: i64,
    /// Se IGNORA: quien registra es el usuario de la sesión.
    #[serde(default)]
    usuario_id: Option<i64>,
    /// Se IGNORA: el servidor no confía en el id que mande el navegador.
    #[serde(default)]
    #[allow(dead_code)]
    autorizado_por: Option<i64>,
    /// PIN del dueño (cajero no admin). Se verifica aquí. El escritorio no lo
    /// conoce y lo ignora.
    #[serde(default)]
    pin_autorizacion: Option<String>,
    /// true = se regresa en efectivo de la caja (RETIRO); false = por el
    /// mismo medio (no sale de caja). Default igual que el escritorio:
    /// efectivo solo si la venta fue en efectivo.
    #[serde(default)]
    reembolso_efectivo: Option<bool>,
    motivo: String,
    items: Vec<ItemDevolucionWeb>,
}

#[derive(Deserialize)]
struct ItemDevolucionWeb {
    venta_detalle_id: i64,
    cantidad: f64,
}

/// Mensaje cuando una venta del escritorio todavía tiene FKs crudas aquí.
const MSG_VENTA_SIN_SINCRONIZAR: &str =
    "Esta venta todavía no está sincronizada correctamente; regístrala en el POS de escritorio";

/// ¿Las FKs de esta fila (y sus hijos) ya están en ids del servidor? Las filas
/// hechas en la web siempre; las del escritorio solo si el sync v2 las tradujo
/// o la reparación las corrigió (sync_fk_estado).
async fn fks_en_ids_del_servidor(
    conn: &mut sqlx::PgConnection,
    tabla: &str,
    uuid: &str,
) -> Result<bool, ApiError> {
    if !crate::sync_fk_map::es_uuid_escritorio(uuid) {
        return Ok(true);
    }
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM sync_fk_estado \
                         WHERE tabla = $1 AND uuid = $2 AND estado IN ('traducido', 'reparado'))",
    )
    .bind(tabla)
    .bind(uuid)
    .fetch_one(&mut *conn)
    .await?)
}

async fn crear_devolucion(
    state: &AppState,
    claims: &Claims,
    args: Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A {
        // El cliente desktop manda `{ datos: {...} }` (param de Tauri).
        datos: NuevaDevolucionWeb,
    }
    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let d = a.datos;

    let sesion = usuario_de_sesion(state, claims).await?;
    if let Some(uid) = d.usuario_id {
        avisar_usuario_distinto("crear_devolucion", uid, &sesion);
    }

    // Cajero no admin → PIN de un dueño, verificado AQUÍ (antes bastaba con
    // que `autorizado_por` viniera no nulo, con cualquier id).
    let autorizado_por: Option<i64> = if sesion.es_admin {
        None
    } else {
        let pin = d.pin_autorizacion.as_deref().unwrap_or("").trim();
        if pin.is_empty() {
            return Err(ApiError::BadRequest(
                "Se requiere autorización del dueño para registrar devoluciones".into(),
            ));
        }
        match buscar_dueno_por_pin(state, pin).await? {
            Some(id) => Some(id),
            None => return Err(ApiError::BadRequest("PIN del dueño incorrecto".into())),
        }
    };

    let caja_equipo = caja::caja_de(&state.pool, claims).await?;
    let mut tx = state.pool.begin().await?;
    // Caja 'web': candado primero y fecha de la caja. La de la tienda usa el
    // reloj real al tomar el folio.
    let ahora = if caja_equipo == Caja::Web {
        caja::bloquear(&mut tx, Caja::Web).await?;
        Some(caja::ahora_caja(&mut tx, Caja::Web).await?)
    } else {
        None
    };
    let creada = crear_devolucion_tx(
        &mut tx, caja_equipo, sesion.id, autorizado_por, &d, ahora.as_deref(),
    )
    .await?;
    tx.commit().await?;
    Ok(creada)
}

/// Núcleo de la devolución (puerto de `crear_devolucion_core` del escritorio),
/// dentro de la transacción del llamador. Con caja 'web' el llamador ya tomó
/// el candado y pasa `ahora`; con la caja de la tienda `ahora` es None y la
/// fecha sale del reloj real al tomar el folio.
///
/// Reglas:
///   - partidas repetidas se agrupan antes de validar;
///   - lo ya devuelto solo cuenta por pares (venta, partida) consistentes
///     (las filas del escritorio con ids crudos no se cuelan);
///   - si con esta devolución la venta queda completa se regresa exactamente
///     lo cobrado (el cobro se redondeó al peso);
///   - el RETIRO solo existe si el reembolso es en efectivo, y va a la caja
///     del equipo que devuelve (de ahí sale el dinero);
///   - un equipo con caja propia solo devuelve ventas de la caja web;
///   - una venta hecha en el escritorio solo se devuelve aquí cuando sus FKs
///     ya están en ids del servidor (si no, se devolvería stock a otro
///     producto).
async fn crear_devolucion_tx(
    conn: &mut sqlx::PgConnection,
    caja_equipo: Caja,
    usuario_id: i64,
    autorizado_por: Option<i64>,
    d: &NuevaDevolucionWeb,
    ahora: Option<&str>,
) -> Result<Value, ApiError> {
    if d.motivo.trim().is_empty() {
        return Err(ApiError::BadRequest("El motivo es obligatorio".into()));
    }
    if d.items.is_empty() {
        return Err(ApiError::BadRequest("Debe incluir al menos un producto a devolver".into()));
    }
    // Agrupar por partida: si la misma partida venía dos veces, cada renglón
    // se validaba por separado contra lo ya devuelto y se podía devolver —y
    // retirar de caja— más de lo vendido.
    let mut por_partida: Vec<(i64, f64)> = Vec::new();
    for it in &d.items {
        if !it.cantidad.is_finite() || it.cantidad <= 0.0
            || it.cantidad > crate::venta_calc::MAX_CANTIDAD
        {
            return Err(ApiError::BadRequest("Las cantidades deben ser mayores a 0".into()));
        }
        match por_partida.iter_mut().find(|(id, _)| *id == it.venta_detalle_id) {
            Some((_, c)) => *c += it.cantidad,
            None => por_partida.push((it.venta_detalle_id, it.cantidad)),
        }
    }

    // Venta no anulada (bloqueada: dos devoluciones simultáneas de la misma
    // venta no pueden pasar ambas la validación de cantidades).
    let venta_row = sqlx::query(
        "SELECT folio, uuid, caja, metodo_pago, total::float8 AS total FROM ventas \
         WHERE id = $1 AND anulada = 0 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(d.venta_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| ApiError::BadRequest("Venta no encontrada o está anulada".into()))?;
    let venta_folio: String = venta_row.get("folio");
    let venta_uuid: String = venta_row.get("uuid");
    let caja_venta: String = venta_row.get("caja");
    let metodo_venta: String = venta_row.get("metodo_pago");
    let total_venta: f64 = venta_row.get("total");

    if caja_equipo == Caja::Web && caja_venta != Caja::Web.as_str() {
        return Err(ApiError::BadRequest(
            "Este equipo tiene caja propia: solo devuelve ventas de la caja web. Esta venta es de \
             la caja de la tienda; regístrala en el POS de escritorio o en un equipo en modo espejo."
                .into(),
        ));
    }
    if !fks_en_ids_del_servidor(&mut *conn, "ventas", &venta_uuid).await? {
        return Err(ApiError::BadRequest(MSG_VENTA_SIN_SINCRONIZAR.into()));
    }
    let reembolso_efectivo = d.reembolso_efectivo.unwrap_or(metodo_venta == "efectivo");

    // Validar cantidades contra cada venta_detalle.
    let mut total_devuelto: f64 = 0.0;
    // (venta_detalle_id, producto_id, cantidad, precio_unitario, subtotal)
    let mut items_validados: Vec<(i64, i64, f64, f64, f64)> = Vec::new();
    for &(venta_detalle_id, cantidad) in &por_partida {
        let row = sqlx::query(
            "SELECT venta_id, producto_id, cantidad::float8 AS cantidad, \
                    precio_final::float8 AS precio_final \
             FROM venta_detalle \
             WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(venta_detalle_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| ApiError::BadRequest(
            format!("Partida de venta no encontrada: id={}", venta_detalle_id)
        ))?;

        let vd_venta_id: i64 = row.get("venta_id");
        let prod_id: i64 = row.get("producto_id");
        let cantidad_orig: f64 = row.get("cantidad");
        let precio_final: f64 = row.get("precio_final");

        if vd_venta_id != d.venta_id {
            return Err(ApiError::BadRequest("Una de las partidas no pertenece a la venta".into()));
        }

        // Solo cuentan devoluciones cuya venta y partida coinciden: filas del
        // escritorio con ids crudos pueden "apuntar" a esta partida siendo de
        // otra venta (p. ej. D-000309 contra la única partida de una venta web).
        let ya_devuelto: f64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(dd.cantidad), 0)::float8 \
             FROM devolucion_detalle dd \
             JOIN devoluciones dv ON dv.id = dd.devolucion_id \
             WHERE dd.venta_detalle_id = $1 AND dv.venta_id = $2 \
               AND dd.deleted_at IS NULL AND dv.deleted_at IS NULL",
        )
        .bind(venta_detalle_id)
        .bind(d.venta_id)
        .fetch_one(&mut *conn)
        .await?;

        let disponible = cantidad_orig - ya_devuelto;
        if cantidad > disponible + 0.0001 {
            return Err(ApiError::BadRequest(format!(
                "Cantidad excede lo disponible (vendido {}, ya devuelto {}, queda {})",
                cantidad_orig, ya_devuelto, disponible
            )));
        }

        let subtotal = cantidad * precio_final;
        total_devuelto += subtotal;
        items_validados.push((venta_detalle_id, prod_id, cantidad, precio_final, subtotal));
    }

    // Devolución que completa la venta: regresar exactamente lo cobrado (el
    // cobro se redondeó al peso hacia arriba; si no, quedan centavos de
    // sobrante en la caja).
    let restantes = sqlx::query(
        "SELECT vd.id, (vd.cantidad - COALESCE((SELECT SUM(dd.cantidad) FROM devolucion_detalle dd \
                         JOIN devoluciones dv ON dv.id = dd.devolucion_id \
                         WHERE dd.venta_detalle_id = vd.id AND dv.venta_id = vd.venta_id \
                           AND dd.deleted_at IS NULL AND dv.deleted_at IS NULL), 0))::float8 AS restante \
         FROM venta_detalle vd WHERE vd.venta_id = $1 AND vd.deleted_at IS NULL",
    )
    .bind(d.venta_id)
    .fetch_all(&mut *conn)
    .await?;
    let pendiente_tras_esta: f64 = restantes
        .iter()
        .map(|r| {
            let id: i64 = r.get("id");
            let restante: f64 = r.get("restante");
            let ahora = por_partida.iter().find(|(p, _)| *p == id).map(|(_, c)| *c).unwrap_or(0.0);
            (restante - ahora).max(0.0)
        })
        .sum();
    if pendiente_tras_esta < 0.0001 {
        let devuelto_antes: f64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(dv.total_devuelto), 0)::float8 FROM devoluciones dv \
             WHERE dv.venta_id = $1 AND dv.deleted_at IS NULL \
               AND EXISTS (SELECT 1 FROM devolucion_detalle dd \
                           JOIN venta_detalle vd ON vd.id = dd.venta_detalle_id \
                           WHERE dd.devolucion_id = dv.id AND vd.venta_id = dv.venta_id)",
        )
        .bind(d.venta_id)
        .fetch_one(&mut *conn)
        .await?;
        total_devuelto = (total_venta - devuelto_antes).max(0.0);
    }
    let total_devuelto = caja::round2(total_devuelto);
    // Stock en orden de producto (como crear_venta): dos operaciones que
    // tocan los mismos productos no se bloquean en orden cruzado.
    items_validados.sort_by_key(|it| it.1);

    // Folio D-YYYYMMDD-NNNN (los del escritorio son D-NNNNNN, no chocan) y
    // fecha de la devolución.
    let (folio, fecha) =
        siguiente_folio_web(&mut *conn, "devoluciones", 'D', LOCK_FOLIO_DEVOLUCIONES, ahora).await?;

    let dev_uuid = uuid::Uuid::now_v7().to_string();
    let devolucion_id: i64 = sqlx::query_scalar(&format!(
        "INSERT INTO devoluciones \
           (uuid, sucursal_id, folio, venta_id, usuario_id, autorizado_por, \
            motivo, total_devuelto, fecha, updated_at) \
         VALUES ($1, 1, $2, $3, $4, $5, $6, $7, $8, {NOW_TEXT}) \
         RETURNING id"
    ))
    .bind(&dev_uuid)
    .bind(&folio)
    .bind(d.venta_id)
    .bind(usuario_id)
    .bind(autorizado_por)
    .bind(&d.motivo)
    .bind(total_devuelto)
    .bind(&fecha)
    .fetch_one(&mut *conn)
    .await?;

    // Detalle + stock. Se suma la cantidad devuelta completa: la pieza
    // regresa al anaquel aunque la venta haya frenado el stock en 0.
    for (vd_id, prod_id, cantidad, precio, subtotal) in &items_validados {
        sqlx::query(&format!(
            "INSERT INTO devolucion_detalle \
               (uuid, devolucion_id, venta_detalle_id, producto_id, \
                cantidad, precio_unitario, subtotal, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, {NOW_TEXT})"
        ))
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(devolucion_id)
        .bind(vd_id)
        .bind(prod_id)
        .bind(cantidad)
        .bind(precio)
        .bind(caja::round2(*subtotal))
        .execute(&mut *conn)
        .await?;

        sqlx::query(&format!(
            "UPDATE productos SET stock_actual = stock_actual + $1, \
             updated_at = {NOW_TEXT} WHERE id = $2"
        ))
        .bind(cantidad)
        .bind(prod_id)
        .execute(&mut *conn)
        .await?;

        sqlx::query(
            "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
             SELECT 'productos', p.uuid, 1, $1 FROM productos p WHERE p.id = $2",
        )
        .bind(WEB_ORIGIN)
        .bind(*prod_id)
        .execute(&mut *conn)
        .await?;
    }

    // El reembolso solo mueve la caja si se paga en efectivo, y sale de la
    // caja del equipo que lo entrega.
    if reembolso_efectivo && total_devuelto > 0.0 {
        let concepto = format!("Devolución {} de venta {} — {}", folio, venta_folio, d.motivo);
        let (mov_id, _) = caja::insertar_movimiento(
            &mut *conn, caja_equipo, "RETIRO", usuario_id, total_devuelto, &concepto,
            autorizado_por, None, &fecha,
        )
        .await?;
        sqlx::query("UPDATE devoluciones SET movimiento_caja_id = $1 WHERE id = $2")
            .bind(mov_id)
            .bind(devolucion_id)
            .execute(&mut *conn)
            .await?;
    }
    caja::registrar_cursor(&mut *conn, "devoluciones", &dev_uuid).await?;

    let medio = if reembolso_efectivo { "efectivo de caja" } else { "mismo medio de pago (no sale de caja)" };
    caja::bitacora(
        &mut *conn,
        usuario_id,
        "DEVOLUCION",
        "devoluciones",
        Some(devolucion_id),
        &format!(
            "Devolución {} de venta {} — ${:.2} reembolsado en {} — {}",
            folio, venta_folio, total_devuelto, medio, d.motivo
        ),
        &fecha,
    )
    .await?;

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

    // UPDATE + sync_cursor en una transacción: si el cursor fallara, el
    // cambio de estado nunca bajaría al escritorio.
    let mut tx = state.pool.begin().await?;
    let upd_sql = format!(
        "UPDATE ordenes_pedido SET estado = $1, updated_at = {NOW_TEXT} \
         WHERE id = $2 AND deleted_at IS NULL"
    );
    let affected = sqlx::query(&upd_sql)
        .bind(&a.nuevo_estado)
        .bind(a.orden_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if affected == 0 {
        return Err(ApiError::NotFound);
    }

    // Propagar al sync_cursor
    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         SELECT 'ordenes_pedido', uuid, 1, $1 FROM ordenes_pedido WHERE id = $2",
    )
    .bind(WEB_ORIGIN)
    .bind(a.orden_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(json!(true))
}

// =============================================================================
// CAJA — aperturas, movimientos y cortes (motor en caja.rs)
// =============================================================================
//
// Envolturas delgadas sobre caja.rs, con la misma forma de respuesta que los
// comandos del escritorio. La caja la decide el modo del dispositivo
// (`caja::caja_de`):
//   - modo individual → caja 'web': cadena propia que calcula y escribe este
//     servidor (candado por caja + reloj que nunca va hacia atrás);
//   - espejo o token sin dispositivo → caja de la tienda ('principal'): aquí
//     solo se LEE (vista previa, historial, auditoría "según lo
//     sincronizado"); sus cortes y aperturas los hace el escritorio. Las
//     ventas, entradas/retiros y devoluciones de estos equipos sí entran a
//     esa caja y bajan al escritorio, que las cuenta en su corte.
// Quien registra es siempre el usuario de la sesión; el usuario_id del body
// se ignora.

/// Helper local: f64 desde columna NUMERIC.
fn pg_dec(row: &sqlx::postgres::PgRow, name: &str) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    row.try_get::<rust_decimal::Decimal, _>(name).ok()
        .and_then(|d| d.to_f64()).unwrap_or(0.0)
}

/// Rechaza escrituras a la cadena de la caja de la tienda.
fn exigir_caja_web(caja: Caja) -> Result<(), ApiError> {
    if caja == Caja::Web {
        Ok(())
    } else {
        Err(ApiError::BadRequest(caja::MSG_PRINCIPAL_SOLO_ESCRITORIO.into()))
    }
}

async fn obtener_apertura_hoy(state: &AppState, claims: &Claims) -> Result<Value, ApiError> {
    let caja = caja::caja_de(&state.pool, claims).await?;
    let mut conn = state.pool.acquire().await?;
    let ahora = caja::reloj(&mut conn).await?;
    a_json(caja::apertura_del_dia(&mut conn, caja, &ahora[..10]).await?)
}

/// Apertura del día de la caja 'web': conteo de verificación contra lo
/// esperado; si difiere exige nota y registra un movimiento de ajuste.
async fn crear_apertura_caja(
    state: &AppState,
    claims: &Claims,
    args: &Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A {
        datos: caja::NuevaApertura,
    }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let caja_equipo = caja::caja_de(&state.pool, claims).await?;
    exigir_caja_web(caja_equipo)?;
    let sesion = usuario_de_sesion(state, claims).await?;
    if let Some(uid) = a.datos.usuario_id {
        avisar_usuario_distinto("crear_apertura_caja", uid, &sesion);
    }

    let mut tx = state.pool.begin().await?;
    caja::bloquear(&mut tx, caja_equipo).await?;
    let ahora = caja::ahora_caja(&mut tx, caja_equipo).await?;
    let apertura = caja::crear_apertura_core(&mut tx, caja_equipo, sesion.id, a.datos, &ahora).await?;
    tx.commit().await?;
    a_json(apertura)
}

/// Lo que debería haber en caja al abrir (para el modal de apertura).
async fn obtener_esperado_apertura(state: &AppState, claims: &Claims) -> Result<Value, ApiError> {
    let caja = caja::caja_de(&state.pool, claims).await?;
    let mut conn = state.pool.acquire().await?;
    let ahora = caja::ahora_caja(&mut conn, caja).await?;
    a_json(caja::calcular_esperado_apertura(&mut conn, caja, &ahora).await?)
}

/// Momento en que empieza el período abierto (fin del último corte).
async fn obtener_inicio_proximo_cierre(state: &AppState, claims: &Claims) -> Result<Value, ApiError> {
    let caja = caja::caja_de(&state.pool, claims).await?;
    let mut conn = state.pool.acquire().await?;
    Ok(json!(caja::inicio_y_fondo(&mut conn, caja).await?.0))
}

/// Fondo que dejó el último corte (por momento de conteo). Compatibilidad: la
/// apertura usa `obtener_esperado_apertura`.
async fn obtener_fondo_sugerido(state: &AppState, claims: &Claims) -> Result<Value, ApiError> {
    let caja = caja::caja_de(&state.pool, claims).await?;
    let mut conn = state.pool.acquire().await?;
    let fondo = caja::ultimo_corte(&mut conn, caja).await?.map(|u| u.fondo_siguiente);
    Ok(json!(fondo.unwrap_or(2000.0)))
}

/// Primer día anterior a hoy con ventas de la caja 'web' que todavía no
/// entran en ningún corte. Para la caja de la tienda siempre null: el aviso
/// de "cierre pendiente" es del escritorio, que es quien la corta.
async fn verificar_corte_dia_pendiente(
    state: &AppState,
    claims: &Claims,
) -> Result<Value, ApiError> {
    let caja = caja::caja_de(&state.pool, claims).await?;
    if caja != Caja::Web {
        return Ok(Value::Null);
    }
    let mut conn = state.pool.acquire().await?;
    let ahora = caja::reloj(&mut conn).await?;
    Ok(match caja::primer_dia_pendiente(&mut conn, caja, &ahora[..10]).await? {
        Some(d) => json!(d),
        None => Value::Null,
    })
}

/// Cambia el modo de caja del dispositivo actual.
///
///   - Solo 'espejo' o 'individual'.
///   - De espejo a individual: siempre (la caja web es otra cadena).
///   - De individual a espejo: se rechaza solo si la caja web tiene dinero sin
///     cortar en su período abierto (si no, ese dinero quedaría sin quien lo
///     corte). Ya no se cuentan "movimientos sin corte" sin límite de fecha:
///     los 4 retiros huérfanos viejos del escritorio bloqueaban el cambio para
///     siempre.
async fn configurar_modo_caja(
    state: &AppState,
    claims: &Claims,
    args: &Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { modo: String, #[serde(default)] nombre: Option<String> }

    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    if a.modo != "espejo" && a.modo != "individual" {
        return Err(ApiError::BadRequest(
            "Modo inválido: usa 'espejo' o 'individual'".into(),
        ));
    }

    // El centinela de "sin dispositivo" no es un equipo: nunca se le guarda
    // un modo (si no, todas las sesiones sin equipo cambiarían de caja).
    let Some(uuid) = caja::dispositivo_de(claims) else {
        return Err(ApiError::BadRequest(
            "Esta sesión no tiene un dispositivo registrado. Cierra sesión y vuelve a entrar.".into(),
        ));
    };

    let modo_actual = caja::modo_de_dispositivo(&state.pool, claims).await?;
    if modo_actual == "individual" && a.modo == "espejo" {
        let mut conn = state.pool.acquire().await?;
        if caja::hay_efectivo_sin_cortar(&mut conn, Caja::Web).await? {
            return Err(ApiError::BadRequest(
                "No puedes cambiar a modo espejo: la caja web tiene dinero sin cortar \
                 (ventas en efectivo, entradas o retiros desde su último corte). \
                 Haz primero un corte de la caja web."
                    .into(),
            ));
        }
    }

    // Upsert: si no existe el device aún (caso raro: JWT con device pero
    // sin fila — solo posible si alguien borró pos_devices a mano),
    // crearlo. El INSERT/ON CONFLICT garantiza idempotencia.
    sqlx::query(
        "INSERT INTO pos_devices (device_uuid, sucursal_id, nombre, modo_caja) \
         VALUES ($1, 1, $2, $3) \
         ON CONFLICT (device_uuid) DO UPDATE \
            SET modo_caja = EXCLUDED.modo_caja, \
                nombre    = COALESCE(EXCLUDED.nombre, pos_devices.nombre)",
    )
    .bind(uuid)
    .bind(a.nombre.clone().unwrap_or_else(|| format!("Web {}", prefijo_dispositivo(uuid))))
    .bind(&a.modo)
    .execute(&state.pool)
    .await?;

    // Bitácora — útil para revisar cuándo un equipo cambió de caja.
    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            descripcion_legible, origen, fecha)
           VALUES ($1, 'MODO_CAJA_CAMBIADO', 'pos_devices', NULL, $2, 'WEB', {NOW_TEXT})"#
    );
    if let Err(e) = sqlx::query(&audit_sql)
        .bind(claims.sub)
        .bind(format!(
            "Modo de caja del dispositivo {} cambiado: {} → {}",
            prefijo_dispositivo(uuid), modo_actual, a.modo
        ))
        .execute(&state.pool)
        .await
    {
        tracing::warn!("configurar_modo_caja: no se registró en bitácora: {e}");
    }

    let info = info_caja_de_modo(state, &a.modo).await;
    Ok(json!({
        "modo":         a.modo,
        "configurado":  true,
        "device_uuid":  uuid,
        "caja":                  info["caja"],
        "cortes_en_este_equipo": info["cortes_en_este_equipo"],
        "escritorio_recibe_web": info["escritorio_recibe_web"],
    }))
}

/// Lee el modo actual del dispositivo + flag de "ya configurado" + la caja con
/// la que trabaja. `configurado=false` solo cuando el frontend acaba de
/// generar un device_uuid nuevo; el frontend usa esa señal para abrir el
/// modal de bienvenida.
async fn obtener_modo_caja(state: &AppState, claims: &Claims) -> Result<Value, ApiError> {
    let Some(uuid) = caja::dispositivo_de(claims) else {
        // Sin device en el JWT (o el centinela): caja de la tienda, sin modal.
        let info = info_caja_de_modo(state, "espejo").await;
        return Ok(json!({
            "modo": "espejo",
            "configurado": true,
            "device_uuid": null,
            "caja":                  info["caja"],
            "cortes_en_este_equipo": info["cortes_en_este_equipo"],
            "escritorio_recibe_web": info["escritorio_recibe_web"],
        }));
    };

    let row = sqlx::query(
        "SELECT modo_caja, nombre FROM pos_devices WHERE device_uuid = $1",
    )
    .bind(uuid)
    .fetch_optional(&state.pool)
    .await?;

    let (modo, configurado, nombre) = match row {
        Some(r) => (
            r.get::<String, _>("modo_caja"),
            true,
            r.try_get::<Option<String>, _>("nombre").ok().flatten(),
        ),
        None => ("espejo".to_string(), false, None),
    };
    let info = info_caja_de_modo(state, &modo).await;
    Ok(json!({
        "modo":          modo,
        "configurado":   configurado,
        "device_uuid":   uuid,
        "nombre":        nombre,
        "caja":                  info["caja"],
        "cortes_en_este_equipo": info["cortes_en_este_equipo"],
        "escritorio_recibe_web": info["escritorio_recibe_web"],
    }))
}

/// Entrada o retiro de efectivo (sin ser una venta) en la caja del equipo.
async fn crear_movimiento_caja(
    state: &AppState,
    claims: &Claims,
    args: Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { datos: NuevoMov }
    #[derive(Deserialize)]
    struct NuevoMov {
        tipo: String,
        /// Se IGNORA: quien registra es el usuario de la sesión.
        #[serde(default)]
        usuario_id: Option<i64>,
        monto: f64,
        concepto: String,
        /// Se IGNORA (antes se guardaba tal cual): quién autorizó lo decide
        /// el servidor con el PIN o el rol de la sesión.
        #[serde(default)]
        #[allow(dead_code)]
        autorizado_por: Option<i64>,
        #[serde(default)] pin_autorizacion: Option<String>,
    }

    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let d = a.datos;

    if !d.monto.is_finite() || d.monto <= 0.0 || d.monto > crate::venta_calc::MAX_IMPORTE {
        return Err(ApiError::BadRequest("El monto debe ser mayor a cero".into()));
    }
    // numeric(12,2): redondeamos aquí para que lo guardado, lo devuelto y lo
    // que suma el corte sean la misma cifra.
    let monto = caja::round2(d.monto);
    if monto <= 0.0 {
        return Err(ApiError::BadRequest("El monto debe ser mayor a cero".into()));
    }
    if d.tipo != "ENTRADA" && d.tipo != "RETIRO" {
        return Err(ApiError::BadRequest("Tipo de movimiento inválido".into()));
    }
    if d.concepto.trim().is_empty() {
        return Err(ApiError::BadRequest("El concepto es obligatorio".into()));
    }

    // Quien registra = el de la sesión. Antes el rol se leía del usuario_id
    // del body: un vendedor podía mandar el id del dueño y saltarse el PIN.
    let sesion = usuario_de_sesion(state, claims).await?;
    if let Some(uid) = d.usuario_id {
        avisar_usuario_distinto("crear_movimiento_caja", uid, &sesion);
    }
    let caja_equipo = caja::caja_de(&state.pool, claims).await?;

    // Movimiento + sync_cursor + bitácora en UNA transacción: si el cursor
    // fallara, no debe quedar un movimiento que nunca baja al escritorio.
    let mut tx = state.pool.begin().await?;
    // El PIN (bcrypt) se resuelve antes del candado: no depende de la cadena.
    let autorizado_por = if d.tipo == "RETIRO" {
        caja::resolver_autorizacion_retiro(&mut tx, sesion.id, monto, d.pin_autorizacion.as_deref())
            .await?
    } else {
        None
    };
    // Caja 'web': candado + fecha de la caja. Caja de la tienda: reloj real
    // (si llega tarde al escritorio, él la re-fecha al siguiente corte).
    let fecha = if caja_equipo == Caja::Web {
        caja::bloquear(&mut tx, caja_equipo).await?;
        caja::ahora_caja(&mut tx, caja_equipo).await?
    } else {
        caja::reloj(&mut tx).await?
    };
    let (id, _) = caja::insertar_movimiento(
        &mut tx, caja_equipo, &d.tipo, sesion.id, monto, &d.concepto, autorizado_por, None, &fecha,
    )
    .await?;
    let accion = if d.tipo == "ENTRADA" { "ENTRADA_CAJA" } else { "RETIRO_CAJA" };
    caja::bitacora(
        &mut tx,
        sesion.id,
        accion,
        "movimientos_caja",
        Some(id),
        &format!("{} de ${:.2} — {}", d.tipo, monto, d.concepto),
        &fecha,
    )
    .await?;
    tx.commit().await?;

    let usuario_nombre: String = sqlx::query_scalar(
        "SELECT nombre_completo FROM usuarios WHERE id = $1",
    )
    .bind(sesion.id)
    .fetch_optional(&state.pool)
    .await?
    .unwrap_or_else(|| "Desconocido".into());

    a_json(caja::MovimientoCaja {
        id,
        tipo: d.tipo,
        usuario_id: sesion.id,
        usuario_nombre,
        monto,
        concepto: d.concepto,
        autorizado_por,
        corte_id: None,
        fecha,
    })
}

/// Movimientos del período en curso de la caja del equipo (sin corte,
/// posteriores al último corte), más recientes primero.
async fn listar_movimientos_sin_corte(
    state: &AppState,
    claims: &Claims,
) -> Result<Value, ApiError> {
    let caja = caja::caja_de(&state.pool, claims).await?;
    let mut conn = state.pool.acquire().await?;
    a_json(caja::movimientos_sin_corte(&mut conn, caja).await?)
}

/// Vista previa del corte: desde el último corte de la caja hasta ahora. Los
/// parámetros fechaInicio/fechaFin que mandan clientes viejos se ignoran.
async fn calcular_datos_corte(
    state: &AppState,
    claims: &Claims,
) -> Result<Value, ApiError> {
    let caja = caja::caja_de(&state.pool, claims).await?;
    let mut conn = state.pool.acquire().await?;
    let ahora = caja::ahora_caja(&mut conn, caja).await?;
    a_json(caja::calcular_periodo(&mut conn, caja, &ahora).await?)
}

/// Auditoría de solo lectura de la caja del equipo (solo dueños).
async fn auditar_caja(state: &AppState, claims: &Claims, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct A {
        #[serde(default, alias = "limite_dias")]
        limite_dias: Option<i64>,
    }
    let sesion = usuario_de_sesion(state, claims).await?;
    if !sesion.es_admin {
        return Err(ApiError::Forbidden);
    }
    let a: A = serde_json::from_value(args.clone()).unwrap_or_default();
    let dias = a.limite_dias.unwrap_or(90).clamp(1, 3650);
    let caja = caja::caja_de(&state.pool, claims).await?;
    let mut conn = state.pool.acquire().await?;
    a_json(caja::auditar(&mut conn, caja, dias).await?)
}

// =============================================================================
// REPORTES — agregaciones del servidor
// =============================================================================
//
// Antes la página Reportes hacía esto en el FRONTEND:
//   1. buscar_ventas() → 1799 filas
//   2. .slice(0, 200)              ← BUG: cortaba a las 200 más recientes
//   3. for cada venta: invoke('obtener_detalle_venta')  ← 200 round-trips
//   4. agregaba productos en memoria
//
// Problemas:
//   - El slice falseaba los conteos (un producto con 20 ventas distribuidas
//     en el rango aparecía con 11 si esas 9 caían fuera de las 200 más
//     recientes).
//   - 200 RPCs serializados era lentísimo.
//
// Estos handlers hacen la agregación en una sola query SQL en el servidor.
// Devuelven exactamente lo que necesita la UI sin necesidad de iterar.
//
// Son de solo lectura: cuentan todas las ventas (escritorio y web, de
// cualquier caja), sin filtro por modo de caja.

// Args comunes a los 4: solo rango de fechas (camelCase desde frontend).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RangoReporte {
    fecha_inicio: String,
    fecha_fin: String,
    #[serde(default)] limite: Option<i64>,
}

/// Top N productos vendidos en el rango (cantidad + total).
/// Reemplaza el patrón frontend de iterar venta por venta.
async fn obtener_top_productos(
    state: &AppState,
    args: &Value,
) -> Result<Value, ApiError> {
    let a: RangoReporte = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let limite = a.limite.unwrap_or(10).clamp(1, 100);

    let sql = format!(
        r#"
        SELECT vd.producto_id,
               COALESCE(p.codigo, '') AS codigo,
               COALESCE(p.nombre, '(producto eliminado)') AS nombre,
               SUM(vd.cantidad)::numeric    AS cantidad,
               SUM(vd.subtotal)::numeric    AS total
        FROM venta_detalle vd
        JOIN ventas v ON v.id = vd.venta_id
        LEFT JOIN productos p ON p.id = vd.producto_id
        WHERE v.deleted_at IS NULL
          AND vd.deleted_at IS NULL
          AND v.anulada = 0
          AND substr(v.fecha, 1, 10) BETWEEN substr($1, 1, 10) AND substr($2, 1, 10)
        GROUP BY vd.producto_id, p.codigo, p.nombre
        ORDER BY cantidad DESC
        LIMIT $3
        "#
    );
    let rows = sqlx::query(&sql)
        .bind(&a.fecha_inicio)
        .bind(&a.fecha_fin)
        .bind(limite)
        .fetch_all(&state.pool)
        .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "producto_id": r.get::<i64, _>("producto_id"),
        "codigo":      r.get::<String, _>("codigo"),
        "nombre":      r.get::<String, _>("nombre"),
        "cantidad":    pg_dec(r, "cantidad"),
        "total":       pg_dec(r, "total"),
    })).collect::<Vec<_>>()))
}

/// Totales de venta agrupados por vendedor.
async fn obtener_ventas_por_vendedor(
    state: &AppState,
    args: &Value,
) -> Result<Value, ApiError> {
    let a: RangoReporte = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let sql = format!(
        r#"
        SELECT v.usuario_id,
               COALESCE(u.nombre_completo, '(usuario ' || v.usuario_id || ')') AS nombre,
               COUNT(*)::bigint            AS count,
               COALESCE(SUM(v.total), 0)::numeric AS total
        FROM ventas v
        LEFT JOIN usuarios u ON u.id = v.usuario_id
        WHERE v.deleted_at IS NULL
          AND v.anulada = 0
          AND substr(v.fecha, 1, 10) BETWEEN substr($1, 1, 10) AND substr($2, 1, 10)
        GROUP BY v.usuario_id, u.nombre_completo
        ORDER BY total DESC
        "#
    );
    let rows = sqlx::query(&sql)
        .bind(&a.fecha_inicio)
        .bind(&a.fecha_fin)
        .fetch_all(&state.pool)
        .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "nombre": r.get::<String, _>("nombre"),
        "count":  r.get::<i64, _>("count"),
        "total":  pg_dec(r, "total"),
    })).collect::<Vec<_>>()))
}

/// Totales agrupados por método de pago.
async fn obtener_ventas_por_metodo(
    state: &AppState,
    args: &Value,
) -> Result<Value, ApiError> {
    let a: RangoReporte = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let sql = format!(
        r#"
        SELECT v.metodo_pago AS metodo,
               COUNT(*)::bigint            AS count,
               COALESCE(SUM(v.total), 0)::numeric AS total
        FROM ventas v
        WHERE v.deleted_at IS NULL
          AND v.anulada = 0
          AND substr(v.fecha, 1, 10) BETWEEN substr($1, 1, 10) AND substr($2, 1, 10)
        GROUP BY v.metodo_pago
        ORDER BY total DESC
        "#
    );
    let rows = sqlx::query(&sql)
        .bind(&a.fecha_inicio)
        .bind(&a.fecha_fin)
        .fetch_all(&state.pool)
        .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "metodo": r.get::<String, _>("metodo"),
        "count":  r.get::<i64, _>("count"),
        "total":  pg_dec(r, "total"),
    })).collect::<Vec<_>>()))
}

/// Totales agrupados por día del rango.
async fn obtener_ventas_por_dia(
    state: &AppState,
    args: &Value,
) -> Result<Value, ApiError> {
    let a: RangoReporte = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let sql = format!(
        r#"
        SELECT substr(v.fecha, 1, 10) AS fecha,
               COUNT(*)::bigint            AS count,
               COALESCE(SUM(v.total), 0)::numeric AS total
        FROM ventas v
        WHERE v.deleted_at IS NULL
          AND v.anulada = 0
          AND substr(v.fecha, 1, 10) BETWEEN substr($1, 1, 10) AND substr($2, 1, 10)
        GROUP BY substr(v.fecha, 1, 10)
        ORDER BY fecha ASC
        "#
    );
    let rows = sqlx::query(&sql)
        .bind(&a.fecha_inicio)
        .bind(&a.fecha_fin)
        .fetch_all(&state.pool)
        .await?;

    Ok(json!(rows.iter().map(|r| json!({
        "fecha": r.get::<String, _>("fecha"),
        "count": r.get::<i64, _>("count"),
        "total": pg_dec(r, "total"),
    })).collect::<Vec<_>>()))
}

/// Confirmar y guardar un corte (de turno o del día) de la caja 'web'. Se
/// recalcula en la transacción, con el candado de la caja, y se rechaza con
/// DATOS_CAMBIARON si el esperado ya no es el que vio el cajero. El retiro de
/// turno viaja DENTRO del corte (`datos.retiro`).
async fn crear_corte(
    state: &AppState,
    claims: &Claims,
    args: Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { datos: caja::NuevoCorte }
    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let caja_equipo = caja::caja_de(&state.pool, claims).await?;
    exigir_caja_web(caja_equipo)?;
    let sesion = usuario_de_sesion(state, claims).await?;
    if let Some(uid) = a.datos.usuario_id {
        avisar_usuario_distinto("crear_corte", uid, &sesion);
    }

    let mut tx = state.pool.begin().await?;
    caja::bloquear(&mut tx, caja_equipo).await?;
    let ahora = caja::ahora_caja(&mut tx, caja_equipo).await?;
    let creado = caja::crear_corte_core(&mut tx, caja_equipo, sesion.id, a.datos, &ahora).await?;
    tx.commit().await?;
    a_json(creado)
}

/// Cortes de la caja del equipo, más recientes primero.
async fn listar_cortes(
    state: &AppState,
    claims: &Claims,
    args: &Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize, Default)]
    struct A { #[serde(default)] limite: Option<i64> }
    let a: A = serde_json::from_value(args.clone()).unwrap_or_default();
    let caja = caja::caja_de(&state.pool, claims).await?;
    let mut conn = state.pool.acquire().await?;
    a_json(caja::listar_cortes(&mut conn, caja, a.limite.unwrap_or(50)).await?)
}

/// Detalle completo de un corte (denominaciones + movimientos + vendedores).
async fn obtener_detalle_corte(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    struct A { id: i64 }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let mut conn = state.pool.acquire().await?;
    a_json(caja::detalle_corte(&mut conn, a.id).await?)
}

/// Busca un usuario admin/dueño por PIN comparando contra el bcrypt hash.
///
/// Admin = `roles.es_admin = 1`, igual que el desktop (`verificar_pin_dueno`
/// en src-tauri/src/commands/auth.rs). Antes era `rol_id IN (1,2)`, pero en
/// la tabla `roles` el 2 es 'vendedor': el PIN de cualquier vendedora
/// autorizaba descuentos, devoluciones y retiros grandes en el POS web.
async fn buscar_dueno_por_pin(state: &AppState, pin: &str) -> Result<Option<i64>, ApiError> {
    let candidatos = sqlx::query(
        "SELECT u.id, u.pin FROM usuarios u JOIN roles r ON r.id = u.rol_id \
         WHERE r.es_admin = 1 AND u.activo = 1 AND u.deleted_at IS NULL",
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

/// Valida el % de descuento de un cliente: 0..100, a 2 decimales
/// (numeric(6,2)). Ese % se aplica después sin PIN a todas las partidas
/// (ver crear_venta).
///
/// PENDIENTE de decisión del dueño: ¿un vendedor puede dejar a un cliente con
/// más % que su propio límite sin PIN? El escritorio no lo impide, así que
/// por ahora aquí tampoco (solo se valida el rango).
fn validar_descuento_cliente(nuevo_pct: f64) -> Result<f64, ApiError> {
    if !nuevo_pct.is_finite() || !(0.0..=100.0).contains(&nuevo_pct) {
        return Err(ApiError::BadRequest("El descuento debe estar entre 0 y 100%".into()));
    }
    Ok(crate::venta_calc::round2(nuevo_pct))
}

async fn crear_cliente(state: &AppState, claims: &Claims, args: Value) -> Result<Value, ApiError> {
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
    let sesion = usuario_de_sesion(state, claims).await?;
    let descuento_porcentaje = validar_descuento_cliente(a.descuento_porcentaje)?;

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
        .bind(descuento_porcentaje)
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
    let _ = a.usuario_id; // se usa el de la sesión
    sqlx::query(&audit_sql)
        .bind(sesion.id)
        .bind(id)
        .bind(format!("Cliente creado: {} ({}%)", a.nombre, descuento_porcentaje))
        .execute(&mut *tx)
        .await?;

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

async fn actualizar_cliente(state: &AppState, claims: &Claims, args: Value) -> Result<Value, ApiError> {
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

    let sesion = usuario_de_sesion(state, claims).await?;
    let mut tx = state.pool.begin().await?;

    let prev = sqlx::query(
        "SELECT uuid, descuento_porcentaje FROM clientes \
         WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(c.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    let uuid: String = prev.get("uuid");
    let pct_anterior = prev
        .try_get::<rust_decimal::Decimal, _>("descuento_porcentaje")
        .map(dec_f64)
        .unwrap_or(0.0);
    let descuento_porcentaje = validar_descuento_cliente(c.descuento_porcentaje)?;

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
        .bind(descuento_porcentaje)
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
    let _ = a.usuario_id; // se usa el de la sesión
    sqlx::query(&audit_sql)
        .bind(sesion.id)
        .bind(c.id)
        .bind(format!("Cliente editado: {} ({}% → {}%)", c.nombre, pct_anterior, descuento_porcentaje))
        .execute(&mut *tx)
        .await?;

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
    sqlx::query(&audit_sql)
        .bind(a.usuario_id)
        .bind(accion)
        .bind(a.id)
        .bind(descr)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(json!(true))
}

// =============================================================================
// VENTAS — listar del día y anular (paridad con `commands/ventas.rs:224, 319`)
// =============================================================================

/// Ventas del día actual en zona horaria de México. Solo lectura: todas las
/// ventas del día (escritorio y web), simétrico al escritorio; cada fila trae
/// `caja` y `origen`.
async fn listar_ventas_dia(state: &AppState) -> Result<Value, ApiError> {
    use rust_decimal::prelude::ToPrimitive;

    let sql = format!(
        r#"
        SELECT v.id, v.folio, v.total, v.metodo_pago, v.anulada, v.fecha,
               v.caja, v.origen,
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
        "#
    );
    let rows = sqlx::query(&sql)
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
        "caja":            r.get::<String, _>("caja"),
        "origen":          r.get::<String, _>("origen"),
    })).collect::<Vec<_>>()))
}

/// Anular una venta desde la web. Reglas (las del escritorio):
///   1. Solo el día en curso — para días anteriores se usa devolución.
///   2. No debe tener devoluciones parciales registradas.
///   3. Restaura el stock completo de cada partida.
///   4. Si la venta fue en efectivo y ya entró en un corte cerrado, ese corte
///      ya contó el dinero: el reembolso sale AHORA y se registra como RETIRO
///      del período en curso. Si sigue en el período abierto no hace falta
///      (el corte excluye ventas anuladas).
///
/// Cajas:
///   - venta de la caja de la tienda ('principal', incluidas las espejo):
///     se rechaza; su cadena es del escritorio. Se registra una devolución.
///   - venta de la caja 'web': se anula aquí, con el candado de esa caja; el
///     reembolso (si aplica) es un RETIRO de la caja web.
///
/// Identidad y permisos se resuelven AQUÍ:
///   - quien anula = usuario de la sesión (se ignora `usuarioId` del body);
///   - cajero no admin → `pinAutorizacion` de un dueño (roles.es_admin = 1).
/// La anulación baja al escritorio: updated_at + sync_cursor de la venta, de
/// cada producto cuyo stock se restauró y del movimiento.
async fn anular_venta(
    state: &AppState,
    claims: &Claims,
    args: &Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct A {
        venta_id: i64,
        /// Se IGNORA: quien anula es el usuario de la sesión.
        #[serde(default)]
        usuario_id: Option<i64>,
        motivo: String,
        /// PIN del dueño cuando el cajero no es admin. El escritorio no lo
        /// manda a su comando local y aquí se verifica.
        #[serde(default)]
        pin_autorizacion: Option<String>,
    }

    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    if a.motivo.trim().is_empty() {
        return Err(ApiError::BadRequest("El motivo es obligatorio".into()));
    }

    let sesion = usuario_de_sesion(state, claims).await?;
    if let Some(uid) = a.usuario_id {
        avisar_usuario_distinto("anular_venta", uid, &sesion);
    }
    if !sesion.es_admin {
        let pin = a.pin_autorizacion.as_deref().unwrap_or("").trim();
        if pin.is_empty() {
            return Err(ApiError::BadRequest(
                "Se requiere el PIN del dueño para anular una venta".into(),
            ));
        }
        if buscar_dueno_por_pin(state, pin).await?.is_none() {
            return Err(ApiError::BadRequest("PIN del dueño incorrecto".into()));
        }
    }

    // La caja de la venta no cambia nunca (el sync no la toca): se lee antes
    // para tomar su candado PRIMERO, igual que crear_venta.
    let caja_venta: Option<String> = sqlx::query_scalar(
        "SELECT caja FROM ventas WHERE id = $1 AND anulada = 0 AND deleted_at IS NULL",
    )
    .bind(a.venta_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(caja_venta) = caja_venta else {
        return Err(ApiError::BadRequest("Venta no encontrada o ya anulada".into()));
    };
    if caja_venta != Caja::Web.as_str() {
        return Err(ApiError::BadRequest(MSG_ANULAR_CAJA_TIENDA.into()));
    }

    let mut tx = state.pool.begin().await?;
    caja::bloquear(&mut tx, Caja::Web).await?;
    let ahora = caja::ahora_caja(&mut tx, Caja::Web).await?;
    anular_venta_tx(&mut tx, sesion.id, a.venta_id, &a.motivo, &ahora).await?;
    tx.commit().await?;
    Ok(json!(true))
}

const MSG_ANULAR_CAJA_TIENDA: &str =
    "Las ventas de la caja de la tienda se anulan en el POS de escritorio; desde aquí registra una devolución";

/// Núcleo de la anulación de una venta de la caja 'web' (puerto de
/// `anular_venta_core` del escritorio). Dentro de la transacción del
/// llamador, que ya tomó el candado de la caja web; `ahora` = `ahora_caja`.
async fn anular_venta_tx(
    conn: &mut sqlx::PgConnection,
    usuario_id: i64,
    venta_id: i64,
    motivo: &str,
    ahora: &str,
) -> Result<(), ApiError> {
    // FOR UPDATE: dos anulaciones simultáneas no pueden restaurar el stock
    // dos veces.
    let v = sqlx::query(&format!(
        "SELECT folio, fecha, metodo_pago, total::float8 AS total, caja, uuid, \
                {} AS momento \
         FROM ventas WHERE id = $1 AND anulada = 0 AND deleted_at IS NULL FOR UPDATE",
        caja::MOMENTO_SQL
    ))
    .bind(venta_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| ApiError::BadRequest("Venta no encontrada o ya anulada".into()))?;

    let folio: String = v.get("folio");
    let fecha_venta: String = v.get("fecha");
    let metodo_pago: String = v.get("metodo_pago");
    let total_venta: f64 = v.get("total");
    let caja_venta: String = v.get("caja");
    let venta_uuid: String = v.get("uuid");
    let momento_venta: String = v.get("momento");

    if caja_venta != Caja::Web.as_str() {
        return Err(ApiError::BadRequest(MSG_ANULAR_CAJA_TIENDA.into()));
    }

    let hoy = ahora.get(..10).unwrap_or(ahora);
    if !fecha_venta.starts_with(hoy) {
        return Err(ApiError::BadRequest(
            "Solo se puede anular una venta del día en curso. Para ventas anteriores, usa el flujo de devolución.".into(),
        ));
    }

    // ¿Tiene devoluciones parciales? Solo cuentan las que de verdad son de
    // esta venta (venta y partidas coinciden; ver crear_devolucion).
    let num_dev: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM devoluciones dv \
         WHERE dv.venta_id = $1 AND dv.deleted_at IS NULL \
           AND EXISTS (SELECT 1 FROM devolucion_detalle dd \
                       JOIN venta_detalle vd ON vd.id = dd.venta_detalle_id \
                       WHERE dd.devolucion_id = dv.id AND vd.venta_id = dv.venta_id)",
    )
    .bind(venta_id)
    .fetch_one(&mut *conn)
    .await?;
    if num_dev > 0 {
        return Err(ApiError::BadRequest(
            "La venta tiene devoluciones parciales registradas. No se puede anular completa.".into(),
        ));
    }

    // ¿La venta ya quedó dentro de un corte cerrado? Si su momento en la
    // cadena es anterior o igual al del último corte, ese corte ya contó su
    // efectivo.
    let ya_cortada = caja::ultimo_corte(&mut *conn, Caja::Web)
        .await?
        .map(|u| momento_venta.as_str() <= u.fin.as_str())
        .unwrap_or(false);

    // Restaurar stock: la cantidad completa de cada partida (la pieza regresa
    // al anaquel aunque al vender se haya frenado en 0). En orden de producto.
    let items = sqlx::query(
        "SELECT producto_id, cantidad::float8 AS cantidad FROM venta_detalle \
         WHERE venta_id = $1 AND deleted_at IS NULL ORDER BY producto_id, id",
    )
    .bind(venta_id)
    .fetch_all(&mut *conn)
    .await?;
    let upd_stock_sql = format!(
        "UPDATE productos SET stock_actual = stock_actual + $1, updated_at = {NOW_TEXT} \
         WHERE id = $2"
    );
    let mut productos_tocados: Vec<i64> = Vec::new();
    for it in &items {
        let prod_id: i64 = it.get("producto_id");
        let cantidad: f64 = it.get("cantidad");
        sqlx::query(&upd_stock_sql)
            .bind(cantidad)
            .bind(prod_id)
            .execute(&mut *conn)
            .await?;
        if !productos_tocados.contains(&prod_id) {
            productos_tocados.push(prod_id);
        }
    }

    // Marcar como anulada (updated_at para que gane en el sync).
    sqlx::query(&format!(
        "UPDATE ventas SET anulada = 1, anulada_por = $1, motivo_anulacion = $2, \
         updated_at = {NOW_TEXT} WHERE id = $3"
    ))
    .bind(usuario_id)
    .bind(motivo)
    .bind(venta_id)
    .execute(&mut *conn)
    .await?;

    // Reembolso de una venta en efectivo que ya entró en un corte cerrado.
    if metodo_pago == "efectivo" && ya_cortada && total_venta > 0.0 {
        let concepto = format!(
            "Reembolso por anulación de venta {} (ya incluida en un corte) — {}",
            folio, motivo
        );
        caja::insertar_movimiento(
            &mut *conn, Caja::Web, "RETIRO", usuario_id, caja::round2(total_venta), &concepto,
            None, None, ahora,
        )
        .await?;
    }

    // sync_cursor: productos (stock) y la venta, igual que crear_venta.
    for prod_id in &productos_tocados {
        sqlx::query(
            "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
             SELECT 'productos', p.uuid, 1, $1 FROM productos p WHERE p.id = $2",
        )
        .bind(WEB_ORIGIN)
        .bind(*prod_id)
        .execute(&mut *conn)
        .await?;
    }
    caja::registrar_cursor(&mut *conn, "ventas", &venta_uuid).await?;

    caja::bitacora(
        &mut *conn,
        usuario_id,
        "ANULACION",
        "ventas",
        Some(venta_id),
        &format!("Venta {} anulada — Motivo: {}", folio, motivo),
        ahora,
    )
    .await?;
    Ok(())
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

// =============================================================================
// USUARIOS — CRUD (paridad con `commands/usuarios.rs:103-269`)
// =============================================================================
//
// Frontend (`pages/Usuarios.tsx`) manda:
//   - crear_usuario:        { usuario: {...}, adminId }
//   - actualizar_usuario:   { usuario: {...}, adminId }
//   - toggle_usuario_activo:{ usuarioId, activo, adminId }
//
// El admin que realiza la acción se loguea en `audit_log` con su id.
// PIN y password se guardan como bcrypt hash (10 rounds, mismo cost del desktop).

async fn crear_usuario(
    state: &AppState,
    claims: &Claims,
    args: Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct A {
        usuario: NuevoUsuario,
        #[serde(default)] admin_id: Option<i64>,
    }
    #[derive(Deserialize)]
    struct NuevoUsuario {
        nombre_completo: String,
        nombre_usuario: String,
        pin: String,
        password: String,
        rol_id: i64,
    }

    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let u = a.usuario;

    if u.nombre_completo.trim().is_empty() {
        return Err(ApiError::BadRequest("Nombre completo obligatorio".into()));
    }
    if u.nombre_usuario.trim().is_empty() {
        return Err(ApiError::BadRequest("Nombre de usuario obligatorio".into()));
    }
    if u.pin.len() < 4 {
        return Err(ApiError::BadRequest("El PIN debe tener al menos 4 caracteres".into()));
    }
    if u.password.len() < 4 {
        return Err(ApiError::BadRequest("La contraseña debe tener al menos 4 caracteres".into()));
    }

    let existe: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM usuarios \
         WHERE lower(nombre_usuario) = lower($1) AND deleted_at IS NULL",
    )
    .bind(&u.nombre_usuario)
    .fetch_one(&state.pool)
    .await?;
    if existe > 0 {
        return Err(ApiError::BadRequest("Ya existe un usuario con ese nombre".into()));
    }

    let pin_hash = bcrypt::hash(&u.pin, 10)
        .map_err(|e| ApiError::BadRequest(format!("Error al hashear PIN: {}", e)))?;
    let pwd_hash = bcrypt::hash(&u.password, 10)
        .map_err(|e| ApiError::BadRequest(format!("Error al hashear contraseña: {}", e)))?;

    let uuid = uuid::Uuid::now_v7().to_string();
    let mut tx = state.pool.begin().await?;

    let ins_sql = format!(
        r#"INSERT INTO usuarios
              (uuid, nombre_completo, nombre_usuario, pin, password_hash,
               rol_id, activo, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, 1, {NOW_TEXT}, {NOW_TEXT})
           RETURNING id, created_at"#
    );
    let row = sqlx::query(&ins_sql)
        .bind(&uuid)
        .bind(&u.nombre_completo)
        .bind(&u.nombre_usuario)
        .bind(&pin_hash)
        .bind(&pwd_hash)
        .bind(u.rol_id)
        .fetch_one(&mut *tx)
        .await?;
    let id: i64 = row.get("id");
    let created_at: String = row.get("created_at");

    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('usuarios', $1, 1, $2)",
    )
    .bind(&uuid)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    let admin_id = a.admin_id.unwrap_or(claims.sub);
    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            descripcion_legible, origen, fecha)
           VALUES ($1, 'USUARIO_CREADO', 'usuarios', $2, $3, 'WEB', {NOW_TEXT})"#
    );
    sqlx::query(&audit_sql)
        .bind(admin_id)
        .bind(id)
        .bind(format!("Usuario creado: {} ({})", u.nombre_completo, u.nombre_usuario))
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    let rol_nombre = rol_nombre_por_id(state, u.rol_id).await;
    let es_admin = rol_es_admin(state, u.rol_id).await;

    Ok(json!({
        "id":              id,
        "nombre_completo": u.nombre_completo,
        "nombre_usuario":  u.nombre_usuario,
        "rol_id":          u.rol_id,
        "rol_nombre":      rol_nombre,
        "es_admin":        es_admin,
        "activo":          true,
        "ultimo_login":    Value::Null,
        "created_at":      created_at,
    }))
}

async fn actualizar_usuario(
    state: &AppState,
    claims: &Claims,
    args: Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct A {
        usuario: ActualizarIn,
        #[serde(default)] admin_id: Option<i64>,
    }
    #[derive(Deserialize)]
    struct ActualizarIn {
        id: i64,
        nombre_completo: String,
        nombre_usuario: String,
        rol_id: i64,
        #[serde(default)] nuevo_pin: Option<String>,
        #[serde(default)] nuevo_password: Option<String>,
    }

    let a: A = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;
    let u = a.usuario;

    let existe: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM usuarios \
         WHERE lower(nombre_usuario) = lower($1) AND id != $2 AND deleted_at IS NULL",
    )
    .bind(&u.nombre_usuario)
    .bind(u.id)
    .fetch_one(&state.pool)
    .await?;
    if existe > 0 {
        return Err(ApiError::BadRequest("Ya existe otro usuario con ese nombre".into()));
    }

    let mut tx = state.pool.begin().await?;

    let uuid: String = sqlx::query_scalar(
        "SELECT uuid FROM usuarios WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(u.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;

    let upd_sql = format!(
        "UPDATE usuarios SET nombre_completo = $1, nombre_usuario = $2, \
                             rol_id = $3, updated_at = {NOW_TEXT} \
         WHERE id = $4 AND deleted_at IS NULL"
    );
    sqlx::query(&upd_sql)
        .bind(&u.nombre_completo)
        .bind(&u.nombre_usuario)
        .bind(u.rol_id)
        .bind(u.id)
        .execute(&mut *tx)
        .await?;

    if let Some(p) = u.nuevo_pin.as_deref() {
        if !p.is_empty() {
            if p.len() < 4 {
                return Err(ApiError::BadRequest("El PIN debe tener al menos 4 caracteres".into()));
            }
            let h = bcrypt::hash(p, 10)
                .map_err(|e| ApiError::BadRequest(format!("Error PIN: {}", e)))?;
            sqlx::query("UPDATE usuarios SET pin = $1 WHERE id = $2")
                .bind(&h).bind(u.id).execute(&mut *tx).await?;
        }
    }
    if let Some(p) = u.nuevo_password.as_deref() {
        if !p.is_empty() {
            if p.len() < 4 {
                return Err(ApiError::BadRequest("La contraseña debe tener al menos 4 caracteres".into()));
            }
            let h = bcrypt::hash(p, 10)
                .map_err(|e| ApiError::BadRequest(format!("Error pwd: {}", e)))?;
            sqlx::query("UPDATE usuarios SET password_hash = $1 WHERE id = $2")
                .bind(&h).bind(u.id).execute(&mut *tx).await?;
        }
    }

    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('usuarios', $1, 1, $2)",
    )
    .bind(&uuid)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    let admin_id = a.admin_id.unwrap_or(claims.sub);
    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            descripcion_legible, origen, fecha)
           VALUES ($1, 'USUARIO_EDITADO', 'usuarios', $2, $3, 'WEB', {NOW_TEXT})"#
    );
    sqlx::query(&audit_sql)
        .bind(admin_id)
        .bind(u.id)
        .bind(format!("Usuario editado: {}", u.nombre_completo))
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(json!(true))
}

async fn toggle_usuario_activo(
    state: &AppState,
    claims: &Claims,
    args: &Value,
) -> Result<Value, ApiError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct A {
        usuario_id: i64,
        activo: bool,
        #[serde(default)] admin_id: Option<i64>,
    }
    let a: A = serde_json::from_value(args.clone())
        .map_err(|e| ApiError::BadRequest(format!("args inválidos: {}", e)))?;

    let admin_id = a.admin_id.unwrap_or(claims.sub);

    if a.usuario_id == admin_id && !a.activo {
        return Err(ApiError::BadRequest("No puedes desactivarte a ti mismo".into()));
    }

    let mut tx = state.pool.begin().await?;

    let row = sqlx::query(
        "SELECT uuid, nombre_completo FROM usuarios \
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(a.usuario_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    let uuid: String = row.get("uuid");
    let nombre: String = row.get("nombre_completo");

    let upd_sql = format!(
        "UPDATE usuarios SET activo = $1, updated_at = {NOW_TEXT} WHERE id = $2"
    );
    sqlx::query(&upd_sql)
        .bind(if a.activo { 1 } else { 0 })
        .bind(a.usuario_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) \
         VALUES ('usuarios', $1, 1, $2)",
    )
    .bind(&uuid)
    .bind(WEB_ORIGIN)
    .execute(&mut *tx)
    .await?;

    let accion = if a.activo { "USUARIO_ACTIVADO" } else { "USUARIO_DESACTIVADO" };
    let descr = format!("Usuario {} {}", nombre, if a.activo { "activado" } else { "desactivado" });
    let audit_sql = format!(
        r#"INSERT INTO audit_log
           (usuario_id, accion, tabla_afectada, registro_id,
            descripcion_legible, origen, fecha)
           VALUES ($1, $2, 'usuarios', $3, $4, 'WEB', {NOW_TEXT})"#
    );
    sqlx::query(&audit_sql)
        .bind(admin_id)
        .bind(accion)
        .bind(a.usuario_id)
        .bind(descr)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(json!(true))
}


// Pruebas de integración contra Postgres (se saltan sin TEST_DATABASE_URL).
#[cfg(test)]
#[path = "rpc_precios_tests.rs"]
mod precios_tests;

// Caja web y caja de la tienda (caja.rs + envolturas de aquí), también contra
// Postgres.
#[cfg(test)]
#[path = "rpc_caja_tests.rs"]
mod caja_tests;
