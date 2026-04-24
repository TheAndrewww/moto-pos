// main.rs — Entry point del servidor remoto Moto Refaccionaria (Fase 3.2).

mod config;
mod error;
mod db;
mod auth;
mod sync;
mod api;
mod rpc;

use axum::{
    routing::{get, post, put},
    Router,
};
use std::{net::SocketAddr, path::PathBuf};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::PgPool,
    pub jwt_secret: Vec<u8>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cfg = config::Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;
    db::run_migrations(&pool).await?;

    let state = AppState {
        pool,
        jwt_secret: cfg.jwt_secret.into_bytes(),
    };

    // Directorio de la SPA buildeada (vite build → dist/). Configurable via
    // STATIC_DIR; default `./static` (donde el Dockerfile copia el dist).
    let static_dir: PathBuf = std::env::var("STATIC_DIR")
        .unwrap_or_else(|_| "./static".into())
        .into();
    let index_html = static_dir.join("index.html");
    let serve_static = ServeDir::new(&static_dir)
        .not_found_service(ServeFile::new(&index_html));

    let app = Router::new()
        .route("/health", get(health))
        .route("/auth/login", post(auth::login))
        // Sync
        .route("/sync/push", post(sync::push))
        .route("/sync/pull", get(sync::pull))
        // Panel web — catálogo
        .route("/api/dashboard", get(api::dashboard_resumen))
        .route("/api/sucursales", get(api::sucursales_list))
        .route("/api/productos", get(api::productos_list).post(api::productos_create))
        .route("/api/productos/:uuid",
            put(api::productos_update).delete(api::productos_delete))
        .route("/api/categorias", get(api::categorias_list))
        .route("/api/proveedores", get(api::proveedores_list))
        .route("/api/clientes", get(api::clientes_list))
        .route("/api/ventas", get(api::ventas_list))
        .route("/api/ventas/:uuid", get(api::ventas_detalle))
        // POS modo web: dispatcher que mapea comandos Tauri → handlers Postgres
        .route("/rpc/:cmd", post(rpc::dispatch))
        // SPA estática (último — captura todo lo que no matchea arriba).
        // El fallback a index.html cubre client-side routing.
        .fallback_service(serve_static)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = format!("0.0.0.0:{}", cfg.port).parse()?;
    tracing::info!("servidor escuchando en {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}
