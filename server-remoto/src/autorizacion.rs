// autorizacion.rs — Autorizaciones del dueño firmadas por el servidor (POS web).
//
// Antes, el navegador pedía `verificar_pin_dueno` (→ bool) y después mandaba
// lo que quisiera en `autorizado_por`. El servidor nunca podía saber si de
// verdad hubo PIN. Ahora `autorizar_descuento` verifica el PIN y devuelve un
// token corto firmado con JWT_SECRET que dice QUIÉN autorizó, A QUIÉN, QUÉ y
// HASTA CUÁNTO. Las RPC que mueven dinero aceptan la excepción solo con ese
// token.
//
// Sin tabla nueva (stateless). El token no sirve como sesión: le faltan los
// campos obligatorios de `auth::Claims` (sub/email/role/sucursal) y lleva
// `typ = "autorizacion"`; a la inversa, un token de sesión no tiene `typ`.

use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

pub const TYP: &str = "autorizacion";
/// Vida del token: un carrito puede quedar abierto un rato mientras el
/// cliente decide. Reutilizable dentro de este lapso SOLO por el mismo
/// cajero, misma acción y mismo producto/venta.
pub const TTL_SEGUNDOS: i64 = 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutorizacionClaims {
    pub typ: String,
    /// Hoy solo "descuento". (Devoluciones y retiros mandan el PIN en la
    /// misma llamada y el servidor lo verifica ahí mismo.)
    pub accion: String,
    /// usuarios.id del dueño que tecleó su PIN.
    pub dueno: i64,
    /// usuarios.id del cajero de la sesión que lo pidió.
    pub cajero: i64,
    /// Descuento: producto al que aplica.
    #[serde(default)]
    pub objeto_id: Option<i64>,
    /// Descuento: porcentaje máximo autorizado.
    #[serde(default)]
    pub max_pct: Option<f64>,
    pub exp: i64,
}

pub fn emitir(
    secret: &[u8],
    accion: &str,
    dueno: i64,
    cajero: i64,
    objeto_id: Option<i64>,
    max_pct: Option<f64>,
    ahora_unix: i64,
) -> Result<String, jsonwebtoken::errors::Error> {
    let c = AutorizacionClaims {
        typ: TYP.into(),
        accion: accion.into(),
        dueno,
        cajero,
        objeto_id,
        max_pct,
        exp: ahora_unix + TTL_SEGUNDOS,
    };
    encode(&Header::default(), &c, &EncodingKey::from_secret(secret))
}

/// Verifica firma, expiración, tipo, acción, cajero y objeto. Devuelve los
/// claims si todo coincide; `None` si no (el caller decide el mensaje).
pub fn verificar(
    secret: &[u8],
    token: &str,
    accion: &str,
    cajero: i64,
    objeto_id: Option<i64>,
) -> Option<AutorizacionClaims> {
    let data = decode::<AutorizacionClaims>(
        token,
        &DecodingKey::from_secret(secret),
        &Validation::default(), // HS256 + exp obligatorio
    )
    .ok()?;
    let c = data.claims;
    if c.typ != TYP || c.accion != accion || c.cajero != cajero {
        return None;
    }
    if objeto_id.is_some() && c.objeto_id != objeto_id {
        return None;
    }
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    const S: &[u8] = b"secreto-de-prueba";

    fn ahora() -> i64 { chrono::Utc::now().timestamp() }

    #[test]
    fn token_valido_para_su_cajero_y_producto() {
        let t = emitir(S, "descuento", 2, 66, Some(4699), Some(30.0), ahora()).unwrap();
        let c = verificar(S, &t, "descuento", 66, Some(4699)).unwrap();
        assert_eq!((c.dueno, c.max_pct), (2, Some(30.0)));
    }

    #[test]
    fn otro_cajero_otro_producto_otra_accion_o_firma_falla() {
        let t = emitir(S, "descuento", 2, 66, Some(4699), Some(30.0), ahora()).unwrap();
        assert!(verificar(S, &t, "descuento", 4, Some(4699)).is_none());
        assert!(verificar(S, &t, "descuento", 66, Some(1)).is_none());
        assert!(verificar(S, &t, "devolucion", 66, Some(4699)).is_none());
        assert!(verificar(b"otro", &t, "descuento", 66, Some(4699)).is_none());
    }

    #[test]
    fn token_expirado_falla() {
        let t = emitir(S, "descuento", 2, 66, Some(1), Some(30.0), ahora() - 2 * TTL_SEGUNDOS).unwrap();
        assert!(verificar(S, &t, "descuento", 66, Some(1)).is_none());
    }

    #[test]
    fn token_de_sesion_no_es_autorizacion_y_viceversa() {
        #[derive(Serialize, Deserialize)]
        struct Claims { sub: i64, email: String, role: String, sucursal: i64, exp: i64 }
        let ses = encode(&Header::default(),
            &Claims { sub: 2, email: "dueno".into(), role: "admin".into(), sucursal: 1, exp: ahora() + 999 },
            &EncodingKey::from_secret(S)).unwrap();
        assert!(verificar(S, &ses, "descuento", 2, None).is_none());

        let aut = emitir(S, "descuento", 2, 66, Some(1), Some(30.0), ahora()).unwrap();
        assert!(decode::<Claims>(&aut, &DecodingKey::from_secret(S), &Validation::default()).is_err());
    }
}
