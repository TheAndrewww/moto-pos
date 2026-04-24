// db.rs — Pool Postgres y migraciones.

use anyhow::Result;
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

pub async fn connect(url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await?;
    Ok(pool)
}

/// Corre las migraciones SQL del directorio `migrations/` en arranque.
/// Usa sqlx::migrate! para registro automático (tabla _sqlx_migrations).
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    tracing::info!("migraciones aplicadas");
    Ok(())
}
