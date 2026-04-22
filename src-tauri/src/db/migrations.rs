// db/migrations.rs — Migraciones incrementales usando PRAGMA user_version
// Cada migración bump la versión en +1. Nuevas migraciones se añaden al final del array.

use rusqlite::{Connection, Result};

type MigrationFn = fn(&Connection) -> Result<()>;

/// Lista ordenada de migraciones. Índice 0 = migración que lleva de v0 → v1.
const MIGRATIONS: &[MigrationFn] = &[
    migracion_001_listas_precio_y_clientes_activo,
    migracion_002_eliminar_listas_precio,
];

pub fn aplicar_migraciones(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let target = MIGRATIONS.len() as i64;

    if version >= target {
        log::info!("BD en versión {version}, al día (target {target})");
        return Ok(());
    }

    for (idx, m) in MIGRATIONS.iter().enumerate() {
        let v = (idx + 1) as i64;
        if v <= version { continue; }
        log::info!("Aplicando migración v{v}...");
        m(conn)?;
        conn.execute_batch(&format!("PRAGMA user_version = {v}"))?;
    }
    Ok(())
}

// ─── Migración 001 ────────────────────────────────────────
// Agrega listas de precio a productos y columnas tipo_precio + activo a clientes.
fn migracion_001_listas_precio_y_clientes_activo(conn: &Connection) -> Result<()> {
    // productos: precio_mayoreo, precio_especial (precio_venta ya existe y actúa como menudeo)
    let _ = conn.execute("ALTER TABLE productos ADD COLUMN precio_mayoreo REAL NOT NULL DEFAULT 0", []);
    let _ = conn.execute("ALTER TABLE productos ADD COLUMN precio_especial REAL NOT NULL DEFAULT 0", []);

    // Inicializar precios nuevos con precio_venta para productos existentes
    conn.execute(
        "UPDATE productos SET precio_mayoreo = precio_venta WHERE precio_mayoreo = 0",
        [],
    )?;
    conn.execute(
        "UPDATE productos SET precio_especial = precio_venta WHERE precio_especial = 0",
        [],
    )?;

    // clientes: tipo_precio + activo
    let _ = conn.execute(
        "ALTER TABLE clientes ADD COLUMN tipo_precio TEXT NOT NULL DEFAULT 'menudeo'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE clientes ADD COLUMN activo INTEGER NOT NULL DEFAULT 1",
        [],
    );

    Ok(())
}

// ─── Migración 002 ────────────────────────────────────────
// Elimina columnas de listas de precio: ahora se maneja un único precio_venta
// y los descuentos vienen por cliente o por códigos de descuento.
// SQLite 3.35+ (bundled en rusqlite 0.31) soporta ALTER TABLE DROP COLUMN.
fn migracion_002_eliminar_listas_precio(conn: &Connection) -> Result<()> {
    let _ = conn.execute("ALTER TABLE productos DROP COLUMN precio_mayoreo", []);
    let _ = conn.execute("ALTER TABLE productos DROP COLUMN precio_especial", []);
    let _ = conn.execute("ALTER TABLE clientes DROP COLUMN tipo_precio", []);
    Ok(())
}
