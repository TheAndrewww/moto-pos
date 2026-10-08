// sync/client.rs — Cliente HTTP del servidor remoto.
//
// Protocolo v2: el push manda `proto: 2`, `refs` (FK como uuids) y, en
// productos, `base`, y recibe `marcas` (la marca guardada en el servidor de
// cada fila aceptada); el pull manda `device_uuid` y `proto=2` y recibe
// `caja_v2_desde`. Los endpoints nuevos (/sync/id-map,
// /sync/reparar, /sync/refs, /sync/fila) pueden no existir en un servidor
// viejo: 404/405 (o la página de la SPA en vez de JSON) = "el servidor aún
// no se actualiza" → `Respuesta::NoSoportado`, se reintenta más tarde sin
// llenar el log de errores.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);
/// La reparación del servidor recorre tablas grandes en una transacción.
const TIMEOUT_REPARAR: Duration = Duration::from_secs(180);

/// Versión del protocolo de sync que habla este POS.
pub const PROTO: u32 = 2;

#[derive(Debug, Serialize)]
pub struct PushBody {
    pub device_uuid: String,
    pub sucursal_id: i64,
    pub proto: u32,
    pub cambios: Vec<CambioOut>,
}

#[derive(Debug, Serialize)]
pub struct CambioOut {
    pub tabla: String,
    pub operacion: String,
    pub data: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refs: Option<Value>,
    /// Solo productos: la fila como estaba la última vez que este equipo y el
    /// servidor coincidieron (sync_base). Con ella el servidor fusiona campo
    /// por campo y aplica la existencia como diferencia.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct PushResponse {
    #[serde(default)]
    pub aceptados: Vec<String>,   // uuids
    #[serde(default)]
    pub rechazados: Vec<Rechazado>,
    /// Informativo: uuids referenciados que el servidor no conoce.
    #[serde(default)]
    pub faltantes: Vec<String>,
    /// `{uuid: updated_at}`: la marca con la que el servidor GUARDÓ cada fila
    /// aceptada (con su reloj; recorta las marcas del futuro). Los servidores
    /// viejos no la mandan. Se lee como JSON libre para que un valor raro
    /// nunca tumbe la respuesta completa del push (ver `marcas()`).
    #[serde(default)]
    pub marcas: Value,
}

impl PushResponse {
    /// Marcas del servidor por uuid, normalizadas 'YYYY-MM-DD HH:MM:SS'.
    /// Se ignora lo que no sea una fecha válida.
    pub fn marcas(&self) -> HashMap<String, String> {
        let mut out = HashMap::new();
        let Some(m) = self.marcas.as_object() else { return out };
        for (uuid, v) in m {
            let Some(s) = v.as_str() else { continue };
            let marca = super::fk::normalizar_marca(s.trim());
            if chrono::NaiveDateTime::parse_from_str(&marca, "%Y-%m-%d %H:%M:%S").is_ok() {
                out.insert(uuid.clone(), marca);
            }
        }
        out
    }
}

#[derive(Debug, Deserialize)]
pub struct Rechazado {
    pub uuid: String,
    pub motivo: String,
}

#[derive(Debug, Deserialize)]
pub struct PullResponse {
    pub cambios: Vec<Value>,
    pub next_cursor: String,
    #[serde(default)]
    pub hay_mas: bool,
    /// Hora local en que se actualizó el servidor (sync_meta); los
    /// servidores viejos no lo mandan.
    #[serde(default)]
    pub caja_v2_desde: Option<String>,
}

