// commands/presupuestos.rs — CRUD de presupuestos (cotizaciones)

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::State;
use crate::commands::auth::AppState;

#[derive(Serialize, Clone)]
pub struct Presupuesto {
    pub id: i64,
    pub folio: String,
    pub usuario_nombre: String,
    pub cliente_nombre: Option<String>,
    pub estado: String,
    pub notas: Option<String>,
    pub vigencia_dias: i64,
    pub total: f64,
    pub fecha: String,
}

#[derive(Serialize, Clone)]
pub struct PresupuestoDetalle {
    pub id: i64,
    pub producto_id: Option<i64>,
    pub producto_nombre: Option<String>,
    pub descripcion: String,
    pub cantidad: f64,
    pub precio_unitario: f64,
    pub descuento_porcentaje: f64,
    pub subtotal: f64,
}

#[derive(Deserialize)]
pub struct ItemPresupuesto {
    pub producto_id: Option<i64>,
    pub descripcion: String,
    pub cantidad: f64,
    pub precio_unitario: f64,
    pub descuento_porcentaje: f64,
    pub subtotal: f64,
}

#[derive(Deserialize)]
pub struct DatosPresupuesto {
    pub usuario_id: i64,
    pub cliente_id: Option<i64>,
    pub notas: Option<String>,
    pub vigencia_dias: Option<i64>,
    pub total: f64,
    pub items: Vec<ItemPresupuesto>,
}

// ─── Secuencia de folios de presupuesto ───

fn generar_folio_presupuesto(db: &rusqlite::Connection) -> Result<String, String> {
    // Usamos un contador simple basado en COUNT de presupuestos
    let count: i64 = db.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM presupuestos",
        [], |row| row.get(0),
    ).map_err(|e| e.to_string())?;
    Ok(format!("P-{:06}", count + 1))
}

#[tauri::command]
pub fn crear_presupuesto(
    presupuesto: DatosPresupuesto,
    state: State<'_, AppState>,
) -> Result<Presupuesto, String> {
    let db = state.db.lock().unwrap();
    let folio = generar_folio_presupuesto(&db)?;
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let vigencia = presupuesto.vigencia_dias.unwrap_or(7);

    db.execute(
        r#"INSERT INTO presupuestos (folio, usuario_id, cliente_id, estado, notas, vigencia_dias, total, fecha)
           VALUES (?, ?, ?, 'pendiente', ?, ?, ?, ?)"#,
        rusqlite::params![
            folio, presupuesto.usuario_id, presupuesto.cliente_id,
            presupuesto.notas, vigencia, presupuesto.total, now
        ],
    ).map_err(|e| e.to_string())?;

    let presup_id = db.last_insert_rowid();

    for item in &presupuesto.items {
        db.execute(
            r#"INSERT INTO presupuesto_detalle
               (presupuesto_id, producto_id, descripcion, cantidad, precio_unitario, descuento_porcentaje, subtotal)
               VALUES (?, ?, ?, ?, ?, ?, ?)"#,
            rusqlite::params![
                presup_id, item.producto_id, item.descripcion,
                item.cantidad, item.precio_unitario, item.descuento_porcentaje, item.subtotal
            ],
        ).map_err(|e| e.to_string())?;
    }

    // Bitácora
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
           VALUES (?, 'PRESUPUESTO_CREADO', 'presupuestos', ?, ?, 'POS')"#,
        rusqlite::params![
            presupuesto.usuario_id, presup_id,
            format!("Presupuesto {} creado | Total: ${:.2}", folio, presupuesto.total)
        ],
    );

    // Obtener nombre del usuario
    let usuario_nombre: String = db.query_row(
        "SELECT nombre_completo FROM usuarios WHERE id = ?",
        rusqlite::params![presupuesto.usuario_id],
        |row| row.get(0),
    ).unwrap_or_else(|_| "—".to_string());

    let cliente_nombre: Option<String> = presupuesto.cliente_id.and_then(|cid| {
        db.query_row(
            "SELECT nombre FROM clientes WHERE id = ?",
            rusqlite::params![cid],
            |row| row.get(0),
        ).ok()
    });

    Ok(Presupuesto {
        id: presup_id,
        folio,
        usuario_nombre,
        cliente_nombre,
        estado: "pendiente".to_string(),
        notas: presupuesto.notas,
        vigencia_dias: vigencia,
        total: presupuesto.total,
        fecha: now,
    })
}

