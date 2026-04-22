// commands/usuarios.rs — Gestión de usuarios (solo dueño)

use serde::{Deserialize, Serialize};
use tauri::State;
use super::auth::AppState;
use chrono::Utc;

// ─── Structs ──────────────────────────────────────────────

#[derive(Serialize, Clone, Debug)]
pub struct UsuarioInfo {
    pub id: i64,
    pub nombre_completo: String,
    pub nombre_usuario: String,
    pub rol_id: i64,
    pub rol_nombre: String,
    pub es_admin: bool,
    pub activo: bool,
    pub ultimo_login: Option<String>,
    pub created_at: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct RolInfo {
    pub id: i64,
    pub nombre: String,
    pub es_admin: bool,
}

#[derive(Deserialize)]
pub struct NuevoUsuario {
    pub nombre_completo: String,
    pub nombre_usuario: String,
    pub pin: String,
    pub password: String,
    pub rol_id: i64,
}

#[derive(Deserialize)]
pub struct ActualizarUsuario {
    pub id: i64,
    pub nombre_completo: String,
    pub nombre_usuario: String,
    pub rol_id: i64,
    pub nuevo_pin: Option<String>,
    pub nuevo_password: Option<String>,
}

// ─── Comandos ─────────────────────────────────────────────

/// Listar todos los usuarios
#[tauri::command]
pub fn listar_usuarios(state: State<'_, AppState>) -> Vec<UsuarioInfo> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        r#"
        SELECT u.id, u.nombre_completo, u.nombre_usuario, u.rol_id,
               r.nombre, r.es_admin, u.activo, u.ultimo_login, u.created_at
        FROM usuarios u
        JOIN roles r ON r.id = u.rol_id
        ORDER BY u.nombre_completo
        "#,
    ).unwrap();

