// commands/respaldos.rs — Respaldos automáticos y manuales de la BD SQLite
// - Crear respaldo (VACUUM INTO — copia atómica y compactada)
// - Listar respaldos existentes
// - Restaurar respaldo (usa rusqlite::backup para restaurar páginas en vivo)
// - Auto-respaldo al arrancar si no se hizo uno hoy

use chrono::Local;
use rusqlite::backup::Backup;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

use super::auth::AppState;

const RETENCION_MAX: usize = 30;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Respaldo {
    pub nombre: String,
    pub ruta: String,
    pub tamanio_bytes: u64,
    pub created_at: String,
}

fn backups_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir()
        .map_err(|e| format!("No se pudo obtener app_data_dir: {e}"))?;
    let dir = base.join("backups");
    fs::create_dir_all(&dir).map_err(|e| format!("No se pudo crear carpeta de respaldos: {e}"))?;
    Ok(dir)
}

fn db_path(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir()
        .map_err(|e| format!("No se pudo obtener app_data_dir: {e}"))?;
    Ok(base.join("pos_database.db"))
}

fn listar_archivos(dir: &PathBuf) -> Vec<Respaldo> {
    let mut resultado = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else { return resultado; };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("db") { continue; }
        let nombre = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if !nombre.starts_with("pos_backup_") { continue; }
        let meta = match entry.metadata() { Ok(m) => m, Err(_) => continue };
        let tamanio = meta.len();
        let created = meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| {
                let secs = d.as_secs() as i64;
                chrono::DateTime::<Local>::from(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64)
                ).format("%Y-%m-%d %H:%M:%S").to_string()
            })
            .unwrap_or_default();
        resultado.push(Respaldo {
            nombre,
            ruta: path.to_string_lossy().to_string(),
            tamanio_bytes: tamanio,
            created_at: created,
        });
    }
    resultado.sort_by(|a, b| b.nombre.cmp(&a.nombre));
    resultado
}

fn rotar(dir: &PathBuf) {
    let archivos = listar_archivos(dir);
    for r in archivos.iter().skip(RETENCION_MAX) {
        let _ = fs::remove_file(&r.ruta);
    }
}

fn crear_respaldo_en(dir: &PathBuf, db: &Connection) -> Result<Respaldo, String> {
    let stamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let nombre = format!("pos_backup_{stamp}.db");
    let destino = dir.join(&nombre);

    db.execute(
        &format!("VACUUM INTO '{}'", destino.to_string_lossy().replace('\'', "''")),
        [],
    ).map_err(|e| format!("Error al crear respaldo: {e}"))?;

    let meta = fs::metadata(&destino).map_err(|e| e.to_string())?;
    Ok(Respaldo {
        nombre,
        ruta: destino.to_string_lossy().to_string(),
        tamanio_bytes: meta.len(),
        created_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

#[tauri::command]
pub fn crear_respaldo(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Respaldo, String> {
    let dir = backups_dir(&app)?;
    let db = state.db.lock().unwrap();
    let r = crear_respaldo_en(&dir, &db)?;
    drop(db);
    rotar(&dir);
    Ok(r)
}

#[tauri::command]
pub fn listar_respaldos(app: AppHandle) -> Result<Vec<Respaldo>, String> {
    let dir = backups_dir(&app)?;
    Ok(listar_archivos(&dir))
}

#[tauri::command]
pub fn restaurar_respaldo(
    ruta: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let backup_path = PathBuf::from(&ruta);
    if !backup_path.exists() {
        return Err("El archivo de respaldo no existe".into());
    }

    // 1) Crear respaldo de seguridad del estado actual antes de restaurar
    let dir = backups_dir(&app)?;
    {
        let db = state.db.lock().unwrap();
        let _ = crear_respaldo_en(&dir, &db); // best-effort
    }

    // 2) Restaurar vía backup API (reemplaza páginas in-place)
    let src = Connection::open(&backup_path)
        .map_err(|e| format!("No se pudo abrir el respaldo: {e}"))?;
    let mut db = state.db.lock().unwrap();
    let backup = Backup::new(&src, &mut *db)
        .map_err(|e| format!("No se pudo iniciar la restauración: {e}"))?;
    backup.run_to_completion(500, Duration::from_millis(100), None)
        .map_err(|e| format!("Error durante la restauración: {e}"))?;

    Ok(())
}

/// Se llama al iniciar la app. Solo crea respaldo si no hay uno de hoy
/// y si el respaldo automático está activado en config_negocio.
#[tauri::command]
pub fn respaldo_auto_si_necesario(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<Respaldo>, String> {
    // Verificar si el respaldo automático está activado
    {
        let db = state.db.lock().unwrap();
        let activo: i64 = db.query_row(
            "SELECT respaldo_auto_activo FROM config_negocio WHERE id = 1",
            [],
            |r| r.get(0),
        ).unwrap_or(1);
        if activo == 0 {
            log::info!("Respaldo automático desactivado en config_negocio");
            return Ok(None);
        }
    }

    let dir = backups_dir(&app)?;
    let hoy = Local::now().format("%Y%m%d").to_string();
    let prefijo_hoy = format!("pos_backup_{hoy}_");
    let archivos = listar_archivos(&dir);
    if archivos.iter().any(|r| r.nombre.starts_with(&prefijo_hoy)) {
        return Ok(None);
    }
    let db = state.db.lock().unwrap();
    let r = crear_respaldo_en(&dir, &db)?;
    drop(db);
    rotar(&dir);
    log::info!("Respaldo automático creado: {}", r.nombre);
    Ok(Some(r))
}

/// Helper llamado desde `setup()` de Tauri al arranque.
/// Idéntico a `respaldo_auto_si_necesario` pero sin el tipo State (usa la Mutex<Connection> directo).
pub fn respaldo_auto_startup(app: &AppHandle, db: &Connection, backups_dir_path: &PathBuf) -> Result<(), String> {
    // Check toggle
    let activo: i64 = db.query_row(
        "SELECT respaldo_auto_activo FROM config_negocio WHERE id = 1",
        [],
        |r| r.get(0),
    ).unwrap_or(1);
    if activo == 0 { return Ok(()); }

    let hoy = Local::now().format("%Y%m%d").to_string();
    let prefijo_hoy = format!("pos_backup_{hoy}_");
    let archivos = listar_archivos(backups_dir_path);
    if archivos.iter().any(|r| r.nombre.starts_with(&prefijo_hoy)) {
        return Ok(());
    }

    let r = crear_respaldo_en(backups_dir_path, db)?;
    rotar(backups_dir_path);
    log::info!("Respaldo automático de arranque creado: {}", r.nombre);
    let _ = app; // suprime unused warning si no se usa
    Ok(())
}

/// Obtiene la ruta de la carpeta de respaldos (expuesto para `setup`).
pub fn obtener_backups_dir(app: &AppHandle) -> Result<PathBuf, String> {
    backups_dir(app)
}

#[tauri::command]
pub fn obtener_info_bd(app: AppHandle) -> Result<u64, String> {
    let p = db_path(&app)?;
    let meta = fs::metadata(&p).map_err(|e| e.to_string())?;
    Ok(meta.len())
}
