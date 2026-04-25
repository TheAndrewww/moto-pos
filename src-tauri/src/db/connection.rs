// db/connection.rs — Gestión de la conexión SQLite
// Inicializa la BD, aplica schema y seed data

use rusqlite::{Connection, Result};
use std::path::Path;
use std::fs;
use chrono::{Local, DateTime, Duration as ChronoDuration};

use super::schema::{SCHEMA_V1, SEED_DATA};
use super::migrations::aplicar_migraciones;

const VACUUM_INTERVAL_DAYS: i64 = 7;

/// Inicializa la base de datos SQLite en la ruta indicada.
/// Aplica PRAGMA de rendimiento, crea tablas e inserta datos iniciales.
pub fn init_database(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;

    // Aplicar schema (crea tablas e índices)
    conn.execute_batch(SCHEMA_V1)?;

    // IMPORTANTE: las migraciones DEBEN correr antes de SEED_DATA.
    // SEED_DATA hace INSERT OR IGNORE en tablas sincronizadas (ej. categorias);
    // esos INSERT disparan triggers que asumen el esquema migrado (uuid,
    // updated_at). Si el esquema todavía no está reparado, el seed truena.
    aplicar_migraciones(&conn)?;

    // Insertar datos iniciales (después de migraciones)
    conn.execute_batch(SEED_DATA)?;

    // VACUUM semanal: compactar BD si han pasado >= 7 días
    vacuum_si_toca(&conn, db_path);

    // Crear usuario dueño default si no hay ningún usuario
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM usuarios", [], |row| row.get(0),
    )?;

    if count == 0 {
        log::info!("No hay usuarios — creando usuario dueño default (PIN: 1234)");
        let pin_hash = bcrypt::hash("1234", 10).expect("Error al hashear PIN default");
        let pass_hash = bcrypt::hash("admin", 10).expect("Error al hashear password default");

        conn.execute(
            r#"INSERT INTO usuarios (nombre_completo, nombre_usuario, pin, password_hash, rol_id, activo, created_at)
               VALUES ('Dueño', 'admin', ?, ?, 1, 1, datetime('now'))"#,
            rusqlite::params![pin_hash, pass_hash],
        )?;

        log::info!("Usuario dueño creado: admin / PIN: 1234 / Password: admin");
    }

    log::info!("Base de datos inicializada en: {:?}", db_path);
    Ok(conn)
}

/// Ejecuta VACUUM si han pasado >= VACUUM_INTERVAL_DAYS desde el último.
/// Usa un marker file junto a la BD para trackear la fecha.
fn vacuum_si_toca(conn: &Connection, db_path: &Path) {
    let marker = db_path.with_file_name("vacuum_last.txt");
    let ahora = Local::now();

    let ultima: Option<DateTime<Local>> = fs::read_to_string(&marker)
        .ok()
        .and_then(|s| DateTime::parse_from_rfc3339(s.trim()).ok())
        .map(|dt| dt.with_timezone(&Local));

    let toca = match ultima {
        Some(u) => ahora.signed_duration_since(u) >= ChronoDuration::days(VACUUM_INTERVAL_DAYS),
        None => true, // primera vez
    };

    if !toca { return; }

    log::info!("Ejecutando VACUUM semanal (última: {:?})", ultima);
    match conn.execute("VACUUM", []) {
        Ok(_) => {
            let _ = fs::write(&marker, ahora.to_rfc3339());
            log::info!("VACUUM completado");
        }
        Err(e) => log::warn!("VACUUM falló: {}", e),
    }
}