#[tauri::command]
pub fn listar_presupuestos(
    estado_filtro: Option<String>,
    state: State<'_, AppState>,
) -> Vec<Presupuesto> {
    let db = state.db.lock().unwrap();

    let query = if let Some(ref estado) = estado_filtro {
        format!(
            r#"SELECT p.id, p.folio, u.nombre_completo, c.nombre, p.estado,
                      p.notas, p.vigencia_dias, p.total, p.fecha
               FROM presupuestos p
               LEFT JOIN usuarios u ON u.id = p.usuario_id
               LEFT JOIN clientes c ON c.id = p.cliente_id
               WHERE p.estado = '{}'
               ORDER BY p.fecha DESC"#,
            estado.replace('\'', "")
        )
    } else {
        r#"SELECT p.id, p.folio, u.nombre_completo, c.nombre, p.estado,
                  p.notas, p.vigencia_dias, p.total, p.fecha
           FROM presupuestos p
           LEFT JOIN usuarios u ON u.id = p.usuario_id
           LEFT JOIN clientes c ON c.id = p.cliente_id
           ORDER BY p.fecha DESC"#.to_string()
    };

    let mut stmt = db.prepare(&query).unwrap();
    stmt.query_map([], |row| {
        Ok(Presupuesto {
            id: row.get(0)?,
            folio: row.get(1)?,
            usuario_nombre: row.get(2)?,
            cliente_nombre: row.get(3)?,
            estado: row.get(4)?,
            notas: row.get(5)?,
            vigencia_dias: row.get(6)?,
            total: row.get(7)?,
            fecha: row.get(8)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

#[tauri::command]
pub fn obtener_detalle_presupuesto(
    presupuesto_id: i64,
    state: State<'_, AppState>,
) -> Vec<PresupuestoDetalle> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        r#"SELECT pd.id, pd.producto_id, p.nombre, pd.descripcion,
                  pd.cantidad, pd.precio_unitario, pd.descuento_porcentaje, pd.subtotal
           FROM presupuesto_detalle pd
           LEFT JOIN productos p ON p.id = pd.producto_id
           WHERE pd.presupuesto_id = ?"#,
    ).unwrap();

    stmt.query_map(rusqlite::params![presupuesto_id], |row| {
        Ok(PresupuestoDetalle {
            id: row.get(0)?,
            producto_id: row.get(1)?,
            producto_nombre: row.get(2)?,
            descripcion: row.get(3)?,
            cantidad: row.get(4)?,
            precio_unitario: row.get(5)?,
            descuento_porcentaje: row.get(6)?,
            subtotal: row.get(7)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

#[tauri::command]
pub fn cambiar_estado_presupuesto(
    presupuesto_id: i64,
    nuevo_estado: String,
    usuario_id: i64,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let db = state.db.lock().unwrap();

    db.execute(
        "UPDATE presupuestos SET estado = ? WHERE id = ?",
        rusqlite::params![nuevo_estado, presupuesto_id],
    ).map_err(|e| e.to_string())?;

    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
           VALUES (?, 'PRESUPUESTO_ESTADO', 'presupuestos', ?, ?, 'POS')"#,
        rusqlite::params![
            usuario_id, presupuesto_id,
            format!("Presupuesto #{} cambiado a: {}", presupuesto_id, nuevo_estado)
        ],
    );

    Ok(true)
}
