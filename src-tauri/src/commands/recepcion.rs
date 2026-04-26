// commands/recepcion.rs — Recepción de mercancía (entrada de stock)

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::State;
use crate::commands::auth::AppState;

#[derive(Serialize, Clone)]
pub struct Recepcion {
    pub id: i64,
    pub usuario_nombre: String,
    pub proveedor_nombre: Option<String>,
    pub fecha: String,
    pub notas: Option<String>,
    pub total_items: i64,
}

#[derive(Serialize, Clone)]
pub struct RecepcionDetalleItem {
    pub id: i64,
    pub producto_id: i64,
    pub producto_nombre: String,
    pub producto_codigo: String,
    pub cantidad: f64,
    pub precio_costo: f64,
}

#[derive(Deserialize)]
pub struct ItemRecepcion {
    pub producto_id: i64,
    pub cantidad: f64,
    pub precio_costo: f64,
    /// Nuevo precio de venta. Opcional — si viene `Some(v)` con `v > 0`,
    /// se actualiza también `productos.precio_venta`. Permite recalcular
    /// el precio en la recepción usando los multiplicadores 1.4/1.5/1.7.
    #[serde(default)]
    pub precio_venta: Option<f64>,
}

#[derive(Deserialize)]
pub struct DatosRecepcion {
    pub usuario_id: i64,
    pub proveedor_id: Option<i64>,
    #[serde(default)]
    pub orden_id: Option<i64>,
    pub notas: Option<String>,
    pub items: Vec<ItemRecepcion>,
}

#[tauri::command]
pub fn crear_recepcion(
    recepcion: DatosRecepcion,
    state: State<'_, AppState>,
) -> Result<Recepcion, String> {
    let db = state.db.lock().unwrap();
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Insertar cabecera
    db.execute(
        r#"INSERT INTO recepciones (orden_id, usuario_id, proveedor_id, fecha, notas)
           VALUES (?, ?, ?, ?, ?)"#,
        rusqlite::params![
            recepcion.orden_id, recepcion.usuario_id, recepcion.proveedor_id, now, recepcion.notas
        ],
    ).map_err(|e| e.to_string())?;

    let recep_id = db.last_insert_rowid();
    let total_items = recepcion.items.len() as i64;

    // Insertar detalle y actualizar stock
    for item in &recepcion.items {
        db.execute(
            r#"INSERT INTO recepcion_detalle (recepcion_id, producto_id, cantidad, precio_costo)
               VALUES (?, ?, ?, ?)"#,
            rusqlite::params![recep_id, item.producto_id, item.cantidad, item.precio_costo],
        ).map_err(|e| e.to_string())?;

        // Actualizar stock y precio de costo del producto.
        // Si se mandó un precio_venta > 0, también lo actualizamos —
        // así los multiplicadores 1.4/1.5/1.7 se aplican al recibir.
        // Si viene None o 0, dejamos el precio_venta existente intacto.
        let nuevo_pv = item.precio_venta.filter(|v| *v > 0.0);
        if let Some(pv) = nuevo_pv {
            db.execute(
                r#"UPDATE productos SET
                    stock_actual = stock_actual + ?,
                    precio_costo = ?,
                    precio_venta = ?,
                    updated_at = ?
                   WHERE id = ?"#,
                rusqlite::params![item.cantidad, item.precio_costo, pv, now, item.producto_id],
            ).map_err(|e| e.to_string())?;
        } else {
            db.execute(
                r#"UPDATE productos SET
                    stock_actual = stock_actual + ?,
                    precio_costo = ?,
                    updated_at = ?
                   WHERE id = ?"#,
                rusqlite::params![item.cantidad, item.precio_costo, now, item.producto_id],
            ).map_err(|e| e.to_string())?;
        }

        // Si es contra una orden, actualizar cantidad_recibida del detalle correspondiente
        if let Some(orden_id) = recepcion.orden_id {
            let _ = db.execute(
                r#"UPDATE orden_pedido_detalle
                   SET cantidad_recibida = cantidad_recibida + ?
                   WHERE orden_id = ? AND producto_id = ?"#,
                rusqlite::params![item.cantidad, orden_id, item.producto_id],
            );
        }
    }

    // Si es contra una orden: calcular si ya está totalmente recibida y actualizar estado
    if let Some(orden_id) = recepcion.orden_id {
        let faltante: f64 = db.query_row(
            r#"SELECT COALESCE(SUM(
                   CASE WHEN cantidad_pedida > cantidad_recibida
                        THEN cantidad_pedida - cantidad_recibida ELSE 0 END
               ), 0)
               FROM orden_pedido_detalle WHERE orden_id = ?"#,
            rusqlite::params![orden_id],
            |row| row.get(0),
        ).unwrap_or(0.0);

        let nuevo_estado = if faltante <= 0.0 { "recibida_completa" } else { "recibida_parcial" };
        let _ = db.execute(
            "UPDATE ordenes_pedido SET estado = ?, fecha_recepcion = ? WHERE id = ?",
            rusqlite::params![nuevo_estado, now, orden_id],
        );
    }

    // Bitácora
    let desc = format!(
        "Recepción #{} | {} items | Proveedor: {}",
        recep_id, total_items,
        recepcion.proveedor_id.map_or("N/A".to_string(), |id| id.to_string())
    );
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
           VALUES (?, 'RECEPCION_CREADA', 'recepciones', ?, ?, 'POS')"#,
        rusqlite::params![recepcion.usuario_id, recep_id, desc],
    );

    // Retornar datos
    let usuario_nombre: String = db.query_row(
        "SELECT nombre_completo FROM usuarios WHERE id = ?",
        rusqlite::params![recepcion.usuario_id],
        |row| row.get(0),
    ).unwrap_or_else(|_| "—".to_string());

    let proveedor_nombre: Option<String> = recepcion.proveedor_id.and_then(|pid| {
        db.query_row(
            "SELECT nombre FROM proveedores WHERE id = ?",
            rusqlite::params![pid],
            |row| row.get(0),
        ).ok()
    });

    Ok(Recepcion {
        id: recep_id,
        usuario_nombre,
        proveedor_nombre,
        fecha: now,
        notas: recepcion.notas,
        total_items,
    })
}

