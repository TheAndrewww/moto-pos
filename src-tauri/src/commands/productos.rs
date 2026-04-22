// commands/productos.rs — CRUD de productos y generación de códigos internos

use serde::{Deserialize, Serialize};
use tauri::State;
use super::auth::AppState;
use chrono::Utc;

// ─── Structs ──────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Producto {
    pub id: i64,
    pub codigo: String,
    pub codigo_tipo: String,
    pub nombre: String,
    pub descripcion: Option<String>,
    pub categoria_id: Option<i64>,
    pub categoria_nombre: Option<String>,
    pub precio_costo: f64,
    pub precio_venta: f64,
    pub stock_actual: f64,
    pub stock_minimo: f64,
    pub proveedor_id: Option<i64>,
    pub proveedor_nombre: Option<String>,
    pub foto_url: Option<String>,
    pub activo: bool,
}

#[derive(Deserialize)]
pub struct NuevoProducto {
    pub codigo: Option<String>,
    pub codigo_tipo: Option<String>,
    pub nombre: String,
    pub descripcion: Option<String>,
    pub categoria_id: Option<i64>,
    pub precio_costo: f64,
    pub precio_venta: f64,
    pub stock_actual: f64,
    pub stock_minimo: f64,
    pub proveedor_id: Option<i64>,
    pub foto_url: Option<String>,
}

