// sync/client.rs — Cliente HTTP del servidor remoto.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Serialize)]
pub struct PushBody {
    pub device_uuid: String,
    pub sucursal_id: i64,
    pub cambios: Vec<CambioOut>,
}

#[derive(Debug, Serialize)]
pub struct CambioOut {
    pub tabla: String,
    pub operacion: String,
    pub data: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct PushResponse {
    pub aceptados: Vec<String>,   // uuids
    pub rechazados: Vec<Rechazado>,
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
    pub hay_mas: bool,
}

pub struct RemoteClient {
    base: String,
    token: String,
    http: reqwest::Client,
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
            return Err(format!("push {}: {}", status, body));
        }
        r.json::<PushResponse>().await.map_err(|e| format!("push json: {}", e))
    }

    pub async fn pull(&self, cursor: &str, sucursal_id: i64) -> Result<PullResponse, String> {
        let r = self.http.get(format!("{}/sync/pull", self.base))
            .bearer_auth(&self.token)
            .query(&[("cursor", cursor), ("sucursal_id", &sucursal_id.to_string())])
            .send()
            .await
            .map_err(|e| format!("pull red: {}", e))?;
        if !r.status().is_success() {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            return Err(format!("pull {}: {}", status, body));
        }
        r.json::<PullResponse>().await.map_err(|e| format!("pull json: {}", e))
    }

    pub async fn health(&self) -> bool {
        self.http.get(format!("{}/health", self.base))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}
