// server/handlers.rs — Handlers HTTP del servidor LAN

use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::auth::{emit_jwt, AuthContext};
use super::error::ApiError;
use super::state::ServerState;

// ============================================================
// Health
// ============================================================
pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "ts": Utc::now().timestamp() }))
}

// ============================================================
// Pairing redeem
// ============================================================
#[derive(Deserialize)]
pub struct PairingRedeem {
    pub token: String,
    pub pin: String,
    pub device_name: String,
    pub user_agent: Option<String>,
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub jwt: String,
    pub usuario: UsuarioPublico,
    pub device_id: i64,
}

#[derive(Serialize, Clone)]
pub struct UsuarioPublico {
    pub id: i64,
    pub nombre_completo: String,
    pub nombre_usuario: String,
    pub rol: String,
    pub es_admin: bool,
}

pub async fn pairing_redeem(
    State(state): State<ServerState>,
    Json(body): Json<PairingRedeem>,
) -> Result<Json<AuthResponse>, ApiError> {
    // Validar token en memoria
    {
        let map = state.pairing_tokens.read().await;
        let entry = map.get(&body.token).ok_or_else(|| ApiError::bad_request("Token inválido o expirado"))?;
        if entry.expires_at < Utc::now().timestamp() {
            return Err(ApiError::bad_request("Token expirado"));
        }
    }

    // Validar PIN + crear dispositivo (operación bloqueante)
    let db = state.db.clone();
    let secret = state.jwt_secret.clone();
    let device_name = body.device_name.clone();
    let user_agent = body.user_agent.clone();
    let pin = body.pin.clone();

    let result = tokio::task::spawn_blocking(move || -> Result<AuthResponse, String> {
        let db = db.lock().unwrap();

        // Buscar usuario por PIN
        let mut stmt = db.prepare(
            r#"SELECT u.id, u.nombre_completo, u.nombre_usuario, u.pin, r.nombre, r.es_admin
               FROM usuarios u JOIN roles r ON r.id = u.rol_id
               WHERE u.activo = 1"#,
        ).map_err(|e| e.to_string())?;
        let rows: Vec<(i64, String, String, String, String, bool)> = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
        }).map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        let usuario = rows.into_iter().find(|(_, _, _, hash, _, _)| {
            bcrypt::verify(&pin, hash).unwrap_or(false)
        }).ok_or_else(|| "PIN incorrecto".to_string())?;

        let (usuario_id, nombre_completo, nombre_usuario, _pin_hash, rol_nombre, es_admin) = usuario;

        // Insertar dispositivo (con jti temporal; se actualiza al generar JWT)
        let temp_jti = uuid::Uuid::new_v4().to_string();
        db.execute(
            r#"INSERT INTO dispositivos_conectados (nombre, user_agent, usuario_id, jwt_jti, ultimo_ping)
               VALUES (?, ?, ?, ?, datetime('now','localtime'))"#,
            rusqlite::params![device_name, user_agent, usuario_id, temp_jti],
        ).map_err(|e| e.to_string())?;
        let device_id = db.last_insert_rowid();

        // Emitir JWT con el device_id real
        let (jwt, jti) = emit_jwt(&secret, usuario_id, device_id)?;
        db.execute(
            "UPDATE dispositivos_conectados SET jwt_jti = ? WHERE id = ?",
            rusqlite::params![jti, device_id],
        ).map_err(|e| e.to_string())?;

        // Bitácora
        let _ = db.execute(
            r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
               VALUES (?, 'DISPOSITIVO_EMPAREJADO', 'dispositivos_conectados', ?, ?, 'WEB')"#,
            rusqlite::params![usuario_id, device_id, format!("Dispositivo emparejado: {}", device_name)],
        );

        Ok(AuthResponse {
            jwt,
            device_id,
            usuario: UsuarioPublico {
                id: usuario_id,
                nombre_completo,
                nombre_usuario,
                rol: rol_nombre,
                es_admin,
            },
        })
    }).await.map_err(|e| ApiError::internal(e.to_string()))??;

    // Invalidar el token de pairing después de uso exitoso
    {
        let mut map = state.pairing_tokens.write().await;
        map.remove(&body.token);
    }

    Ok(Json(result))
}

