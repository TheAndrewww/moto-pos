// commands/devoluciones.rs — Devoluciones parciales de venta

use serde::{Deserialize, Serialize};
use tauri::State;
use super::auth::AppState;
use chrono::Local;

// ─── Structs ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ItemDevolucion {
    pub venta_detalle_id: i64,
    pub cantidad: f64,
}

#[derive(Deserialize)]
pub struct NuevaDevolucion {
    pub venta_id: i64,
    pub usuario_id: i64,
    pub autorizado_por: Option<i64>,
    pub motivo: String,
    pub items: Vec<ItemDevolucion>,
}

#[derive(Serialize)]
pub struct DevolucionCreada {
    pub id: i64,
    pub folio: String,
    pub total_devuelto: f64,
    pub fecha: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct DevolucionResumen {
    pub id: i64,
    pub folio: String,
    pub venta_id: i64,
    pub venta_folio: String,
    pub usuario_nombre: String,
    pub autorizado_por_nombre: Option<String>,
    pub motivo: String,
    pub total_devuelto: f64,
    pub num_items: i64,
    pub fecha: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct DevolucionDetalleItem {
    pub producto_id: i64,
    pub codigo: String,
    pub nombre: String,
    pub cantidad: f64,
    pub precio_unitario: f64,
    pub subtotal: f64,
}

#[derive(Serialize)]
pub struct DevolucionDetalle {
    pub devolucion: DevolucionResumen,
    pub items: Vec<DevolucionDetalleItem>,
}

// ─── Comandos ─────────────────────────────────────────────

/// Crear una devolución parcial (o total) — restaura stock y registra RETIRO de caja.
#[tauri::command]
pub fn crear_devolucion(
    datos: NuevaDevolucion,
    state: State<'_, AppState>,
) -> Result<DevolucionCreada, String> {
    if datos.motivo.trim().is_empty() {
        return Err("El motivo es obligatorio".to_string());
    }
    if datos.items.is_empty() {
        return Err("Debe incluir al menos un producto a devolver".to_string());
    }
    for it in &datos.items {
        if it.cantidad <= 0.0 {
            return Err("Las cantidades deben ser mayores a 0".to_string());
        }
    }

    let db = state.db.lock().unwrap();
    let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Verificar venta no anulada
    let venta_folio: String = db.query_row(
        "SELECT folio FROM ventas WHERE id = ? AND anulada = 0",
        rusqlite::params![datos.venta_id],
        |row| row.get(0),
    ).map_err(|_| "Venta no encontrada o está anulada".to_string())?;

    // Verificar rol del usuario: si no es dueño, requiere autorizado_por
    let es_admin: i64 = db.query_row(
        "SELECT r.es_admin FROM usuarios u JOIN roles r ON r.id = u.rol_id WHERE u.id = ?",
        rusqlite::params![datos.usuario_id],
        |row| row.get(0),
    ).unwrap_or(0);

    if es_admin == 0 && datos.autorizado_por.is_none() {
        return Err("Se requiere autorización del dueño para registrar devoluciones".to_string());
    }

    // Validar cantidades contra cada venta_detalle
    let mut total_devuelto = 0.0_f64;
    let mut items_validados: Vec<(i64, i64, f64, f64, f64)> = Vec::new();
    // (venta_detalle_id, producto_id, cantidad, precio_unitario, subtotal)

    for item in &datos.items {
        let (vd_venta_id, prod_id, cantidad_orig, precio_final): (i64, i64, f64, f64) = db.query_row(
            "SELECT venta_id, producto_id, cantidad, precio_final FROM venta_detalle WHERE id = ?",
            rusqlite::params![item.venta_detalle_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).map_err(|_| format!("Partida de venta no encontrada: id={}", item.venta_detalle_id))?;

        if vd_venta_id != datos.venta_id {
            return Err("Una de las partidas no pertenece a la venta".to_string());
        }

        let ya_devuelto: f64 = db.query_row(
            "SELECT COALESCE(SUM(cantidad), 0) FROM devolucion_detalle WHERE venta_detalle_id = ?",
            rusqlite::params![item.venta_detalle_id],
            |row| row.get(0),
        ).unwrap_or(0.0);

        let disponible = cantidad_orig - ya_devuelto;
        if item.cantidad > disponible + 0.0001 {
            return Err(format!(
                "Cantidad excede lo disponible (vendido {}, ya devuelto {}, queda {})",
                cantidad_orig, ya_devuelto, disponible
            ));
        }

        let subtotal = item.cantidad * precio_final;
        total_devuelto += subtotal;
        items_validados.push((item.venta_detalle_id, prod_id, item.cantidad, precio_final, subtotal));
    }

    // Generar folio D-XXXXXX
    let ultimo: i64 = db.query_row(
        "SELECT ultimo_valor FROM devolucion_folio_secuencia WHERE id = 1",
        [], |row| row.get(0),
    ).unwrap_or(0);
    let nuevo = ultimo + 1;
    db.execute(
        "UPDATE devolucion_folio_secuencia SET ultimo_valor = ? WHERE id = 1",
        rusqlite::params![nuevo],
    ).map_err(|e| e.to_string())?;
    let folio = format!("D-{:06}", nuevo);

    // Transacción
    db.execute("BEGIN TRANSACTION", []).map_err(|e| e.to_string())?;

    // Insertar devolución
    let r = db.execute(
        r#"INSERT INTO devoluciones (folio, venta_id, usuario_id, autorizado_por, motivo,
                                     total_devuelto, fecha)
           VALUES (?, ?, ?, ?, ?, ?, ?)"#,
        rusqlite::params![
            folio, datos.venta_id, datos.usuario_id,
            datos.autorizado_por, datos.motivo, total_devuelto, now
        ],
    );
    if let Err(e) = r {
        let _ = db.execute("ROLLBACK", []);
        return Err(e.to_string());
    }
    let devolucion_id = db.last_insert_rowid();

    // Insertar detalle y restaurar stock
    for (vd_id, prod_id, cantidad, precio, subtotal) in &items_validados {
        let r = db.execute(
            r#"INSERT INTO devolucion_detalle
               (devolucion_id, venta_detalle_id, producto_id, cantidad,
                precio_unitario, subtotal)
               VALUES (?, ?, ?, ?, ?, ?)"#,
            rusqlite::params![devolucion_id, vd_id, prod_id, cantidad, precio, subtotal],
        );
        if let Err(e) = r {
            let _ = db.execute("ROLLBACK", []);
            return Err(format!("Error al insertar detalle: {}", e));
        }

        let r = db.execute(
            "UPDATE productos SET stock_actual = stock_actual + ?, updated_at = ? WHERE id = ?",
            rusqlite::params![cantidad, now, prod_id],
        );
        if let Err(e) = r {
            let _ = db.execute("ROLLBACK", []);
            return Err(format!("Error al restaurar stock: {}", e));
        }
    }

    // Movimiento de caja (RETIRO) por el monto devuelto
    let concepto = format!("Devolución {} de venta {} — {}", folio, venta_folio, datos.motivo);
    let r = db.execute(
        r#"INSERT INTO movimientos_caja (tipo, usuario_id, monto, concepto, autorizado_por, fecha)
           VALUES ('RETIRO', ?, ?, ?, ?, ?)"#,
        rusqlite::params![datos.usuario_id, total_devuelto, concepto, datos.autorizado_por, now],
    );
    if let Err(e) = r {
        let _ = db.execute("ROLLBACK", []);
        return Err(format!("Error al registrar movimiento de caja: {}", e));
    }
    let mov_id = db.last_insert_rowid();

    let r = db.execute(
        "UPDATE devoluciones SET movimiento_caja_id = ? WHERE id = ?",
        rusqlite::params![mov_id, devolucion_id],
    );
    if let Err(e) = r {
        let _ = db.execute("ROLLBACK", []);
        return Err(format!("Error al vincular movimiento: {}", e));
    }

    // Bitácora
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id,
           descripcion_legible, origen)
           VALUES (?, 'DEVOLUCION', 'devoluciones', ?, ?, 'POS')"#,
        rusqlite::params![
            datos.usuario_id, devolucion_id,
            format!("Devolución {} de venta {} — ${:.2} — {}", folio, venta_folio, total_devuelto, datos.motivo)
        ],
    );

