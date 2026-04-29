// commands/ventas.rs — Comandos de ventas para el POS

use serde::{Deserialize, Serialize};
use tauri::State;
use super::auth::AppState;
use chrono::Local;

// ─── Structs ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ItemVenta {
    pub producto_id: i64,
    pub cantidad: f64,
    pub precio_original: f64,
    pub descuento_porcentaje: f64,
    pub descuento_monto: f64,
    pub precio_final: f64,
    pub subtotal: f64,
    pub autorizado_por: Option<i64>,
}

#[derive(Deserialize)]
pub struct NuevaVenta {
    pub usuario_id: i64,
    pub cliente_id: Option<i64>,
    pub subtotal: f64,
    pub descuento: f64,
    pub total: f64,
    pub metodo_pago: String,
    pub monto_recibido: f64,
    pub cambio: f64,
    pub items: Vec<ItemVenta>,
    #[serde(default)]
    pub presupuesto_origen_id: Option<i64>,
}

#[derive(Serialize)]
pub struct VentaCreada {
    pub id: i64,
    pub folio: String,
    pub total: f64,
    pub cambio: f64,
    pub fecha: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct VentaResumen {
    pub id: i64,
    pub folio: String,
    pub usuario_nombre: String,
    pub cliente_nombre: Option<String>,
    pub total: f64,
    pub metodo_pago: String,
    pub anulada: bool,
    pub fecha: String,
    pub num_productos: i64,
}

#[derive(Serialize, Clone, Debug)]
pub struct VentaDetalleItem {
    pub id: i64,
    pub producto_id: i64,
    pub codigo: String,
    pub nombre: String,
    pub cantidad: f64,
    pub cantidad_devuelta: f64,
    pub cantidad_disponible: f64,
    pub precio_original: f64,
    pub descuento_porcentaje: f64,
    pub descuento_monto: f64,
    pub precio_final: f64,
    pub subtotal: f64,
}

#[derive(Serialize, Clone, Debug)]
pub struct VentaDetalleCompleto {
    pub id: i64,
    pub folio: String,
    pub usuario_id: i64,
    pub usuario_nombre: String,
    pub cliente_id: Option<i64>,
    pub cliente_nombre: Option<String>,
    pub subtotal: f64,
    pub descuento: f64,
    pub total: f64,
    pub metodo_pago: String,
    pub anulada: bool,
    pub anulada_por_nombre: Option<String>,
    pub motivo_anulacion: Option<String>,
    pub fecha: String,
    pub items: Vec<VentaDetalleItem>,
    pub total_devuelto: f64,
}

#[derive(Serialize, Clone, Debug)]
pub struct EstadisticasDia {
    pub total_ventas: f64,
    pub num_transacciones: i64,
    pub efectivo: f64,
    pub tarjeta: f64,
    pub transferencia: f64,
    pub producto_top_nombre: Option<String>,
    pub producto_top_cantidad: f64,
}

// ─── Comandos ─────────────────────────────────────────────

/// Crear una venta completa con todos sus items
#[tauri::command]
pub fn crear_venta(
    venta: NuevaVenta,
    state: State<'_, AppState>,
) -> Result<VentaCreada, String> {
    let db = state.db.lock().unwrap();
    let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Nota: Aunque el POS permite vender sin existencias (el empleado puede agregar al carrito),
    // el stock_actual siempre se frena en cero (0) mediante MAX() en lugar de quedar en negativo.

    // Generar folio con secuencia dedicada (nunca se duplica)
    let ultimo_folio: i64 = db.query_row(
        "SELECT ultimo_valor FROM folio_secuencia WHERE id = 1",
        [], |row| row.get(0),
    ).unwrap_or(0);
    let nuevo_folio = ultimo_folio + 1;
    db.execute(
        "UPDATE folio_secuencia SET ultimo_valor = ? WHERE id = 1",
        rusqlite::params![nuevo_folio],
    ).map_err(|e| e.to_string())?;
    let folio = format!("V-{:06}", nuevo_folio);

    // Usar transacción para asegurar atomicidad
    db.execute("BEGIN TRANSACTION", []).map_err(|e| e.to_string())?;

    // Insertar venta
    let result = db.execute(
        r#"INSERT INTO ventas (folio, usuario_id, cliente_id, subtotal, descuento, total,
           metodo_pago, monto_recibido, cambio, fecha)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        rusqlite::params![
            folio, venta.usuario_id, venta.cliente_id,
            venta.subtotal, venta.descuento, venta.total,
            venta.metodo_pago, venta.monto_recibido, venta.cambio, now
        ],
    );