// ============================================================
// Login directo con PIN (dispositivo ya emparejado sin JWT)
// ============================================================
#[derive(Deserialize)]
pub struct LoginPinBody {
    pub pin: String,
    pub device_id: i64,
}

pub async fn login_pin(
    State(state): State<ServerState>,
    Json(body): Json<LoginPinBody>,
) -> Result<Json<AuthResponse>, ApiError> {
    let db = state.db.clone();
    let secret = state.jwt_secret.clone();
    let pin = body.pin.clone();
    let device_id = body.device_id;

    let result = tokio::task::spawn_blocking(move || -> Result<AuthResponse, String> {
        let db = db.lock().unwrap();

        let mut stmt = db.prepare(
            r#"SELECT u.id, u.nombre_completo, u.nombre_usuario, u.pin, r.nombre, r.es_admin
               FROM usuarios u JOIN roles r ON r.id = u.rol_id
               WHERE u.activo = 1"#,
        ).map_err(|e| e.to_string())?;
        let rows: Vec<(i64, String, String, String, String, bool)> = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
        }).map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        let usuario = rows.into_iter().find(|(_, _, _, hash, _, _)| {
            bcrypt::verify(&pin, hash).unwrap_or(false)
        }).ok_or_else(|| "PIN incorrecto".to_string())?;
        let (usuario_id, nombre_completo, nombre_usuario, _, rol_nombre, es_admin) = usuario;

        // Verificar que el dispositivo existe y no está revocado
        let revocado: i64 = db.query_row(
            "SELECT revocado FROM dispositivos_conectados WHERE id = ?",
            rusqlite::params![device_id],
            |row| row.get(0),
        ).map_err(|_| "Dispositivo no registrado".to_string())?;
        if revocado != 0 {
            return Err("Dispositivo revocado".to_string());
        }

        let (jwt, jti) = emit_jwt(&secret, usuario_id, device_id)?;
        db.execute(
            "UPDATE dispositivos_conectados SET jwt_jti = ?, usuario_id = ? WHERE id = ?",
            rusqlite::params![jti, usuario_id, device_id],
        ).map_err(|e| e.to_string())?;

        Ok(AuthResponse {
            jwt,
            device_id,
            usuario: UsuarioPublico {
                id: usuario_id,
                nombre_completo,
                nombre_usuario,
                rol: rol_nombre,
                es_admin,
            },
        })
    }).await.map_err(|e| ApiError::internal(e.to_string()))??;

    Ok(Json(result))
}

// ============================================================
// /me
// ============================================================
pub async fn me(
    Extension(ctx): Extension<AuthContext>,
    State(state): State<ServerState>,
) -> Result<Json<UsuarioPublico>, ApiError> {
    let db = state.db.clone();
    let uid = ctx.usuario_id;
    let u = tokio::task::spawn_blocking(move || -> Result<UsuarioPublico, String> {
        let db = db.lock().unwrap();
        db.query_row(
            r#"SELECT u.id, u.nombre_completo, u.nombre_usuario, r.nombre, r.es_admin
               FROM usuarios u JOIN roles r ON r.id = u.rol_id
               WHERE u.id = ?"#,
            rusqlite::params![uid],
            |row| Ok(UsuarioPublico {
                id: row.get(0)?,
                nombre_completo: row.get(1)?,
                nombre_usuario: row.get(2)?,
                rol: row.get(3)?,
                es_admin: row.get(4)?,
            }),
        ).map_err(|e| e.to_string())
    }).await.map_err(|e| ApiError::internal(e.to_string()))??;
    Ok(Json(u))
}

