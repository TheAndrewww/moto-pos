// commands/auth.rs — Comandos de autenticación para el POS
// Login por PIN, usuario+contraseña, sesiones y bitácora

use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::{Arc, Mutex};
use rusqlite::Connection;
use chrono::Utc;

/// Estado compartido de la aplicación
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
}

/// Datos del usuario autenticado
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UsuarioSesion {
    pub id: i64,
    pub nombre_completo: String,
    pub nombre_usuario: String,
    pub rol_id: i64,
    pub rol_nombre: String,
    pub es_admin: bool,
    pub sesion_id: i64,
    pub permisos: Vec<Permiso>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Permiso {
    pub modulo: String,
    pub accion: String,
    pub permitido: bool,
}

/// Resultado de login
#[derive(Serialize, Deserialize)]
pub struct LoginResult {
    pub ok: bool,
    pub usuario: Option<UsuarioSesion>,
    pub error: Option<String>,
}

/// Login con PIN de 4 dígitos (modo rápido del POS)
#[tauri::command]
pub fn login_pin(
    pin: String,
    state: State<'_, AppState>,
) -> LoginResult {
    let db = state.db.lock().unwrap();

    // Verificar que el PIN no esté vacío y sea numérico
    if pin.len() != 4 || !pin.chars().all(|c| c.is_ascii_digit()) {
        return LoginResult {
            ok: false,
            usuario: None,
            error: Some("PIN inválido".to_string()),
        };
    }


    // Buscar todos los usuarios activos y verificar PIN
    let mut stmt = db.prepare(
        r#"
        SELECT u.id, u.nombre_completo, u.nombre_usuario, u.pin, u.rol_id,
               r.nombre as rol_nombre, r.es_admin
        FROM usuarios u
        JOIN roles r ON r.id = u.rol_id
        WHERE u.activo = 1
        "#,
    ).unwrap();

    let usuarios: Vec<_> = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,  // pin_hash
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, bool>(6)?,
        ))
    }).unwrap().filter_map(|r| r.ok()).collect();

    // Verificar PIN contra bcrypt
    for (id, nombre_completo, nombre_usuario, pin_hash, rol_id, rol_nombre, es_admin) in usuarios {
        let pin_valido = bcrypt::verify(&pin, &pin_hash).unwrap_or(false);
        if pin_valido {
            // Actualizar último login
            let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let _ = db.execute(
                "UPDATE usuarios SET ultimo_login = ? WHERE id = ?",
                rusqlite::params![now, id],
            );

            // Crear sesión
            let sesion_id = db.execute(
                "INSERT INTO sesiones (usuario_id, origen) VALUES (?, 'POS')",
                rusqlite::params![id],
            ).map(|_| db.last_insert_rowid()).unwrap_or(0);

            // Cargar permisos
            let permisos = cargar_permisos(&db, rol_id);

            // Registrar en bitácora
            let _ = db.execute(
                r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
                   VALUES (?, 'LOGIN', 'sesiones', ?, ?, 'POS')"#,
                rusqlite::params![
                    id, sesion_id,
                    format!("Inicio de sesión (PIN): {}", nombre_usuario)
                ],
            );

            return LoginResult {
                ok: true,
                usuario: Some(UsuarioSesion {
                    id,
                    nombre_completo,
                    nombre_usuario,
                    rol_id,
                    rol_nombre,
                    es_admin,
                    sesion_id,
                    permisos,
                }),
                error: None,
            };
        }
    }

    LoginResult {
        ok: false,
        usuario: None,
        error: Some("PIN incorrecto".to_string()),
    }
}

/// Login con usuario + contraseña (primera vez o cambio de sesión)
#[tauri::command]
pub fn login_password(
    nombre_usuario: String,
    password: String,
    state: State<'_, AppState>,
) -> LoginResult {
    let db = state.db.lock().unwrap();

    let result = db.query_row(
        r#"
        SELECT u.id, u.nombre_completo, u.nombre_usuario, u.password_hash, u.rol_id,
               r.nombre as rol_nombre, r.es_admin
        FROM usuarios u
        JOIN roles r ON r.id = u.rol_id
        WHERE u.nombre_usuario = ? AND u.activo = 1
        "#,
        rusqlite::params![nombre_usuario],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, bool>(6)?,
            ))
        },
    );

    match result {
        Ok((id, nombre_completo, nombre_usuario_db, password_hash, rol_id, rol_nombre, es_admin)) => {
            let password_valido = bcrypt::verify(&password, &password_hash).unwrap_or(false);
            if !password_valido {
                return LoginResult {
                    ok: false,
                    usuario: None,
                    error: Some("Contraseña incorrecta".to_string()),
                };
            }

            let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let _ = db.execute(
                "UPDATE usuarios SET ultimo_login = ? WHERE id = ?",
                rusqlite::params![now, id],
            );

            let sesion_id = db.execute(
                "INSERT INTO sesiones (usuario_id, origen) VALUES (?, 'POS')",
                rusqlite::params![id],
            ).map(|_| db.last_insert_rowid()).unwrap_or(0);

            let permisos = cargar_permisos(&db, rol_id);

            let _ = db.execute(
                r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
                   VALUES (?, 'LOGIN', 'sesiones', ?, ?, 'POS')"#,
                rusqlite::params![
                    id, sesion_id,
                    format!("Inicio de sesión (contraseña): {}", nombre_usuario_db)
                ],
            );

            LoginResult {
                ok: true,
                usuario: Some(UsuarioSesion {
                    id,
                    nombre_completo,
                    nombre_usuario: nombre_usuario_db,
                    rol_id,
                    rol_nombre,
                    es_admin,
                    sesion_id,
                    permisos,
                }),
                error: None,
            }
        }
        Err(_) => LoginResult {
            ok: false,
            usuario: None,
            error: Some("Usuario no encontrado".to_string()),
        },
    }
}