    db.execute("COMMIT", []).map_err(|e| e.to_string())?;

    Ok(DevolucionCreada {
        id: devolucion_id,
        folio,
        total_devuelto,
        fecha: now,
    })
}

/// Listar devoluciones recientes
#[tauri::command]
pub fn listar_devoluciones(
    limite: Option<i64>,
    state: State<'_, AppState>,
) -> Result<Vec<DevolucionResumen>, String> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        r#"SELECT d.id, d.folio, d.venta_id, v.folio AS venta_folio,
                  u.nombre_completo, ua.nombre_completo,
                  d.motivo, d.total_devuelto,
                  (SELECT COUNT(*) FROM devolucion_detalle dd WHERE dd.devolucion_id = d.id),
                  d.fecha
           FROM devoluciones d
           JOIN ventas v ON v.id = d.venta_id
           JOIN usuarios u ON u.id = d.usuario_id
           LEFT JOIN usuarios ua ON ua.id = d.autorizado_por
           ORDER BY d.fecha DESC
           LIMIT ?"#,
    ).map_err(|e| e.to_string())?;

    let rows = stmt.query_map(rusqlite::params![limite.unwrap_or(100)], |row| {
        Ok(DevolucionResumen {
            id: row.get(0)?,
            folio: row.get(1)?,
            venta_id: row.get(2)?,
            venta_folio: row.get(3)?,
            usuario_nombre: row.get(4)?,
            autorizado_por_nombre: row.get(5)?,
            motivo: row.get(6)?,
            total_devuelto: row.get(7)?,
            num_items: row.get(8)?,
            fecha: row.get(9)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();
    Ok(rows)
}

/// Detalle de una devolución
#[tauri::command]
pub fn obtener_detalle_devolucion(
    id: i64,
    state: State<'_, AppState>,
) -> Result<DevolucionDetalle, String> {
    let db = state.db.lock().unwrap();

    let res = db.query_row(
        r#"SELECT d.id, d.folio, d.venta_id, v.folio,
                  u.nombre_completo, ua.nombre_completo,
                  d.motivo, d.total_devuelto,
                  (SELECT COUNT(*) FROM devolucion_detalle dd WHERE dd.devolucion_id = d.id),
                  d.fecha
           FROM devoluciones d
           JOIN ventas v ON v.id = d.venta_id
           JOIN usuarios u ON u.id = d.usuario_id
           LEFT JOIN usuarios ua ON ua.id = d.autorizado_por
           WHERE d.id = ?"#,
        rusqlite::params![id],
        |row| Ok(DevolucionResumen {
            id: row.get(0)?,
            folio: row.get(1)?,
            venta_id: row.get(2)?,
            venta_folio: row.get(3)?,
            usuario_nombre: row.get(4)?,
            autorizado_por_nombre: row.get(5)?,
            motivo: row.get(6)?,
            total_devuelto: row.get(7)?,
            num_items: row.get(8)?,
            fecha: row.get(9)?,
        }),
    ).map_err(|_| "Devolución no encontrada".to_string())?;

    let mut stmt = db.prepare(
        r#"SELECT dd.producto_id, p.codigo, p.nombre,
                  dd.cantidad, dd.precio_unitario, dd.subtotal
           FROM devolucion_detalle dd
           JOIN productos p ON p.id = dd.producto_id
           WHERE dd.devolucion_id = ?
           ORDER BY dd.id"#,
    ).map_err(|e| e.to_string())?;

    let items = stmt.query_map(rusqlite::params![id], |row| {
        Ok(DevolucionDetalleItem {
            producto_id: row.get(0)?,
            codigo: row.get(1)?,
            nombre: row.get(2)?,
            cantidad: row.get(3)?,
            precio_unitario: row.get(4)?,
            subtotal: row.get(5)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(DevolucionDetalle { devolucion: res, items })
}
