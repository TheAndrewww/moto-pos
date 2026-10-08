// commands/sync_remoto.rs — Comandos Tauri para configurar/monitorear el sync remoto.

use serde::{Deserialize, Serialize};
use tauri::State;
use crate::commands::auth::AppState;
use crate::sync::{state as sstate, outbox, inbox, tareas, worker, client::RemoteClient};
use crate::sync::state::TareasSync;

#[derive(Debug, Serialize)]
pub struct EstadoSync {
    pub activo: bool,
    pub remote_url: Option<String>,
    pub device_uuid: String,
    pub sucursal_id: i64,
    pub last_push_at: Option<String>,
    pub last_pull_at: Option<String>,
    pub pendientes: i64,
    /// Cambios recibidos del servidor que esperan en la bandeja (sync_inbox).
    pub recibidos_pendientes: i64,
    /// Tareas únicas del sync v2 (mapa de ids, reparación, re-descarga).
    pub tareas: TareasSync,
    pub replay_pendiente: bool,
}

#[tauri::command]
pub fn obtener_estado_sync(state: State<AppState>) -> Result<EstadoSync, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let cfg = sstate::leer(&conn).map_err(|e| e.to_string())?
        .ok_or_else(|| "sync_state no existe".to_string())?;
    let pendientes = outbox::contar_pendientes(&conn).map_err(|e| e.to_string())?;
    let recibidos_pendientes = inbox::contar(&conn).unwrap_or(0);
    let tareas = sstate::leer_tareas(&conn).unwrap_or_default();
    Ok(EstadoSync {
        activo: cfg.activo,
        remote_url: cfg.remote_url,
        device_uuid: cfg.device_uuid,
        sucursal_id: cfg.sucursal_id,
        last_push_at: cfg.last_push_at,
        last_pull_at: cfg.last_pull_at,
        pendientes,
        recibidos_pendientes,
        replay_pendiente: tareas.replay_pendiente(),
        tareas,
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

    // 2. Persistir en sync_state + resetear el outbox: cualquier fila que
    //    estaba fallando con 401 ahora puede reintentarse con el token
    //    fresco. Sin esto, el usuario tendría que ir a "Reintentar
    //    fallidos" manualmente después de reconfigurar — confuso porque
    //    intuitivamente "guardar credenciales" debería ser suficiente.
    {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        sstate::guardar_credenciales(
            &conn,
            input.remote_url.trim_end_matches('/'),
            &login.token,
            sucursal_final,
        ).map_err(|e| e.to_string())?;

        // Resetear intentos/error de todas las pendientes — el token nuevo
        // probablemente ya no falle por 401.
        let n = crate::sync::outbox::reintentar_fallidos(&conn)
            .map_err(|e| e.to_string())?;
        if n > 0 {
            log::info!("configurar_sync: {} filas pendientes resetearon su estado de error", n);
        }
    }

    obtener_estado_sync(state)
}

#[tauri::command]
pub fn desactivar_sync(state: State<AppState>) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    sstate::desactivar(&conn).map_err(|e| e.to_string())
}

