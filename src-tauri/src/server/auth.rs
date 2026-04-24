// server/auth.rs — JWT y middleware de autenticación

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, Request},
    middleware::Next,
    response::Response,
};
use chrono::Utc;
use jsonwebtoken::{encode, decode, Header, EncodingKey, DecodingKey, Validation, Algorithm};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::state::ServerState;

const JWT_DURATION_DAYS: i64 = 30;

/// Claims del JWT
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: i64,         // usuario_id
    pub device_id: i64,   // id de dispositivos_conectados
    pub jti: String,      // ID único del token
    pub iat: i64,
    pub exp: i64,
}

/// Contexto autenticado disponible en cada handler
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub usuario_id: i64,
    pub device_id: i64,
    pub jti: String,
}

pub fn emit_jwt(secret: &[u8], usuario_id: i64, device_id: i64) -> Result<(String, String), String> {
    let now = Utc::now().timestamp();
    let jti = Uuid::new_v4().to_string();
    let claims = Claims {
        sub: usuario_id,
        device_id,
        jti: jti.clone(),
        iat: now,
        exp: now + (JWT_DURATION_DAYS * 86400),
    };
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret),
    ).map_err(|e| e.to_string())?;
    Ok((token, jti))
}

pub fn decode_jwt(secret: &[u8], token: &str) -> Result<Claims, String> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret),
        &Validation::new(Algorithm::HS256),
    ).map_err(|e| e.to_string())?;
    Ok(data.claims)
}

/// Middleware que valida el JWT y verifica que el jti no esté revocado.
pub async fn require_auth(
    State(state): State<ServerState>,
    headers: HeaderMap,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let claims = decode_jwt(&state.jwt_secret, token)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    // Verificar que el dispositivo no esté revocado + actualizar último ping
    let jti = claims.jti.clone();
    let db = state.db.clone();
    let jti_check = jti.clone();
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let valid: bool = tokio::task::spawn_blocking(move || {
        let db = db.lock().unwrap();
        // revocado?
        let revocado: i64 = db.query_row(
            "SELECT revocado FROM dispositivos_conectados WHERE jwt_jti = ?",
            rusqlite::params![jti_check],
            |row| row.get(0),
        ).unwrap_or(1);
        if revocado != 0 {
            return false;
        }
        let _ = db.execute(
            "UPDATE dispositivos_conectados SET ultimo_ping = ? WHERE jwt_jti = ?",
            rusqlite::params![now, jti_check],
        );
        true
    }).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !valid {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let ctx = AuthContext {
        usuario_id: claims.sub,
        device_id: claims.device_id,
        jti,
    };
    req.extensions_mut().insert(ctx);
    Ok(next.run(req).await)
}