    if let Err(e) = result {
        let _ = db.execute("ROLLBACK", []);
        return Err(e.to_string());
    }

    let venta_id = db.last_insert_rowid();

    // Insertar detalle y actualizar stock
    for item in &venta.items {
        let r = db.execute(
            r#"INSERT INTO venta_detalle
               (venta_id, producto_id, cantidad,
                precio_original, descuento_porcentaje, descuento_monto,
                precio_final, subtotal, autorizado_por)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            rusqlite::params![
                venta_id, item.producto_id, item.cantidad,
                item.precio_original,
                item.descuento_porcentaje, item.descuento_monto,
                item.precio_final, item.subtotal, item.autorizado_por
            ],
        );

        if let Err(e) = r {
            let _ = db.execute("ROLLBACK", []);
            return Err(format!("Error al insertar detalle: {}", e));
        }

        // Descontar stock (no permitir que baje de cero)
        let r = db.execute(
            "UPDATE productos SET stock_actual = MAX(0, stock_actual - ?), updated_at = ? WHERE id = ?",
            rusqlite::params![item.cantidad, now, item.producto_id],
        );

        if let Err(e) = r {
            let _ = db.execute("ROLLBACK", []);
            return Err(format!("Error al actualizar stock: {}", e));
        }
    }

    // Bitácora
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id,
           descripcion_legible, origen)
           VALUES (?, 'VENTA', 'ventas', ?, ?, 'POS')"#,
        rusqlite::params![
            venta.usuario_id, venta_id,
            format!("Venta {} — ${:.2} — {} — {} productos",
                folio, venta.total, venta.metodo_pago, venta.items.len())
        ],
    );

    // NOTA: No creamos movimiento_caja para ventas en efectivo porque
    // calcular_datos_corte ya consulta la tabla 'ventas' directamente.
    // Crear un movimiento aquí causaría doble conteo.

    // Si la venta viene de un presupuesto, marcarlo como convertido
    if let Some(presup_id) = venta.presupuesto_origen_id {
        let _ = db.execute(
            "UPDATE presupuestos SET estado = 'convertido', venta_id = ? WHERE id = ? AND estado != 'cancelado'",
            rusqlite::params![venta_id, presup_id],
        );
    }

    db.execute("COMMIT", []).map_err(|e| e.to_string())?;

    Ok(VentaCreada {
        id: venta_id,
        folio: folio.clone(),
        total: venta.total,
        cambio: venta.cambio,
        fecha: now,
    })
}