/// Compara la hora local de la computadora contra la del servidor remoto.
/// Devuelve el desfase en segundos (positivo = la PC va ADELANTADA,
/// negativo = la PC va ATRASADA). `None` si no hay sync configurado o el
/// servidor no responde (sin red no podemos saber — no alarmar).
///
/// Contexto: el POS registra ventas con la hora de la PC. Si el reloj se
/// desconfigura (pila CMOS, cambio manual, zona horaria), las ventas se
/// guardan con fecha equivocada y "desaparecen" de las vistas del día.
/// Caso real: PC con 12h28m de atraso → todas las ventas de la mañana
/// quedaron fechadas el día anterior.
#[tauri::command]
pub async fn verificar_hora_sistema(state: State<'_, AppState>) -> Result<Option<i64>, String> {
    let url = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        match sstate::leer(&conn).map_err(|e| e.to_string())? {
            Some(cfg) => cfg.remote_url,
            None => None,
        }
    };
    let Some(url) = url else { return Ok(None) };

    let http = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    let time_url = format!("{}/time", url.trim_end_matches('/'));
    let resp = match http.get(&time_url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return Ok(None), // sin red o server viejo sin /time: no alarmar
    };

    #[derive(serde::Deserialize)]
    struct TimeResp { epoch: i64 }
    let t: TimeResp = match resp.json().await {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };

    let local_epoch = chrono::Utc::now().timestamp();
    Ok(Some(local_epoch - t.epoch))
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
    // (stock_sucursal salió del sync en v2: nadie la lee.)
    let tablas: &[&str] = &[
        "productos", "proveedores", "clientes", "usuarios", "categorias",
        "sucursales",
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

/// Fuerza un ciclo de sync (push + pull) inmediatamente, sin esperar al
/// intervalo de 30s del worker. Útil cuando la cola está atorada o cuando
/// el usuario quiere ver datos en el servidor de inmediato. Nunca corre junto
/// con el ciclo periódico: espera a que termine el que está en curso (hasta
/// 30 s) o devuelve "Ya hay una sincronización en curso…".
#[tauri::command]
pub async fn forzar_sync_ahora(state: State<'_, AppState>) -> Result<(), String> {
    let db = state.db.clone();
    crate::sync::worker::forzar_ciclo(db).await
}

/// Devuelve hasta `limite` filas del outbox que han fallado al sincronizar
/// (intentos > 0). Ordenadas por intentos DESC para mostrar primero las
/// más problemáticas. La UI puede mostrar `ultimo_error` para diagnóstico.
/// Después vienen los cambios recibidos que esperan en la bandeja de
/// entrada (`fuente = 'inbox'`, sin payload).
#[tauri::command]
pub fn listar_errores_outbox(
    limite: Option<i64>,
    state: State<AppState>,
) -> Result<Vec<crate::sync::outbox::OutboxErrorFila>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    crate::sync::outbox::errores_con_bandeja(&conn, limite.unwrap_or(50))
        .map_err(|e| e.to_string())
}

/// Resetea intentos y errores de TODAS las filas pendientes fallidas. El
/// próximo ciclo del worker las reintentará. Útil cuando el problema que
/// causaba los errores ya se resolvió (e.g. ahora hay red, ahora existe
/// la fila referenciada en postgres, etc.).
#[tauri::command]
pub fn reintentar_errores_outbox(state: State<AppState>) -> Result<i64, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let n = crate::sync::outbox::reintentar_fallidos(&conn)
        .map_err(|e| e.to_string())?;
    log::info!("reintentar_errores_outbox: {} filas reseteadas", n);
    Ok(n as i64)
}

/// Marca filas específicas como sincronizadas SIN enviarlas. Operación
/// destructiva — el dato local se mantiene pero el servidor nunca lo verá.
/// Solo úsalo para descartar filas que bloquean la cola y cuya pérdida en
/// el servidor es aceptable.
#[tauri::command]
pub fn descartar_filas_outbox(
    ids: Vec<i64>,
    state: State<AppState>,
) -> Result<i64, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let n = crate::sync::outbox::descartar(&conn, &ids)
        .map_err(|e| e.to_string())?;
    log::info!("descartar_filas_outbox: {} filas descartadas", n);
    Ok(n as i64)
}

// ─── Sync v2: reparación de referencias y diagnóstico ─────

fn cliente_y_device(state: &State<'_, AppState>) -> Result<(RemoteClient, String), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let cfg = sstate::leer(&conn).map_err(|e| e.to_string())?
        .ok_or("sync_state no existe")?;
    let (Some(url), Some(token)) = (cfg.remote_url, cfg.remote_token) else {
        return Err("La sincronización no está configurada.".to_string());
    };
    Ok((RemoteClient::new(&url, &token)?, cfg.device_uuid))
}

/// Sube al servidor el mapa id local → uuid de las tablas con referencias
/// (en tandas de 5000). El servidor lo usa para traducir los ids de este
/// equipo. Se puede repetir sin problema.
#[tauri::command]
pub async fn subir_mapa_ids(state: State<'_, AppState>) -> Result<tareas::ReporteMapa, String> {
    let (client, device) = cliente_y_device(&state)?;
    let db = state.db.clone();
    // Nunca junto con un ciclo de sync (que también corre las tareas únicas).
    let _turno = worker::tomar_turno(worker::ESPERA_TURNO).await?;
    tareas::subir_mapa_ids(&db, &client, &device).await.map_err(|e| e.mensaje())
}

