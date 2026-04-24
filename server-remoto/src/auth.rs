// auth.rs — Login del panel web y emisión/validación de JWT.
//
// El mismo token JWT sirve para:
//   - Llamadas del panel web (después de login email+password)
//   - Llamadas del POS a /sync/push y /sync/pull (obtenido vía configurar_sync)
//
// Claims:
//   sub:      admin_users.id
//   email:    admin_users.email
//   role:     "admin" | "device"
//   sucursal: i64 default
//   exp:      unix seconds

use axum::{
    extract::State,
    http::{header::AUTHORIZATION, HeaderMap},
    Json,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: i64,
    pub email: String,
    pub role: String,
    pub sucursal: i64,
    pub exp: i64,
}

#[derive(Debug, Deserialize)]
pub struct LoginInput {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginOutput {
    pub token: String,
    pub sucursal_id: i64,
    pub nombre: String,
    pub es_super_admin: bool,
}

#[derive(sqlx::FromRow)]
struct AdminRow {
    id: i64,
    email: String,
    password_hash: String,
    nombre: String,
    sucursal_id: Option<i64>,
    es_super_admin: bool,
    activo: bool,
}

pub async fn login(
    State(state): State<AppState>,
    Json(input): Json<LoginInput>,
) -> ApiResult<Json<LoginOutput>> {
    let row: AdminRow = sqlx::query_as(
        r#"
        SELECT id, email, password_hash, nombre, sucursal_id, es_super_admin, activo
        FROM admin_users
        WHERE lower(email) = lower($1)
        LIMIT 1
        "#,
    )
    .bind(&input.email)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::Unauthorized)?;

    if !row.activo {
        return Err(ApiError::Forbidden);
    }

    let ok = bcrypt::verify(&input.password, &row.password_hash)?;
    if !ok {
        return Err(ApiError::Unauthorized);
    }

    let sucursal_id = row.sucursal_id.unwrap_or(1);
    let token = emitir_token(
        &state.jwt_secret,
        row.id,
        &row.email,
        "admin",
        sucursal_id,
        Duration::days(30),
    )?;

    Ok(Json(LoginOutput {
        token,
        sucursal_id,
        nombre: row.nombre,
        es_super_admin: row.es_super_admin,
    }))
}

pub fn emitir_token(
    secret: &[u8],
    sub: i64,
    email: &str,
    role: &str,
    sucursal: i64,
    ttl: Duration,
) -> Result<String, jsonwebtoken::errors::Error> {
    let exp = (Utc::now() + ttl).timestamp();
    let claims = Claims {
        sub,
        email: email.to_string(),
        role: role.to_string(),
        sucursal,
        exp,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret),
    )
}

/// Extrae y valida el bearer token de los headers.
pub fn autenticar(headers: &HeaderMap, secret: &[u8]) -> ApiResult<Claims> {
    let h = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    let token = h.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret),
        &Validation::default(),
    )
    .map_err(|_| ApiError::Unauthorized)?;
    Ok(data.claims)
}
