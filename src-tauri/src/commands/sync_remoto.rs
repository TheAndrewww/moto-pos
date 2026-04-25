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

/// Encola TODOS los registros existentes de las tablas sincronizables en sync_outbox.
/// Usado tras configurar sync por primera vez para subir datos preexistentes
/// (los triggers solo capturan cambios futuros, no históricos).
/// Devuelve cuántas filas se encolaron en total.
#[tauri::command]
pub fn backfill_outbox(state: State<AppState>) -> Result<i64, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;

    // Mismas tablas que tienen triggers de outbox (ver migrations.rs::migracion_005_triggers_outbox)
    let tablas: &[&str] = &[
        "productos", "proveedores", "clientes", "usuarios", "categorias",
        "sucursales", "stock_sucursal",
        "ventas", "presupuestos", "ordenes_pedido", "recepciones",
        "cortes", "devoluciones", "transferencias",
        "movimientos_caja", "aperturas_caja",
    ];

    let mut total: i64 = 0;
    for tabla in tablas {
        // INSERT OR IGNORE para no duplicar entradas pendientes existentes.
        // La PK conflictual (tabla, uuid) WHERE synced_at IS NULL evita reabrir
        // entradas ya sincronizadas — pero para backfill queremos justamente
        // que se reabran si no se sincronizaron. Usamos INSERT con ON CONFLICT
        // que toca created_at para forzar reintento.
        let sql = format!(
            r#"
            INSERT INTO sync_outbox (tabla, uuid, operacion)
            SELECT '{tabla}', uuid, 'UPDATE'
              FROM {tabla}
             WHERE uuid IS NOT NULL
            ON CONFLICT(tabla, uuid) WHERE synced_at IS NULL
            DO UPDATE SET created_at = datetime('now'), intentos = 0, ultimo_error = NULL
            "#,
            tabla = tabla
        );
        match conn.execute(&sql, []) {
            Ok(n) => total += n as i64,
            Err(e) => {
                // Si la tabla no tiene columna uuid o no existe, ignoramos y seguimos
                log::warn!("backfill_outbox: tabla '{}' falló: {}", tabla, e);
            }
        }
    }

    log::info!("backfill_outbox: {} filas encoladas", total);
    Ok(total)
}
