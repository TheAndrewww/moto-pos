// server/mod.rs — Servidor HTTPS embebido (Fase 3.1)
//
// Arranca en un tokio task cuando Tauri inicia. Expone endpoints REST
// autenticados por JWT que la PWA móvil consume por WiFi local.

pub mod auth;
pub mod error;
pub mod handlers;
pub mod state;
pub mod tls;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use rusqlite::Connection;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};

pub use state::ServerState;

pub struct ServerInfo {
    pub port: u16,
    pub ips: Vec<IpAddr>,
    pub state: ServerState,
}

/// Arranca el servidor HTTPS. Se llama desde tauri setup dentro de un tokio::spawn.
pub async fn start_server(
    db: Arc<Mutex<Connection>>,
    jwt_secret: Vec<u8>,
    cert_dir: PathBuf,
    pwa_dist_dir: Option<PathBuf>,
    starting_port: u16,
) -> Result<ServerInfo, String> {
    // Detectar IP LAN (todas las interfaces)
    let mut ips: Vec<IpAddr> = Vec::new();
    if let Ok(lan) = local_ip_address::local_ip() {
        ips.push(lan);
    }
    if let Ok(list) = local_ip_address::list_afinet_netifas() {
        for (_, ip) in list {
            if !ips.contains(&ip) && !ip.is_loopback() && ip.is_ipv4() {
                ips.push(ip);
            }
        }
    }

    // Generar/cargar certificado
    let tls_files = tls::ensure_cert(&cert_dir, &ips)?;
    let rustls_config = tls::load_rustls_config(&tls_files).await?;

    // Estado compartido
    let state = ServerState {
        db,
        jwt_secret: Arc::new(jwt_secret),
        pairing_tokens: Arc::new(RwLock::new(HashMap::new())),
        port: starting_port,
        cert_dir,
    };

    // Buscar puerto disponible
    let mut port = starting_port;
    let bound = loop {
        let addr: SocketAddr = ([0, 0, 0, 0], port).into();
        match std::net::TcpListener::bind(addr) {
            Ok(l) => { drop(l); break addr; }
            Err(_) if port < starting_port + 10 => { port += 1; }
            Err(e) => return Err(format!("No hay puerto disponible: {}", e)),
        }
    };
    log::info!("Servidor móvil en https://0.0.0.0:{} — IPs: {:?}", port, ips);

    // Construir router
    let app = build_router(state.clone(), pwa_dist_dir);

    // Actualizar puerto en state (clone para retorno)
    let mut info_state = state.clone();
    info_state.port = port;

    // Spawn del servidor — independiente del task que devuelve ServerInfo
    tokio::spawn(async move {
        if let Err(e) = axum_server::bind_rustls(bound, rustls_config)
            .serve(app.into_make_service())
            .await
        {
            log::error!("Servidor móvil terminó con error: {}", e);
        }
    });

    Ok(ServerInfo { port, ips, state: info_state })
}

fn build_router(state: ServerState, pwa_dist_dir: Option<PathBuf>) -> Router {
    // Rutas públicas (sin auth)
    let public = Router::new()
        .route("/api/health", get(handlers::health))
        .route("/api/pairing/redeem", post(handlers::pairing_redeem))
        .route("/api/auth/login_pin", post(handlers::login_pin));

    // Rutas protegidas (requieren JWT válido)
    let protected = Router::new()
        .route("/api/me", get(handlers::me))
        .route("/api/productos", get(handlers::productos_buscar))
        .route("/api/productos/por_codigo/:codigo", get(handlers::producto_por_codigo))
        .route("/api/proveedores", get(handlers::proveedores))
        .route("/api/ordenes_pedido", get(handlers::ordenes_listar))
        .route("/api/ordenes_pedido/:id", get(handlers::orden_detalle))
        .route("/api/recepciones", post(handlers::recepcion_crear))
        .layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let mut router = Router::new()
        .merge(public)
        .merge(protected)
        .layer(cors)
        .with_state(state);

    // Servir la PWA estática si el directorio existe
    if let Some(dir) = pwa_dist_dir {
        if dir.exists() {
            let serve = tower_http::services::ServeDir::new(dir.clone())
                .fallback(tower_http::services::ServeFile::new(dir.join("index.html")));
            router = router.fallback_service(serve);
        }
    }

    router
}
