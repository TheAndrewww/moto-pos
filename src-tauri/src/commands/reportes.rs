// commands/reportes.rs — Agregaciones de ventas para la página de Reportes.
//
// Estos comandos son la paridad Tauri (SQLite local) de los handlers
// `obtener_top_productos`/`obtener_ventas_por_*` que viven en
// `server-remoto/src/rpc.rs` para la web. La página `Reportes.tsx` los
// invoca por igual en ambas plataformas — sin estos en Tauri, el desktop
// daría "command not found".
//
// El bug que estos reemplazan: la versión vieja del frontend iteraba
// hasta 200 ventas y agregaba en memoria. Eso falseaba el top de productos
// cuando había más de 200 ventas en el rango. Con SQL agregado en el
// servidor (o en SQLite vía estos handlers) el conteo es exacto.

use serde::{Deserialize, Serialize};
use tauri::State;
use super::auth::AppState;

// Args comunes: rango de fechas en formato "YYYY-MM-DD HH:MM:SS" y límite
// opcional (solo lo usa top_productos; los demás devuelven todas las
// agregaciones del rango porque la cardinalidad es baja).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RangoReporte {
    pub fecha_inicio: String,
    pub fecha_fin: String,
    #[serde(default)]
    pub limite: Option<i64>,
}

#[derive(Serialize)]
pub struct TopProducto {
    pub producto_id: i64,
    pub codigo: String,
    pub nombre: String,
    pub cantidad: f64,
    pub total: f64,
}

#[derive(Serialize)]
pub struct VendedorAgg {
    pub nombre: String,
    pub count: i64,
    pub total: f64,
}

#[derive(Serialize)]
pub struct MetodoAgg {
    pub metodo: String,
    pub count: i64,
    pub total: f64,
}

#[derive(Serialize)]
pub struct DiaAgg {
    pub fecha: String,
    pub count: i64,
    pub total: f64,
}

/// Top N productos vendidos en el rango (cantidad + total).
/// Reemplaza el patrón viejo del frontend de iterar venta por venta.
///
/// Comparación por `substr(fecha, 1, 10)` para que sea robusta frente a
/// fechas en formato con espacio o ISO con 'T'.
#[tauri::command]
pub fn obtener_top_productos(
    rango: RangoReporte,
    state: State<'_, AppState>,
) -> Result<Vec<TopProducto>, String> {
    let limite = rango.limite.unwrap_or(10).clamp(1, 100);
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = db.prepare(
        r#"
        SELECT vd.producto_id,
               COALESCE(p.codigo, '') AS codigo,
               COALESCE(p.nombre, '(producto eliminado)') AS nombre,
               SUM(vd.cantidad) AS cantidad,
               SUM(vd.subtotal) AS total
        FROM venta_detalle vd
        JOIN ventas v ON v.id = vd.venta_id
        LEFT JOIN productos p ON p.id = vd.producto_id
        WHERE v.anulada = 0
          AND substr(v.fecha, 1, 10) BETWEEN substr(?, 1, 10) AND substr(?, 1, 10)
        GROUP BY vd.producto_id, p.codigo, p.nombre
        ORDER BY cantidad DESC
        LIMIT ?
        "#,
    ).map_err(|e| e.to_string())?;

    let rows = stmt.query_map(
        rusqlite::params![rango.fecha_inicio, rango.fecha_fin, limite],
        |row| {
            Ok(TopProducto {
                producto_id: row.get(0)?,
                codigo: row.get(1)?,
                nombre: row.get(2)?,
                cantidad: row.get(3)?,
                total: row.get(4)?,
            })
        },
    ).map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Totales agrupados por vendedor.
#[tauri::command]
pub fn obtener_ventas_por_vendedor(
    rango: RangoReporte,
    state: State<'_, AppState>,
) -> Result<Vec<VendedorAgg>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = db.prepare(
        r#"
        SELECT COALESCE(u.nombre_completo, '(usuario ' || v.usuario_id || ')') AS nombre,
               COUNT(*) AS count,
               COALESCE(SUM(v.total), 0) AS total
        FROM ventas v
        LEFT JOIN usuarios u ON u.id = v.usuario_id
        WHERE v.anulada = 0
          AND substr(v.fecha, 1, 10) BETWEEN substr(?, 1, 10) AND substr(?, 1, 10)
        GROUP BY v.usuario_id, u.nombre_completo
        ORDER BY total DESC
        "#,
    ).map_err(|e| e.to_string())?;

    let rows = stmt.query_map(
        rusqlite::params![rango.fecha_inicio, rango.fecha_fin],
        |row| {
            Ok(VendedorAgg {
                nombre: row.get(0)?,
                count: row.get(1)?,
                total: row.get(2)?,
            })
        },
    ).map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Totales agrupados por método de pago.
#[tauri::command]
pub fn obtener_ventas_por_metodo(
    rango: RangoReporte,
    state: State<'_, AppState>,
) -> Result<Vec<MetodoAgg>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = db.prepare(
        r#"
        SELECT v.metodo_pago AS metodo,
               COUNT(*) AS count,
               COALESCE(SUM(v.total), 0) AS total
        FROM ventas v
        WHERE v.anulada = 0
          AND substr(v.fecha, 1, 10) BETWEEN substr(?, 1, 10) AND substr(?, 1, 10)
        GROUP BY v.metodo_pago
        ORDER BY total DESC
        "#,
    ).map_err(|e| e.to_string())?;

    let rows = stmt.query_map(
        rusqlite::params![rango.fecha_inicio, rango.fecha_fin],
        |row| {
            Ok(MetodoAgg {
                metodo: row.get(0)?,
                count: row.get(1)?,
                total: row.get(2)?,
            })
        },
    ).map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Totales agrupados por día del rango.
#[tauri::command]
pub fn obtener_ventas_por_dia(
    rango: RangoReporte,
    state: State<'_, AppState>,
) -> Result<Vec<DiaAgg>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = db.prepare(
        r#"
        SELECT substr(v.fecha, 1, 10) AS fecha,
               COUNT(*) AS count,
               COALESCE(SUM(v.total), 0) AS total
        FROM ventas v
        WHERE v.anulada = 0
          AND substr(v.fecha, 1, 10) BETWEEN substr(?, 1, 10) AND substr(?, 1, 10)
        GROUP BY substr(v.fecha, 1, 10)
        ORDER BY fecha ASC
        "#,
    ).map_err(|e| e.to_string())?;

    let rows = stmt.query_map(
        rusqlite::params![rango.fecha_inicio, rango.fecha_fin],
        |row| {
            Ok(DiaAgg {
                fecha: row.get(0)?,
                count: row.get(1)?,
                total: row.get(2)?,
            })
        },
    ).map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}
