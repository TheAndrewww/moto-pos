// commands/cortes.rs — Módulo de Cortes de Caja para el POS

use serde::{Deserialize, Serialize};
use tauri::State;
use super::auth::AppState;
use chrono::Local;

// ─── Structs ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct NuevoMovimiento {
    pub tipo: String,           // ENTRADA | RETIRO
    pub usuario_id: i64,
    pub monto: f64,
    pub concepto: String,
    pub autorizado_por: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MovimientoCajaRs {
    pub id: i64,
    pub tipo: String,
    pub usuario_id: i64,
    pub usuario_nombre: String,
    pub monto: f64,
    pub concepto: String,
    pub autorizado_por: Option<i64>,
    pub corte_id: Option<i64>,
    pub fecha: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VendedorResumenRs {
    pub usuario_id: i64,
    pub usuario_nombre: String,
    pub num_ventas: i64,
    pub total_vendido: f64,
    pub hora_inicio: String,
    pub hora_fin: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DatosCorte {
    pub fecha_inicio: String,
    pub fecha_fin: String,
    pub fondo_inicial: f64,
    pub total_ventas_efectivo: f64,
    pub total_ventas_tarjeta: f64,
    pub total_ventas_transferencia: f64,
    pub total_ventas: f64,
    pub num_transacciones: i64,
    pub total_descuentos: f64,
    pub total_anulaciones: f64,
    pub total_entradas_efectivo: f64,
    pub total_retiros_efectivo: f64,
    pub efectivo_esperado: f64,
    pub movimientos: Vec<MovimientoCajaRs>,
    pub vendedores: Vec<VendedorResumenRs>,
}

#[derive(Deserialize)]
pub struct DenominacionInput {
    pub denominacion: f64,
    pub tipo: String,   // BILLETE | MONEDA
    pub cantidad: i64,
}

#[derive(Deserialize)]
pub struct NuevoCorte {
    pub tipo: String,           // PARCIAL | DIA
    pub usuario_id: i64,
    pub fecha_inicio: String,
    pub fecha_fin: String,
    pub datos: DatosCorte,
    pub efectivo_contado: f64,
    pub nota_diferencia: Option<String>,
    pub fondo_siguiente: f64,
    pub denominaciones: Option<Vec<DenominacionInput>>,
}

#[derive(Serialize)]
pub struct CorteCreado {
    pub id: i64,
    pub tipo: String,
    pub diferencia: f64,
    pub efectivo_esperado: f64,
    pub efectivo_contado: f64,
    pub fondo_siguiente: f64,
    pub created_at: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct CorteResumen {
    pub id: i64,
    pub tipo: String,
    pub usuario_nombre: String,
    pub created_at: String,
    pub efectivo_esperado: f64,
    pub efectivo_contado: f64,
    pub diferencia: f64,
    pub fondo_siguiente: f64,
}

#[derive(Serialize)]
pub struct CorteDetalle {
    pub corte: CorteResumen,
    pub denominaciones: Vec<DenominacionDetalle>,
    pub movimientos: Vec<MovimientoCajaRs>,
    pub vendedores: Vec<VendedorResumenRs>,
}

#[derive(Serialize)]
pub struct DenominacionDetalle {
    pub denominacion: f64,
    pub tipo: String,
    pub cantidad: i64,
    pub subtotal: f64,
}

// ─── Comandos ─────────────────────────────────────────────

/// Registrar entrada o retiro de efectivo (sin ser una venta)
#[tauri::command]
pub fn crear_movimiento_caja(
    datos: NuevoMovimiento,
    state: State<'_, AppState>,
) -> Result<MovimientoCajaRs, String> {
    if datos.monto <= 0.0 {
        return Err("El monto debe ser mayor a cero".to_string());
    }

    let db = state.db.lock().unwrap();
    let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    db.execute(
        r#"INSERT INTO movimientos_caja (tipo, usuario_id, monto, concepto, autorizado_por, fecha)
           VALUES (?, ?, ?, ?, ?, ?)"#,
        rusqlite::params![
            datos.tipo, datos.usuario_id, datos.monto,
            datos.concepto, datos.autorizado_por, now
        ],
    ).map_err(|e| e.to_string())?;

    let id = db.last_insert_rowid();

    let usuario_nombre: String = db.query_row(
        "SELECT nombre_completo FROM usuarios WHERE id = ?",
        rusqlite::params![datos.usuario_id],
        |row| row.get(0),
    ).unwrap_or_else(|_| "Desconocido".to_string());

    let accion = if datos.tipo == "ENTRADA" { "ENTRADA_CAJA" } else { "RETIRO_CAJA" };
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id,
           descripcion_legible, origen)
           VALUES (?, ?, 'movimientos_caja', ?, ?, 'POS')"#,
        rusqlite::params![
            datos.usuario_id, accion, id,
            format!("{} de ${:.2} — {}", datos.tipo, datos.monto, datos.concepto)
        ],
    );

    Ok(MovimientoCajaRs {
        id,
        tipo: datos.tipo,
        usuario_id: datos.usuario_id,
        usuario_nombre,
        monto: datos.monto,
        concepto: datos.concepto,
        autorizado_por: datos.autorizado_por,
        corte_id: None,
        fecha: now,
    })
}

/// Listar movimientos de caja que aún no han sido asociados a un corte
#[tauri::command]
pub fn listar_movimientos_sin_corte(
    state: State<'_, AppState>,
) -> Result<Vec<MovimientoCajaRs>, String> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        r#"SELECT m.id, m.tipo, m.usuario_id, u.nombre_completo, m.monto,
                  m.concepto, m.autorizado_por, m.corte_id, m.fecha
           FROM movimientos_caja m
           JOIN usuarios u ON u.id = m.usuario_id
           WHERE m.corte_id IS NULL
           ORDER BY m.fecha DESC"#,
    ).map_err(|e| e.to_string())?;

    let items = stmt.query_map([], |row| {
        Ok(MovimientoCajaRs {
            id: row.get(0)?,
            tipo: row.get(1)?,
            usuario_id: row.get(2)?,
            usuario_nombre: row.get(3)?,
            monto: row.get(4)?,
            concepto: row.get(5)?,
            autorizado_por: row.get(6)?,
            corte_id: row.get(7)?,
            fecha: row.get(8)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(items)
}

/// Calcular todos los datos para previsualizar un corte antes de confirmarlo
#[tauri::command]
pub fn calcular_datos_corte(
    fecha_inicio: String,
    fecha_fin: String,
    state: State<'_, AppState>,
) -> Result<DatosCorte, String> {
    let db = state.db.lock().unwrap();

    // Totales de ventas en el rango (no anuladas)
    let efectivo: f64 = db.query_row(
        "SELECT COALESCE(SUM(total), 0) FROM ventas WHERE fecha BETWEEN ? AND ? AND anulada = 0 AND metodo_pago = 'efectivo'",
        rusqlite::params![fecha_inicio, fecha_fin],
        |row| row.get(0),
    ).unwrap_or(0.0);

    let tarjeta: f64 = db.query_row(
        "SELECT COALESCE(SUM(total), 0) FROM ventas WHERE fecha BETWEEN ? AND ? AND anulada = 0 AND metodo_pago = 'tarjeta'",
        rusqlite::params![fecha_inicio, fecha_fin],
        |row| row.get(0),
    ).unwrap_or(0.0);

    let transferencia: f64 = db.query_row(
        "SELECT COALESCE(SUM(total), 0) FROM ventas WHERE fecha BETWEEN ? AND ? AND anulada = 0 AND metodo_pago = 'transferencia'",
        rusqlite::params![fecha_inicio, fecha_fin],
        |row| row.get(0),
    ).unwrap_or(0.0);

    let (num_transacciones, total_ventas, total_descuentos): (i64, f64, f64) = db.query_row(
        "SELECT COUNT(*), COALESCE(SUM(total), 0), COALESCE(SUM(descuento), 0) FROM ventas WHERE fecha BETWEEN ? AND ? AND anulada = 0",
        rusqlite::params![fecha_inicio, fecha_fin],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap_or((0, 0.0, 0.0));

    let total_anulaciones: f64 = db.query_row(
        "SELECT COALESCE(SUM(total), 0) FROM ventas WHERE fecha BETWEEN ? AND ? AND anulada = 1",
        rusqlite::params![fecha_inicio, fecha_fin],
        |row| row.get(0),
    ).unwrap_or(0.0);

    // Movimientos de caja sin corte asignado
    let mut stmt = db.prepare(
        r#"SELECT m.id, m.tipo, m.usuario_id, u.nombre_completo, m.monto,
                  m.concepto, m.autorizado_por, m.corte_id, m.fecha
           FROM movimientos_caja m
           JOIN usuarios u ON u.id = m.usuario_id
           WHERE m.corte_id IS NULL
           ORDER BY m.fecha ASC"#,
    ).map_err(|e| e.to_string())?;

    let movimientos: Vec<MovimientoCajaRs> = stmt.query_map([], |row| {
        Ok(MovimientoCajaRs {
            id: row.get(0)?,
            tipo: row.get(1)?,
            usuario_id: row.get(2)?,
            usuario_nombre: row.get(3)?,
            monto: row.get(4)?,
            concepto: row.get(5)?,
            autorizado_por: row.get(6)?,
            corte_id: row.get(7)?,
            fecha: row.get(8)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    let total_entradas: f64 = movimientos.iter()
        .filter(|m| m.tipo == "ENTRADA")
        .map(|m| m.monto)
        .sum();
    let total_retiros: f64 = movimientos.iter()
        .filter(|m| m.tipo == "RETIRO")
        .map(|m| m.monto)
        .sum();

    // Fondo inicial: prioridad a la apertura del día (si existe).
    // Fallback: fondo_siguiente del corte más reciente. Default: 0.
    let dia = &fecha_inicio[..10]; // YYYY-MM-DD
    let fondo_inicial: f64 = db.query_row(
        "SELECT fondo_declarado FROM aperturas_caja WHERE date(fecha) = ? LIMIT 1",
        rusqlite::params![dia],
        |row| row.get(0),
    ).or_else(|_| {
        db.query_row(
            "SELECT fondo_siguiente FROM cortes ORDER BY created_at DESC LIMIT 1",
            [],
            |row| row.get::<_, f64>(0),
        )
    }).unwrap_or(0.0);

    // fondo + entradas (incluye ventas efectivo) - retiros (incluye devoluciones)
    let efectivo_esperado = fondo_inicial + total_entradas - total_retiros;

    // Resumen por vendedor en el rango
    let mut vstmt = db.prepare(
        r#"SELECT v.usuario_id, u.nombre_completo,
                  COUNT(*) as num_ventas,
                  COALESCE(SUM(v.total), 0) as total,
                  MIN(v.fecha) as hora_inicio,
                  MAX(v.fecha) as hora_fin
           FROM ventas v
           JOIN usuarios u ON u.id = v.usuario_id
           WHERE v.fecha BETWEEN ? AND ? AND v.anulada = 0
           GROUP BY v.usuario_id
           ORDER BY total DESC"#,
    ).map_err(|e| e.to_string())?;

    let vendedores: Vec<VendedorResumenRs> = vstmt.query_map(
        rusqlite::params![fecha_inicio, fecha_fin],
        |row| {
            Ok(VendedorResumenRs {
                usuario_id: row.get(0)?,
                usuario_nombre: row.get(1)?,
                num_ventas: row.get(2)?,
                total_vendido: row.get(3)?,
                hora_inicio: row.get(4)?,
                hora_fin: row.get(5)?,
            })
        }
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(DatosCorte {
        fecha_inicio,
        fecha_fin,
        fondo_inicial,
        total_ventas_efectivo: efectivo,
        total_ventas_tarjeta: tarjeta,
        total_ventas_transferencia: transferencia,
        total_ventas,
        num_transacciones,
        total_descuentos,
        total_anulaciones,
        total_entradas_efectivo: total_entradas,
        total_retiros_efectivo: total_retiros,
        efectivo_esperado,
        movimientos,
        vendedores,
    })
}

/// Confirmar y guardar un corte (parcial o del día)
#[tauri::command]
pub fn crear_corte(
    datos: NuevoCorte,
    state: State<'_, AppState>,
) -> Result<CorteCreado, String> {
    let db = state.db.lock().unwrap();
    let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Solo un corte DIA por día
    if datos.tipo == "DIA" {
        let fecha_hoy = &datos.fecha_inicio[..10]; // YYYY-MM-DD
        let existe: i64 = db.query_row(
            "SELECT COUNT(*) FROM cortes WHERE tipo = 'DIA' AND date(created_at) = ?",
            rusqlite::params![fecha_hoy],
            |row| row.get(0),
        ).unwrap_or(0);

        if existe > 0 {
            return Err("Ya existe un corte del día para esta fecha".to_string());
        }
    }

    let diferencia = datos.efectivo_contado - datos.datos.efectivo_esperado;

    db.execute("BEGIN TRANSACTION", []).map_err(|e| e.to_string())?;

    let result = db.execute(
        r#"INSERT INTO cortes (tipo, usuario_id, fecha_inicio, fecha_fin,
               fondo_inicial, total_ventas_efectivo, total_ventas_tarjeta,
               total_ventas_transferencia, total_ventas, num_transacciones,
               total_descuentos, total_anulaciones, total_entradas_efectivo,
               total_retiros_efectivo, efectivo_esperado, efectivo_contado,
               diferencia, nota_diferencia, fondo_siguiente, created_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        rusqlite::params![
            datos.tipo, datos.usuario_id, datos.fecha_inicio, datos.fecha_fin,
            datos.datos.fondo_inicial,
            datos.datos.total_ventas_efectivo, datos.datos.total_ventas_tarjeta,
            datos.datos.total_ventas_transferencia, datos.datos.total_ventas,
            datos.datos.num_transacciones, datos.datos.total_descuentos,
            datos.datos.total_anulaciones, datos.datos.total_entradas_efectivo,
            datos.datos.total_retiros_efectivo, datos.datos.efectivo_esperado,
            datos.efectivo_contado, diferencia, datos.nota_diferencia,
            datos.fondo_siguiente, now
        ],
    );

    if let Err(e) = result {
        let _ = db.execute("ROLLBACK", []);
        return Err(e.to_string());
    }

    let corte_id = db.last_insert_rowid();

    // Asociar movimientos pendientes a este corte
    if let Err(e) = db.execute(
        "UPDATE movimientos_caja SET corte_id = ? WHERE corte_id IS NULL",
        rusqlite::params![corte_id],
    ) {
        let _ = db.execute("ROLLBACK", []);
        return Err(e.to_string());
    }

    // Guardar denominaciones si se proporcionaron
    if let Some(denoms) = &datos.denominaciones {
        for d in denoms {
            if d.cantidad > 0 {
                let subtotal = d.denominacion * d.cantidad as f64;
                if let Err(e) = db.execute(
                    "INSERT INTO corte_denominaciones (corte_id, denominacion, tipo, cantidad, subtotal) VALUES (?, ?, ?, ?, ?)",
                    rusqlite::params![corte_id, d.denominacion, d.tipo, d.cantidad, subtotal],
                ) {
                    let _ = db.execute("ROLLBACK", []);
                    return Err(e.to_string());
                }
            }
        }
    }

    // Para corte del día, guardar resumen por vendedor
    if datos.tipo == "DIA" {
        for v in &datos.datos.vendedores {
            if let Err(e) = db.execute(
                r#"INSERT INTO corte_vendedores
                   (corte_id, usuario_id, num_ventas, total_vendido, hora_inicio, hora_fin)
                   VALUES (?, ?, ?, ?, ?, ?)"#,
                rusqlite::params![
                    corte_id, v.usuario_id, v.num_ventas,
                    v.total_vendido, v.hora_inicio, v.hora_fin
                ],
            ) {
                let _ = db.execute("ROLLBACK", []);
                return Err(e.to_string());
            }
        }
    }

    // Bitácora
    let accion = if datos.tipo == "DIA" { "CORTE_DIA" } else { "CORTE_PARCIAL" };
    let desc = format!(
        "{} — Esperado: ${:.2} / Contado: ${:.2} / Diferencia: ${:.2}",
        accion, datos.datos.efectivo_esperado, datos.efectivo_contado, diferencia
    );
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id,
           descripcion_legible, origen)
           VALUES (?, ?, 'cortes', ?, ?, 'POS')"#,
        rusqlite::params![datos.usuario_id, accion, corte_id, desc],
    );

    db.execute("COMMIT", []).map_err(|e| e.to_string())?;

    Ok(CorteCreado {
        id: corte_id,
        tipo: datos.tipo,
        diferencia,
        efectivo_esperado: datos.datos.efectivo_esperado,
        efectivo_contado: datos.efectivo_contado,
        fondo_siguiente: datos.fondo_siguiente,
        created_at: now,
    })
}

/// Listar cortes con resumen
#[tauri::command]
pub fn listar_cortes(
    limite: i64,
    state: State<'_, AppState>,
) -> Result<Vec<CorteResumen>, String> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        r#"SELECT c.id, c.tipo, u.nombre_completo, c.created_at,
                  c.efectivo_esperado, c.efectivo_contado, c.diferencia, c.fondo_siguiente
           FROM cortes c
           JOIN usuarios u ON u.id = c.usuario_id
           ORDER BY c.created_at DESC
           LIMIT ?"#,
    ).map_err(|e| e.to_string())?;

    let items = stmt.query_map(rusqlite::params![limite], |row| {
        Ok(CorteResumen {
            id: row.get(0)?,
            tipo: row.get(1)?,
            usuario_nombre: row.get(2)?,
            created_at: row.get(3)?,
            efectivo_esperado: row.get(4)?,
            efectivo_contado: row.get(5)?,
            diferencia: row.get(6)?,
            fondo_siguiente: row.get(7)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(items)
}

/// Obtener detalle completo de un corte (denominaciones + movimientos + vendedores)
#[tauri::command]
pub fn obtener_detalle_corte(
    id: i64,
    state: State<'_, AppState>,
) -> Result<CorteDetalle, String> {
    let db = state.db.lock().unwrap();

    let corte = db.query_row(
        r#"SELECT c.id, c.tipo, u.nombre_completo, c.created_at,
                  c.efectivo_esperado, c.efectivo_contado, c.diferencia, c.fondo_siguiente
           FROM cortes c
           JOIN usuarios u ON u.id = c.usuario_id
           WHERE c.id = ?"#,
        rusqlite::params![id],
        |row| Ok(CorteResumen {
            id: row.get(0)?,
            tipo: row.get(1)?,
            usuario_nombre: row.get(2)?,
            created_at: row.get(3)?,
            efectivo_esperado: row.get(4)?,
            efectivo_contado: row.get(5)?,
            diferencia: row.get(6)?,
            fondo_siguiente: row.get(7)?,
        }),
    ).map_err(|_| "Corte no encontrado".to_string())?;

    let mut dstmt = db.prepare(
        "SELECT denominacion, tipo, cantidad, subtotal FROM corte_denominaciones WHERE corte_id = ? ORDER BY denominacion DESC",
    ).map_err(|e| e.to_string())?;

    let denominaciones: Vec<DenominacionDetalle> = dstmt.query_map(rusqlite::params![id], |row| {
        Ok(DenominacionDetalle {
            denominacion: row.get(0)?,
            tipo: row.get(1)?,
            cantidad: row.get(2)?,
            subtotal: row.get(3)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    let mut mstmt = db.prepare(
        r#"SELECT m.id, m.tipo, m.usuario_id, u.nombre_completo, m.monto,
                  m.concepto, m.autorizado_por, m.corte_id, m.fecha
           FROM movimientos_caja m
           JOIN usuarios u ON u.id = m.usuario_id
           WHERE m.corte_id = ?
           ORDER BY m.fecha ASC"#,
    ).map_err(|e| e.to_string())?;

    let movimientos: Vec<MovimientoCajaRs> = mstmt.query_map(rusqlite::params![id], |row| {
        Ok(MovimientoCajaRs {
            id: row.get(0)?,
            tipo: row.get(1)?,
            usuario_id: row.get(2)?,
            usuario_nombre: row.get(3)?,
            monto: row.get(4)?,
            concepto: row.get(5)?,
            autorizado_por: row.get(6)?,
            corte_id: row.get(7)?,
            fecha: row.get(8)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    let mut vstmt = db.prepare(
        r#"SELECT cv.usuario_id, u.nombre_completo, cv.num_ventas, cv.total_vendido,
                  cv.hora_inicio, cv.hora_fin
           FROM corte_vendedores cv
           JOIN usuarios u ON u.id = cv.usuario_id
           WHERE cv.corte_id = ?
           ORDER BY cv.total_vendido DESC"#,
    ).map_err(|e| e.to_string())?;

    let vendedores: Vec<VendedorResumenRs> = vstmt.query_map(rusqlite::params![id], |row| {
        Ok(VendedorResumenRs {
            usuario_id: row.get(0)?,
            usuario_nombre: row.get(1)?,
            num_ventas: row.get(2)?,
            total_vendido: row.get(3)?,
            hora_inicio: row.get(4)?,
            hora_fin: row.get(5)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(CorteDetalle { corte, denominaciones, movimientos, vendedores })
}

// ─── Apertura de caja ─────────────────────────────────────

#[derive(Deserialize)]
pub struct NuevaApertura {
    pub usuario_id: i64,
    pub fondo_declarado: f64,
    pub nota: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct AperturaCaja {
    pub id: i64,
    pub usuario_id: i64,
    pub usuario_nombre: String,
    pub fondo_declarado: f64,
    pub nota: Option<String>,
    pub fecha: String,
}

/// Registrar la apertura de caja del día
#[tauri::command]
pub fn crear_apertura_caja(
    datos: NuevaApertura,
    state: State<'_, AppState>,
) -> Result<AperturaCaja, String> {
    if datos.fondo_declarado < 0.0 {
        return Err("El fondo no puede ser negativo".to_string());
    }

    let db = state.db.lock().unwrap();
    let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Validar que no exista ya una apertura para hoy
    let existe: i64 = db.query_row(
        "SELECT COUNT(*) FROM aperturas_caja WHERE date(fecha) = date('now', 'localtime')",
        [],
        |row| row.get(0),
    ).unwrap_or(0);

    if existe > 0 {
        return Err("Ya existe una apertura de caja para hoy".to_string());
    }

    db.execute(
        "INSERT INTO aperturas_caja (usuario_id, fondo_declarado, nota, fecha) VALUES (?, ?, ?, ?)",
        rusqlite::params![datos.usuario_id, datos.fondo_declarado, datos.nota, now],
    ).map_err(|e| e.to_string())?;

    let id = db.last_insert_rowid();

    let usuario_nombre: String = db.query_row(
        "SELECT nombre_completo FROM usuarios WHERE id = ?",
        rusqlite::params![datos.usuario_id],
        |row| row.get(0),
    ).unwrap_or_else(|_| "Desconocido".to_string());

    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id,
           descripcion_legible, origen)
           VALUES (?, 'APERTURA_CAJA', 'aperturas_caja', ?, ?, 'POS')"#,
        rusqlite::params![
            datos.usuario_id, id,
            format!("Apertura de caja con fondo de ${:.2}", datos.fondo_declarado)
        ],
    );

    Ok(AperturaCaja {
        id,
        usuario_id: datos.usuario_id,
        usuario_nombre,
        fondo_declarado: datos.fondo_declarado,
        nota: datos.nota,
        fecha: now,
    })
}

/// Obtener la apertura de caja de hoy (si existe)
#[tauri::command]
pub fn obtener_apertura_hoy(
    state: State<'_, AppState>,
) -> Result<Option<AperturaCaja>, String> {
    let db = state.db.lock().unwrap();

    let resultado = db.query_row(
        r#"SELECT a.id, a.usuario_id, u.nombre_completo, a.fondo_declarado, a.nota, a.fecha
           FROM aperturas_caja a
           JOIN usuarios u ON u.id = a.usuario_id
           WHERE date(a.fecha) = date('now', 'localtime')
           LIMIT 1"#,
        [],
        |row| Ok(AperturaCaja {
            id: row.get(0)?,
            usuario_id: row.get(1)?,
            usuario_nombre: row.get(2)?,
            fondo_declarado: row.get(3)?,
            nota: row.get(4)?,
            fecha: row.get(5)?,
        }),
    ).ok();

    Ok(resultado)
}

/// Obtener el fondo sugerido para la próxima apertura
/// (= fondo_siguiente del último corte del día)
#[tauri::command]
pub fn obtener_fondo_sugerido(
    state: State<'_, AppState>,
) -> Result<f64, String> {
    let db = state.db.lock().unwrap();
    let fondo: f64 = db.query_row(
        "SELECT fondo_siguiente FROM cortes WHERE tipo = 'DIA' ORDER BY created_at DESC LIMIT 1",
        [],
        |row| row.get(0),
    ).unwrap_or(2000.0); // Default: $2,000
    Ok(fondo)
}

/// Verificar si hay un corte del día pendiente de ayer
#[tauri::command]
pub fn verificar_corte_dia_pendiente(
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    let db = state.db.lock().unwrap();

    // Obtener la fecha del día más reciente con ventas que no tenga corte DIA
    let resultado: Option<String> = db.query_row(
        r#"SELECT date(v.fecha) as dia
           FROM ventas v
           WHERE date(v.fecha) < date('now', 'localtime')
           AND NOT EXISTS (
               SELECT 1 FROM cortes c
               WHERE c.tipo = 'DIA'
               AND date(c.fecha_fin) = date(v.fecha)
           )
           GROUP BY dia
           ORDER BY dia DESC
           LIMIT 1"#,
        [],
        |row| row.get(0),
    ).ok();

    Ok(resultado)
}
