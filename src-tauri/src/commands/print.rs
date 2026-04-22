// commands/print.rs — Impresión de HTML vía navegador del sistema
// Tauri 2 WebView no soporta window.print(), así que guardamos
// el HTML en un archivo temporal y lo abrimos en el navegador.

use std::fs;
use tauri::AppHandle;
use tauri::Manager;

#[tauri::command]
pub fn imprimir_html(html: String, app: AppHandle) -> Result<(), String> {
    let app_dir = app.path().app_data_dir()
        .map_err(|e| format!("No se pudo obtener directorio: {e}"))?;
    let print_dir = app_dir.join("print_temp");
    fs::create_dir_all(&print_dir)
        .map_err(|e| format!("No se pudo crear directorio temporal: {e}"))?;

    // Limpiar archivos viejos de impresión (más de 1 hora)
    if let Ok(entries) = fs::read_dir(&print_dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if modified.elapsed().unwrap_or_default().as_secs() > 3600 {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
        }
    }

    // Agregar auto-print al HTML para que imprima al abrir en el navegador
    let html_con_autoprint = if html.contains("</body>") {
        html.replace("</body>", r#"<script>window.onload=function(){window.print();}</script></body>"#)
    } else {
        format!("{html}<script>window.onload=function(){{window.print();}}</script>")
    };

    let filename = format!("print_{}.html", chrono::Local::now().format("%H%M%S"));
    let filepath = print_dir.join(&filename);
    fs::write(&filepath, &html_con_autoprint)
        .map_err(|e| format!("No se pudo escribir archivo: {e}"))?;

    // Abrir en el navegador del sistema
    let url = format!("file://{}", filepath.to_string_lossy());
    open::that(&url).map_err(|e| format!("No se pudo abrir el navegador: {e}"))?;

    Ok(())
}
