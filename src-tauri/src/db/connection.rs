// db/connection.rs — Gestión de la conexión SQLite
// Inicializa la BD, aplica schema y seed data

use rusqlite::{Connection, Result};
use std::path::Path;

use super::schema::{SCHEMA_V1, SEED_DATA};
use super::migrations::aplicar_migraciones;

/// Inicializa la base de datos SQLite en la ruta indicada.
/// Aplica PRAGMA de rendimiento, crea tablas e inserta datos iniciales.
pub fn init_database(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;

    // Aplicar schema (crea tablas e índices)
    conn.execute_batch(SCHEMA_V1)?;

    // Insertar datos iniciales
    conn.execute_batch(SEED_DATA)?;

    // Aplicar migraciones incrementales (columnas nuevas, etc.)
    aplicar_migraciones(&conn)?;

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