/// Respuesta de un endpoint que un servidor viejo puede no tener.
#[derive(Debug)]
pub enum Respuesta<T> {
    Ok(T),
    /// El servidor no tiene el endpoint (aún no se actualiza).
    NoSoportado,
    /// El servidor se negó a confirmar (p. ej. la verificación de la
    /// reparación falló); trae su reporte.
    Rechazado(Value),
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct IdMapResp {
    #[serde(default)]
    pub recibidos: i64,
    /// uuids que el servidor no tiene (puede venir como lista o como número).
    #[serde(default)]
    pub desconocidos: Value,
}

pub struct RemoteClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

/// ¿El status indica que el endpoint no existe en este servidor?
fn es_no_soportado(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::METHOD_NOT_ALLOWED
}

impl RemoteClient {
    pub fn new(base_url: &str, token: &str) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            base: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http,
        })
    }

    pub async fn push(&self, body: &PushBody) -> Result<PushResponse, String> {
        let r = self.http.post(format!("{}/sync/push", self.base))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|e| format!("push red: {}", e))?;
        if !r.status().is_success() {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            return Err(format!("push {}: {}", status, recortar(&body)));
        }
        r.json::<PushResponse>().await.map_err(|e| format!("push json: {}", e))
    }

    pub async fn pull(&self, cursor: &str, sucursal_id: i64, device_uuid: &str) -> Result<PullResponse, String> {
        let r = self.http.get(format!("{}/sync/pull", self.base))
            .bearer_auth(&self.token)
            .query(&[
                ("cursor", cursor.to_string()),
                ("sucursal_id", sucursal_id.to_string()),
                ("device_uuid", device_uuid.to_string()),
                ("proto", PROTO.to_string()),
            ])
            .send()
            .await
            .map_err(|e| format!("pull red: {}", e))?;
        if !r.status().is_success() {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            return Err(format!("pull {}: {}", status, recortar(&body)));
        }
        r.json::<PullResponse>().await.map_err(|e| format!("pull json: {}", e))
    }

    /// POST /sync/id-map — sube pares (id local, uuid) de una tabla.
    pub async fn subir_mapa(
        &self,
        device_uuid: &str,
        tabla: &str,
        pares: &[(i64, String)],
    ) -> Result<Respuesta<IdMapResp>, String> {
        let body = serde_json::json!({
            "device_uuid": device_uuid,
            "tabla": tabla,
            "pares": pares.iter().map(|(id, u)| serde_json::json!([id, u])).collect::<Vec<_>>(),
        });
        let r = self.http.post(format!("{}/sync/id-map", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("id-map red: {}", e))?;
        self.leer_json("id-map", r).await
    }

    /// POST /sync/reparar — el servidor repara las FK de las filas de este
    /// equipo con el mapa de ids subido. Devuelve su reporte. 409 = la
    /// verificación del servidor falló y no se confirmó nada
    /// (`Respuesta::Rechazado` con el reporte).
    pub async fn reparar(&self, device_uuid: &str) -> Result<Respuesta<Value>, String> {
        let r = self.http.post(format!("{}/sync/reparar", self.base))
            .bearer_auth(&self.token)
            .timeout(TIMEOUT_REPARAR)
            .json(&serde_json::json!({ "device_uuid": device_uuid }))
            .send()
            .await
            .map_err(|e| format!("reparar red: {}", e))?;
        if r.status() == reqwest::StatusCode::CONFLICT {
            let texto = r.text().await.unwrap_or_default();
            let v = serde_json::from_str::<Value>(&texto).unwrap_or(Value::String(recortar(&texto)));
            return Ok(Respuesta::Rechazado(v));
        }
        self.leer_json("reparar", r).await
    }

    /// POST /sync/refs — referencias (en significado del servidor) de filas
    /// por uuid: `{uuid: {columna: uuid | null}}` (padres e hijos).
    pub async fn refs(&self, tabla: &str, uuids: &[String]) -> Result<Respuesta<Value>, String> {
        let r = self.http.post(format!("{}/sync/refs", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "tabla": tabla, "uuids": uuids }))
            .send()
            .await
            .map_err(|e| format!("refs red: {}", e))?;
        self.leer_json("refs", r).await
    }

    /// GET /sync/fila — un cambio en formato de pull v2 (para reintentos de
    /// la bandeja de entrada). `Ok(None)` = el servidor no tiene la fila.
    pub async fn fila(&self, tabla: &str, uuid: &str) -> Result<Respuesta<Option<Value>>, String> {
        let r = self.http.get(format!("{}/sync/fila", self.base))
            .bearer_auth(&self.token)
            .query(&[("tabla", tabla), ("uuid", uuid)])
            .send()
            .await
            .map_err(|e| format!("fila red: {}", e))?;
        match self.leer_json::<Value>("fila", r).await? {
            Respuesta::NoSoportado | Respuesta::Rechazado(_) => Ok(Respuesta::NoSoportado),
            Respuesta::Ok(v) => {
                // Tolerar `{cambio: {...}}` o el cambio directo.
                let cambio = match v.get("cambio") {
                    Some(c) => c.clone(),
                    None => v,
                };
                if cambio.is_null() {
                    Ok(Respuesta::Ok(None))
                } else if cambio.get("__tabla").is_some() {
                    Ok(Respuesta::Ok(Some(cambio)))
                } else {
                    Ok(Respuesta::NoSoportado)
                }
            }
        }
    }

    /// Lee una respuesta JSON de un endpoint nuevo. 404/405 o un cuerpo que
    /// no es JSON (la SPA responde index.html a rutas desconocidas) =
    /// servidor sin actualizar.
    async fn leer_json<T: serde::de::DeserializeOwned>(
        &self,
        nombre: &str,
        r: reqwest::Response,
    ) -> Result<Respuesta<T>, String> {
        let status = r.status();
        if es_no_soportado(status) {
            return Ok(Respuesta::NoSoportado);
        }
        let texto = r.text().await.map_err(|e| format!("{nombre} lectura: {e}"))?;
        if !status.is_success() {
            return Err(format!("{nombre} {status}: {}", recortar(&texto)));
        }
        match serde_json::from_str::<T>(&texto) {
            Ok(v) => Ok(Respuesta::Ok(v)),
            Err(_) if texto.trim_start().starts_with('<') => Ok(Respuesta::NoSoportado),
            Err(e) => Err(format!("{nombre} json: {e}")),
        }
    }

    pub async fn health(&self) -> bool {
        self.http.get(format!("{}/health", self.base))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

/// Recorta cuerpos de error largos para que no inunden el log / la UI.
fn recortar(s: &str) -> String {
    if s.chars().count() > 300 {
        format!("{}…", s.chars().take(300).collect::<String>())
    } else {
        s.to_string()
    }
}