    stmt.query_map([], |row| {
        Ok(UsuarioInfo {
            id: row.get(0)?,
            nombre_completo: row.get(1)?,
            nombre_usuario: row.get(2)?,
            rol_id: row.get(3)?,
            rol_nombre: row.get(4)?,
            es_admin: row.get(5)?,
            activo: row.get(6)?,
            ultimo_login: row.get(7)?,
            created_at: row.get(8)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Listar roles disponibles
#[tauri::command]
pub fn listar_roles(state: State<'_, AppState>) -> Vec<RolInfo> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, nombre, es_admin FROM roles ORDER BY id"
    ).unwrap();

    stmt.query_map([], |row| {
        Ok(RolInfo {
            id: row.get(0)?,
            nombre: row.get(1)?,
            es_admin: row.get(2)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Crear un nuevo usuario
#[tauri::command]
pub fn crear_usuario(
    usuario: NuevoUsuario,
    admin_id: i64,
    state: State<'_, AppState>,
) -> Result<UsuarioInfo, String> {
    let db = state.db.lock().unwrap();

    // Verificar que no exista el nombre de usuario
    let existe: bool = db.query_row(
        "SELECT COUNT(*) > 0 FROM usuarios WHERE nombre_usuario = ?",
        rusqlite::params![usuario.nombre_usuario],
        |row| row.get(0),
    ).unwrap_or(false);

    if existe {
        return Err("Ya existe un usuario con ese nombre".to_string());
    }

    // Hash del PIN y contraseña
    let pin_hash = bcrypt::hash(&usuario.pin, 10)
        .map_err(|e| format!("Error al hashear PIN: {}", e))?;
    let password_hash = bcrypt::hash(&usuario.password, 10)
        .map_err(|e| format!("Error al hashear contraseña: {}", e))?;

    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    db.execute(
        r#"INSERT INTO usuarios (nombre_completo, nombre_usuario, pin, password_hash, rol_id, activo, created_at)
           VALUES (?, ?, ?, ?, ?, 1, ?)"#,
        rusqlite::params![
            usuario.nombre_completo, usuario.nombre_usuario,
            pin_hash, password_hash, usuario.rol_id, now
        ],
    ).map_err(|e| e.to_string())?;

    let id = db.last_insert_rowid();

    // Bitácora
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
           VALUES (?, 'USUARIO_CREADO', 'usuarios', ?, ?, 'POS')"#,
        rusqlite::params![
            admin_id, id,
            format!("Usuario creado: {} ({})", usuario.nombre_completo, usuario.nombre_usuario)
        ],
    );

    // Obtener info del rol
    let (rol_nombre, es_admin) = db.query_row(
        "SELECT nombre, es_admin FROM roles WHERE id = ?",
        rusqlite::params![usuario.rol_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
    ).unwrap_or(("Desconocido".to_string(), false));

    Ok(UsuarioInfo {
        id,
        nombre_completo: usuario.nombre_completo,
        nombre_usuario: usuario.nombre_usuario,
        rol_id: usuario.rol_id,
        rol_nombre,
        es_admin,
        activo: true,
        ultimo_login: None,
        created_at: now,
    })
}

/// Actualizar un usuario existente
#[tauri::command]
pub fn actualizar_usuario(
    usuario: ActualizarUsuario,
    admin_id: i64,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let db = state.db.lock().unwrap();
    let _now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Verificar que el nombre de usuario no esté duplicado
    let existe: bool = db.query_row(
        "SELECT COUNT(*) > 0 FROM usuarios WHERE nombre_usuario = ? AND id != ?",
        rusqlite::params![usuario.nombre_usuario, usuario.id],
        |row| row.get(0),
    ).unwrap_or(false);

    if existe {
        return Err("Ya existe otro usuario con ese nombre".to_string());
    }

    db.execute(
        "UPDATE usuarios SET nombre_completo = ?, nombre_usuario = ?, rol_id = ? WHERE id = ?",
        rusqlite::params![usuario.nombre_completo, usuario.nombre_usuario, usuario.rol_id, usuario.id],
    ).map_err(|e| e.to_string())?;

    // Actualizar PIN si se proporcionó
    if let Some(ref pin) = usuario.nuevo_pin {
        if !pin.is_empty() {
            let pin_hash = bcrypt::hash(pin, 10).map_err(|e| e.to_string())?;
            db.execute(
                "UPDATE usuarios SET pin = ? WHERE id = ?",
                rusqlite::params![pin_hash, usuario.id],
            ).map_err(|e| e.to_string())?;
        }
    }

    // Actualizar contraseña si se proporcionó
    if let Some(ref password) = usuario.nuevo_password {
        if !password.is_empty() {
            let pass_hash = bcrypt::hash(password, 10).map_err(|e| e.to_string())?;
            db.execute(
                "UPDATE usuarios SET password_hash = ? WHERE id = ?",
                rusqlite::params![pass_hash, usuario.id],
            ).map_err(|e| e.to_string())?;
        }
    }

    // Bitácora
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
           VALUES (?, 'USUARIO_EDITADO', 'usuarios', ?, ?, 'POS')"#,
        rusqlite::params![
            admin_id, usuario.id,
            format!("Usuario editado: {}", usuario.nombre_completo)
        ],
    );

    Ok(true)
}

/// Activar o desactivar un usuario
#[tauri::command]
pub fn toggle_usuario_activo(
    usuario_id: i64,
    activo: bool,
    admin_id: i64,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let db = state.db.lock().unwrap();

    // No permitir desactivar al propio admin
    if usuario_id == admin_id && !activo {
        return Err("No puedes desactivarte a ti mismo".to_string());
    }

    db.execute(
        "UPDATE usuarios SET activo = ? WHERE id = ?",
        rusqlite::params![activo as i32, usuario_id],
    ).map_err(|e| e.to_string())?;

    // Bitácora
    let nombre: String = db.query_row(
        "SELECT nombre_completo FROM usuarios WHERE id = ?",
        rusqlite::params![usuario_id],
        |row| row.get(0),
    ).unwrap_or_default();

    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
           VALUES (?, ?, 'usuarios', ?, ?, 'POS')"#,
        rusqlite::params![
            admin_id,
            if activo { "USUARIO_ACTIVADO" } else { "USUARIO_DESACTIVADO" },
            usuario_id,
            format!("Usuario {} {}", nombre, if activo { "activado" } else { "desactivado" })
        ],
    );

    Ok(true)
}