/// Repara las referencias (usuario, producto, venta…) entre este equipo y el
/// servidor: sube el mapa de ids, pide al servidor que corrija las filas de
/// este equipo y corrige aquí las FK de las filas que se hicieron en la web
/// (solo columnas de referencia; nunca montos ni cortes). Devuelve el
/// reporte. Después el worker re-descarga las filas web faltantes.
#[tauri::command]
pub async fn reparar_referencias(state: State<'_, AppState>) -> Result<tareas::ReporteReparacion, String> {
    let (client, device) = cliente_y_device(&state)?;
    let db = state.db.clone();
    // Nunca junto con un ciclo de sync (que también puede estar reparando).
    let _turno = worker::tomar_turno(worker::ESPERA_TURNO).await?;
    tareas::reparar_referencias(&db, &client, &device, true).await.map_err(|e| e.mensaje())
}

#[derive(Debug, Serialize)]
pub struct TablaDiagnostico {
    pub tabla: String,
    /// "nombre TIPO [NOT NULL] [DEFAULT x]" por columna; vacío si la tabla no existe.
    pub columnas: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticoSync {
    pub version_bd: i64,
    /// Filas en sync_suppress_flag (debe ser 0 fuera de un apply).
    pub bandera_supresion: i64,
    pub outbox_pendientes: i64,
    pub outbox_con_error: i64,
    pub errores_outbox: Vec<outbox::OutboxErrorFila>,
    pub bandeja: Vec<inbox::ConteoMotivo>,
    pub tareas: TareasSync,
    pub tablas: Vec<TablaDiagnostico>,
}

/// Diagnóstico del sync: esquema local de las tablas sincronizadas, errores
/// del outbox, conteo de la bandeja de entrada y estado de la bandera de
/// supresión de triggers. Solo lectura.
#[tauri::command]
pub fn diagnostico_sync(state: State<AppState>) -> Result<DiagnosticoSync, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    diagnostico(&conn).map_err(|e| e.to_string())
}

pub(crate) fn diagnostico(conn: &rusqlite::Connection) -> rusqlite::Result<DiagnosticoSync> {
    let version_bd: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let bandera_supresion: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_suppress_flag", [], |r| r.get(0))
        .unwrap_or(-1);
    let outbox_pendientes = outbox::contar_pendientes(conn)?;
    let outbox_con_error: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_outbox WHERE synced_at IS NULL AND intentos > 0",
        [],
        |r| r.get(0),
    )?;
    let errores_outbox = outbox::errores(conn, 20)?;
    let bandeja = inbox::contar_por_motivo(conn).unwrap_or_default();
    let tareas = sstate::leer_tareas(conn).unwrap_or_default();

    let mut nombres: Vec<&str> = crate::sync::fk::FK_MAP.iter().map(|(t, _)| *t).collect();
    for (_, hijos) in crate::sync::fk::AGGREGATES {
        for (h, _) in *hijos {
            if !nombres.contains(h) {
                nombres.push(h);
            }
        }
    }
    nombres.extend(["sync_outbox", "sync_inbox", "sync_state"]);
    let mut tablas = Vec::with_capacity(nombres.len());
    for t in nombres {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({t})"))?;
        let columnas: Vec<String> = stmt
            .query_map([], |r| {
                let nombre: String = r.get(1)?;
                let tipo: Option<String> = r.get(2)?;
                let notnull: i64 = r.get(3)?;
                let dflt: Option<String> = r.get(4)?;
                let mut s = format!("{} {}", nombre, tipo.unwrap_or_default());
                if notnull != 0 { s.push_str(" NOT NULL"); }
                if let Some(d) = dflt { s.push_str(&format!(" DEFAULT {d}")); }
                Ok(s.trim().to_string())
            })?
            .collect::<rusqlite::Result<_>>()?;
        tablas.push(TablaDiagnostico { tabla: t.to_string(), columnas });
    }

    Ok(DiagnosticoSync {
        version_bd,
        bandera_supresion,
        outbox_pendientes,
        outbox_con_error,
        errores_outbox,
        bandeja,
        tareas,
        tablas,
    })
}
