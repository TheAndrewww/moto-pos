// error.rs — Tipo de error unificado para handlers axum.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("not found")]
    NotFound,

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    #[error(transparent)]
    Jwt(#[from] jsonwebtoken::errors::Error),

    #[error(transparent)]
    Bcrypt(#[from] bcrypt::BcryptError),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, self.to_string()),
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            ApiError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            _ => {
                tracing::error!("internal error: {:?}", self);
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