#[derive(Deserialize)]
pub struct ActualizarProducto {
    pub id: i64,
    pub nombre: String,
    pub descripcion: Option<String>,
    pub categoria_id: Option<i64>,
    pub precio_costo: f64,
    pub precio_venta: f64,
    pub stock_minimo: f64,
    pub proveedor_id: Option<i64>,
    pub foto_url: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Categoria {
    pub id: i64,
    pub nombre: String,
    pub descripcion: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Proveedor {
    pub id: i64,
    pub nombre: String,
    pub contacto: Option<String>,
    pub telefono: Option<String>,
    pub email: Option<String>,
    pub notas: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Cliente {
    pub id: i64,
    pub nombre: String,
    pub telefono: Option<String>,
    pub email: Option<String>,
    pub descuento_porcentaje: f64,
    pub notas: Option<String>,
    pub activo: bool,
}

#[derive(Deserialize)]
pub struct ActualizarCliente {
    pub id: i64,
    pub nombre: String,
    pub telefono: Option<String>,
    pub email: Option<String>,
    pub descuento_porcentaje: f64,
    pub notas: Option<String>,
}

// ─── Helpers ──────────────────────────────────────────────

fn normalizar_texto(texto: &str) -> String {
    texto
        .to_lowercase()
        .replace('á', "a")
        .replace('é', "e")
        .replace('í', "i")
        .replace('ó', "o")
        .replace('ú', "u")
        .replace('ñ', "n")
        .replace('ü', "u")
}

// ─── Comandos de Productos ────────────────────────────────

/// Listar todos los productos activos
#[tauri::command]
pub fn listar_productos(state: State<'_, AppState>) -> Vec<Producto> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        r#"
        SELECT p.id, p.codigo, p.codigo_tipo, p.nombre, p.descripcion,
               p.categoria_id, c.nombre, p.precio_costo, p.precio_venta,
               p.stock_actual, p.stock_minimo, p.proveedor_id, pr.nombre,
               p.foto_url, p.activo
        FROM productos p
        LEFT JOIN categorias c ON c.id = p.categoria_id
        LEFT JOIN proveedores pr ON pr.id = p.proveedor_id
        WHERE p.activo = 1
        ORDER BY p.nombre ASC
        "#,
    ).unwrap();

    stmt.query_map([], |row| {
        Ok(Producto {
            id: row.get(0)?,
            codigo: row.get(1)?,
            codigo_tipo: row.get(2)?,
            nombre: row.get(3)?,
            descripcion: row.get(4)?,
            categoria_id: row.get(5)?,
            categoria_nombre: row.get(6)?,
            precio_costo: row.get(7)?,
            precio_venta: row.get(8)?,
            stock_actual: row.get(9)?,
            stock_minimo: row.get(10)?,
            proveedor_id: row.get(11)?,
            proveedor_nombre: row.get(12)?,
            foto_url: row.get(13)?,
            activo: row.get(14)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Obtener producto por código (para escaneo)
#[tauri::command]
pub fn obtener_producto_por_codigo(
    codigo: String,
    state: State<'_, AppState>,
) -> Option<Producto> {
    let db = state.db.lock().unwrap();
    db.query_row(
        r#"
        SELECT p.id, p.codigo, p.codigo_tipo, p.nombre, p.descripcion,
               p.categoria_id, c.nombre, p.precio_costo, p.precio_venta,
               p.stock_actual, p.stock_minimo, p.proveedor_id, pr.nombre,
               p.foto_url, p.activo
        FROM productos p
        LEFT JOIN categorias c ON c.id = p.categoria_id
        LEFT JOIN proveedores pr ON pr.id = p.proveedor_id
        WHERE p.codigo = ? AND p.activo = 1
        "#,
        rusqlite::params![codigo],
        |row| {
            Ok(Producto {
                id: row.get(0)?,
                codigo: row.get(1)?,
                codigo_tipo: row.get(2)?,
                nombre: row.get(3)?,
                descripcion: row.get(4)?,
                categoria_id: row.get(5)?,
                categoria_nombre: row.get(6)?,
                precio_costo: row.get(7)?,
                precio_venta: row.get(8)?,
                stock_actual: row.get(9)?,
                stock_minimo: row.get(10)?,
                proveedor_id: row.get(11)?,
                proveedor_nombre: row.get(12)?,
                foto_url: row.get(13)?,
                activo: row.get(14)?,
            })
        },
    ).ok()
}

/// Generar código interno MR-XXXXX
#[tauri::command]
pub fn generar_codigo_interno(state: State<'_, AppState>) -> Result<String, String> {
    let db = state.db.lock().unwrap();

    // Obtener el último valor de la secuencia y avanzar
    let ultimo: i64 = db.query_row(
        "SELECT ultimo_valor FROM codigo_secuencia WHERE id = 1",
        [],
        |row| row.get(0),
    ).map_err(|e| e.to_string())?;

    let nuevo = ultimo + 1;
    db.execute(
        "UPDATE codigo_secuencia SET ultimo_valor = ? WHERE id = 1",
        rusqlite::params![nuevo],
    ).map_err(|e| e.to_string())?;

    Ok(format!("MR-{:05}", nuevo))
}

/// Crear un nuevo producto
#[tauri::command]
pub fn crear_producto(
    producto: NuevoProducto,
    usuario_id: i64,
    state: State<'_, AppState>,
) -> Result<Producto, String> {
    let db = state.db.lock().unwrap();

    // Generar código si no se proporcionó
    let codigo = match producto.codigo {
        Some(c) if !c.is_empty() => c,
        _ => {
            let ultimo: i64 = db.query_row(
                "SELECT ultimo_valor FROM codigo_secuencia WHERE id = 1",
                [], |row| row.get(0),
            ).map_err(|e| e.to_string())?;
            let nuevo = ultimo + 1;
            db.execute(
                "UPDATE codigo_secuencia SET ultimo_valor = ? WHERE id = 1",
                rusqlite::params![nuevo],
            ).map_err(|e| e.to_string())?;
            format!("MR-{:05}", nuevo)
        }
    };

    let codigo_tipo = producto.codigo_tipo.unwrap_or_else(|| "INTERNO".to_string());
    let search_text = normalizar_texto(&format!("{} {} {}",
        codigo,
        producto.nombre,
        producto.descripcion.as_deref().unwrap_or("")
    ));

    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    db.execute(
        r#"INSERT INTO productos
           (codigo, codigo_tipo, nombre, descripcion, categoria_id,
            precio_costo, precio_venta,
            stock_actual, stock_minimo, proveedor_id, foto_url, search_text,
            created_at, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        rusqlite::params![
            codigo, codigo_tipo, producto.nombre, producto.descripcion, producto.categoria_id,
            producto.precio_costo, producto.precio_venta,
            producto.stock_actual, producto.stock_minimo, producto.proveedor_id,
            producto.foto_url, search_text, now, now
        ],
    ).map_err(|e| e.to_string())?;

    let id = db.last_insert_rowid();

    // Bitácora
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
           VALUES (?, 'PRODUCTO_CREADO', 'productos', ?, ?, 'POS')"#,
        rusqlite::params![
            usuario_id, id,
            format!("Producto creado: {} ({})", producto.nombre, codigo)
        ],
    );

    // Devolver el producto completo
    Ok(Producto {
        id,
        codigo,
        codigo_tipo,
        nombre: producto.nombre,
        descripcion: producto.descripcion,
        categoria_id: producto.categoria_id,
        categoria_nombre: None,
        precio_costo: producto.precio_costo,
        precio_venta: producto.precio_venta,
        stock_actual: producto.stock_actual,
        stock_minimo: producto.stock_minimo,
        proveedor_id: producto.proveedor_id,
        proveedor_nombre: None,
        foto_url: producto.foto_url,
        activo: true,
    })
}

/// Actualizar un producto existente
#[tauri::command]
pub fn actualizar_producto(
    producto: ActualizarProducto,
    usuario_id: i64,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let db = state.db.lock().unwrap();

    // Obtener datos anteriores para bitácora
    let datos_ant: Option<String> = db.query_row(
        "SELECT nombre || ' | costo:' || precio_costo || ' | venta:' || precio_venta FROM productos WHERE id = ?",
        rusqlite::params![producto.id],
        |row| row.get(0),
    ).ok();

    let search_text = normalizar_texto(&format!("{} {}",
        producto.nombre,
        producto.descripcion.as_deref().unwrap_or("")
    ));

    let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    db.execute(
        r#"UPDATE productos SET
            nombre = ?, descripcion = ?, categoria_id = ?,
            precio_costo = ?, precio_venta = ?,
            stock_minimo = ?, proveedor_id = ?, foto_url = ?,
            search_text = ?, updated_at = ?
           WHERE id = ?"#,
        rusqlite::params![
            producto.nombre, producto.descripcion, producto.categoria_id,
            producto.precio_costo, producto.precio_venta,
            producto.stock_minimo, producto.proveedor_id, producto.foto_url,
            search_text, now, producto.id
        ],
    ).map_err(|e| e.to_string())?;

    // Bitácora
    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id,
           datos_anteriores, descripcion_legible, origen)
           VALUES (?, 'PRODUCTO_EDITADO', 'productos', ?, ?, ?, 'POS')"#,
        rusqlite::params![
            usuario_id, producto.id,
            datos_ant.unwrap_or_default(),
            format!("Producto editado: {}", producto.nombre)
        ],
    );

    Ok(true)
}

/// Productos con stock ≤ stock_minimo (alerta de reorden)
#[tauri::command]
pub fn listar_productos_stock_bajo(state: State<'_, AppState>) -> Vec<Producto> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        r#"
        SELECT p.id, p.codigo, p.codigo_tipo, p.nombre, p.descripcion,
               p.categoria_id, c.nombre, p.precio_costo, p.precio_venta,
               p.stock_actual, p.stock_minimo, p.proveedor_id, pr.nombre,
               p.foto_url, p.activo
        FROM productos p
        LEFT JOIN categorias c ON c.id = p.categoria_id
        LEFT JOIN proveedores pr ON pr.id = p.proveedor_id
        WHERE p.activo = 1 AND p.stock_minimo > 0 AND p.stock_actual <= p.stock_minimo
        ORDER BY (p.stock_actual / NULLIF(p.stock_minimo, 0)) ASC, p.nombre ASC
        "#,
    ).unwrap();

    stmt.query_map([], |row| {
        Ok(Producto {
            id: row.get(0)?, codigo: row.get(1)?, codigo_tipo: row.get(2)?,
            nombre: row.get(3)?, descripcion: row.get(4)?,
            categoria_id: row.get(5)?, categoria_nombre: row.get(6)?,
            precio_costo: row.get(7)?, precio_venta: row.get(8)?,
            stock_actual: row.get(9)?, stock_minimo: row.get(10)?,
            proveedor_id: row.get(11)?, proveedor_nombre: row.get(12)?,
            foto_url: row.get(13)?, activo: row.get(14)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

// ─── Comandos de Categorías ───────────────────────────────

#[tauri::command]
pub fn listar_categorias(state: State<'_, AppState>) -> Vec<Categoria> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, nombre, descripcion FROM categorias ORDER BY nombre"
    ).unwrap();

    stmt.query_map([], |row| {
        Ok(Categoria {
            id: row.get(0)?,
            nombre: row.get(1)?,
            descripcion: row.get(2)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

// ─── Comandos de Proveedores ──────────────────────────────

#[tauri::command]
pub fn listar_proveedores(state: State<'_, AppState>) -> Vec<Proveedor> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, nombre, contacto, telefono, email, notas FROM proveedores ORDER BY nombre"
    ).unwrap();

    stmt.query_map([], |row| {
        Ok(Proveedor {
            id: row.get(0)?,
            nombre: row.get(1)?,
            contacto: row.get(2)?,
            telefono: row.get(3)?,
            email: row.get(4)?,
            notas: row.get(5)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

// ─── Comandos de Clientes ─────────────────────────────────

#[tauri::command]
pub fn listar_clientes(state: State<'_, AppState>) -> Vec<Cliente> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id, nombre, telefono, email, descuento_porcentaje, notas, activo FROM clientes ORDER BY nombre"
    ).unwrap();

    stmt.query_map([], |row| {
        Ok(Cliente {
            id: row.get(0)?,
            nombre: row.get(1)?,
            telefono: row.get(2)?,
            email: row.get(3)?,
            descuento_porcentaje: row.get(4)?,
            notas: row.get(5)?,
            activo: row.get::<_, i64>(6)? != 0,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

#[tauri::command]
pub fn crear_cliente(
    nombre: String,
    telefono: Option<String>,
    email: Option<String>,
    descuento_porcentaje: f64,
    notas: Option<String>,
    state: State<'_, AppState>,
) -> Result<Cliente, String> {
    let db = state.db.lock().unwrap();

    db.execute(
        "INSERT INTO clientes (nombre, telefono, email, descuento_porcentaje, notas, activo) VALUES (?, ?, ?, ?, ?, 1)",
        rusqlite::params![nombre, telefono, email, descuento_porcentaje, notas],
    ).map_err(|e| e.to_string())?;

    let id = db.last_insert_rowid();
    Ok(Cliente { id, nombre, telefono, email, descuento_porcentaje, notas, activo: true })
}

#[tauri::command]
pub fn actualizar_cliente(
    datos: ActualizarCliente,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let db = state.db.lock().unwrap();
    db.execute(
        "UPDATE clientes SET nombre = ?, telefono = ?, email = ?, descuento_porcentaje = ?, notas = ? WHERE id = ?",
        rusqlite::params![
            datos.nombre, datos.telefono, datos.email,
            datos.descuento_porcentaje, datos.notas, datos.id
        ],
    ).map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
pub fn toggle_cliente_activo(id: i64, state: State<'_, AppState>) -> Result<bool, String> {
    let db = state.db.lock().unwrap();
    db.execute(
        "UPDATE clientes SET activo = CASE activo WHEN 1 THEN 0 ELSE 1 END WHERE id = ?",
        rusqlite::params![id],
    ).map_err(|e| e.to_string())?;
    Ok(true)
}

// ─── Comandos de Config ───────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ConfigDescuentos {
    pub descuento_max_vendedor_pct: f64,
    pub descuento_max_total_pct: f64,
    pub precio_minimo_global_margen: f64,
}

#[tauri::command]
pub fn obtener_config_descuentos(state: State<'_, AppState>) -> ConfigDescuentos {
    let db = state.db.lock().unwrap();
    db.query_row(
        "SELECT descuento_max_vendedor_pct, descuento_max_total_pct, precio_minimo_global_margen FROM config_descuentos WHERE id = 1",
        [],
        |row| Ok(ConfigDescuentos {
            descuento_max_vendedor_pct: row.get(0)?,
            descuento_max_total_pct: row.get(1)?,
            precio_minimo_global_margen: row.get(2)?,
        }),
    ).unwrap_or(ConfigDescuentos {
        descuento_max_vendedor_pct: 15.0,
        descuento_max_total_pct: 10.0,
        precio_minimo_global_margen: 5.0,
    })
}

// ─── Configuración del negocio (para tickets) ───────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ConfigNegocio {
    pub nombre: String,
    pub direccion: String,
    pub telefono: String,
    pub rfc: String,
    pub mensaje_pie: String,
}

#[tauri::command]
pub fn obtener_config_negocio(state: State<'_, AppState>) -> ConfigNegocio {
    let db = state.db.lock().unwrap();
    db.query_row(
        "SELECT nombre, direccion, telefono, rfc, mensaje_pie FROM config_negocio WHERE id = 1",
        [],
        |row| Ok(ConfigNegocio {
            nombre: row.get(0)?,
            direccion: row.get(1)?,
            telefono: row.get(2)?,
            rfc: row.get(3)?,
            mensaje_pie: row.get(4)?,
        }),
    ).unwrap_or(ConfigNegocio {
        nombre: "Moto Refaccionaria".into(),
        direccion: String::new(),
        telefono: String::new(),
        rfc: String::new(),
        mensaje_pie: "¡Gracias por su compra!".into(),
    })
}

#[tauri::command]
pub fn actualizar_config_negocio(
    datos: ConfigNegocio,
    state: State<'_, AppState>,
) -> Result<ConfigNegocio, String> {
    let db = state.db.lock().unwrap();
    db.execute(
        "UPDATE config_negocio SET nombre = ?, direccion = ?, telefono = ?, rfc = ?, mensaje_pie = ?, updated_at = datetime('now') WHERE id = 1",
        rusqlite::params![datos.nombre, datos.direccion, datos.telefono, datos.rfc, datos.mensaje_pie],
    ).map_err(|e| e.to_string())?;
    Ok(datos)
}
