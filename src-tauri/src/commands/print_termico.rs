// commands/print_termico.rs — Impresión térmica ESC/POS real
//
// Estrategia: genera bytes ESC/POS crudos y los envía al spooler del SO.
// - macOS/Linux: `lp -d <printer> -o raw <file>`
// - Windows: `copy /B <file> \\localhost\<printer>` (o PowerShell Out-Printer)
//
// No depende de crates externos — ESC/POS es un protocolo muy estable.

use serde::{Deserialize, Serialize};
use tauri::State;
use super::auth::AppState;
use std::io::Write;
use std::process::Command;

// ─── Constantes ESC/POS ─────────────────────────────────────
const ESC: u8 = 0x1B;
const GS: u8 = 0x1D;

// ─── Structs ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DatosTicketTermico {
    pub negocio_nombre: String,
    pub negocio_direccion: String,
    pub negocio_telefono: String,
    pub negocio_rfc: String,
    pub mensaje_pie: String,
    pub folio: String,
    pub fecha: String,
    pub usuario: String,
    pub cliente: Option<String>,
    pub items: Vec<ItemTicket>,
    pub subtotal: f64,
    pub descuento: f64,
    /// Monto agregado por redondeo al peso siguiente (>= 0). Opcional para
    /// retrocompatibilidad con tickets viejos que no lo enviaban.
    #[serde(default)]
    pub redondeo: f64,
    pub total: f64,
    pub metodo_pago: String,
}

#[derive(Deserialize)]
pub struct ItemTicket {
    pub cantidad: f64,
    pub nombre: String,
    pub precio_unitario: f64,
    pub subtotal: f64,
}

#[derive(Serialize)]
pub struct ImpresoraInfo {
    pub nombre: String,
    pub default: bool,
}

// ─── Helpers ─────────────────────────────────────────────────

fn centrado(b: &mut Vec<u8>) {
    b.extend_from_slice(&[ESC, b'a', 1]);
}
fn izquierda(b: &mut Vec<u8>) {
    b.extend_from_slice(&[ESC, b'a', 0]);
}
fn negrita_on(b: &mut Vec<u8>) {
    b.extend_from_slice(&[ESC, b'E', 1]);
}
fn negrita_off(b: &mut Vec<u8>) {
    b.extend_from_slice(&[ESC, b'E', 0]);
}
fn doble_tamano(b: &mut Vec<u8>) {
    b.extend_from_slice(&[GS, b'!', 0x11]); // doble ancho + alto
}
fn tamano_normal(b: &mut Vec<u8>) {
    b.extend_from_slice(&[GS, b'!', 0x00]);
}
fn cortar_papel(b: &mut Vec<u8>) {
    b.extend_from_slice(&[GS, b'V', 0x42, 0]); // corte parcial
}
fn init(b: &mut Vec<u8>) {
    b.extend_from_slice(&[ESC, b'@']); // reset
}

fn linea_divisoria(b: &mut Vec<u8>, ancho: usize) {
    let linea = "-".repeat(ancho);
    let _ = writeln!(b, "{}", linea);
}

fn linea_kv(b: &mut Vec<u8>, etiqueta: &str, valor: &str, ancho: usize) {
    let espacio = ancho.saturating_sub(etiqueta.len() + valor.len());
    let _ = writeln!(b, "{}{}{}", etiqueta, " ".repeat(espacio), valor);
}

/// Trunca un nombre de producto si excede ancho (con ellipsis).
fn truncar(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() }
    else {
        let recortado: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", recortado)
    }
}

// ─── Generación del ticket ───────────────────────────────────

pub fn generar_ticket_venta(datos: &DatosTicketTermico) -> Vec<u8> {
    let ancho: usize = 32; // típico 58mm; 48 para 80mm
    let mut b: Vec<u8> = Vec::with_capacity(2048);

    init(&mut b);

    // ─── Encabezado ────────────────────────────
    centrado(&mut b);
    negrita_on(&mut b);
    doble_tamano(&mut b);
    let _ = writeln!(b, "{}", datos.negocio_nombre);
    tamano_normal(&mut b);
    negrita_off(&mut b);

    if !datos.negocio_direccion.is_empty() {
        let _ = writeln!(b, "{}", datos.negocio_direccion);
    }
    if !datos.negocio_telefono.is_empty() {
        let _ = writeln!(b, "Tel: {}", datos.negocio_telefono);
    }
    if !datos.negocio_rfc.is_empty() {
        let _ = writeln!(b, "RFC: {}", datos.negocio_rfc);
    }

    izquierda(&mut b);
    linea_divisoria(&mut b, ancho);

    // ─── Info de venta ────────────────────────
    let _ = writeln!(b, "Folio: {}", datos.folio);
    let _ = writeln!(b, "Fecha: {}", datos.fecha);
    let _ = writeln!(b, "Cajero: {}", datos.usuario);
    if let Some(cli) = &datos.cliente {
        let _ = writeln!(b, "Cliente: {}", cli);
    }
    linea_divisoria(&mut b, ancho);

    // ─── Items ─────────────────────────────────
    // Formato: "NN x Nombre     $$.$$"
    for item in &datos.items {
        let prefijo = format!("{} x ", item.cantidad);
        let precio_str = format!("${:.2}", item.subtotal);
        let max_nombre = ancho.saturating_sub(prefijo.len() + precio_str.len() + 1);
        let nombre = truncar(&item.nombre, max_nombre);
        let padding = ancho.saturating_sub(prefijo.len() + nombre.chars().count() + precio_str.len());
        let _ = writeln!(b, "{}{}{}{}", prefijo, nombre, " ".repeat(padding), precio_str);
        if item.cantidad > 1.0 {
            let unit = format!("  @ ${:.2} c/u", item.precio_unitario);
            let _ = writeln!(b, "{}", unit);
        }
    }
    linea_divisoria(&mut b, ancho);

    // ─── Totales ──────────────────────────────
    linea_kv(&mut b, "Subtotal:", &format!("${:.2}", datos.subtotal), ancho);
    if datos.descuento > 0.0 {
        linea_kv(&mut b, "Descuento:", &format!("-${:.2}", datos.descuento), ancho);
    }
    if datos.redondeo > 0.0 {
        linea_kv(&mut b, "Redondeo:", &format!("+${:.2}", datos.redondeo), ancho);
    }
    negrita_on(&mut b);
    linea_kv(&mut b, "TOTAL:", &format!("${:.2}", datos.total), ancho);
    negrita_off(&mut b);
    linea_kv(&mut b, "Pago:", &datos.metodo_pago, ancho);

    linea_divisoria(&mut b, ancho);

    // ─── Pie ──────────────────────────────────
    centrado(&mut b);
    if !datos.mensaje_pie.is_empty() {
        let _ = writeln!(b, "{}", datos.mensaje_pie);
    }
    let _ = writeln!(b, "");
    let _ = writeln!(b, "");
    let _ = writeln!(b, "");

    cortar_papel(&mut b);
    b
}

