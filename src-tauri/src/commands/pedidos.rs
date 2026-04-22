// commands/pedidos.rs — Gestión de pedidos a proveedores

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::State;
use crate::commands::auth::AppState;

#[derive(Serialize, Clone)]
pub struct OrdenPedido {
    pub id: i64,
    pub proveedor_nombre: Option<String>,
    pub usuario_nombre: String,
    pub estado: String,
    pub notas: Option<String>,
    pub fecha: String,
    pub total_items: i64,
}

#[derive(Serialize, Clone)]
pub struct OrdenPedidoDetalle {
    pub id: i64,
    pub producto_id: i64,
    pub producto_nombre: String,
    pub producto_codigo: String,
    pub cantidad_pedida: f64,
    pub cantidad_recibida: f64,
    pub precio_costo: f64,
}

#[derive(Deserialize)]
pub struct ItemOrden {
    pub producto_id: i64,
    pub cantidad_pedida: f64,
    pub precio_costo: f64,
}

#[derive(Deserialize)]
pub struct DatosOrden {
    pub usuario_id: i64,
    pub proveedor_id: Option<i64>,
    pub notas: Option<String>,
    pub items: Vec<ItemOrden>,
}

#[tauri::command]
pub fn crear_orden_pedido(
    orden: DatosOrden,
    state: State<'_, AppState>,
) -> Result<OrdenPedido, String> {
    let db = state.db.lock().unwrap();
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let siguiente_id: i64 = db.query_row(
        "SELECT COALESCE(MAX(id), 0) + 1 FROM ordenes_pedido",
        [], |row| row.get(0),
    ).unwrap_or(1);
    let folio = format!("P-{:06}", siguiente_id);

    db.execute(
        r#"INSERT INTO ordenes_pedido (folio, proveedor_id, usuario_id, estado, notas, fecha_pedido)
           VALUES (?, ?, ?, 'borrador', ?, ?)"#,
        rusqlite::params![folio, orden.proveedor_id, orden.usuario_id, orden.notas, now],
    ).map_err(|e| e.to_string())?;

    let orden_id = db.last_insert_rowid();
    let total_items = orden.items.len() as i64;

    for item in &orden.items {
        db.execute(
            r#"INSERT INTO orden_pedido_detalle (orden_id, producto_id, cantidad_pedida, precio_costo)
               VALUES (?, ?, ?, ?)"#,
            rusqlite::params![orden_id, item.producto_id, item.cantidad_pedida, item.precio_costo],
        ).map_err(|e| e.to_string())?;
    }

    // Bitácora
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
           VALUES (?, 'PEDIDO_CREADO', 'ordenes_pedido', ?, ?, 'POS')"#,
        rusqlite::params![
            orden.usuario_id, orden_id,
            format!("Pedido #{} creado | {} items", orden_id, total_items)
        ],
    );

    let usuario_nombre: String = db.query_row(
        "SELECT nombre_completo FROM usuarios WHERE id = ?",
        rusqlite::params![orden.usuario_id], |row| row.get(0),
    ).unwrap_or_else(|_| "—".to_string());

    let proveedor_nombre: Option<String> = orden.proveedor_id.and_then(|pid| {
        db.query_row("SELECT nombre FROM proveedores WHERE id = ?",
            rusqlite::params![pid], |row| row.get(0)).ok()
    });

    Ok(OrdenPedido {
        id: orden_id,
        proveedor_nombre,
        usuario_nombre,
        estado: "borrador".to_string(),
        notas: orden.notas,
        fecha: now,
        total_items,
    })
}

#[tauri::command]
pub fn listar_ordenes_pedido(
    estado_filtro: Option<String>,
    state: State<'_, AppState>,
) -> Vec<OrdenPedido> {
    let db = state.db.lock().unwrap();

    let query = if let Some(ref estado) = estado_filtro {
        format!(
            r#"SELECT o.id, p.nombre, u.nombre_completo, o.estado, o.notas, o.fecha_pedido,
                      (SELECT COUNT(*) FROM orden_pedido_detalle WHERE orden_id = o.id)
               FROM ordenes_pedido o
               LEFT JOIN proveedores p ON p.id = o.proveedor_id
               LEFT JOIN usuarios u ON u.id = o.usuario_id
               WHERE o.estado = '{}'
               ORDER BY o.fecha_pedido DESC"#,
            estado.replace('\'', "")
        )
    } else {
        r#"SELECT o.id, p.nombre, u.nombre_completo, o.estado, o.notas, o.fecha_pedido,
                  (SELECT COUNT(*) FROM orden_pedido_detalle WHERE orden_id = o.id)
           FROM ordenes_pedido o
           LEFT JOIN proveedores p ON p.id = o.proveedor_id
           LEFT JOIN usuarios u ON u.id = o.usuario_id
           ORDER BY o.fecha_pedido DESC"#.to_string()
    };

    let mut stmt = db.prepare(&query).unwrap();
    stmt.query_map([], |row| {
        Ok(OrdenPedido {
            id: row.get(0)?,
            proveedor_nombre: row.get(1)?,
            usuario_nombre: row.get(2)?,
            estado: row.get(3)?,
            notas: row.get(4)?,
            fecha: row.get(5)?,
            total_items: row.get(6)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

#[tauri::command]
pub fn obtener_detalle_orden(
    orden_id: i64,
    state: State<'_, AppState>,
) -> Vec<OrdenPedidoDetalle> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        r#"SELECT od.id, od.producto_id, p.nombre, p.codigo,
                  od.cantidad_pedida, od.cantidad_recibida, od.precio_costo
           FROM orden_pedido_detalle od
           LEFT JOIN productos p ON p.id = od.producto_id
           WHERE od.orden_id = ?"#,
    ).unwrap();

    stmt.query_map(rusqlite::params![orden_id], |row| {
        Ok(OrdenPedidoDetalle {
            id: row.get(0)?,
            producto_id: row.get(1)?,
            producto_nombre: row.get(2)?,
            producto_codigo: row.get(3)?,
            cantidad_pedida: row.get(4)?,
            cantidad_recibida: row.get(5)?,
            precio_costo: row.get(6)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

#[tauri::command]
pub fn cambiar_estado_orden(
    orden_id: i64,
    nuevo_estado: String,
    usuario_id: i64,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let db = state.db.lock().unwrap();

    db.execute(
        "UPDATE ordenes_pedido SET estado = ? WHERE id = ?",
        rusqlite::params![nuevo_estado, orden_id],
    ).map_err(|e| e.to_string())?;

    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
           VALUES (?, 'PEDIDO_ESTADO', 'ordenes_pedido', ?, ?, 'POS')"#,
        rusqlite::params![
            usuario_id, orden_id,
            format!("Pedido #{} → {}", orden_id, nuevo_estado)
        ],
    );

    Ok(true)
}
