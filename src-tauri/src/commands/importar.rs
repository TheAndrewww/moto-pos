// commands/importar.rs — Importador de catálogo de productos desde CSV pipe-separado
// Formato esperado: codigo | nombre | precio_venta | stock | proveedor
// Modo `reemplazar`: borra ventas de prueba + productos + proveedores antes de importar.
// Modo upsert: mantiene datos existentes; actualiza por código o inserta si no existe.

use serde::Serialize;
use tauri::State;
use super::auth::AppState;
use chrono::Utc;
use std::collections::HashMap;
use std::fs;

#[derive(Serialize)]
pub struct ResultadoImportacion {
    pub total_lineas: usize,
    pub insertados: usize,
    pub actualizados: usize,
    pub omitidos: usize,
    pub proveedores_creados: usize,
    pub errores: Vec<String>,
}

fn normalizar_texto(t: &str) -> String {
    t.to_lowercase()
        .replace('á', "a").replace('é', "e").replace('í', "i")
        .replace('ó', "o").replace('ú', "u").replace('ñ', "n")
        .replace('ü', "u")
}

fn parse_f64(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(0.0) }
}

#[tauri::command]
pub fn importar_catalogo_csv(
    ruta: String,
    reemplazar: bool,
    state: State<'_, AppState>,
) -> Result<ResultadoImportacion, String> {
    let contenido = fs::read_to_string(&ruta)
        .map_err(|e| format!("No se pudo leer el archivo: {}", e))?;

    let mut db = state.db.lock().unwrap();
    let tx = db.transaction().map_err(|e| e.to_string())?;

    // ─── Modo reemplazo: limpieza en orden FK inverso ───────
    if reemplazar {
        // Orden: hijos → padres. Cada tabla referencia productos/ventas/proveedores.
        let tablas = [
            "devolucion_detalle",
            "devoluciones",
            "venta_detalle",
            "ventas",
            "presupuesto_detalle",
            "presupuestos",
            "orden_pedido_detalle",
            "ordenes_pedido",
            "recepcion_detalle",
            "recepciones",
            "corte_denominaciones",
            "corte_vendedores",
            "movimientos_caja",
            "cortes",
            "productos",
            "proveedores",
        ];
        for tabla in tablas {
            tx.execute(&format!("DELETE FROM {}", tabla), [])
                .map_err(|e| format!("DELETE {}: {}", tabla, e))?;
        }
        // Reiniciar AUTOINCREMENT de las tablas reiniciadas
        let _ = tx.execute(
            "DELETE FROM sqlite_sequence WHERE name IN (
                'productos','proveedores','ventas','venta_detalle',
                'devoluciones','devolucion_detalle','presupuestos','presupuesto_detalle',
                'ordenes_pedido','orden_pedido_detalle','recepciones','recepcion_detalle',
                'cortes','corte_denominaciones','corte_vendedores','movimientos_caja'
            )",
            [],
        );
    }

    let mut res = ResultadoImportacion {
        total_lineas: 0,
        insertados: 0,
        actualizados: 0,
        omitidos: 0,
        proveedores_creados: 0,
        errores: Vec::new(),
    };
    let now = Utc::now().to_rfc3339();

    // Cache de proveedores: nombre → id
    let mut proveedores_cache: HashMap<String, i64> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT id, nombre FROM proveedores").map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?;
        for row in rows {
            if let Ok((id, nombre)) = row {
                proveedores_cache.insert(nombre.to_uppercase(), id);
            }
        }
    }

    for (idx, linea) in contenido.lines().enumerate() {
        let linea = linea.trim();
        if linea.is_empty() { continue; }
        res.total_lineas += 1;

        let cols: Vec<&str> = linea.split('|').collect();
        if cols.len() < 5 {
            res.errores.push(format!("Línea {}: se esperaban 5 columnas, hay {}", idx + 1, cols.len()));
            res.omitidos += 1;
            continue;
        }

        let codigo = cols[0].trim();
        let nombre = cols[1].trim();
        let precio = parse_f64(cols[2]);
        let stock = parse_f64(cols[3]);
        let proveedor_raw = cols[4].trim();

        if codigo.is_empty() || nombre.is_empty() {
            res.omitidos += 1;
            continue;
        }

        // ─── Resolver / crear proveedor ─────────────────────
        let proveedor_id: Option<i64> = if proveedor_raw.is_empty() {
            None
        } else {
            let key = proveedor_raw.to_uppercase();
            if let Some(&id) = proveedores_cache.get(&key) {
                Some(id)
            } else {
                match tx.execute(
                    "INSERT INTO proveedores (nombre) VALUES (?)",
                    [proveedor_raw],
                ) {
                    Ok(_) => {
                        let id = tx.last_insert_rowid();
                        proveedores_cache.insert(key, id);
                        res.proveedores_creados += 1;
                        Some(id)
                    }
                    Err(e) => {
                        res.errores.push(format!("Línea {} proveedor '{}': {}", idx + 1, proveedor_raw, e));
                        None
                    }
                }
            }
        };

        let search_text = normalizar_texto(&format!("{} {}", codigo, nombre));

        // ─── UPSERT producto ────────────────────────────────
        let existe: Option<i64> = tx.query_row(
            "SELECT id FROM productos WHERE codigo = ?",
            [codigo],
            |r| r.get(0),
        ).ok();

        if let Some(id) = existe {
            let r = tx.execute(
                "UPDATE productos SET nombre = ?, precio_venta = ?, stock_actual = ?,
                 proveedor_id = ?, search_text = ?, activo = 1, updated_at = ?
                 WHERE id = ?",
                rusqlite::params![nombre, precio, stock, proveedor_id, search_text, now, id],
            );
            match r {
                Ok(_) => res.actualizados += 1,
                Err(e) => {
                    res.errores.push(format!("Línea {} ({}): {}", idx + 1, codigo, e));
                    res.omitidos += 1;
                }
            }
        } else {
            let r = tx.execute(
                "INSERT INTO productos (codigo, codigo_tipo, nombre, precio_costo, precio_venta,
                 stock_actual, stock_minimo, proveedor_id, search_text, activo, created_at, updated_at)
                 VALUES (?, 'INTERNO', ?, 0, ?, ?, 0, ?, ?, 1, ?, ?)",
                rusqlite::params![codigo, nombre, precio, stock, proveedor_id, search_text, now, now],
            );
            match r {
                Ok(_) => res.insertados += 1,
                Err(e) => {
                    res.errores.push(format!("Línea {} ({}): {}", idx + 1, codigo, e));
                    res.omitidos += 1;
                }
            }
        }
    }

    tx.commit().map_err(|e| e.to_string())?;

    if res.errores.len() > 50 {
        res.errores.truncate(50);
        res.errores.push("... (truncado)".to_string());
    }

    log::info!(
        "Importación catálogo: {} insertados, {} actualizados, {} omitidos, {} proveedores creados (de {} líneas)",
        res.insertados, res.actualizados, res.omitidos, res.proveedores_creados, res.total_lineas
    );

    Ok(res)
}
