// commands/sync_remoto.rs — Comandos Tauri para configurar/monitorear el sync remoto.

use serde::{Deserialize, Serialize};
use tauri::State;
use crate::commands::auth::AppState;
use crate::sync::{state as sstate, outbox, client::RemoteClient};

#[derive(Debug, Serialize)]
pub struct EstadoSync {
    pub activo: bool,
    pub remote_url: Option<String>,
    pub device_uuid: String,
    pub sucursal_id: i64,
    pub last_push_at: Option<String>,
    pub last_pull_at: Option<String>,
    pub pendientes: i64,
}

#[tauri::command]
pub fn obtener_estado_sync(state: State<AppState>) -> Result<EstadoSync, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let cfg = sstate::leer(&conn).map_err(|e| e.to_string())?
        .ok_or_else(|| "sync_state no existe".to_string())?;
    let pendientes = outbox::contar_pendientes(&conn).map_err(|e| e.to_string())?;
    Ok(EstadoSync {
        activo: cfg.activo,
        remote_url: cfg.remote_url,
        device_uuid: cfg.device_uuid,
        sucursal_id: cfg.sucursal_id,
        last_push_at: cfg.last_push_at,
        last_pull_at: cfg.last_pull_at,
        pendientes,
    })
}

#[derive(Debug, Deserialize)]
pub struct ConfigurarSyncInput {
    pub remote_url: String,
    pub email: String,
    pub password: String,
    pub sucursal_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    token: String,
    sucursal_id: i64,
}

#[tauri::command]
pub async fn configurar_sync(
    input: ConfigurarSyncInput,
    state: State<'_, AppState>,
) -> Result<EstadoSync, String> {
    // 1. Hacer login contra el remoto para obtener JWT
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let login_url = format!("{}/auth/login", input.remote_url.trim_end_matches('/'));
    let resp = http.post(&login_url)
        .json(&serde_json::json!({ "email": input.email, "password": input.password }))
        .send()
        .await
        .map_err(|e| format!("No se pudo conectar: {}", e))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Login rechazado ({}): {}", status, body));
    }
    let login: LoginResponse = resp.json().await.map_err(|e| format!("Login JSON: {}", e))?;

    let sucursal_final = input.sucursal_id.unwrap_or(login.sucursal_id);

    // 2. Persistir en sync_state
    {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        sstate::guardar_credenciales(
            &conn,
            input.remote_url.trim_end_matches('/'),
            &login.token,
            sucursal_final,
        ).map_err(|e| e.to_string())?;
    }

    obtener_estado_sync(state)
}

#[tauri::command]
pub fn desactivar_sync(state: State<AppState>) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    sstate::desactivar(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn probar_conexion_sync(state: State<'_, AppState>) -> Result<bool, String> {
    let (url, token) = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let cfg = sstate::leer(&conn).map_err(|e| e.to_string())?
            .ok_or("sync_state no existe")?;
        (cfg.remote_url, cfg.remote_token)
    };
    let (Some(url), Some(token)) = (url, token) else {
        return Ok(false);
    };
    let client = RemoteClient::new(&url, &token)?;
    Ok(client.health().await)
}
