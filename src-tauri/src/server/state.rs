// server/state.rs — Estado compartido del servidor HTTP

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use rusqlite::Connection;
use tokio::sync::RwLock;

/// Entrada de token de emparejamiento — vive en memoria, 5 min
#[derive(Clone, Debug)]
pub struct PairingEntry {
    pub token: String,
    pub expires_at: i64, // timestamp unix
}

/// Estado compartido del servidor Axum.
/// La conexión SQLite es la misma del AppState principal (arc-compartida).
#[derive(Clone)]
pub struct ServerState {
    pub db: Arc<Mutex<Connection>>,
    pub jwt_secret: Arc<Vec<u8>>,
    pub pairing_tokens: Arc<RwLock<HashMap<String, PairingEntry>>>,
    pub port: u16,
    pub cert_dir: PathBuf,
}

impl ServerState {
    /// Limpia tokens de emparejamiento expirados.
    pub async fn purge_expired_pairings(&self) {
        let now = chrono::Utc::now().timestamp();
        let mut map = self.pairing_tokens.write().await;
        map.retain(|_, entry| entry.expires_at > now);
    }
}