#[tauri::command]
pub fn listar_recepciones(state: State<'_, AppState>) -> Vec<Recepcion> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        r#"SELECT r.id, u.nombre_completo, p.nombre, r.fecha, r.notas,
                  (SELECT COUNT(*) FROM recepcion_detalle WHERE recepcion_id = r.id)
           FROM recepciones r
           LEFT JOIN usuarios u ON u.id = r.usuario_id
           LEFT JOIN proveedores p ON p.id = r.proveedor_id
           ORDER BY r.fecha DESC"#,
    ).unwrap();

    stmt.query_map([], |row| {
        Ok(Recepcion {
            id: row.get(0)?,
            usuario_nombre: row.get(1)?,
            proveedor_nombre: row.get(2)?,
            fecha: row.get(3)?,
            notas: row.get(4)?,
            total_items: row.get(5)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

#[tauri::command]
pub fn obtener_detalle_recepcion(
    recepcion_id: i64,
    state: State<'_, AppState>,
) -> Vec<RecepcionDetalleItem> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        r#"SELECT rd.id, rd.producto_id, p.nombre, p.codigo, rd.cantidad, rd.precio_costo
           FROM recepcion_detalle rd
           LEFT JOIN productos p ON p.id = rd.producto_id
           WHERE rd.recepcion_id = ?"#,
    ).unwrap();

    stmt.query_map(rusqlite::params![recepcion_id], |row| {
        Ok(RecepcionDetalleItem {
            id: row.get(0)?,
            producto_id: row.get(1)?,
            producto_nombre: row.get(2)?,
            producto_codigo: row.get(3)?,
            cantidad: row.get(4)?,
            precio_costo: row.get(5)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}
