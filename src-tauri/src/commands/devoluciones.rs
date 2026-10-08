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
    /// ¿El reembolso sale del efectivo de la caja? Si no se indica, se usa el
    /// método de pago de la venta: efectivo → sí; tarjeta/transferencia → no
    /// (se reembolsa por el mismo medio y la caja no se mueve).
    #[serde(default)]
    pub reembolso_efectivo: Option<bool>,
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
    crear_devolucion_core(&db, datos, &now)
}

pub(crate) fn crear_devolucion_core(
    db: &rusqlite::Connection,
    datos: NuevaDevolucion,
    now: &str,
) -> Result<DevolucionCreada, String> {
    let now = now.to_string();
    if datos.motivo.trim().is_empty() {
        return Err("El motivo es obligatorio".to_string());
    }
    if datos.items.is_empty() {
        return Err("Debe incluir al menos un producto a devolver".to_string());
    }

    // Verificar venta no anulada ni borrada (borrado lógico del sync, igual
    // que el servidor)
    let (venta_folio, venta_metodo, venta_total): (String, String, f64) = db.query_row(
        "SELECT folio, metodo_pago, total FROM ventas WHERE id = ? AND anulada = 0 AND deleted_at IS NULL",
        rusqlite::params![datos.venta_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).map_err(|_| "Venta no encontrada o está anulada".to_string())?;

    let reembolso_efectivo = datos.reembolso_efectivo.unwrap_or(venta_metodo == "efectivo");

    // Verificar rol del usuario: si no es dueño, requiere autorizado_por
    let es_admin: i64 = db.query_row(
        "SELECT r.es_admin FROM usuarios u JOIN roles r ON r.id = u.rol_id WHERE u.id = ?",
        rusqlite::params![datos.usuario_id],
        |row| row.get(0),
    ).unwrap_or(0);

    if es_admin == 0 && datos.autorizado_por.is_none() {
        return Err("Se requiere autorización del dueño para registrar devoluciones".to_string());
    }

    // Agrupar por partida: si la misma partida viene dos veces en la
    // solicitud, cada renglón se validaba por separado contra lo ya devuelto
    // en BD y se podía devolver más de lo vendido.
    let mut por_partida: Vec<(i64, f64)> = Vec::new();
    for it in &datos.items {
        if it.cantidad <= 0.0 {
            return Err("Las cantidades deben ser mayores a 0".to_string());
        }
        match por_partida.iter_mut().find(|(id, _)| *id == it.venta_detalle_id) {
            Some((_, c)) => *c += it.cantidad,
            None => por_partida.push((it.venta_detalle_id, it.cantidad)),
        }
    }

    // Validar cantidades contra cada venta_detalle
    let mut total_devuelto = 0.0_f64;
    let mut items_validados: Vec<(i64, i64, f64, f64, f64)> = Vec::new();
    // (venta_detalle_id, producto_id, cantidad, precio_unitario, subtotal)

    for (vd_id, cantidad) in &por_partida {
        let (vd_venta_id, prod_id, cantidad_orig, precio_final): (i64, i64, f64, f64) = db.query_row(
            "SELECT venta_id, producto_id, cantidad, precio_final FROM venta_detalle WHERE id = ?",
            rusqlite::params![vd_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).map_err(|_| format!("Partida de venta no encontrada: id={}", vd_id))?;

        if vd_venta_id != datos.venta_id {
            return Err("Una de las partidas no pertenece a la venta".to_string());
        }

        let ya_devuelto: f64 = db.query_row(
            "SELECT COALESCE(SUM(cantidad), 0) FROM devolucion_detalle WHERE venta_detalle_id = ?",
            rusqlite::params![vd_id],
            |row| row.get(0),
        ).unwrap_or(0.0);

        let disponible = cantidad_orig - ya_devuelto;
        if *cantidad > disponible + 0.0001 {
            return Err(format!(
                "Cantidad excede lo disponible (vendido {}, ya devuelto {}, queda {})",
                cantidad_orig, ya_devuelto, disponible
            ));
        }

        let subtotal = cantidad * precio_final;
        total_devuelto += subtotal;
        items_validados.push((*vd_id, prod_id, *cantidad, precio_final, subtotal));
    }

    // Devolución que completa la venta: regresar exactamente lo que se cobró.
    // El cobro se redondea al peso hacia arriba (67.80 → 68), así que sumar
    // precio × cantidad regresaba 67.80 aunque el cliente pagó 68 y el corte
    // registró 68. Esos centavos descuadraban la caja.
    let pendiente_tras_esta: f64 = {
        let mut stmt = db.prepare(
            "SELECT vd.id, vd.cantidad - COALESCE((SELECT SUM(dd.cantidad) FROM devolucion_detalle dd \
                                                  WHERE dd.venta_detalle_id = vd.id), 0) \
             FROM venta_detalle vd WHERE vd.venta_id = ?",
        ).map_err(|e| e.to_string())?;
        let filas: Vec<(i64, f64)> = stmt.query_map(rusqlite::params![datos.venta_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        }).map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
        filas.iter().map(|(id, restante)| {
            let ahora = por_partida.iter().find(|(p, _)| p == id).map(|(_, c)| *c).unwrap_or(0.0);
            (restante - ahora).max(0.0)
        }).sum()
    };
    if pendiente_tras_esta < 0.0001 {
        let devuelto_antes: f64 = db.query_row(
            "SELECT COALESCE(SUM(total_devuelto), 0) FROM devoluciones WHERE venta_id = ?",
            rusqlite::params![datos.venta_id],
            |r| r.get(0),
        ).unwrap_or(0.0);
        total_devuelto = (venta_total - devuelto_antes).max(0.0);
    }
    total_devuelto = super::cortes::round2(total_devuelto);

    // Transacción (el folio se consume dentro para no dejar huecos si falla)
    db.execute("BEGIN TRANSACTION", []).map_err(|e| e.to_string())?;

    let resultado = (|| -> Result<(i64, String), String> {
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

        db.execute(
            r#"INSERT INTO devoluciones (folio, venta_id, usuario_id, autorizado_por, motivo,
                                         total_devuelto, fecha)
               VALUES (?, ?, ?, ?, ?, ?, ?)"#,
            rusqlite::params![
                folio, datos.venta_id, datos.usuario_id,
                datos.autorizado_por, datos.motivo, total_devuelto, now
            ],
        ).map_err(|e| e.to_string())?;
        let devolucion_id = db.last_insert_rowid();

        for (vd_id, prod_id, cantidad, precio, subtotal) in &items_validados {
            db.execute(
                r#"INSERT INTO devolucion_detalle
                   (devolucion_id, venta_detalle_id, producto_id, cantidad,
                    precio_unitario, subtotal)
                   VALUES (?, ?, ?, ?, ?, ?)"#,
                rusqlite::params![devolucion_id, vd_id, prod_id, cantidad, precio, subtotal],
            ).map_err(|e| format!("Error al insertar detalle: {}", e))?;

            // Se suma la cantidad devuelta completa: la pieza regresa al
            // anaquel, aunque la venta haya descontado menos porque el stock
            // se frena en 0 (ver la regla de stock en ventas.rs).
            db.execute(
                "UPDATE productos SET stock_actual = stock_actual + ?, updated_at = ? WHERE id = ?",
                rusqlite::params![cantidad, now, prod_id],
            ).map_err(|e| format!("Error al restaurar stock: {}", e))?;
        }

        // El reembolso solo mueve la caja si se paga en efectivo. Antes se
        // registraba SIEMPRE un retiro, también para ventas con tarjeta o
        // transferencia: la caja "perdía" en papel un dinero que nunca salió
        // y el corte marcaba sobrante.
        // El retiro siempre es de la caja de la tienda (caja 'principal' por
        // default), también si la venta vino de la web: el dinero sale de
        // este cajón. Con el reloj atrasado (en o antes del último corte)
        // entra igual al período abierto: registrado_at = fin del corte + 1 s
        // (ver "RELOJ MONÓTONO" en cortes.rs).
        if reembolso_efectivo && total_devuelto > 0.0 {
            let concepto = format!("Devolución {} de venta {} — {}", folio, venta_folio, datos.motivo);
            db.execute(
                r#"INSERT INTO movimientos_caja (tipo, usuario_id, monto, concepto, autorizado_por, fecha,
                                                 registrado_at)
                   VALUES ('RETIRO', ?, ?, ?, ?, ?, ?)"#,
                rusqlite::params![
                    datos.usuario_id, total_devuelto, concepto, datos.autorizado_por, now,
                    super::cortes::registrado_at_para(db, &now)
                ],
            ).map_err(|e| format!("Error al registrar movimiento de caja: {}", e))?;
            let mov_id = db.last_insert_rowid();

            db.execute(
                "UPDATE devoluciones SET movimiento_caja_id = ? WHERE id = ?",
                rusqlite::params![mov_id, devolucion_id],
            ).map_err(|e| format!("Error al vincular movimiento: {}", e))?;
        }

        let medio = if reembolso_efectivo { "efectivo de caja" } else { "mismo medio de pago (no sale de caja)" };
        let _ = db.execute(
            r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id,
               descripcion_legible, origen)
               VALUES (?, 'DEVOLUCION', 'devoluciones', ?, ?, 'POS')"#,
            rusqlite::params![
                datos.usuario_id, devolucion_id,
                format!("Devolución {} de venta {} — ${:.2} reembolsado en {} — {}",
                        folio, venta_folio, total_devuelto, medio, datos.motivo)
            ],
        );
        Ok((devolucion_id, folio))
    })();

    let (devolucion_id, folio) = match resultado {
        Ok(v) => { db.execute("COMMIT", []).map_err(|e| e.to_string())?; v }
        Err(e) => { let _ = db.execute("ROLLBACK", []); return Err(e); }
    };

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
