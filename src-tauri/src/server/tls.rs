// server/tls.rs — Genera/carga certificado HTTPS auto-firmado.
// Uso: HTTPS es necesario para que el navegador móvil permita acceso a la cámara
// en redes LAN (fuera de localhost). El cert es auto-firmado y los dispositivos
// lo confían una vez.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType, Ia5String};
use axum_server::tls_rustls::RustlsConfig;

pub struct TlsFiles {
    pub cert_pem: PathBuf,
    pub key_pem: PathBuf,
}

pub fn ensure_cert(cert_dir: &Path, ips: &[IpAddr]) -> Result<TlsFiles, String> {
    std::fs::create_dir_all(cert_dir).map_err(|e| e.to_string())?;
    let cert_path = cert_dir.join("pos-lan.crt.pem");
    let key_path = cert_dir.join("pos-lan.key.pem");

    if cert_path.exists() && key_path.exists() {
        return Ok(TlsFiles { cert_pem: cert_path, key_pem: key_path });
    }

    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "POS Moto Refaccionaria");
    dn.push(DnType::OrganizationName, "Moto Refaccionaria");
    params.distinguished_name = dn;

    let mut sans: Vec<SanType> = Vec::new();
    sans.push(SanType::DnsName(Ia5String::try_from("localhost").map_err(|e| e.to_string())?));
    for ip in ips {
        sans.push(SanType::IpAddress(*ip));
    }
    params.subject_alt_names = sans;

    let key_pair = KeyPair::generate().map_err(|e| e.to_string())?;
    let cert = params.self_signed(&key_pair).map_err(|e| e.to_string())?;

    std::fs::write(&cert_path, cert.pem()).map_err(|e| e.to_string())?;
    std::fs::write(&key_path, key_pair.serialize_pem()).map_err(|e| e.to_string())?;

    Ok(TlsFiles { cert_pem: cert_path, key_pem: key_path })
}

pub async fn load_rustls_config(tls: &TlsFiles) -> Result<RustlsConfig, String> {
    RustlsConfig::from_pem_file(&tls.cert_pem, &tls.key_pem)
        .await
        .map_err(|e| e.to_string())
}
