// lib.rs — Entry point de Tauri para POS Moto Refaccionaria

mod db;
mod commands;

use commands::auth::{AppState, login_pin, login_password, logout, verificar_pin_dueno, resolver_dueno_por_pin, crear_usuario_inicial};
use commands::productos::{
    listar_productos, obtener_producto_por_codigo, generar_codigo_interno,
    crear_producto, actualizar_producto, listar_productos_stock_bajo,
    listar_categorias, listar_proveedores,
    listar_clientes, crear_cliente, actualizar_cliente, toggle_cliente_activo,
    obtener_config_descuentos,
    obtener_config_negocio, actualizar_config_negocio,
};
use commands::ventas::{
    crear_venta, listar_ventas_dia, obtener_estadisticas_dia, anular_venta,
    buscar_ventas, obtener_detalle_venta,
};
use commands::devoluciones::{
    crear_devolucion, listar_devoluciones, obtener_detalle_devolucion,
};
use commands::usuarios::{
    listar_usuarios, listar_roles, crear_usuario, actualizar_usuario, toggle_usuario_activo,
};
use commands::bitacora::listar_bitacora;
use commands::presupuestos::{
    crear_presupuesto, listar_presupuestos,
    obtener_detalle_presupuesto, cambiar_estado_presupuesto,
};
use commands::recepcion::{
    crear_recepcion, listar_recepciones, obtener_detalle_recepcion,
};
use commands::pedidos::{
    crear_orden_pedido, listar_ordenes_pedido,
    obtener_detalle_orden, cambiar_estado_orden,
};
use commands::cortes::{
    crear_movimiento_caja, listar_movimientos_sin_corte,
    calcular_datos_corte, crear_corte,
    listar_cortes, obtener_detalle_corte,
    verificar_corte_dia_pendiente,
    crear_apertura_caja, obtener_apertura_hoy, obtener_fondo_sugerido,
};
use commands::respaldos::{
    crear_respaldo, listar_respaldos, restaurar_respaldo,
    respaldo_auto_si_necesario, obtener_info_bd,
};
use commands::print::imprimir_html;
use commands::importar::importar_catalogo_csv;
use db::connection::init_database;
use std::sync::Mutex;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Obtener el directorio de datos de la app (multiplataforma)
            let app_data_dir = app.path().app_data_dir()
                .expect("No se pudo obtener el directorio de datos de la app");

            // Crear el directorio si no existe
            std::fs::create_dir_all(&app_data_dir)
                .expect("No se pudo crear el directorio de datos");

            let db_path = app_data_dir.join("pos_database.db");
            log::info!("Inicializando BD en: {:?}", db_path);

            // Inicializar la base de datos
            let conn = init_database(&db_path)
                .expect("Error al inicializar la base de datos");

            // Compartir el estado con todos los comandos
            app.manage(AppState {
                db: Mutex::new(conn),
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Auth
            login_pin,
            login_password,
            logout,
            verificar_pin_dueno,
            resolver_dueno_por_pin,
            crear_usuario_inicial,
            // Productos
            listar_productos,
            obtener_producto_por_codigo,
            generar_codigo_interno,
            crear_producto,
            actualizar_producto,
            listar_productos_stock_bajo,
            listar_categorias,
            listar_proveedores,
            // Clientes
            listar_clientes,
            crear_cliente,
            actualizar_cliente,
            toggle_cliente_activo,
            // Config
            obtener_config_descuentos,
            obtener_config_negocio,
            actualizar_config_negocio,
            // Ventas
            crear_venta,
            listar_ventas_dia,
            obtener_estadisticas_dia,
            anular_venta,
            buscar_ventas,
            obtener_detalle_venta,
            // Devoluciones
            crear_devolucion,
            listar_devoluciones,
            obtener_detalle_devolucion,
            // Usuarios
            listar_usuarios,
            listar_roles,
            crear_usuario,
            actualizar_usuario,
            toggle_usuario_activo,
            // Bitácora
            listar_bitacora,
            // Presupuestos
            crear_presupuesto,
            listar_presupuestos,
            obtener_detalle_presupuesto,
            cambiar_estado_presupuesto,
            // Recepción
            crear_recepcion,
            listar_recepciones,
            obtener_detalle_recepcion,
            // Pedidos
            crear_orden_pedido,
            listar_ordenes_pedido,
            obtener_detalle_orden,
            cambiar_estado_orden,
            // Cortes de caja
            crear_movimiento_caja,
            listar_movimientos_sin_corte,
            calcular_datos_corte,
            crear_corte,
            listar_cortes,
            obtener_detalle_corte,
            verificar_corte_dia_pendiente,
            // Apertura de caja
            crear_apertura_caja,
            obtener_apertura_hoy,
            obtener_fondo_sugerido,
            // Respaldos
            crear_respaldo,
            listar_respaldos,
            restaurar_respaldo,
            respaldo_auto_si_necesario,
            obtener_info_bd,
            // Impresión
            imprimir_html,
            // Importación
            importar_catalogo_csv,
        ])
        .run(tauri::generate_context!())
        .expect("Error al iniciar el POS");
}
