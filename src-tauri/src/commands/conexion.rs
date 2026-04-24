// commands/conexion.rs — Comandos Tauri para la página "Conexión móvil"
//
// Expone al frontend del POS:
//  - info del servidor (IPs, puerto, estado)
//  - generación de token+QR de emparejamiento
//  - listar/revocar dispositivos emparejados

use rand::Rng;
use serde::Serialize;
use tauri::State;

use crate::commands::auth::AppState;
use crate::server::state::PairingEntry;
use crate::ServerSlot;

#[derive(Serialize)]
pub struct ServerInfoApi {
    pub activo: bool,
    pub port: u16,
    pub ips: Vec<String>,
}

#[derive(Serialize)]
pub struct PairingQr {
    pub token: String,
    pub url: String,        // https://<ip>:<port>/pairing?token=...
    pub qr_svg: String,     // SVG inline
    pub expires_in: i64,    // segundos
}

#[derive(Serialize, Clone)]
pub struct DispositivoRow {
    pub id: i64,
    pub nombre: String,
    pub user_agent: Option<String>,
    pub usuario_id: i64,
    pub usuario_nombre: String,
    pub ultimo_ping: Option<String>,
    pub ip_ultima: Option<String>,
    pub created_at: String,
    pub revocado: bool,
}

#[tauri::command]
pub fn obtener_info_servidor(
    slot: State<'_, ServerSlot>,
) -> ServerInfoApi {
    let guard = slot.read().ok();
    match guard.as_deref().and_then(|o| o.as_ref()) {
        Some(s) => {
            let ips = match local_ip_address::list_afinet_netifas() {
                Ok(list) => list.into_iter()
                    .filter(|(_, ip)| ip.is_ipv4() && !ip.is_loopback())
                    .map(|(_, ip)| ip.to_string())
                    .collect(),
                Err(_) => vec![],
            };
            ServerInfoApi { activo: true, port: s.port, ips }
        }
        None => ServerInfoApi { activo: false, port: 0, ips: vec![] },
    }
}

#[tauri::command]
pub async fn generar_qr_emparejamiento(
    ip: String,
    slot: State<'_, ServerSlot>,
) -> Result<PairingQr, String> {
    // Clonamos lo que necesitamos dentro del lock para no mantenerlo mientras awaiteamos.
    let (port, tokens) = {
        let guard = slot.read().map_err(|_| "Slot envenenado")?;
        let s = guard.as_ref().ok_or("Servidor móvil aún no está activo")?;
        (s.port, s.pairing_tokens.clone())
    };

    // Generar token aleatorio (32 bytes → hex). Scoping del RNG para que no cruce await.
    let token: String = {
        let mut rng = rand::thread_rng();
        (0..32).map(|_| format!("{:02x}", rng.gen::<u8>())).collect()
    };

    let expires_in: i64 = 300; // 5 min
    let entry = PairingEntry {
        token: token.clone(),
        expires_at: chrono::Utc::now().timestamp() + expires_in,
    };

    {
        let mut map = tokens.write().await;
        map.insert(token.clone(), entry);
    }

    let url = format!("https://{}:{}/pairing?token={}", ip, port, token);

    // Generar QR como SVG
    let code = qrcode::QrCode::new(url.as_bytes()).map_err(|e| e.to_string())?;
    let qr_svg = code.render::<qrcode::render::svg::Color>()
        .min_dimensions(280, 280)
        .build();

    Ok(PairingQr { token, url, qr_svg, expires_in })
}

#[tauri::command]
pub fn listar_dispositivos(state: State<'_, AppState>) -> Vec<DispositivoRow> {
    let db = state.db.lock().unwrap();
    let mut stmt = match db.prepare(
        r#"SELECT d.id, d.nombre, d.user_agent, d.usuario_id, u.nombre_completo,
                  d.ultimo_ping, d.ip_ultima, d.created_at, d.revocado
           FROM dispositivos_conectados d
           LEFT JOIN usuarios u ON u.id = d.usuario_id
           ORDER BY d.revocado ASC, d.ultimo_ping DESC"#,
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map([], |row| {
        Ok(DispositivoRow {
            id: row.get(0)?,
            nombre: row.get(1)?,
            user_agent: row.get(2)?,
            usuario_id: row.get(3)?,
            usuario_nombre: row.get::<_, Option<String>>(4)?.unwrap_or_else(|| "—".into()),
            ultimo_ping: row.get(5)?,
            ip_ultima: row.get(6)?,
            created_at: row.get(7)?,
            revocado: row.get::<_, i64>(8)? != 0,
        })
    }).map(|iter| iter.filter_map(|r| r.ok()).collect()).unwrap_or_default()
}

#[tauri::command]
pub fn revocar_dispositivo(
    id: i64,
    usuario_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let db = state.db.lock().unwrap();
    db.execute(
        "UPDATE dispositivos_conectados SET revocado = 1 WHERE id = ?",
        rusqlite::params![id],
    ).map_err(|e| e.to_string())?;

    let _ = db.execute(
        r#"INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen)
           VALUES (?, 'DISPOSITIVO_REVOCADO', 'dispositivos_conectados', ?, ?, 'POS')"#,
        rusqlite::params![usuario_id, id, format!("Dispositivo #{} revocado", id)],
    );
    Ok(())
}
