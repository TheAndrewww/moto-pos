// db/migrations.rs — Migraciones incrementales usando PRAGMA user_version
// Cada migración bump la versión en +1. Nuevas migraciones se añaden al final del array.

use rusqlite::{Connection, Result};

type MigrationFn = fn(&Connection) -> Result<()>;

/// Lista ordenada de migraciones. Índice 0 = migración que lleva de v0 → v1.
const MIGRATIONS: &[MigrationFn] = &[
    migracion_001_listas_precio_y_clientes_activo,
    migracion_002_eliminar_listas_precio,
    migracion_003_respaldo_auto_config,
    migracion_004_sync_remoto_y_sucursales,
    migracion_005_triggers_outbox,
    migracion_006_impresora_termica,
    migracion_007_uuid_auto_y_backfill,
    migracion_008_reparar_esquema_sync,
    migracion_009_proveedores_activo,
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

// ─── Migración 003 ────────────────────────────────────────
// Agrega configuración de respaldo automático a config_negocio.
fn migracion_003_respaldo_auto_config(conn: &Connection) -> Result<()> {
    let _ = conn.execute(
        "ALTER TABLE config_negocio ADD COLUMN respaldo_auto_activo INTEGER NOT NULL DEFAULT 1",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE config_negocio ADD COLUMN respaldo_auto_hora TEXT NOT NULL DEFAULT '23:00'",
        [],
    );
    Ok(())
}

// ─── Migración 004 ────────────────────────────────────────
// Fase 3.2: Preparación para sync con servidor remoto y multi-sucursal.
//
// Cambios:
//   1. Tabla `sucursales` + sucursal por defecto (id=1, "Principal").
//   2. Tabla `stock_sucursal` + copia del stock actual a sucursal 1.
//   3. Columna `sucursal_id` en tablas operacionales (DEFAULT 1).
//   4. Columnas `uuid`, `updated_at`, `deleted_at` en tablas sincronizables.
//   5. UUIDs poblados para filas existentes.
//   6. Tablas `sync_outbox` + `sync_state` (infraestructura de sync).
//   7. Triggers para mantener updated_at y alimentar sync_outbox.
//   8. Tabla `transferencias` (stub para futuras sucursales).
fn migracion_004_sync_remoto_y_sucursales(conn: &Connection) -> Result<()> {
    // --- 1. Sucursales ---
    conn.execute_batch(r#"
        CREATE TABLE IF NOT EXISTS sucursales (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            uuid        TEXT NOT NULL UNIQUE,
            nombre      TEXT NOT NULL,
            direccion   TEXT,
            telefono    TEXT,
            activa      INTEGER NOT NULL DEFAULT 1,
            created_at  TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
            deleted_at  TEXT
        );
    "#)?;
    // Sucursal default (id=1). Usa uuid fijo para que sea idempotente.
    conn.execute(
        "INSERT OR IGNORE INTO sucursales (id, uuid, nombre, direccion, telefono) \
         VALUES (1, '00000000-0000-7000-8000-000000000001', 'Principal', '', '')",
        [],
    )?;

    // --- 2. Stock por sucursal ---
    conn.execute_batch(r#"
        CREATE TABLE IF NOT EXISTS stock_sucursal (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            uuid          TEXT NOT NULL UNIQUE,
            producto_id   INTEGER NOT NULL REFERENCES productos(id),
            sucursal_id   INTEGER NOT NULL REFERENCES sucursales(id),
            stock_actual  REAL NOT NULL DEFAULT 0,
            stock_minimo  REAL NOT NULL DEFAULT 0,
            updated_at    TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(producto_id, sucursal_id)
        );
        CREATE INDEX IF NOT EXISTS idx_stock_suc_producto ON stock_sucursal(producto_id);
        CREATE INDEX IF NOT EXISTS idx_stock_suc_sucursal ON stock_sucursal(sucursal_id);
    "#)?;
    // Copiar stock actual de productos a stock_sucursal (sucursal 1)
    conn.execute(
        "INSERT OR IGNORE INTO stock_sucursal (uuid, producto_id, sucursal_id, stock_actual, stock_minimo) \
         SELECT lower(hex(randomblob(16))), id, 1, stock_actual, stock_minimo FROM productos",
        [],
    )?;

    // --- 3. sucursal_id en tablas operacionales ---
    for tabla in &[
        "ventas", "recepciones", "cortes", "movimientos_caja",
        "aperturas_caja", "devoluciones", "ordenes_pedido", "presupuestos",
    ] {
        let sql = format!(
            "ALTER TABLE {} ADD COLUMN sucursal_id INTEGER NOT NULL DEFAULT 1 REFERENCES sucursales(id)",
            tabla
        );
        let _ = conn.execute(&sql, []); // Ignora error si ya existe
    }

    // --- 4. uuid + updated_at + deleted_at en tablas sincronizables ---
    // Tablas que ya tienen updated_at → solo agregar uuid + deleted_at
    let con_updated: &[&str] = &[
        "productos", "proveedores", "clientes", "usuarios",
    ];
    for tabla in con_updated {
        let _ = conn.execute(&format!("ALTER TABLE {} ADD COLUMN uuid TEXT", tabla), []);
        let _ = conn.execute(&format!("ALTER TABLE {} ADD COLUMN deleted_at TEXT", tabla), []);
    }

    // Tablas sin updated_at → agregar todo
    let sin_updated: &[&str] = &[
        "categorias", "ventas", "venta_detalle", "presupuestos", "presupuesto_detalle",
        "ordenes_pedido", "orden_pedido_detalle", "recepciones", "recepcion_detalle",
        "cortes", "corte_denominaciones", "corte_vendedores", "movimientos_caja",
        "aperturas_caja", "devoluciones", "devolucion_detalle",
    ];
    for tabla in sin_updated {
        let _ = conn.execute(&format!("ALTER TABLE {} ADD COLUMN uuid TEXT", tabla), []);
        let _ = conn.execute(
            &format!("ALTER TABLE {} ADD COLUMN updated_at TEXT NOT NULL DEFAULT (datetime('now'))", tabla),
            [],
        );
        let _ = conn.execute(&format!("ALTER TABLE {} ADD COLUMN deleted_at TEXT", tabla), []);
    }

    // --- 5. Poblar uuids en filas existentes ---
    let todas_sync: &[&str] = &[
        "productos", "proveedores", "clientes", "usuarios", "categorias",
        "ventas", "venta_detalle", "presupuestos", "presupuesto_detalle",
        "ordenes_pedido", "orden_pedido_detalle", "recepciones", "recepcion_detalle",
        "cortes", "corte_denominaciones", "corte_vendedores", "movimientos_caja",
        "aperturas_caja", "devoluciones", "devolucion_detalle",
    ];
    for tabla in todas_sync {
        // lower(hex(randomblob(16))) genera un hex de 32 chars (no es UUID formal pero es único y estable)
        let sql = format!(
            "UPDATE {} SET uuid = lower(hex(randomblob(16))) WHERE uuid IS NULL",
            tabla
        );
        conn.execute(&sql, [])?;
        let _ = conn.execute(
            &format!("CREATE UNIQUE INDEX IF NOT EXISTS idx_{}_uuid ON {}(uuid)", tabla, tabla),
            [],
        );
    }

    // --- 6. Infraestructura de sync ---
    conn.execute_batch(r#"
        -- Cola de cambios salientes (push al remoto)
        CREATE TABLE IF NOT EXISTS sync_outbox (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            tabla       TEXT NOT NULL,
            uuid        TEXT NOT NULL,
            operacion   TEXT NOT NULL CHECK(operacion IN ('INSERT', 'UPDATE', 'DELETE')),
            payload     TEXT,
            created_at  TEXT NOT NULL DEFAULT (datetime('now')),
            synced_at   TEXT,
            intentos    INTEGER NOT NULL DEFAULT 0,
            ultimo_error TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_outbox_pendiente ON sync_outbox(synced_at) WHERE synced_at IS NULL;
        CREATE INDEX IF NOT EXISTS idx_outbox_tabla_uuid ON sync_outbox(tabla, uuid);

        -- Estado del POS como cliente sync (singleton, id=1)
        CREATE TABLE IF NOT EXISTS sync_state (
            id                INTEGER PRIMARY KEY CHECK(id = 1),
            device_uuid       TEXT NOT NULL,
            sucursal_id       INTEGER NOT NULL DEFAULT 1 REFERENCES sucursales(id),
            remote_url        TEXT,
            remote_token      TEXT,
            last_push_at      TEXT,
            last_pull_cursor  TEXT,
            last_pull_at      TEXT,
            activo            INTEGER NOT NULL DEFAULT 0,
            updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT OR IGNORE INTO sync_state (id, device_uuid, sucursal_id, activo)
            VALUES (1, lower(hex(randomblob(16))), 1, 0);
    "#)?;

    // Índice para evitar duplicados en outbox (1 fila pendiente por (tabla,uuid))
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_pending_unique \
         ON sync_outbox(tabla, uuid) WHERE synced_at IS NULL",
        [],
    )?;

    // --- 7. Tabla transferencias (stub para multi-sucursal futuro) ---
    conn.execute_batch(r#"
        CREATE TABLE IF NOT EXISTS transferencias (
            id                    INTEGER PRIMARY KEY AUTOINCREMENT,
            uuid                  TEXT NOT NULL UNIQUE,
            folio                 TEXT NOT NULL UNIQUE,
            sucursal_origen_id    INTEGER NOT NULL REFERENCES sucursales(id),
            sucursal_destino_id   INTEGER NOT NULL REFERENCES sucursales(id),
            usuario_id            INTEGER NOT NULL REFERENCES usuarios(id),
            estado                TEXT NOT NULL DEFAULT 'PENDIENTE'
                                  CHECK(estado IN ('PENDIENTE', 'EN_TRANSITO', 'RECIBIDA', 'CANCELADA')),
            notas                 TEXT,
            fecha_envio           TEXT,
            fecha_recepcion       TEXT,
            created_at            TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at            TEXT NOT NULL DEFAULT (datetime('now')),
            deleted_at            TEXT
        );
        CREATE TABLE IF NOT EXISTS transferencia_detalle (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            uuid             TEXT NOT NULL UNIQUE,
            transferencia_id INTEGER NOT NULL REFERENCES transferencias(id),
            producto_id      INTEGER NOT NULL REFERENCES productos(id),
            cantidad         REAL NOT NULL
        );
    "#)?;

    Ok(())
}

// ─── Migración 005 ────────────────────────────────────────
// Triggers que alimentan sync_outbox automáticamente.
//
// Cuando el worker aplica cambios recibidos del remoto, crea una tabla temporal
// `sync_suppress` antes y la destruye después. Los triggers verifican su
// existencia para no crear loops (cambios recibidos → outbox → push → recibir de
// vuelta).
//
// Todos los INSERT/UPDATE a tablas sincronizables quedan marcados como 'UPDATE'
// (upsert semántico). El worker lee el estado actual de la fila al momento del
// push, no el payload del outbox — así deduplica múltiples cambios a la misma
// fila en una sola entrada pendiente.
fn migracion_005_triggers_outbox(conn: &Connection) -> Result<()> {
    // Tabla flag para supresión de triggers durante sync apply.
    // Los triggers verifican si tiene filas: si sí, no escriben en outbox.
    conn.execute_batch(r#"
        CREATE TABLE IF NOT EXISTS sync_suppress_flag (
            id INTEGER PRIMARY KEY CHECK(id = 1)
        );
    "#)?;

    // Tablas con sync row-level (catálogo) + tablas transaccionales (parent-level).
    // Los children de aggregate-sync (venta_detalle, etc.) NO tienen trigger — el
    // worker los incluye al empacar el padre.
    let tablas_sync: &[&str] = &[
        // Catálogo (row-level, LWW)
        "productos", "proveedores", "clientes", "usuarios", "categorias",
        "sucursales", "stock_sucursal",
        // Transaccionales (aggregate con children)
        "ventas", "presupuestos", "ordenes_pedido", "recepciones",
        "cortes", "devoluciones", "transferencias",
        // Append-only standalone
        "movimientos_caja", "aperturas_caja",
    ];

    for tabla in tablas_sync {
        // AFTER INSERT
        let trig_ins = format!(
            r#"
            CREATE TRIGGER IF NOT EXISTS trg_{tabla}_outbox_ins
            AFTER INSERT ON {tabla}
            WHEN NEW.uuid IS NOT NULL
             AND NOT EXISTS (SELECT 1 FROM sync_suppress_flag WHERE id=1)
            BEGIN
                INSERT INTO sync_outbox (tabla, uuid, operacion)
                VALUES ('{tabla}', NEW.uuid, 'UPDATE')
                ON CONFLICT(tabla, uuid) WHERE synced_at IS NULL
                DO UPDATE SET created_at = datetime('now'), intentos = 0, ultimo_error = NULL;
            END;
            "#,
            tabla = tabla
        );
        conn.execute_batch(&trig_ins)?;

        // AFTER UPDATE
        let trig_upd = format!(
            r#"
            CREATE TRIGGER IF NOT EXISTS trg_{tabla}_outbox_upd
            AFTER UPDATE ON {tabla}
            WHEN NEW.uuid IS NOT NULL
             AND NOT EXISTS (SELECT 1 FROM sync_suppress_flag WHERE id=1)
            BEGIN
                INSERT INTO sync_outbox (tabla, uuid, operacion)
                VALUES ('{tabla}', NEW.uuid, 'UPDATE')
                ON CONFLICT(tabla, uuid) WHERE synced_at IS NULL
                DO UPDATE SET created_at = datetime('now'), intentos = 0, ultimo_error = NULL;
            END;
            "#,
            tabla = tabla
        );
        conn.execute_batch(&trig_upd)?;

        // AFTER DELETE (hard delete — las borradas por soft-delete pasan por UPDATE)
        let trig_del = format!(
            r#"
            CREATE TRIGGER IF NOT EXISTS trg_{tabla}_outbox_del
            AFTER DELETE ON {tabla}
            WHEN OLD.uuid IS NOT NULL
             AND NOT EXISTS (SELECT 1 FROM sync_suppress_flag WHERE id=1)
            BEGIN
                INSERT INTO sync_outbox (tabla, uuid, operacion)
                VALUES ('{tabla}', OLD.uuid, 'DELETE')
                ON CONFLICT(tabla, uuid) WHERE synced_at IS NULL
                DO UPDATE SET operacion = 'DELETE', created_at = datetime('now'), intentos = 0, ultimo_error = NULL;
            END;
            "#,
            tabla = tabla
        );
        conn.execute_batch(&trig_del)?;
    }

    // Trigger especial: cuando se actualiza una tabla con updated_at,
    // asegurar que updated_at siempre se bumpea al cambiar (salvo que el remoto
    // lo esté imponiendo mediante sync_suppress).
    // Tablas con updated_at propio que no lo tocan en cada UPDATE.
    let con_updated_auto: &[&str] = &[
        "productos", "proveedores", "clientes", "usuarios", "categorias",
        "sucursales", "stock_sucursal",
        "ventas", "presupuestos", "ordenes_pedido", "recepciones",
        "cortes", "devoluciones", "transferencias",
        "movimientos_caja", "aperturas_caja",
    ];
    for tabla in con_updated_auto {
        let trig = format!(
            r#"
            CREATE TRIGGER IF NOT EXISTS trg_{tabla}_bump_updated
            AFTER UPDATE ON {tabla}
            WHEN NEW.updated_at = OLD.updated_at
             AND NOT EXISTS (SELECT 1 FROM sync_suppress_flag WHERE id=1)
            BEGIN
                UPDATE {tabla} SET updated_at = datetime('now') WHERE id = NEW.id;
            END;
            "#,
            tabla = tabla
        );
        conn.execute_batch(&trig)?;
    }

    Ok(())
}

// ─── Migración 006 ────────────────────────────────────────
// Agrega configuración de impresora térmica al config_negocio.
// Si se deja vacío, el ticket cae al fallback HTML (navegador).
fn migracion_006_impresora_termica(conn: &Connection) -> Result<()> {
    let _ = conn.execute(
        "ALTER TABLE config_negocio ADD COLUMN impresora_termica TEXT NOT NULL DEFAULT ''",
        [],
    );
    Ok(())
}

// ─── Helpers de introspección de esquema ──────────────────
// Usados por migraciones que necesitan reparar esquema condicionalmente.

fn tabla_existe(conn: &Connection, tabla: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?1",
        [tabla],
        |_| Ok(()),
    )
    .is_ok()
}

fn columna_existe(conn: &Connection, tabla: &str, columna: &str) -> bool {
    let sql = format!("PRAGMA table_info({})", tabla);
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let rows = stmt.query_map([], |r| r.get::<_, String>(1));
    if let Ok(it) = rows {
        for nombre in it.flatten() {
            if nombre == columna {
                return true;
            }
        }
    }
    false
}

// ─── Migración 007 ────────────────────────────────────────
// Arregla un bug crítico de sync: varios INSERT (crear_producto,
// crear_cliente, crear_usuario, importar_catalogo_csv, etc.) NO generaban
// uuid al insertar filas. Eso dejaba miles de filas con uuid=NULL que los
// triggers de outbox ignoraban (WHEN NEW.uuid IS NOT NULL), por lo que esas
// filas nunca se sincronizaban al servidor remoto.
//
// Soluciones:
//   1. Reparar esquema: asegurar que todas las tablas sync tengan uuid +
//      updated_at + deleted_at. v4 usaba `let _ = ...` que silenciaba
//      errores, así que en algunas BDs esas columnas pueden faltar y romper
//      triggers (ej. "no such column: NEW.updated_at" al insertar apertura).
//   2. Pueblar uuid en filas existentes con uuid NULL.
//   3. Agregar triggers AFTER INSERT que generen uuid si es NULL — así
//      cualquier INSERT futuro queda blindado sin necesidad de tocar los
//      commands.
//   4. Re-encolar todas las filas en sync_outbox para que el worker las
//      empuje al remoto.
fn migracion_007_uuid_auto_y_backfill(conn: &Connection) -> Result<()> {
    // Tablas que necesitan uuid auto-generado y backfill.
    // (transferencias se omite — ya tiene UNIQUE NOT NULL, no acepta INSERT
    // sin uuid; ese flujo sí lo genera.)
    let tablas: &[&str] = &[
        "productos", "proveedores", "clientes", "usuarios", "categorias",
        "sucursales", "stock_sucursal",
        "ventas", "venta_detalle", "presupuestos", "presupuesto_detalle",
        "ordenes_pedido", "orden_pedido_detalle", "recepciones", "recepcion_detalle",
        "cortes", "corte_denominaciones", "corte_vendedores",
        "movimientos_caja", "aperturas_caja", "devoluciones", "devolucion_detalle",
    ];

    // 0. Reparar esquema: asegurar que cada tabla sync tenga las 3 columnas
    //    requeridas. Si v4 falló silenciosamente para alguna (p.ej. DB
    //    creada antes de incluir esa tabla en SCHEMA_V1), la añadimos aquí.
    //    Usamos NOT NULL DEFAULT '' (constante) y luego UPDATE para poblar
    //    con timestamp real, evitando incompatibilidades con versiones
    //    viejas de SQLite que rechazan ADD COLUMN NOT NULL DEFAULT con
    //    función volátil.
    for tabla in tablas {
        if !tabla_existe(conn, tabla) { continue; }
        if !columna_existe(conn, tabla, "uuid") {
            let _ = conn.execute(&format!("ALTER TABLE {} ADD COLUMN uuid TEXT", tabla), []);
        }
        if !columna_existe(conn, tabla, "updated_at") {
            let _ = conn.execute(
                &format!("ALTER TABLE {} ADD COLUMN updated_at TEXT NOT NULL DEFAULT ''", tabla),
                [],
            );
            let _ = conn.execute(
                &format!("UPDATE {} SET updated_at = datetime('now') WHERE updated_at = ''", tabla),
                [],
            );
        }
        if !columna_existe(conn, tabla, "deleted_at") {
            let _ = conn.execute(&format!("ALTER TABLE {} ADD COLUMN deleted_at TEXT", tabla), []);
        }
    }

    // 1. Backfill de uuids existentes
    for tabla in tablas {
        if !tabla_existe(conn, tabla) { continue; }
        let sql = format!(
            "UPDATE {} SET uuid = lower(hex(randomblob(16))) WHERE uuid IS NULL",
            tabla
        );
        let _ = conn.execute(&sql, []);
    }

    // 2. Trigger AFTER INSERT que auto-genera uuid si la fila se insertó
    //    con uuid NULL. SQLite no permite SET de NEW en BEFORE INSERT, así
    //    que usamos AFTER + UPDATE por rowid (idéntico patrón a los triggers
    //    de updated_at de v5).
    //
    //    El UPDATE recursivo dispara trg_<tabla>_outbox_upd, que ahora sí
    //    ve uuid IS NOT NULL y encola la fila — comportamiento deseado.
    //
    //    Durante apply de pull (sync_suppress_flag activo) no hace falta
    //    suprimirlo porque el remoto siempre envía uuid; el WHEN uuid IS
    //    NULL no se cumple, el trigger no dispara.
    for tabla in tablas {
        let trig = format!(
            r#"
            DROP TRIGGER IF EXISTS trg_{tabla}_uuid_auto;
            CREATE TRIGGER trg_{tabla}_uuid_auto
            AFTER INSERT ON {tabla}
            FOR EACH ROW
            WHEN NEW.uuid IS NULL
            BEGIN
                UPDATE {tabla} SET uuid = lower(hex(randomblob(16))) WHERE rowid = NEW.rowid;
            END;
            "#,
            tabla = tabla
        );
        conn.execute_batch(&trig)?;
    }

    // 3. Re-encolar todas las filas con uuid en sync_outbox para que el
    //    worker las empuje al remoto. Idempotente: ON CONFLICT actualiza
    //    created_at y resetea intentos.
    //
    //    Solo tablas con triggers de outbox (catálogo + transaccionales
    //    padres). Los detalles van junto al padre.
    let tablas_outbox: &[&str] = &[
        "productos", "proveedores", "clientes", "usuarios", "categorias",
        "sucursales", "stock_sucursal",
        "ventas", "presupuestos", "ordenes_pedido", "recepciones",
        "cortes", "devoluciones", "transferencias",
        "movimientos_caja", "aperturas_caja",
    ];
    for tabla in tablas_outbox {
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
        // Ignorar error si la tabla no existe en este esquema
        if let Err(e) = conn.execute(&sql, []) {
            log::warn!("migracion 007: re-encolar tabla '{}' falló: {}", tabla, e);
        }
    }

    Ok(())
}

// ─── Migración 008 ────────────────────────────────────────
// Repara esquemas que quedaron incompletos cuando v4 falló silenciosamente
// para alguna tabla (`let _ = ALTER TABLE ...`). En esas BDs faltan columnas
// como `aperturas_caja.updated_at`, lo que rompe los triggers de v5 que
// referencian `NEW.updated_at` y los triggers de v7 que disparan UPDATE
// recursivo. Resultado: cualquier INSERT a la tabla afectada explota con
// "no such column: NEW.updated_at" y el programa no abre.
//
// Esta migración:
//   1. Recorre todas las tablas sync.
//   2. Si la tabla existe pero le falta `uuid` / `updated_at` / `deleted_at`,
//      las agrega con DEFAULT '' (constante, compatible con SQLite viejos)
//      y luego pobla `updated_at` con datetime('now').
//   3. Pobla uuid en filas con uuid NULL.
//   4. Re-crea los triggers de v5 y v7 con DROP+CREATE para asegurar que
//      todos referencien el esquema reparado.
// ─── Migración 009 ────────────────────────────────────────
// Agrega columna `activo` a proveedores (soft-toggle, mismo patrón que clientes).
// Sin esto la nueva página de Proveedores no puede ocultar/restaurar proveedores
// sin perder los productos que los referencian.
fn migracion_009_proveedores_activo(conn: &Connection) -> Result<()> {
    if !columna_existe(conn, "proveedores", "activo") {
        let _ = conn.execute(
            "ALTER TABLE proveedores ADD COLUMN activo INTEGER NOT NULL DEFAULT 1",
            [],
        );
    }
    Ok(())
}

fn migracion_008_reparar_esquema_sync(conn: &Connection) -> Result<()> {
    let tablas: &[&str] = &[
        "productos", "proveedores", "clientes", "usuarios", "categorias",
        "sucursales", "stock_sucursal",
        "ventas", "venta_detalle", "presupuestos", "presupuesto_detalle",
        "ordenes_pedido", "orden_pedido_detalle", "recepciones", "recepcion_detalle",
        "cortes", "corte_denominaciones", "corte_vendedores",
        "movimientos_caja", "aperturas_caja", "devoluciones", "devolucion_detalle",
        "transferencias", "transferencia_detalle",
    ];

    // 1. Reparar columnas faltantes
    for tabla in tablas {
        if !tabla_existe(conn, tabla) { continue; }

        if !columna_existe(conn, tabla, "uuid") {
            let _ = conn.execute(&format!("ALTER TABLE {} ADD COLUMN uuid TEXT", tabla), []);
        }
        if !columna_existe(conn, tabla, "updated_at") {
            // DEFAULT '' (constante) para compat con SQLite viejos que rechazan
            // ADD COLUMN NOT NULL DEFAULT con función volátil.
            let _ = conn.execute(
                &format!("ALTER TABLE {} ADD COLUMN updated_at TEXT NOT NULL DEFAULT ''", tabla),
                [],
            );
            let _ = conn.execute(
                &format!("UPDATE {} SET updated_at = datetime('now') WHERE updated_at = ''", tabla),
                [],
            );
        }
        if !columna_existe(conn, tabla, "deleted_at") {
            let _ = conn.execute(&format!("ALTER TABLE {} ADD COLUMN deleted_at TEXT", tabla), []);
        }
    }

    // 2. Backfill uuid en filas existentes
    for tabla in tablas {
        if !tabla_existe(conn, tabla) { continue; }
        let _ = conn.execute(
            &format!(
                "UPDATE {} SET uuid = lower(hex(randomblob(16))) WHERE uuid IS NULL",
                tabla
            ),
            [],
        );
    }

    // 3. Re-crear triggers de bump_updated (v5) y uuid_auto (v7) para que
    //    referencien el esquema actual ya reparado. Si v5 o v7 fallaron en
    //    crear algún trigger por columna faltante, ahora se crean limpios.
    let con_updated_auto: &[&str] = &[
        "productos", "proveedores", "clientes", "usuarios", "categorias",
        "sucursales", "stock_sucursal",
        "ventas", "presupuestos", "ordenes_pedido", "recepciones",
        "cortes", "devoluciones", "transferencias",
        "movimientos_caja", "aperturas_caja",
    ];
    for tabla in con_updated_auto {
        if !tabla_existe(conn, tabla) { continue; }
        if !columna_existe(conn, tabla, "updated_at") { continue; }
        let trig = format!(
            r#"
            DROP TRIGGER IF EXISTS trg_{tabla}_bump_updated;
            CREATE TRIGGER trg_{tabla}_bump_updated
            AFTER UPDATE ON {tabla}
            WHEN NEW.updated_at = OLD.updated_at
             AND NOT EXISTS (SELECT 1 FROM sync_suppress_flag WHERE id=1)
            BEGIN
                UPDATE {tabla} SET updated_at = datetime('now') WHERE id = NEW.id;
            END;
            "#,
            tabla = tabla
        );
        let _ = conn.execute_batch(&trig);
    }

    let tablas_uuid_auto: &[&str] = &[
        "productos", "proveedores", "clientes", "usuarios", "categorias",
        "sucursales", "stock_sucursal",
        "ventas", "venta_detalle", "presupuestos", "presupuesto_detalle",
        "ordenes_pedido", "orden_pedido_detalle", "recepciones", "recepcion_detalle",
        "cortes", "corte_denominaciones", "corte_vendedores",
        "movimientos_caja", "aperturas_caja", "devoluciones", "devolucion_detalle",
    ];
    for tabla in tablas_uuid_auto {
        if !tabla_existe(conn, tabla) { continue; }
        if !columna_existe(conn, tabla, "uuid") { continue; }
        let trig = format!(
            r#"
            DROP TRIGGER IF EXISTS trg_{tabla}_uuid_auto;
            CREATE TRIGGER trg_{tabla}_uuid_auto
            AFTER INSERT ON {tabla}
            FOR EACH ROW
            WHEN NEW.uuid IS NULL
            BEGIN
                UPDATE {tabla} SET uuid = lower(hex(randomblob(16))) WHERE rowid = NEW.rowid;
            END;
            "#,
            tabla = tabla
        );
        let _ = conn.execute_batch(&trig);
    }

    Ok(())
}
