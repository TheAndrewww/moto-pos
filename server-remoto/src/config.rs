// config.rs — Variables de entorno del servidor.

use anyhow::{anyhow, Result};

pub struct Config {
    pub database_url: String,
    pub jwt_secret: String,
    pub port: u16,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let database_url = std::env::var("DATABASE_URL")
            .map_err(|_| anyhow!("DATABASE_URL no está definida"))?;
        let jwt_secret = std::env::var("JWT_SECRET")
            .map_err(|_| anyhow!("JWT_SECRET no está definida"))?;
        if jwt_secret.len() < 16 {
            return Err(anyhow!("JWT_SECRET demasiado corta (mínimo 16 chars)"));
        }
        let port = std::env::var("PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3000u16);
        Ok(Self { database_url, jwt_secret, port })
    }
}