// ============================================================
// GET /productos?q=...
// ============================================================
#[derive(Serialize)]
pub struct ProductoApi {
    pub id: i64,
    pub codigo: String,
    pub nombre: String,
    pub stock_actual: f64,
    pub precio_costo: f64,
    pub precio_venta: f64,
    pub proveedor_id: Option<i64>,
    pub proveedor_nombre: Option<String>,
}

pub async fn productos_buscar(
    Extension(_ctx): Extension<AuthContext>,
    State(state): State<ServerState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<ProductoApi>>, ApiError> {
    let q = params.get("q").cloned().unwrap_or_default();
    let proveedor_id = params.get("proveedor_id").and_then(|s| s.parse::<i64>().ok());
    let db = state.db.clone();

    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<ProductoApi>, String> {
        let db = db.lock().unwrap();
        let q_norm = q.trim().to_lowercase();

        let (sql, has_q, has_prov) = match (q_norm.is_empty(), proveedor_id.is_some()) {
            (true, false) => (
                r#"SELECT p.id, p.codigo, p.nombre, p.stock_actual, p.precio_costo, p.precio_venta,
                          p.proveedor_id, pr.nombre
                   FROM productos p LEFT JOIN proveedores pr ON pr.id = p.proveedor_id
                   WHERE p.activo = 1 ORDER BY p.nombre LIMIT 50"#.to_string(), false, false),
            (true, true) => (
                r#"SELECT p.id, p.codigo, p.nombre, p.stock_actual, p.precio_costo, p.precio_venta,
                          p.proveedor_id, pr.nombre
                   FROM productos p LEFT JOIN proveedores pr ON pr.id = p.proveedor_id
                   WHERE p.activo = 1 AND p.proveedor_id = ? ORDER BY p.nombre LIMIT 100"#.to_string(), false, true),
            (false, false) => (
                r#"SELECT p.id, p.codigo, p.nombre, p.stock_actual, p.precio_costo, p.precio_venta,
                          p.proveedor_id, pr.nombre
                   FROM productos p LEFT JOIN proveedores pr ON pr.id = p.proveedor_id
                   WHERE p.activo = 1 AND (LOWER(p.codigo) = ?1 OR LOWER(p.search_text) LIKE '%' || ?1 || '%')
                   ORDER BY CASE WHEN LOWER(p.codigo) = ?1 THEN 0 ELSE 1 END, p.nombre
                   LIMIT 50"#.to_string(), true, false),
            (false, true) => (
                r#"SELECT p.id, p.codigo, p.nombre, p.stock_actual, p.precio_costo, p.precio_venta,
                          p.proveedor_id, pr.nombre
                   FROM productos p LEFT JOIN proveedores pr ON pr.id = p.proveedor_id
                   WHERE p.activo = 1 AND p.proveedor_id = ?2
                     AND (LOWER(p.codigo) = ?1 OR LOWER(p.search_text) LIKE '%' || ?1 || '%')
                   ORDER BY CASE WHEN LOWER(p.codigo) = ?1 THEN 0 ELSE 1 END, p.nombre
                   LIMIT 50"#.to_string(), true, true),
        };

        let mut stmt = db.prepare(&sql).map_err(|e| e.to_string())?;
        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<ProductoApi> {
            Ok(ProductoApi {
                id: row.get(0)?,
                codigo: row.get(1)?,
                nombre: row.get(2)?,
                stock_actual: row.get(3)?,
                precio_costo: row.get(4)?,
                precio_venta: row.get(5)?,
                proveedor_id: row.get(6)?,
                proveedor_nombre: row.get(7)?,
            })
        };
        let iter: Vec<ProductoApi> = match (has_q, has_prov) {
            (false, false) => stmt.query_map([], map_row).map_err(|e| e.to_string())?
                .filter_map(|r| r.ok()).collect(),
            (false, true) => stmt.query_map(rusqlite::params![proveedor_id.unwrap()], map_row)
                .map_err(|e| e.to_string())?.filter_map(|r| r.ok()).collect(),
            (true, false) => stmt.query_map(rusqlite::params![q_norm], map_row)
                .map_err(|e| e.to_string())?.filter_map(|r| r.ok()).collect(),
            (true, true) => stmt.query_map(rusqlite::params![q_norm, proveedor_id.unwrap()], map_row)
                .map_err(|e| e.to_string())?.filter_map(|r| r.ok()).collect(),
        };
        Ok(iter)
    }).await.map_err(|e| ApiError::internal(e.to_string()))??;

    Ok(Json(rows))
}

// ============================================================
// GET /productos/por_codigo/:codigo
// ============================================================
pub async fn producto_por_codigo(
    Extension(_ctx): Extension<AuthContext>,
    State(state): State<ServerState>,
    Path(codigo): Path<String>,
) -> Result<Json<ProductoApi>, ApiError> {
    let db = state.db.clone();
    let r = tokio::task::spawn_blocking(move || -> Result<ProductoApi, String> {
        let db = db.lock().unwrap();
        db.query_row(
            r#"SELECT p.id, p.codigo, p.nombre, p.stock_actual, p.precio_costo, p.precio_venta,
                      p.proveedor_id, pr.nombre
               FROM productos p LEFT JOIN proveedores pr ON pr.id = p.proveedor_id
               WHERE p.codigo = ? AND p.activo = 1"#,
            rusqlite::params![codigo],
            |row| Ok(ProductoApi {
                id: row.get(0)?,
                codigo: row.get(1)?,
                nombre: row.get(2)?,
                stock_actual: row.get(3)?,
                precio_costo: row.get(4)?,
                precio_venta: row.get(5)?,
                proveedor_id: row.get(6)?,
                proveedor_nombre: row.get(7)?,
            }),
        ).map_err(|e| e.to_string())
    }).await.map_err(|e| ApiError::internal(e.to_string()))?;

    match r {
        Ok(p) => Ok(Json(p)),
        Err(_) => Err(ApiError::not_found("Producto no encontrado")),
    }
}

// ============================================================
// GET /proveedores
// ============================================================
#[derive(Serialize)]
pub struct ProveedorApi {
    pub id: i64,
    pub nombre: String,
}

pub async fn proveedores(
    Extension(_ctx): Extension<AuthContext>,
    State(state): State<ServerState>,
) -> Result<Json<Vec<ProveedorApi>>, ApiError> {
    let db = state.db.clone();
    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<ProveedorApi>, String> {
        let db = db.lock().unwrap();
        let mut stmt = db.prepare("SELECT id, nombre FROM proveedores ORDER BY nombre")
            .map_err(|e| e.to_string())?;
        let iter = stmt.query_map([], |row| Ok(ProveedorApi {
            id: row.get(0)?, nombre: row.get(1)?,
        })).map_err(|e| e.to_string())?;
        Ok(iter.filter_map(|r| r.ok()).collect())
    }).await.map_err(|e| ApiError::internal(e.to_string()))??;
    Ok(Json(rows))
}

// ============================================================
// GET /ordenes_pedido?abiertas=1
// ============================================================
#[derive(Serialize)]
pub struct OrdenResumenApi {
    pub id: i64,
    pub folio: String,
    pub proveedor_id: Option<i64>,
    pub proveedor_nombre: Option<String>,
    pub estado: String,
    pub fecha_pedido: String,
    pub total_items: i64,
}

pub async fn ordenes_listar(
    Extension(_ctx): Extension<AuthContext>,
    State(state): State<ServerState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<OrdenResumenApi>>, ApiError> {
    let solo_abiertas = params.get("abiertas").map(|v| v == "1").unwrap_or(false);
    let db = state.db.clone();
    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<OrdenResumenApi>, String> {
        let db = db.lock().unwrap();
        let sql = if solo_abiertas {
            r#"SELECT o.id, o.folio, o.proveedor_id, p.nombre, o.estado, o.fecha_pedido,
                      (SELECT COUNT(*) FROM orden_pedido_detalle WHERE orden_id = o.id)
               FROM ordenes_pedido o LEFT JOIN proveedores p ON p.id = o.proveedor_id
               WHERE o.estado IN ('enviada','recibida_parcial')
               ORDER BY o.fecha_pedido DESC"#
        } else {
            r#"SELECT o.id, o.folio, o.proveedor_id, p.nombre, o.estado, o.fecha_pedido,
                      (SELECT COUNT(*) FROM orden_pedido_detalle WHERE orden_id = o.id)
               FROM ordenes_pedido o LEFT JOIN proveedores p ON p.id = o.proveedor_id
               ORDER BY o.fecha_pedido DESC LIMIT 50"#
        };
        let mut stmt = db.prepare(sql).map_err(|e| e.to_string())?;
        let iter = stmt.query_map([], |row| Ok(OrdenResumenApi {
            id: row.get(0)?,
            folio: row.get(1)?,
            proveedor_id: row.get(2)?,
            proveedor_nombre: row.get(3)?,
            estado: row.get(4)?,
            fecha_pedido: row.get(5)?,
            total_items: row.get(6)?,
        })).map_err(|e| e.to_string())?;
        Ok(iter.filter_map(|r| r.ok()).collect())
    }).await.map_err(|e| ApiError::internal(e.to_string()))??;
    Ok(Json(rows))
}

// ============================================================
// GET /ordenes_pedido/:id  — detalle con pendientes
// ============================================================
#[derive(Serialize)]
pub struct OrdenDetalleApi {
    pub id: i64,
    pub folio: String,
    pub proveedor_id: Option<i64>,
    pub proveedor_nombre: Option<String>,
    pub estado: String,
    pub fecha_pedido: String,
    pub items: Vec<OrdenItemApi>,
}

#[derive(Serialize)]
pub struct OrdenItemApi {
    pub producto_id: i64,
    pub codigo: String,
    pub nombre: String,
    pub cantidad_pedida: f64,
    pub cantidad_recibida: f64,
    pub pendiente: f64,
    pub precio_costo: f64,
}

pub async fn orden_detalle(
    Extension(_ctx): Extension<AuthContext>,
    State(state): State<ServerState>,
    Path(id): Path<i64>,
) -> Result<Json<OrdenDetalleApi>, ApiError> {
    let db = state.db.clone();
    let res = tokio::task::spawn_blocking(move || -> Result<OrdenDetalleApi, String> {
        let db = db.lock().unwrap();
        let (folio, proveedor_id, proveedor_nombre, estado, fecha_pedido): (String, Option<i64>, Option<String>, String, String) = db.query_row(
            r#"SELECT o.folio, o.proveedor_id, p.nombre, o.estado, o.fecha_pedido
               FROM ordenes_pedido o LEFT JOIN proveedores p ON p.id = o.proveedor_id
               WHERE o.id = ?"#,
            rusqlite::params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).map_err(|e| e.to_string())?;

        let mut stmt = db.prepare(
            r#"SELECT d.producto_id, p.codigo, p.nombre, d.cantidad_pedida, d.cantidad_recibida, d.precio_costo
               FROM orden_pedido_detalle d JOIN productos p ON p.id = d.producto_id
               WHERE d.orden_id = ?"#,
        ).map_err(|e| e.to_string())?;
        let items: Vec<OrdenItemApi> = stmt.query_map(rusqlite::params![id], |row| {
            let pedida: f64 = row.get(3)?;
            let recibida: f64 = row.get(4)?;
            Ok(OrdenItemApi {
                producto_id: row.get(0)?,
                codigo: row.get(1)?,
                nombre: row.get(2)?,
                cantidad_pedida: pedida,
                cantidad_recibida: recibida,
                pendiente: (pedida - recibida).max(0.0),
                precio_costo: row.get(5)?,
            })
        }).map_err(|e| e.to_string())?.filter_map(|r| r.ok()).collect();

        Ok(OrdenDetalleApi { id, folio, proveedor_id, proveedor_nombre, estado, fecha_pedido, items })
    }).await.map_err(|e| ApiError::internal(e.to_string()))??;
    Ok(Json(res))
}

// ============================================================
// POST /recepciones
// ============================================================
#[derive(Deserialize)]
pub struct RecepcionBody {
    pub proveedor_id: Option<i64>,
    pub orden_id: Option<i64>,
    pub notas: Option<String>,
    pub items: Vec<RecepcionItemBody>,
}

#[derive(Deserialize)]
pub struct RecepcionItemBody {
    pub producto_id: i64,
    pub cantidad: f64,
    pub precio_costo: f64,
}

#[derive(Serialize)]
pub struct RecepcionResp {
    pub id: i64,
    pub total_items: i64,
}

pub async fn recepcion_crear(
    Extension(ctx): Extension<AuthContext>,
    State(state): State<ServerState>,
    Json(body): Json<RecepcionBody>,
) -> Result<Json<RecepcionResp>, ApiError> {
    if body.items.is_empty() {
        return Err(ApiError::bad_request("Sin items"));
    }
    let db = state.db.clone();
    let usuario_id = ctx.usuario_id;
    let res = tokio::task::spawn_blocking(move || -> Result<RecepcionResp, String> {
        let db = db.lock().unwrap();
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        db.execute(
            r#"INSERT INTO recepciones (orden_id, usuario_id, proveedor_id, fecha, notas)
               VALUES (?, ?, ?, ?, ?)"#,
            rusqlite::params![body.orden_id, usuario_id, body.proveedor_id, now, body.notas],
        ).map_err(|e| e.to_string())?;
        let recep_id = db.last_insert_rowid();
        let total_items = body.items.len() as i64;

        for item in &body.items {
            db.execute(
                r#"INSERT INTO recepcion_detalle (recepcion_id, producto_id, cantidad, precio_costo)
                   VALUES (?, ?, ?, ?)"#,
                rusqlite::params![recep_id, item.producto_id, item.cantidad, item.precio_costo],
            ).map_err(|e| e.to_string())?;
            db.execute(
                r#"UPDATE productos SET stock_actual = stock_actual + ?, precio_costo = ?, updated_at = ?
                   WHERE id = ?"#,
                rusqlite::params![item.cantidad, item.precio_costo, now, item.producto_id],
            ).map_err(|e| e.to_string())?;

            if let Some(oid) = body.orden_id {
                let _ = db.execute(
                    r#"UPDATE orden_pedido_detalle SET cantidad_recibida = cantidad_recibida + ?
                       WHERE orden_id = ? AND producto_id = ?"#,
                    rusqlite::params![item.cantidad, oid, item.producto_id],
                );
            }
        }

        if let Some(oid) = body.orden_id {
            let faltante: f64 = db.query_row(
                r#"SELECT COALESCE(SUM(
                       CASE WHEN cantidad_pedida > cantidad_recibida
                            THEN cantidad_pedida - cantidad_recibida ELSE 0 END
                   ), 0) FROM orden_pedido_detalle WHERE orden_id = ?"#,
                rusqlite::params![oid], |row| row.get(0),
            ).unwrap_or(0.0);
            let nuevo_estado = if faltante <= 0.0 { "recibida_completa" } else { "recibida_parcial" };
            let _ = db.execute(
                "UPDATE ordenes_pedido SET estado = ?, fecha_recepcion = ? WHERE id = ?",
                rusqlite::params![nuevo_estado, now, oid],
            );
        }

        let _ = db.execute(
            r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
               VALUES (?, 'RECEPCION_CREADA', 'recepciones', ?, ?, 'WEB')"#,
            rusqlite::params![usuario_id, recep_id, format!("Recepción #{} desde móvil · {} items", recep_id, total_items)],
        );

        Ok(RecepcionResp { id: recep_id, total_items })
    }).await.map_err(|e| ApiError::internal(e.to_string()))??;
    Ok(Json(res))
}
