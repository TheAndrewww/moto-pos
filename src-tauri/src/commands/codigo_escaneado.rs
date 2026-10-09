// commands/codigo_escaneado.rs — Resolver lo que llega del lector de códigos.
//
// Misma regla que src/lib/codigoEscaneado.ts (pantallas de Punto de Venta y
// Recepción); aquí la usan la búsqueda por código del escritorio y la app del
// celular (servidor LAN):
//
// 1. Guion leído como apóstrofo: con el lector en teclado inglés y Windows en
//    español, CDI-023 llega como CDI'023. Ningún código del inventario lleva
//    apóstrofo, así que se convierte de vuelta.
// 2. Códigos de proveedor con descripción (Alessia):
//      S017'KEY26 CDI'023 UNIDAD DE CDI 12 Vcc DM'125 17'19 ...
//    Se busca un código del inventario que aparezca como palabra completa
//    dentro de lo escaneado (solo códigos de 4+ caracteres con algún dígito:
//    hay productos cuyo código es una palabra común, como "cilindro").
//
// Orden: exacto → exacto tras normalizar → contenido. Si dentro hay más de un
// código posible no se adivina: se devuelven los candidatos.

use rusqlite::Connection;

const MIN_LARGO_CONTENIDO: usize = 4;

#[derive(Debug, PartialEq, Eq)]
pub enum Resolucion {
    Uno(i64),
    Varios(Vec<i64>),
    Ninguno,
}

/// Sin espacios en los extremos, apóstrofos/acentos sueltos → guion y
/// espacios repetidos reducidos a uno.
pub fn normalizar(raw: &str) -> String {
    let cambiado: String = raw
        .chars()
        .map(|c| match c {
            '\'' | '’' | '‘' | '´' | '`' => '-',
            c => c,
        })
        .collect();
    cambiado.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn es_separador(c: Option<char>) -> bool {
    match c {
        None => true,
        Some(c) => c.is_whitespace()
            || matches!(c, ',' | ';' | ':' | '|' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '*'),
    }
}

/// Posición (en bytes) donde `codigo` aparece en `texto` como palabra completa.
fn como_palabra(texto: &str, codigo: &str) -> Option<usize> {
    let mut desde = 0;
    while desde <= texto.len() {
        let i = desde + texto[desde..].find(codigo)?;
        let antes = texto[..i].chars().next_back();
        let despues = texto[i + codigo.len()..].chars().next();
        if es_separador(antes) && es_separador(despues) {
            return Some(i);
        }
        // Avanzar al siguiente carácter (respetando UTF-8).
        desde = i + texto[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    None
}

/// Algoritmo puro sobre (id, código) de los productos activos.
pub fn resolver_en(raw: &str, productos: &[(i64, String)]) -> Resolucion {
    let crudo = raw.trim();
    if crudo.is_empty() {
        return Resolucion::Ninguno;
    }
    // 1. Exacto.
    if let Some((id, _)) = productos.iter().find(|(_, c)| c == crudo) {
        return Resolucion::Uno(*id);
    }
    // 2. Exacto tras normalizar, sin distinguir mayúsculas.
    let texto = normalizar(raw).to_uppercase();
    let normalizados: Vec<i64> = productos
        .iter()
        .filter(|(_, c)| normalizar(c).to_uppercase() == texto)
        .map(|(id, _)| *id)
        .collect();
    match normalizados.len() {
        1 => return Resolucion::Uno(normalizados[0]),
        n if n > 1 => return Resolucion::Varios(normalizados),
        _ => {}
    }
    // 3. Código del inventario dentro de una cadena larga.
    if !texto.contains(' ') {
        return Resolucion::Ninguno;
    }
    let mut hallados: Vec<(i64, usize, String)> = Vec::new();
    for (id, c) in productos {
        let cod = normalizar(c).to_uppercase();
        if cod.chars().count() < MIN_LARGO_CONTENIDO || !cod.chars().any(|ch| ch.is_ascii_digit()) {
            continue;
        }
        if let Some(pos) = como_palabra(&texto, &cod) {
            hallados.push((*id, pos, cod));
        }
    }
    // Un código contenido en otro también hallado es parte del más largo.
    let mut utiles: Vec<&(i64, usize, String)> = hallados
        .iter()
        .filter(|h| !hallados.iter().any(|o| o.2.len() > h.2.len() && o.2.contains(h.2.as_str())))
        .collect();
    utiles.sort_by(|a, b| a.1.cmp(&b.1).then(b.2.len().cmp(&a.2.len())));
    match utiles.len() {
        0 => Resolucion::Ninguno,
        1 => Resolucion::Uno(utiles[0].0),
        _ => Resolucion::Varios(utiles.iter().map(|h| h.0).collect()),
    }
}

/// Resuelve contra los productos activos de la BD.
pub fn resolver(db: &Connection, raw: &str) -> Result<Resolucion, String> {
    let mut stmt = db
        .prepare("SELECT id, codigo FROM productos WHERE activo = 1 AND deleted_at IS NULL")
        .map_err(|e| e.to_string())?;
    let productos: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(resolver_en(raw, &productos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv() -> Vec<(i64, String)> {
        vec![
            (1, "CDI-023".into()),
            (2, "CDI-02".into()),
            (3, "cilindro".into()),
            (4, "12".into()),
            (5, "BAL-6002-2RS".into()),
            (6, "FT-150".into()),
            (7, "DM-125".into()),
        ]
    }

    const ALESSIA: &str = "S017'KEY26 CDI'023 UNIDAD DE CDI 12 Vcc DM'1250 17'19 - DM'150 SPORT 19'20";

    #[test]
    fn exacto_y_apostrofo() {
        assert_eq!(resolver_en("CDI-023", &inv()), Resolucion::Uno(1));
        assert_eq!(resolver_en("  CDI-023 ", &inv()), Resolucion::Uno(1));
        assert_eq!(resolver_en("CDI'023", &inv()), Resolucion::Uno(1));
        assert_eq!(resolver_en("cdi'023", &inv()), Resolucion::Uno(1));
        assert_eq!(resolver_en("BAL'6002'2RS", &inv()), Resolucion::Uno(5));
        assert_eq!(resolver_en("NO-EXISTE", &inv()), Resolucion::Ninguno);
        assert_eq!(resolver_en("   ", &inv()), Resolucion::Ninguno);
    }

    #[test]
    fn codigo_dentro_de_cadena_de_alessia() {
        // CDI-02 está contenido en CDI-023 (se descarta), "12" es corto,
        // "cilindro" no tiene dígitos, DM-125 no es palabra completa en DM-1250.
        assert_eq!(resolver_en(ALESSIA, &inv()), Resolucion::Uno(1));
    }

    #[test]
    fn varios_candidatos_en_orden_de_aparicion() {
        let r = resolver_en("X1 DM'125 FT'150 PIEZA", &inv());
        assert_eq!(r, Resolucion::Varios(vec![7, 6]));
    }

    #[test]
    fn palabra_comun_no_se_detecta_dentro() {
        assert_eq!(resolver_en("KIT CILINDRO COMPLETO", &inv()), Resolucion::Ninguno);
        // Pero sí exacta.
        assert_eq!(resolver_en("cilindro", &inv()), Resolucion::Uno(3));
    }

    #[test]
    fn texto_con_acentos_no_rompe_utf8() {
        assert_eq!(resolver_en("BALATA ÑANDÚ CDI'023 ÁÉÍ", &inv()), Resolucion::Uno(1));
    }
}