// ─── Envío al spooler ────────────────────────────────────────

#[cfg(target_os = "windows")]
fn enviar_bytes_a_impresora(bytes: &[u8], impresora: &str) -> Result<(), String> {
    use std::io::Write as _;
    // Escribir a temp file y copiar en binario a la cola de la impresora
    let mut tmp = std::env::temp_dir();
    tmp.push(format!("pos_ticket_{}.prn", chrono::Local::now().timestamp_nanos_opt().unwrap_or(0)));
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
    }
    let target = format!(r"\\localhost\{}", impresora);
    let status = Command::new("cmd")
        .args(["/C", "copy", "/B", tmp.to_string_lossy().as_ref(), &target])
        .status()
        .map_err(|e| format!("No se pudo ejecutar copy: {}", e))?;
    let _ = std::fs::remove_file(&tmp);
    if !status.success() {
        return Err(format!("Falló la impresión en '{}' (code {:?})", impresora, status.code()));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn enviar_bytes_a_impresora(bytes: &[u8], impresora: &str) -> Result<(), String> {
    // macOS / Linux: usar `lp -d <impresora> -o raw`
    let mut tmp = std::env::temp_dir();
    tmp.push(format!("pos_ticket_{}.prn", chrono::Local::now().timestamp_nanos_opt().unwrap_or(0)));
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
    }
    let status = Command::new("lp")
        .args(["-d", impresora, "-o", "raw", tmp.to_string_lossy().as_ref()])
        .status()
        .map_err(|e| format!("No se pudo ejecutar lp: {}", e))?;
    let _ = std::fs::remove_file(&tmp);
    if !status.success() {
        return Err(format!("Falló la impresión en '{}' (code {:?})", impresora, status.code()));
    }
    Ok(())
}

// ─── Comandos Tauri ──────────────────────────────────────────

#[tauri::command]
pub fn imprimir_ticket_termico(
    datos: DatosTicketTermico,
    impresora: String,
    _state: State<'_, AppState>,
) -> Result<(), String> {
    if impresora.trim().is_empty() {
        return Err("Debes especificar una impresora".into());
    }
    let bytes = generar_ticket_venta(&datos);
    enviar_bytes_a_impresora(&bytes, impresora.trim())?;
    log::info!("Ticket térmico impreso en '{}' ({} bytes)", impresora, bytes.len());
    Ok(())
}

/// Lista impresoras instaladas en el sistema.
#[tauri::command]
pub fn listar_impresoras() -> Result<Vec<ImpresoraInfo>, String> {
    #[cfg(target_os = "windows")]
    {
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command",
                "Get-Printer | Select-Object -ExpandProperty Name"])
            .output()
            .map_err(|e| format!("Get-Printer falló: {}", e))?;
        let texto = String::from_utf8_lossy(&out.stdout);
        let default_out = Command::new("powershell")
            .args(["-NoProfile", "-Command",
                "(Get-CimInstance -Class Win32_Printer | Where-Object Default).Name"])
            .output()
            .ok();
        let default_name = default_out
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let impresoras = texto.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .map(|nombre| {
                let default = nombre == default_name;
                ImpresoraInfo { nombre, default }
            })
            .collect();
        return Ok(impresoras);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let out = Command::new("lpstat")
            .args(["-p"])
            .output()
            .map_err(|e| format!("lpstat falló: {} — ¿está CUPS instalado?", e))?;
        let texto = String::from_utf8_lossy(&out.stdout);
        let default_out = Command::new("lpstat").args(["-d"]).output().ok();
        let default_name = default_out
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.split_whitespace().last().map(|x| x.to_string()))
            .unwrap_or_default();
        let impresoras = texto.lines()
            .filter_map(|l| {
                // formato: "printer NOMBRE is idle. ..."
                let mut parts = l.split_whitespace();
                if parts.next()? == "printer" {
                    parts.next().map(|n| n.to_string())
                } else { None }
            })
            .map(|nombre| {
                let default = nombre == default_name;
                ImpresoraInfo { nombre, default }
            })
            .collect();
        Ok(impresoras)
    }
}