/// Cerrar sesión del usuario actual
#[tauri::command]
pub fn logout(
    usuario_id: i64,
    sesion_id: i64,
    nombre_usuario: String,
    state: State<'_, AppState>,
) -> bool {
    let db = state.db.lock().unwrap();
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let _ = db.execute(
        "UPDATE sesiones SET fin = ? WHERE id = ?",
        rusqlite::params![now, sesion_id],
    );

    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
           VALUES (?, 'LOGOUT', 'sesiones', ?, ?, 'POS')"#,
        rusqlite::params![
            usuario_id, sesion_id,
            format!("Cierre de sesión: {}", nombre_usuario)
        ],
    );

    true
}

/// Verificar PIN del dueño para autorizar operaciones sensibles
#[tauri::command]
pub fn verificar_pin_dueno(
    pin: String,
    state: State<'_, AppState>,
) -> bool {
    let db = state.db.lock().unwrap();

    // Buscar usuarios con rol de dueño (es_admin = 1)
    let mut stmt = db.prepare(
        "SELECT u.pin FROM usuarios u JOIN roles r ON r.id = u.rol_id WHERE r.es_admin = 1 AND u.activo = 1"
    ).unwrap();

    let pin_hashes: Vec<String> = stmt.query_map([], |row| {
        row.get::<_, String>(0)
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect();

    pin_hashes.iter().any(|pin_hash| bcrypt::verify(&pin, pin_hash).unwrap_or(false))
}

/// Resolver el ID del dueño que coincide con el PIN — para guardar `autorizado_por`.
#[tauri::command]
pub fn resolver_dueno_por_pin(
    pin: String,
    state: State<'_, AppState>,
) -> Option<i64> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT u.id, u.pin FROM usuarios u JOIN roles r ON r.id = u.rol_id WHERE r.es_admin = 1 AND u.activo = 1"
    ).ok()?;
    let rows: Vec<(i64, String)> = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    }).ok()?.filter_map(|r| r.ok()).collect();
    rows.into_iter()
        .find(|(_, hash)| bcrypt::verify(&pin, hash).unwrap_or(false))
        .map(|(id, _)| id)
}

/// Crear el primer usuario dueño (setup inicial)
#[tauri::command]
pub fn crear_usuario_inicial(
    nombre_completo: String,
    nombre_usuario: String,
    pin: String,
    password: String,
    state: State<'_, AppState>,
) -> Result<i64, String> {
    let db = state.db.lock().unwrap();

    // Solo se puede usar si no existen usuarios
    let count: i64 = db.query_row(
        "SELECT COUNT(*) FROM usuarios", [], |row| row.get(0)
    ).unwrap_or(1);

    if count > 0 {
        return Err("Ya existen usuarios en el sistema".to_string());
    }

    // Hashear PIN y contraseña
    let pin_hash = bcrypt::hash(&pin, bcrypt::DEFAULT_COST)
        .map_err(|e| e.to_string())?;
    let password_hash = bcrypt::hash(&password, bcrypt::DEFAULT_COST)
        .map_err(|e| e.to_string())?;

    db.execute(
        r#"INSERT INTO usuarios (nombre_completo, nombre_usuario, pin, password_hash, rol_id)
           VALUES (?, ?, ?, ?, 1)"#,
        rusqlite::params![nombre_completo, nombre_usuario, pin_hash, password_hash],
    ).map_err(|e| e.to_string())?;

    let id = db.last_insert_rowid();

    // Registrar en bitácora
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
           VALUES (?, 'USUARIO_CREADO', 'usuarios', ?, ?, 'POS')"#,
        rusqlite::params![id, id, format!("Usuario inicial creado: {}", nombre_usuario)],
    );

    Ok(id)
}

/// Cargar permisos de un rol
fn cargar_permisos(db: &Connection, rol_id: i64) -> Vec<Permiso> {
    let mut stmt = db.prepare(
        "SELECT modulo, accion, permitido FROM permisos WHERE rol_id = ?"
    ).unwrap();

    stmt.query_map(rusqlite::params![rol_id], |row| {
        Ok(Permiso {
            modulo: row.get(0)?,
            accion: row.get(1)?,
            permitido: row.get::<_, bool>(2)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}