/// Obtener ventas del día actual
#[tauri::command]
pub fn listar_ventas_dia(state: State<'_, AppState>) -> Vec<VentaResumen> {
    let db = state.db.lock().unwrap();
    let hoy = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut stmt = db.prepare(
        r#"
        SELECT v.id, v.folio, u.nombre_completo, cl.nombre,
               v.total, v.metodo_pago, v.anulada, v.fecha,
               (SELECT COUNT(*) FROM venta_detalle vd WHERE vd.venta_id = v.id)
        FROM ventas v
        JOIN usuarios u ON u.id = v.usuario_id
        LEFT JOIN clientes cl ON cl.id = v.cliente_id
        WHERE date(v.fecha) = ?
        ORDER BY v.fecha DESC
        "#,
    ).unwrap();

    stmt.query_map(rusqlite::params![hoy], |row| {
        Ok(VentaResumen {
            id: row.get(0)?,
            folio: row.get(1)?,
            usuario_nombre: row.get(2)?,
            cliente_nombre: row.get(3)?,
            total: row.get(4)?,
            metodo_pago: row.get(5)?,
            anulada: row.get(6)?,
            fecha: row.get(7)?,
            num_productos: row.get(8)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Estadísticas del día
#[tauri::command]
pub fn obtener_estadisticas_dia(state: State<'_, AppState>) -> EstadisticasDia {
    let db = state.db.lock().unwrap();

    let hoy = chrono::Local::now().format("%Y-%m-%d").to_string();

    let (total, num) = db.query_row(
        "SELECT COALESCE(SUM(total), 0), COUNT(*) FROM ventas WHERE date(fecha) = ? AND anulada = 0",
        rusqlite::params![hoy],
        |row| Ok((row.get::<_, f64>(0)?, row.get::<_, i64>(1)?)),
    ).unwrap_or((0.0, 0));

    let efectivo: f64 = db.query_row(
        "SELECT COALESCE(SUM(total), 0) FROM ventas WHERE date(fecha) = ? AND anulada = 0 AND metodo_pago = 'efectivo'",
        rusqlite::params![hoy], |row| row.get(0),
    ).unwrap_or(0.0);

    let tarjeta: f64 = db.query_row(
        "SELECT COALESCE(SUM(total), 0) FROM ventas WHERE date(fecha) = ? AND anulada = 0 AND metodo_pago = 'tarjeta'",
        rusqlite::params![hoy], |row| row.get(0),
    ).unwrap_or(0.0);

    let transferencia: f64 = db.query_row(
        "SELECT COALESCE(SUM(total), 0) FROM ventas WHERE date(fecha) = ? AND anulada = 0 AND metodo_pago = 'transferencia'",
        rusqlite::params![hoy], |row| row.get(0),
    ).unwrap_or(0.0);

    // Producto más vendido
    let top = db.query_row(
        r#"
        SELECT p.nombre, SUM(vd.cantidad) as qty
        FROM venta_detalle vd
        JOIN ventas v ON v.id = vd.venta_id
        JOIN productos p ON p.id = vd.producto_id
        WHERE date(v.fecha) = ? AND v.anulada = 0
        GROUP BY vd.producto_id
        ORDER BY qty DESC
        LIMIT 1
        "#,
        rusqlite::params![hoy],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
    );

    let (producto_top_nombre, producto_top_cantidad) = match top {
        Ok((n, q)) => (Some(n), q),
        Err(_) => (None, 0.0),
    };

    EstadisticasDia {
        total_ventas: total,
        num_transacciones: num,
        efectivo,
        tarjeta,
        transferencia,
        producto_top_nombre,
        producto_top_cantidad,
    }
}

/// Anular una venta (solo dueño)
#[tauri::command]
pub fn anular_venta(
    venta_id: i64,
    usuario_id: i64,
    motivo: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let db = state.db.lock().unwrap();
    let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Verificar que la venta existe, no está anulada y es del mismo día
    let (folio, fecha_venta, metodo_pago, total_venta): (String, String, String, f64) = db.query_row(
        "SELECT folio, fecha, metodo_pago, total FROM ventas WHERE id = ? AND anulada = 0",
        rusqlite::params![venta_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).map_err(|_| "Venta no encontrada o ya anulada".to_string())?;

    let hoy = Local::now().format("%Y-%m-%d").to_string();
    if !fecha_venta.starts_with(&hoy) {
        return Err("Solo se puede anular una venta del día en curso. Para ventas anteriores, usa el flujo de devolución.".to_string());
    }

    // Verificar que no tenga devoluciones parciales ya aplicadas
    let num_dev: i64 = db.query_row(
        "SELECT COUNT(*) FROM devoluciones WHERE venta_id = ?",
        rusqlite::params![venta_id],
        |row| row.get(0),
    ).unwrap_or(0);
    if num_dev > 0 {
        return Err("La venta tiene devoluciones parciales registradas. No se puede anular completa.".to_string());
    }

    db.execute("BEGIN TRANSACTION", []).map_err(|e| e.to_string())?;

    // Restaurar stock
    let mut stmt = db.prepare(
        "SELECT producto_id, cantidad FROM venta_detalle WHERE venta_id = ?"
    ).map_err(|e| e.to_string())?;

    let items: Vec<(i64, f64)> = stmt.query_map(
        rusqlite::params![venta_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    for (prod_id, cantidad) in &items {
        let _ = db.execute(
            "UPDATE productos SET stock_actual = stock_actual + ?, updated_at = ? WHERE id = ?",
            rusqlite::params![cantidad, now, prod_id],
        );
    }

    // Marcar como anulada
    db.execute(
        "UPDATE ventas SET anulada = 1, anulada_por = ?, motivo_anulacion = ? WHERE id = ?",
        rusqlite::params![usuario_id, motivo, venta_id],
    ).map_err(|e| { let _ = db.execute("ROLLBACK", []); e.to_string() })?;

    // NOTA: No creamos movimiento_caja para anulaciones porque
    // calcular_datos_corte ya filtra ventas con anulada = 0.
    // Crear un retiro aquí causaría doble conteo.

    // Bitácora
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id,
           descripcion_legible, origen)
           VALUES (?, 'ANULACION', 'ventas', ?, ?, 'POS')"#,
        rusqlite::params![
            usuario_id, venta_id,
            format!("Venta {} anulada — Motivo: {}", folio, motivo)
        ],
    );

    db.execute("COMMIT", []).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Buscar ventas por folio / rango de fechas / cliente (histórico completo)
#[tauri::command]
pub fn buscar_ventas(
    folio: Option<String>,
    fecha_inicio: Option<String>,
    fecha_fin: Option<String>,
    cliente_texto: Option<String>,
    articulo_texto: Option<String>,
    limite: Option<i64>,
    state: State<'_, AppState>,
) -> Result<Vec<VentaResumen>, String> {
    let db = state.db.lock().unwrap();

    let mut sql = String::from(
        r#"SELECT v.id, v.folio, u.nombre_completo, cl.nombre,
                  v.total, v.metodo_pago, v.anulada, v.fecha,
                  (SELECT COUNT(*) FROM venta_detalle vd WHERE vd.venta_id = v.id)
           FROM ventas v
           JOIN usuarios u ON u.id = v.usuario_id
           LEFT JOIN clientes cl ON cl.id = v.cliente_id
           WHERE 1=1"#,
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(f) = folio.as_ref().filter(|s| !s.trim().is_empty()) {
        sql.push_str(" AND v.folio LIKE ?");
        params.push(Box::new(format!("%{}%", f.trim())));
    }
    if let Some(fi) = fecha_inicio.as_ref().filter(|s| !s.trim().is_empty()) {
        sql.push_str(" AND date(v.fecha) >= date(?)");
        params.push(Box::new(fi.clone()));
    }
    if let Some(ff) = fecha_fin.as_ref().filter(|s| !s.trim().is_empty()) {
        sql.push_str(" AND date(v.fecha) <= date(?)");
        params.push(Box::new(ff.clone()));
    }
    if let Some(c) = cliente_texto.as_ref().filter(|s| !s.trim().is_empty()) {
        sql.push_str(" AND (cl.nombre LIKE ? OR cl.telefono LIKE ?)");
        let like = format!("%{}%", c.trim());
        params.push(Box::new(like.clone()));
        params.push(Box::new(like));
    }
    if let Some(a) = articulo_texto.as_ref().filter(|s| !s.trim().is_empty()) {
        sql.push_str(r#" AND EXISTS (
            SELECT 1 FROM venta_detalle vd
            JOIN productos p ON p.id = vd.producto_id
            WHERE vd.venta_id = v.id AND (p.codigo LIKE ? OR p.search_text LIKE ?)
        )"#);
        let like = format!("%{}%", super::productos::normalizar_texto(a.trim()));
        let like_codigo = format!("%{}%", a.trim());
        params.push(Box::new(like_codigo));
        params.push(Box::new(like));
    }
    sql.push_str(" ORDER BY v.fecha DESC LIMIT ?");
    params.push(Box::new(limite.unwrap_or(100)));

    let mut stmt = db.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())), |row| {
            Ok(VentaResumen {
                id: row.get(0)?,
                folio: row.get(1)?,
                usuario_nombre: row.get(2)?,
                cliente_nombre: row.get(3)?,
                total: row.get(4)?,
                metodo_pago: row.get(5)?,
                anulada: row.get(6)?,
                fecha: row.get(7)?,
                num_productos: row.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Detalle completo de una venta (items + cantidades devueltas)
#[tauri::command]
pub fn obtener_detalle_venta(
    venta_id: i64,
    state: State<'_, AppState>,
) -> Result<VentaDetalleCompleto, String> {
    let db = state.db.lock().unwrap();

    let (id, folio, usuario_id, usuario_nombre, cliente_id, cliente_nombre,
         subtotal, descuento, total, metodo_pago, anulada, anulada_por_nombre,
         motivo_anulacion, fecha): (
        i64, String, i64, String, Option<i64>, Option<String>,
        f64, f64, f64, String, bool, Option<String>,
        Option<String>, String
    ) = db.query_row(
        r#"SELECT v.id, v.folio, v.usuario_id, u.nombre_completo,
                  v.cliente_id, cl.nombre,
                  v.subtotal, v.descuento, v.total, v.metodo_pago,
                  v.anulada, ua.nombre_completo, v.motivo_anulacion, v.fecha
           FROM ventas v
           JOIN usuarios u ON u.id = v.usuario_id
           LEFT JOIN usuarios ua ON ua.id = v.anulada_por
           LEFT JOIN clientes cl ON cl.id = v.cliente_id
           WHERE v.id = ?"#,
        rusqlite::params![venta_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                  row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
                  row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?,
                  row.get(12)?, row.get(13)?)),
    ).map_err(|_| "Venta no encontrada".to_string())?;

    let mut stmt = db.prepare(
        r#"SELECT vd.id, vd.producto_id, p.codigo, p.nombre,
                  vd.cantidad, vd.precio_original, vd.descuento_porcentaje,
                  vd.descuento_monto, vd.precio_final, vd.subtotal,
                  COALESCE((SELECT SUM(dd.cantidad) FROM devolucion_detalle dd
                            WHERE dd.venta_detalle_id = vd.id), 0) AS cantidad_devuelta
           FROM venta_detalle vd
           JOIN productos p ON p.id = vd.producto_id
           WHERE vd.venta_id = ?
           ORDER BY vd.id"#,
    ).map_err(|e| e.to_string())?;

    let items: Vec<VentaDetalleItem> = stmt
        .query_map(rusqlite::params![venta_id], |row| {
            let cantidad: f64 = row.get(4)?;
            let devuelta: f64 = row.get(10)?;
            Ok(VentaDetalleItem {
                id: row.get(0)?,
                producto_id: row.get(1)?,
                codigo: row.get(2)?,
                nombre: row.get(3)?,
                cantidad,
                cantidad_devuelta: devuelta,
                cantidad_disponible: (cantidad - devuelta).max(0.0),
                precio_original: row.get(5)?,
                descuento_porcentaje: row.get(6)?,
                descuento_monto: row.get(7)?,
                precio_final: row.get(8)?,
                subtotal: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    let total_devuelto: f64 = db.query_row(
        "SELECT COALESCE(SUM(total_devuelto), 0) FROM devoluciones WHERE venta_id = ?",
        rusqlite::params![venta_id],
        |row| row.get(0),
    ).unwrap_or(0.0);

    Ok(VentaDetalleCompleto {
        id, folio, usuario_id, usuario_nombre, cliente_id, cliente_nombre,
        subtotal, descuento, total, metodo_pago, anulada, anulada_por_nombre,
        motivo_anulacion, fecha, items, total_devuelto,
    })
}
