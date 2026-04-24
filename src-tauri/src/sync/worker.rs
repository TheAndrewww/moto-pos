// sync/worker.rs — Loop periódico de sincronización (push + pull).
//
// Arranca como tokio task al inicio de la app. Cada INTERVALO:
//   1. Si sync_state.activo == 1 y hay remote_url/token → procede.
//   2. Empuja hasta BATCH_PUSH filas del outbox.
//   3. Jala cambios del remoto desde last_pull_cursor.
//   4. Registra resultado en sync_state.
//
// Nunca bloquea el hilo principal. Errores solo logean (no panic).

use std::sync::{Arc, Mutex};
use std::time::Duration;
use rusqlite::Connection;
use serde_json::json;

use super::{client::{RemoteClient, PushBody, CambioOut}, outbox, state, apply, payload};

const INTERVALO: Duration = Duration::from_secs(30);
const BATCH_PUSH: i64 = 100;
const BATCH_PULL_LIMITE_DIAS_CLEANUP: i64 = 7;

pub fn arrancar(db: Arc<Mutex<Connection>>) {
    tauri::async_runtime::spawn(async move {
        // Instalar el crypto provider de rustls para reqwest
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Jitter inicial para no arrancar exactamente al boot
        tokio::time::sleep(Duration::from_secs(5)).await;

        loop {
            if let Err(e) = ciclo(&db).await {
                log::warn!("sync ciclo falló: {}", e);
            }
            tokio::time::sleep(INTERVALO).await;
        }
    });
}

async fn ciclo(db: &Arc<Mutex<Connection>>) -> Result<(), String> {
    // 1. Leer configuración de sync
    let cfg = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        state::leer(&conn).map_err(|e| e.to_string())?
    };
    let Some(cfg) = cfg else { return Ok(()); };
    if !cfg.activo { return Ok(()); }
    let (Some(url), Some(token)) = (cfg.remote_url.clone(), cfg.remote_token.clone()) else {
        return Ok(());
    };

    let client = RemoteClient::new(&url, &token)?;

    // 2. Push pendientes
    if let Err(e) = push(db, &client, &cfg).await {
        log::warn!("sync push error: {}", e);
    }

    // 3. Pull nuevos
    if let Err(e) = pull(db, &client, &cfg).await {
        log::warn!("sync pull error: {}", e);
    }

    // 4. Limpieza periódica del outbox
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let _ = outbox::limpiar_antiguos(&conn, BATCH_PULL_LIMITE_DIAS_CLEANUP);
    }

    Ok(())
}

async fn push(
    db: &Arc<Mutex<Connection>>,
    client: &RemoteClient,
    cfg: &state::SyncConfig,
) -> Result<(), String> {
    // Leer pendientes + construir payloads (bajo lock corto)
    let (pendientes, cambios_out) = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let pendientes = outbox::pendientes(&conn, BATCH_PUSH).map_err(|e| e.to_string())?;
        if pendientes.is_empty() { return Ok(()); }

        let mut cambios = Vec::with_capacity(pendientes.len());
        for p in &pendientes {
            let (data, children) = if p.operacion == "DELETE" {
                (json!({ "uuid": p.uuid, "deleted": true }), None)
            } else {
                match payload::construir_payload(&conn, &p.tabla, &p.uuid) {
                    Ok(Some(mut v)) => {
                        let children = if let Some(obj) = v.as_object_mut() {
                            obj.remove("__children")
                        } else { None };
                        (v, children)
                    }
                    Ok(None) => continue,  // fila ya no existe — nada que enviar
                    Err(e) => {
                        let conn2 = db.lock().map_err(|e| e.to_string())?;
                        let _ = outbox::marcar_error(&conn2, p.id, &format!("payload: {}", e));
                        continue;
                    }
                }
            };
            cambios.push(CambioOut {
                tabla: p.tabla.clone(),
                operacion: p.operacion.clone(),
                data,
                children,
            });
        }
        (pendientes, cambios)
    };

    if cambios_out.is_empty() { return Ok(()); }

    let body = PushBody {
        device_uuid: cfg.device_uuid.clone(),
        sucursal_id: cfg.sucursal_id,
        cambios: cambios_out,
    };

    match client.push(&body).await {
        Ok(resp) => {
            let aceptados_set: std::collections::HashSet<&String> = resp.aceptados.iter().collect();
            let ids_ok: Vec<i64> = pendientes.iter()
                .filter(|p| aceptados_set.contains(&p.uuid))
                .map(|p| p.id)
                .collect();

            let conn = db.lock().map_err(|e| e.to_string())?;
            outbox::marcar_sincronizados(&conn, &ids_ok).map_err(|e| e.to_string())?;

            for r in &resp.rechazados {
                if let Some(p) = pendientes.iter().find(|p| &p.uuid == &r.uuid) {
                    let _ = outbox::marcar_error(&conn, p.id, &r.motivo);
                }
            }
            state::actualizar_push_at(&conn).map_err(|e| e.to_string())?;
            log::info!("sync push: {} aceptados, {} rechazados", ids_ok.len(), resp.rechazados.len());
        }
        Err(e) => {
            let conn = db.lock().map_err(|e| e.to_string())?;
            for p in &pendientes {
                let _ = outbox::marcar_error(&conn, p.id, &e);
            }
            return Err(e);
        }
    }
    Ok(())
}

async fn pull(
    db: &Arc<Mutex<Connection>>,
    client: &RemoteClient,
    cfg: &state::SyncConfig,
) -> Result<(), String> {
    let cursor = cfg.last_pull_cursor.clone().unwrap_or_default();
    let resp = client.pull(&cursor, cfg.sucursal_id).await?;
    if resp.cambios.is_empty() {
        let conn = db.lock().map_err(|e| e.to_string())?;
        state::actualizar_pull(&conn, &resp.next_cursor).map_err(|e| e.to_string())?;
        return Ok(());
    }

    let aplicados = {
        let mut conn = db.lock().map_err(|e| e.to_string())?;
        apply::aplicar_lote(&mut conn, &resp.cambios).map_err(|e| e.to_string())?
    };

    let conn = db.lock().map_err(|e| e.to_string())?;
    state::actualizar_pull(&conn, &resp.next_cursor).map_err(|e| e.to_string())?;
    log::info!("sync pull: {} cambios aplicados, cursor={}", aplicados, resp.next_cursor);

    // Si hay más, haremos pull adicional en el siguiente ciclo
    Ok(())
}
