// commands/exportar.rs — Escribir contenido de texto a un archivo (para exportación CSV)

use std::fs;

#[tauri::command]
pub fn escribir_archivo(ruta: String, contenido: String) -> Result<(), String> {
    fs::write(&ruta, contenido.as_bytes())
        .map_err(|e| format!("Error al escribir archivo: {}", e))
}
