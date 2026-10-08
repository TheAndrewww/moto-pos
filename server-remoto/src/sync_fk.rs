// sync_fk.rs — Piezas del sync v2 que no son endpoints:
//
//   * Esquema real de Postgres (information_schema) para filtrar las columnas
//     que manda el escritorio y bindear cada valor con su tipo real.
//   * Reloj: comparación LWW con marcas normalizadas y tope en "ahora".
//   * Traducción de llaves foráneas (FK): cada lado numera sus filas por su
//     cuenta, así que una FK entera solo significa algo del lado que la
//     escribió. Las FK viajan como uuid (`refs`) o se traducen con el mapa
//     `sync_id_map` (id local del escritorio → uuid → id de aquí).
//   * Celdas pendientes (`sync_fk_pendiente`) y su reintento
//     (compara-e-intercambia con `valor_puesto`).
//   * Política por fila del push (v2 / escritorio viejo / fila web / sin_mapa).
//   * Combinación a tres bandas de productos con la `base` del escritorio,
//     decidida por contenido, y la huella que hace idempotente ese push.
//   * Celdas que la web escribió en filas del escritorio sin reparar
//     (sync_fk_celda_web): se quitan cuando el escritorio las reescribe.
//   * Referencias para el pull (`__refs`) y limpieza para clientes viejos.
//   * Reparación de las FK de filas viejas del escritorio (SPEC §5) y su
//     reversa por lote.
//
// Las decisiones están separadas en funciones puras (sin BD) para poder
// probarlas; las funciones async solo traen datos y escriben.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Mutex;

use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{json, Map, Value};
use sqlx::postgres::PgArguments;
use sqlx::query::Query;
use sqlx::{PgPool, Postgres, Row, Transaction};
use tokio::sync::OnceCell;

use crate::error::{ApiError, ApiResult};
use crate::sync_fk_map::{
    es_uuid_escritorio, fks_de, hijos_de, tabla_de_auditoria, RefKind, AGGREGATES,
    AUDIT_TABLAS_TRADUCIBLES, FK_MAP,
};

/// Hora local de la tienda en el formato de `updated_at`.
pub const SQL_AHORA_LOCAL: &str =
    "SELECT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS')";

/// Candado (advisory lock) que separa la reparación de los push: cada push
/// toma la versión compartida; la reparación, la exclusiva.
pub const LLAVE_REPARACION: i64 = 0x5359_4E43_464B_5632; // "SYNCFKV2"

/// Regex de uuid hecho en el escritorio, para SQL.
const SQL_HEX32: &str = "'^[0-9a-f]{32}$'";

// =============================================================================
// Utilidades de tablas
// =============================================================================

/// ¿La tabla es hija de un agregado? Devuelve (padre, columna_fk).
pub fn padre_de(tabla_hijo: &str) -> Option<(&'static str, &'static str)> {
    for (padre, hijos) in AGGREGATES {
        for (h, fk) in *hijos {
            if *h == tabla_hijo {
                return Some((padre, fk));
            }
        }
    }
    None
}

fn es_traducible(kind: &RefKind) -> bool {
    matches!(kind, RefKind::Tabla(_) | RefKind::Polimorfico(_))
}

/// ¿La tabla (o alguno de sus hijos) tiene celdas que se traducen por uuid?
pub fn tiene_fks_traducibles(tabla: &str) -> bool {
    fks_de(tabla).iter().any(|(_, k)| es_traducible(k))
        || hijos_de(tabla)
            .iter()
            .any(|(h, _)| fks_de(h).iter().any(|(_, k)| es_traducible(k)))
}

/// Tabla destino de una celda FK de `data`, o None si la celda no se
/// traduce (Paso, Padre o polimórfica hacia una tabla que no viaja).
pub fn tabla_destino(kind: &RefKind, data: &Map<String, Value>) -> Option<&'static str> {
    match kind {
        RefKind::Tabla(t) => Some(t),
        RefKind::Polimorfico(col) => data
            .get(*col)
            .and_then(|v| v.as_str())
            .and_then(tabla_de_auditoria),
        RefKind::Padre(_) | RefKind::Paso(_) => None,
    }
}

/// ¿La fila la hizo el escritorio? Por formato de uuid; en audit_log por
/// `origen` ('POS' = escritorio), porque ahí el servidor también genera 32 hex.
pub fn es_fila_escritorio(tabla: &str, uuid: &str, data: &Map<String, Value>) -> bool {
    if tabla == "audit_log" {
        return data.get("origen").and_then(|v| v.as_str()) == Some("POS");
    }
    es_uuid_escritorio(uuid)
}

/// ¿Las celdas FK de la fila ya están en "idioma del servidor"? Sí si la hizo
/// la web, o si es del escritorio y ya se tradujo (push v2) o se reparó.
pub fn en_significado_servidor(es_escritorio: bool, estado: Option<&str>) -> bool {
    !es_escritorio || matches!(estado, Some("traducido") | Some("reparado"))
}

/// Pull de un cliente viejo: ¿hay que quitar las columnas FK de la fila para
/// que conserve sus valores locales? Sí para filas del escritorio que tienen
/// cualquier estado (traducido/reparado, y también pendiente/sin_mapa, cuyas
/// celdas pueden estar a medio traducir o con el centinela 0).
pub fn quitar_fks_para_legacy(es_escritorio: bool, estado: Option<&str>) -> bool {
    es_escritorio && estado.is_some()
}

/// Pull de un cliente viejo: filas de caja hechas en la web no se mandan (ya
/// fallaban en escritorios viejos; protege a un binario viejo reinstalado).
pub fn es_caja_web_legacy(tabla: &str, uuid: &str) -> bool {
    matches!(tabla, "cortes" | "movimientos_caja" | "aperturas_caja" | "ventas")
        && !es_uuid_escritorio(uuid)
}

/// Quita de `data` las columnas FK pendientes aquí (su valor es un centinela
/// o lo que había antes; un cliente viejo debe conservar el suyo).
pub fn quitar_columnas(data: &mut Map<String, Value>, columnas: Option<&HashSet<String>>) {
    for c in columnas.into_iter().flatten() {
        data.remove(c);
    }
}

/// Quita de `data` las columnas FK que se traducen (Tabla/Polimórfico).
pub fn quitar_fks(tabla: &str, data: &mut Map<String, Value>) {
    let quitar: Vec<&str> = fks_de(tabla)
        .iter()
        .filter(|(_, k)| tabla_destino(k, data).is_some())
        .map(|(c, _)| *c)
        .collect();
    for c in quitar {
        data.remove(c);
    }
}

/// Entero de un valor JSON (número entero, flotante sin fracción o texto).
pub fn valor_entero(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.fract() == 0.0 && f.abs() < 9.0e15)
                .map(|f| f as i64)
        }),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

// =============================================================================
// Reloj (LWW)
// =============================================================================

/// Normaliza una marca para comparar: primeros 19 caracteres, 'T' → ' '.
pub fn normalizar_ts(s: &str) -> String {
    s.chars().take(19).collect::<String>().replace('T', " ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lww {
    /// Aplicar. Si trae marca, es la que hay que escribir (normalizada y con
    /// tope en "ahora"); None = el cambio no trae updated_at.
    Aplicar(Option<String>),
    /// El servidor tiene una versión estrictamente más nueva.
    Rechazar,
}

/// Marca entrante normalizada y con tope en "ahora" (None si no trae).
pub fn marca_efectiva(entrante: Option<&str>, ahora: &str) -> Option<String> {
    let e = normalizar_ts(entrante?);
    if e.is_empty() {
        return None;
    }
    let ahora = normalizar_ts(ahora);
    Some(if e > ahora { ahora } else { e })
}

/// LWW del push: marcas normalizadas; una marca entrante en el futuro se
/// recorta a "ahora" (hora local del servidor); se rechaza solo si lo
/// guardado es ESTRICTAMENTE más nuevo (igual = aplicar).
pub fn decidir_lww(guardado: Option<&str>, entrante: Option<&str>, ahora: &str) -> Lww {
    // Sin marca: como antes, se acepta sin tocar updated_at.
    let Some(efectiva) = marca_efectiva(entrante, ahora) else { return Lww::Aplicar(None) };
    if let Some(g) = guardado.map(normalizar_ts) {
        if !g.is_empty() && g > efectiva {
            return Lww::Rechazar;
        }
    }
    Lww::Aplicar(Some(efectiva))
}

// =============================================================================
// Esquema real de Postgres + filtro de columnas + bind tipado
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipoCol {
    Entero,
    Numerico,
    Real,
    Texto,
    Booleano,
    Otro,
}

#[derive(Debug, Clone)]
pub struct ColInfo {
    pub tipo: TipoCol,
    /// `data_type` tal cual lo reporta information_schema (para CAST).
    pub tipo_pg: String,
    pub nullable: bool,
    pub con_default: bool,
    /// Decimales de una columna numeric(p, s) (None = sin escala declarada,
    /// o la columna no es numeric). Postgres redondea al guardar; para
    /// comparar contenido hay que redondear igual (el escritorio guarda REAL).
    pub escala: Option<u32>,
}

impl ColInfo {
    pub fn nueva(tipo_pg: &str, nullable: bool, con_default: bool) -> Self {
        let tipo = match tipo_pg {
            "bigint" | "integer" | "smallint" => TipoCol::Entero,
            "numeric" | "decimal" => TipoCol::Numerico,
            "real" | "double precision" => TipoCol::Real,
            "text" | "character varying" | "character" => TipoCol::Texto,
            "boolean" => TipoCol::Booleano,
            _ => TipoCol::Otro,
        };
        Self { tipo, tipo_pg: tipo_pg.to_string(), nullable, con_default, escala: None }
    }

    /// Con la escala de information_schema (solo cuenta en columnas numeric).
    pub fn con_escala(mut self, escala: Option<u32>) -> Self {
        self.escala = if self.tipo == TipoCol::Numerico { escala } else { None };
        self
    }

    /// El decimal como lo guardaría Postgres en esta columna: redondeado a su
    /// escala, con el punto medio lejos del cero (igual que numeric).
    pub fn redondear(&self, d: Decimal) -> Decimal {
        match self.escala {
            Some(s) => d.round_dp_with_strategy(s, RoundingStrategy::MidpointAwayFromZero),
            None => d,
        }
    }
}

pub type ColumnasTabla = HashMap<String, ColInfo>;

#[derive(Debug, Default)]
pub struct EsquemaPg {
    pub tablas: HashMap<String, ColumnasTabla>,
}

impl EsquemaPg {
    pub fn columnas(&self, tabla: &str) -> Option<&ColumnasTabla> {
        self.tablas.get(tabla)
    }
}

static ESQUEMA: OnceCell<EsquemaPg> = OnceCell::const_new();

/// Esquema de las tablas sincronizadas (se lee una vez, después de las
/// migraciones, en el primer uso).
pub async fn esquema(pool: &PgPool) -> ApiResult<&'static EsquemaPg> {
    ESQUEMA.get_or_try_init(|| cargar_esquema(pool)).await
}

async fn cargar_esquema(pool: &PgPool) -> ApiResult<EsquemaPg> {
    let tablas: Vec<String> = FK_MAP.iter().map(|(t, _)| t.to_string()).collect();
    let rows = sqlx::query(
        "SELECT table_name::text AS t, column_name::text AS c, data_type::text AS tipo, \
                (is_nullable = 'YES') AS nullable, (column_default IS NOT NULL) AS con_default, \
                numeric_scale::int AS escala \
           FROM information_schema.columns \
          WHERE table_schema = 'public' AND table_name = ANY($1)",
    )
    .bind(&tablas)
    .fetch_all(pool)
    .await?;
    let mut e = EsquemaPg::default();
    for r in rows {
        let t: String = r.get("t");
        let c: String = r.get("c");
        let tipo: String = r.get("tipo");
        let escala: Option<i32> = r.get("escala");
        let info = ColInfo::nueva(&tipo, r.get("nullable"), r.get("con_default"))
            .con_escala(escala.and_then(|s| u32::try_from(s).ok()));
        e.tablas.entry(t).or_default().insert(c, info);
    }
    Ok(e)
}

/// Valor ya convertido al tipo real de la columna.
#[derive(Debug, Clone, PartialEq)]
pub enum ValorPg {
    Entero(Option<i64>),
    Numerico(Option<Decimal>),
    Real(Option<f64>),
    Texto(Option<String>),
    Booleano(Option<bool>),
    /// Tipo que no manejamos directo: se manda como texto con CAST.
    Otro(Option<String>),
}

impl ValorPg {
    pub fn es_nulo(&self) -> bool {
        match self {
            ValorPg::Entero(v) => v.is_none(),
            ValorPg::Numerico(v) => v.is_none(),
            ValorPg::Real(v) => v.is_none(),
            ValorPg::Texto(v) | ValorPg::Otro(v) => v.is_none(),
            ValorPg::Booleano(v) => v.is_none(),
        }
    }
}

fn decimal_de_texto(s: &str) -> Option<Decimal> {
    Decimal::from_str(s).ok().or_else(|| Decimal::from_scientific(s).ok())
}

fn texto_limpio(s: &str) -> String {
    // Postgres TEXT no acepta NUL (error 22021); SQLite sí.
    if s.contains('\0') { s.replace('\0', "") } else { s.to_string() }
}

/// Convierte un valor JSON al tipo real de la columna.
pub fn convertir_valor(info: &ColInfo, v: &Value) -> Result<ValorPg, String> {
    let r = match info.tipo {
        TipoCol::Entero => ValorPg::Entero(match v {
            Value::Null => None,
            Value::Bool(b) => Some(*b as i64),
            Value::Number(n) => match n.as_i64() {
                Some(i) => Some(i),
                None => Some(n.as_f64().ok_or("número fuera de rango")?.round() as i64),
            },
            Value::String(s) if s.trim().is_empty() => None,
            Value::String(s) => match s.trim().parse::<i64>() {
                Ok(i) => Some(i),
                Err(_) => Some(
                    s.trim()
                        .parse::<f64>()
                        .map_err(|_| format!("'{s}' no es un número entero"))?
                        .round() as i64,
                ),
            },
            _ => return Err("se esperaba un número entero".into()),
        }),
        TipoCol::Numerico => ValorPg::Numerico(match v {
            Value::Null => None,
            Value::Bool(b) => Some(Decimal::from(*b as i64)),
            Value::Number(n) => Some(
                decimal_de_texto(&n.to_string())
                    .or_else(|| n.as_f64().and_then(Decimal::from_f64))
                    .ok_or_else(|| format!("'{n}' no es un número válido"))?,
            ),
            Value::String(s) if s.trim().is_empty() => None,
            Value::String(s) => Some(
                decimal_de_texto(s.trim()).ok_or_else(|| format!("'{s}' no es un número"))?,
            ),
            _ => return Err("se esperaba un número".into()),
        }),
        TipoCol::Real => ValorPg::Real(match v {
            Value::Null => None,
            Value::Bool(b) => Some(*b as i64 as f64),
            Value::Number(n) => n.as_f64(),
            Value::String(s) if s.trim().is_empty() => None,
            Value::String(s) => {
                Some(s.trim().parse::<f64>().map_err(|_| format!("'{s}' no es un número"))?)
            }
            _ => return Err("se esperaba un número".into()),
        }),
        TipoCol::Texto => ValorPg::Texto(match v {
            Value::Null => None,
            // '' se queda '' solo en columnas NOT NULL; en las que aceptan
            // NULL se guarda NULL como siempre (la web usa IS NULL).
            Value::String(s) if s.is_empty() => {
                if info.nullable { None } else { Some(String::new()) }
            }
            Value::String(s) => Some(texto_limpio(s)),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            otro => Some(texto_limpio(&otro.to_string())),
        }),
        TipoCol::Booleano => ValorPg::Booleano(match v {
            Value::Null => None,
            Value::Bool(b) => Some(*b),
            Value::Number(n) => Some(n.as_f64().unwrap_or(0.0) != 0.0),
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "" => None,
                "1" | "t" | "true" => Some(true),
                "0" | "f" | "false" => Some(false),
                _ => return Err(format!("'{s}' no es booleano")),
            },
            _ => return Err("se esperaba un booleano".into()),
        }),
        TipoCol::Otro => ValorPg::Otro(match v {
            Value::Null => None,
            Value::String(s) if s.is_empty() && info.nullable => None,
            Value::String(s) => Some(texto_limpio(s)),
            otro => Some(otro.to_string()),
        }),
    };
    Ok(r)
}

/// Columnas que nunca se escriben desde el JSON del cliente.
pub const COLUMNAS_IGNORADAS: &[&str] = &["id", "synced_at"];
/// Columnas que nunca se sobreescriben en un ON CONFLICT (solo al insertar).
pub const COLUMNAS_INMUTABLES: &[&str] = &["uuid", "origen", "caja"];

/// Filtro de columnas: solo las que existen de verdad en la tabla de
/// Postgres (menos id/synced_at), convertidas a su tipo. Un NULL para una
/// columna NOT NULL con default se omite (que la BD ponga el default).
/// Devuelve (columnas, llaves descartadas).
pub fn preparar_columnas(
    cols: &ColumnasTabla,
    data: &Map<String, Value>,
) -> Result<(Vec<(String, ValorPg)>, Vec<String>), String> {
    let mut out = Vec::with_capacity(data.len());
    let mut descartadas = Vec::new();
    for (k, v) in data {
        if COLUMNAS_IGNORADAS.contains(&k.as_str()) || k.starts_with("__") {
            continue;
        }
        let Some(info) = cols.get(k) else {
            descartadas.push(k.clone());
            continue;
        };
        let valor = convertir_valor(info, v).map_err(|e| format!("columna {k}: {e}"))?;
        if valor.es_nulo() && !info.nullable && info.con_default {
            continue;
        }
        out.push((k.clone(), valor));
    }
    Ok((out, descartadas))
}

static DESCARTADAS_AVISADAS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Avisa una sola vez por (tabla, columna) que llegó una llave que no existe
/// en Postgres (puede esconder una columna renombrada).
pub fn avisar_descartadas(tabla: &str, descartadas: &[String]) {
    if descartadas.is_empty() {
        return;
    }
    if let Ok(mut g) = DESCARTADAS_AVISADAS.lock() {
        let set = g.get_or_insert_with(HashSet::new);
        for c in descartadas {
            let k = format!("{tabla}.{c}");
            if set.insert(k) {
                tracing::warn!("sync: columna '{c}' de '{tabla}' no existe en Postgres; se ignora");
            }
        }
    }
}

pub fn bind_valor<'q>(
    q: Query<'q, Postgres, PgArguments>,
    v: &ValorPg,
) -> Query<'q, Postgres, PgArguments> {
    match v {
        ValorPg::Entero(x) => q.bind(*x),
        ValorPg::Numerico(x) => q.bind(*x),
        ValorPg::Real(x) => q.bind(*x),
        ValorPg::Texto(x) | ValorPg::Otro(x) => q.bind(x.clone()),
        ValorPg::Booleano(x) => q.bind(*x),
    }
}

/// SQL del upsert por uuid (puro, para poder probarlo).
pub fn sql_upsert(tabla: &str, columnas: &[(String, ValorPg)], cols: &ColumnasTabla) -> String {
    let lista = columnas
        .iter()
        .map(|(c, _)| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(",");
    let marcadores = columnas
        .iter()
        .enumerate()
        .map(|(i, (c, v))| match v {
            ValorPg::Otro(_) => {
                let tipo = cols.get(c).map(|x| x.tipo_pg.as_str()).unwrap_or("text");
                format!("CAST(${} AS {})", i + 1, tipo)
            }
            _ => format!("${}", i + 1),
        })
        .collect::<Vec<_>>()
        .join(",");
    let sets = columnas
        .iter()
        .filter(|(c, _)| !COLUMNAS_INMUTABLES.contains(&c.as_str()))
        .map(|(c, _)| format!("\"{c}\"=EXCLUDED.\"{c}\""))
        .collect::<Vec<_>>()
        .join(",");
    if sets.is_empty() {
        format!("INSERT INTO {tabla} ({lista}) VALUES ({marcadores}) ON CONFLICT (uuid) DO NOTHING RETURNING id")
    } else {
        format!(
            "INSERT INTO {tabla} ({lista}) VALUES ({marcadores}) ON CONFLICT (uuid) DO UPDATE SET {sets} RETURNING id"
        )
    }
}

/// SQL del UPDATE por uuid de una fila que ya existe (puro): solo las
/// columnas que trae el cambio, menos las inmutables. El uuid va al final.
/// None si no hay nada que actualizar.
pub fn sql_update(tabla: &str, columnas: &[(String, ValorPg)], cols: &ColumnasTabla) -> Option<(String, Vec<usize>)> {
    let mut sets = Vec::new();
    let mut indices = Vec::new();
    for (i, (c, v)) in columnas.iter().enumerate() {
        if COLUMNAS_INMUTABLES.contains(&c.as_str()) {
            continue;
        }
        indices.push(i);
        let n = indices.len();
        let marcador = match v {
            ValorPg::Otro(_) => {
                let tipo = cols.get(c).map(|x| x.tipo_pg.as_str()).unwrap_or("text");
                format!("CAST(${n} AS {tipo})")
            }
            _ => format!("${n}"),
        };
        sets.push(format!("\"{c}\"={marcador}"));
    }
    if sets.is_empty() {
        return None;
    }
    let n = indices.len() + 1;
    Some((format!("UPDATE {tabla} SET {} WHERE uuid = ${n} RETURNING id", sets.join(",")), indices))
}

/// Escribe una fila: UPDATE si ya existe (así un cambio puede omitir
/// columnas NOT NULL — lo guardado se queda —, cosa que un INSERT … ON
/// CONFLICT no permite: Postgres valida NOT NULL antes de ver el conflicto);
/// si no, INSERT … ON CONFLICT(uuid) DO UPDATE. Devuelve el id de la fila.
pub async fn escribir_fila(
    tx: &mut Transaction<'_, Postgres>,
    tabla: &str,
    columnas: &[(String, ValorPg)],
    cols: &ColumnasTabla,
    existe: bool,
) -> ApiResult<i64> {
    if existe {
        let uuid = columnas
            .iter()
            .find(|(c, _)| c == "uuid")
            .and_then(|(_, v)| match v {
                ValorPg::Texto(Some(u)) => Some(u.clone()),
                _ => None,
            })
            .ok_or_else(|| ApiError::BadRequest(format!("{tabla}: falta uuid")))?;
        let id: Option<i64> = match sql_update(tabla, columnas, cols) {
            Some((sql, indices)) => {
                let mut q = sqlx::query(&sql);
                for i in indices {
                    q = bind_valor(q, &columnas[i].1);
                }
                q.bind(&uuid).fetch_optional(&mut **tx).await?.map(|r| r.get::<i64, _>(0))
            }
            None => sqlx::query_scalar(&format!("SELECT id FROM {tabla} WHERE uuid = $1"))
                .bind(&uuid)
                .fetch_optional(&mut **tx)
                .await?,
        };
        if let Some(id) = id {
            return Ok(id);
        }
    }
    upsert_tipado(tx, tabla, columnas, cols).await
}

/// INSERT … ON CONFLICT(uuid) DO UPDATE con columnas ya filtradas y
/// tipadas. Devuelve el id de la fila.
pub async fn upsert_tipado(
    tx: &mut Transaction<'_, Postgres>,
    tabla: &str,
    columnas: &[(String, ValorPg)],
    cols: &ColumnasTabla,
) -> ApiResult<i64> {
    let uuid = columnas
        .iter()
        .find(|(c, _)| c == "uuid")
        .and_then(|(_, v)| match v {
            ValorPg::Texto(Some(u)) => Some(u.clone()),
            _ => None,
        })
        .ok_or_else(|| ApiError::BadRequest(format!("{tabla}: falta uuid")))?;
    let sql = sql_upsert(tabla, columnas, cols);
    let mut q = sqlx::query(&sql);
    for (_, v) in columnas {
        q = bind_valor(q, v);
    }
    if let Some(r) = q.fetch_optional(&mut **tx).await? {
        return Ok(r.get::<i64, _>(0));
    }
    // DO NOTHING con conflicto: la fila ya existía.
    let id: i64 = sqlx::query_scalar(&format!("SELECT id FROM {tabla} WHERE uuid = $1"))
        .bind(&uuid)
        .fetch_one(&mut **tx)
        .await?;
    Ok(id)
}

// =============================================================================
// Traducción de FK (decisiones puras + búsquedas por lote)
// =============================================================================

/// Una búsqueda que hace falta para traducir una celda.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Pedido {
    /// uuid → id de aquí (incluye filas borradas).
    PorUuid(&'static str, String),
    /// id local del escritorio → sync_id_map → uuid → id de aquí.
    PorMapa(&'static str, i64),
}

/// Resultado de las búsquedas de un lote.
#[derive(Debug, Default, Clone)]
pub struct Busquedas {
    /// (tabla, uuid) → id de aquí.
    pub por_uuid: HashMap<(String, String), i64>,
    /// (tabla, id local) → (uuid según el mapa, id de aquí si la fila existe).
    pub por_mapa: HashMap<(String, i64), (String, Option<i64>)>,
}

/// Cómo quedó una celda FK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Celda {
    /// Id del servidor (por uuid o por el mapa).
    Id(i64),
    /// La FK es NULL.
    Nula,
    /// No se pudo traducir todavía (la fila referida aún no está aquí, o el
    /// id local no está en el mapa).
    Pendiente { ref_tabla: &'static str, ref_uuid: Option<String>, raw: Option<i64> },
}

/// Celda pendiente lista para `sync_fk_pendiente`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendienteCelda {
    pub tabla: String,
    pub fila_uuid: String,
    pub columna: String,
    pub ref_tabla: String,
    pub ref_uuid: Option<String>,
    pub raw: Option<i64>,
    /// Valor que quedó en la celda (centinela NULL/0 en una fila nueva, o lo
    /// que ya estaba guardado en una fila existente). resolver_pendientes solo
    /// corrige la celda si TODAVÍA tiene este valor.
    pub valor_puesto: Option<i64>,
}

/// Cómo se tratan las celdas FK que se traducen (Tabla/Polimórfico) de una
/// fila del push. Padre siempre se fija con el id del padre de aquí y Paso se
/// copia tal cual.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Politica {
    /// Traducir por `refs` o por el mapa. Si todavía no se puede: fila nueva →
    /// centinela (NULL, o 0 si la columna es NOT NULL) + pendiente; fila que ya
    /// existe → se conserva lo guardado (la columna sale del cambio) y, si
    /// `pendiente_en_existente`, se anota pendiente con valor_puesto = lo guardado.
    Traducir { pendiente_en_existente: bool },
    /// Escritorio viejo, fila del escritorio sin estado: celdas crudas (ids
    /// del escritorio), como antes de v2. La reparación las traduce después.
    Crudo,
    /// Escritorio viejo, fila hecha en la web: sus celdas no se tocan (el
    /// escritorio viejo guarda ids crudos del servidor). Una fila nueva (p.ej.
    /// un hijo que agregó el escritorio) se traduce como `Traducir`.
    Quitar,
    /// Escritorio viejo, fila 'sin_mapa': las celdas que la reparación ya
    /// tradujo (`traducidas`, en su bitácora) se traducen por el mapa; las
    /// demás siguen crudas para que la reparación las tome después.
    SinMapa { traducidas: HashSet<String> },
}

/// Lo que dejó la traducción de una fila.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResultadoFila {
    /// Celdas que quedan pendientes (se guardan en sync_fk_pendiente).
    pub pendientes: Vec<PendienteCelda>,
    /// Columnas que este cambio escribió con un valor definitivo (o crudo):
    /// su pendiente anterior, si había, ya no aplica.
    pub escritas: Vec<String>,
}

/// Valor de `refs` para una columna: Some(Some(uuid)), Some(None) = NULL,
/// None = la llave no viene (o no es usable) → usar el respaldo.
fn ref_de(refs: Option<&Map<String, Value>>, col: &str) -> Option<Option<String>> {
    match refs?.get(col)? {
        Value::Null => Some(None),
        Value::String(s) if !s.is_empty() => Some(Some(s.clone())),
        _ => None,
    }
}

/// Qué búsqueda necesita una celda (puro). None = no hace falta buscar.
pub fn pedido_celda(
    ref_tabla: &'static str,
    col: &str,
    data: &Map<String, Value>,
    refs: Option<&Map<String, Value>>,
) -> Option<Pedido> {
    match ref_de(refs, col) {
        Some(Some(u)) => Some(Pedido::PorUuid(ref_tabla, u)),
        Some(None) => None,
        None => data.get(col).and_then(valor_entero).map(|raw| Pedido::PorMapa(ref_tabla, raw)),
    }
}

/// Decide una celda (puro). None = la celda no se toca (no viene en la fila
/// ni en refs, o trae algo que no es un id).
pub fn decidir_celda(
    ref_tabla: &'static str,
    col: &str,
    data: &Map<String, Value>,
    refs: Option<&Map<String, Value>>,
    b: &Busquedas,
) -> Option<Celda> {
    match ref_de(refs, col) {
        Some(None) => Some(Celda::Nula),
        Some(Some(u)) => match b.por_uuid.get(&(ref_tabla.to_string(), u.clone())) {
            Some(id) => Some(Celda::Id(*id)),
            None => Some(Celda::Pendiente {
                ref_tabla,
                raw: data.get(col).and_then(valor_entero),
                ref_uuid: Some(u),
            }),
        },
        None => {
            let v = data.get(col)?;
            match v {
                Value::Null => Some(Celda::Nula),
                Value::String(s) if s.trim().is_empty() => Some(Celda::Nula),
                _ => {
                    let raw = valor_entero(v)?;
                    match b.por_mapa.get(&(ref_tabla.to_string(), raw)) {
                        Some((_, Some(id))) => Some(Celda::Id(*id)),
                        Some((u, None)) => Some(Celda::Pendiente {
                            ref_tabla,
                            ref_uuid: Some(u.clone()),
                            raw: Some(raw),
                        }),
                        None => Some(Celda::Pendiente { ref_tabla, ref_uuid: None, raw: Some(raw) }),
                    }
                }
            }
        }
    }
}

/// Búsquedas que necesita una fila (puro).
pub fn recolectar_pedidos(
    tabla: &str,
    data: &Map<String, Value>,
    refs: Option<&Map<String, Value>>,
) -> Vec<Pedido> {
    fks_de(tabla)
        .iter()
        .filter_map(|(col, kind)| {
            let destino = tabla_destino(kind, data)?;
            pedido_celda(destino, col, data, refs)
        })
        .collect()
}

/// Traduce las celdas FK de una fila NUEVA (puro). Padre → `padre_id`;
/// Paso → se copia; Tabla/Polimórfico → id de aquí, NULL, o pendiente
/// (NULL si la columna acepta NULL, si no el centinela 0). (Atajo de
/// `traducir_fila_con` para las pruebas.)
#[cfg(test)]
pub fn traducir_fila(
    tabla: &str,
    fila_uuid: &str,
    data: &mut Map<String, Value>,
    refs: Option<&Map<String, Value>>,
    b: &Busquedas,
    cols: Option<&ColumnasTabla>,
    padre_id: Option<i64>,
) -> Vec<PendienteCelda> {
    traducir_fila_con(
        tabla,
        fila_uuid,
        data,
        refs,
        b,
        cols,
        padre_id,
        None,
        &Politica::Traducir { pendiente_en_existente: true },
        &HashSet::new(),
    )
    .pendientes
}

/// Traduce las celdas FK de una fila en su lugar según la política (puro).
/// `guardada` = la fila como está aquí (None = fila nueva). `omitir` =
/// columnas FK que se quedan como están guardadas (salen del cambio).
#[allow(clippy::too_many_arguments)]
pub fn traducir_fila_con(
    tabla: &str,
    fila_uuid: &str,
    data: &mut Map<String, Value>,
    refs: Option<&Map<String, Value>>,
    b: &Busquedas,
    cols: Option<&ColumnasTabla>,
    padre_id: Option<i64>,
    guardada: Option<&Map<String, Value>>,
    politica: &Politica,
    omitir: &HashSet<String>,
) -> ResultadoFila {
    let mut r = ResultadoFila::default();
    for (col, kind) in fks_de(tabla) {
        match kind {
            RefKind::Paso(_) => {}
            RefKind::Padre(_) => {
                if let Some(pid) = padre_id {
                    data.insert(col.to_string(), json!(pid));
                }
            }
            RefKind::Tabla(_) | RefKind::Polimorfico(_) => {
                // La tabla destino de una polimórfica la decide la fila que
                // llega (o la guardada si no trae tabla_afectada).
                let destino = tabla_destino(kind, data).or_else(|| guardada.and_then(|g| tabla_destino(kind, g)));
                let Some(destino) = destino else { continue };
                if omitir.contains(*col) {
                    data.remove(*col);
                    continue;
                }
                let traducir = match politica {
                    Politica::Traducir { .. } => true,
                    Politica::Crudo => false,
                    Politica::Quitar => {
                        if guardada.is_some() {
                            data.remove(*col);
                            continue;
                        }
                        true
                    }
                    Politica::SinMapa { traducidas } => traducidas.contains(*col),
                };
                if !traducir {
                    // Crudo: se guarda tal cual llegó.
                    if data.contains_key(*col) {
                        r.escritas.push(col.to_string());
                    }
                    continue;
                }
                let Some(celda) = decidir_celda(destino, col, data, refs, b) else { continue };
                match celda {
                    Celda::Id(id) => {
                        data.insert(col.to_string(), json!(id));
                        r.escritas.push(col.to_string());
                    }
                    Celda::Nula => {
                        data.insert(col.to_string(), Value::Null);
                        r.escritas.push(col.to_string());
                    }
                    Celda::Pendiente { ref_tabla, ref_uuid, raw } => {
                        let pendiente = |valor_puesto: Option<i64>| PendienteCelda {
                            tabla: tabla.to_string(),
                            fila_uuid: fila_uuid.to_string(),
                            columna: col.to_string(),
                            ref_tabla: ref_tabla.to_string(),
                            ref_uuid: ref_uuid.clone(),
                            raw,
                            valor_puesto,
                        };
                        match guardada {
                            Some(g) => {
                                // Fila existente: se conserva lo guardado.
                                data.remove(*col);
                                let anota = matches!(politica, Politica::Traducir { pendiente_en_existente: true });
                                if anota {
                                    r.pendientes.push(pendiente(g.get(*col).and_then(valor_entero)));
                                }
                            }
                            None => {
                                let nullable = cols
                                    .and_then(|c| c.get(*col))
                                    .map(|c| c.nullable)
                                    .unwrap_or(true);
                                let (v, puesto) = if nullable { (Value::Null, None) } else { (json!(0), Some(0)) };
                                data.insert(col.to_string(), v);
                                r.pendientes.push(pendiente(puesto));
                            }
                        }
                    }
                }
            }
        }
    }
    r
}

/// Política de un hijo según la de su padre. Un hijo nuevo de una fila web
/// (escritorio viejo) se traduce: no hay nada guardado que conservar.
pub fn politica_hijo(padre: &Politica, hijo_existe: bool, traducidas: HashSet<String>) -> Politica {
    match padre {
        Politica::Quitar if !hijo_existe => Politica::Traducir { pendiente_en_existente: false },
        Politica::SinMapa { .. } => Politica::SinMapa { traducidas },
        otra => otra.clone(),
    }
}

/// Política de la fila padre de un push. `estado` = sync_fk_estado de la fila.
///   v2 → traducir (pendiente con lo guardado en filas existentes).
///   escritorio viejo:
///     fila web → no tocar sus FK (el escritorio viejo trae ids del servidor);
///     fila del escritorio sin estado → crudas (como antes de v2);
///     'sin_mapa' → crudas salvo las que la reparación ya tradujo;
///     con otro estado → traducir por el mapa; sin entrada, se conserva lo guardado.
pub fn politica_de(v2: bool, es_escritorio: bool, estado: Option<&str>) -> Politica {
    if v2 {
        return Politica::Traducir { pendiente_en_existente: true };
    }
    if !es_escritorio {
        return Politica::Quitar;
    }
    match estado {
        None => Politica::Crudo,
        Some("sin_mapa") => Politica::SinMapa { traducidas: HashSet::new() },
        Some(_) => Politica::Traducir { pendiente_en_existente: false },
    }
}

// =============================================================================
// productos: combinación a tres bandas con `base`
// =============================================================================

/// Columnas que la combinación nunca decide por sí misma.
const NO_SE_COMBINAN: &[&str] = &["id", "uuid", "updated_at", "synced_at", "created_at"];

/// ¿Dos valores JSON son el mismo valor para la columna? Compara por el tipo
/// real (10 = 10.0 = "10.00"; '' = NULL en texto que acepta NULL). En una
/// columna numeric(p, s) compara lo que Postgres GUARDARÍA (redondeado a s
/// decimales): el REAL del escritorio 33.335 y el 33.34 guardado aquí son el
/// mismo valor (si no, una base con más decimales parecería siempre un cambio
/// del servidor y la combinación revertiría la edición del escritorio).
pub fn mismo_valor(info: Option<&ColInfo>, a: Option<&Value>, b: Option<&Value>) -> bool {
    let nulo = Value::Null;
    let (a, b) = (a.unwrap_or(&nulo), b.unwrap_or(&nulo));
    if let Some(info) = info {
        if let (Ok(x), Ok(y)) = (convertir_valor(info, a), convertir_valor(info, b)) {
            return match (x, y) {
                (ValorPg::Numerico(Some(x)), ValorPg::Numerico(Some(y))) => info.redondear(x) == info.redondear(y),
                (x, y) => x == y,
            };
        }
    }
    a == b
}

/// Valor de una celda FK como id (None = NULL o vacío).
fn id_de_celda(v: Option<&Value>) -> Option<i64> {
    v.and_then(valor_entero)
}

fn decimal_de(v: Option<&Value>) -> Option<Decimal> {
    match v? {
        Value::Number(n) => decimal_de_texto(&n.to_string()).or_else(|| n.as_f64().and_then(Decimal::from_f64)),
        Value::String(s) if !s.trim().is_empty() => decimal_de_texto(s.trim()),
        _ => None,
    }
}

/// Datos para combinar un producto que cambió en el servidor desde la base
/// del escritorio.
pub struct Combinacion<'a> {
    pub guardada: &'a Map<String, Value>,
    pub base: &'a Map<String, Value>,
    pub cols: &'a ColumnasTabla,
    /// Búsquedas del lote (incluye las de las FK de `base` por el mapa).
    pub busq: &'a Busquedas,
    /// Las FK guardadas ya son ids de aquí (fila web, o del escritorio
    /// traducida/reparada).
    pub guardada_en_idioma_servidor: bool,
    /// Columnas FK que la web escribió en una fila del escritorio sin reparar.
    pub celdas_web: &'a HashSet<String>,
    /// Marca entrante ya normalizada y con tope en "ahora".
    pub entrante_ts: &'a str,
}

/// Resultado de la combinación a tres bandas.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ResultadoCombinacion {
    /// Columnas FK que se quedan como están guardadas (la traducción no las
    /// toca).
    pub omitir: HashSet<String>,
    /// ¿El resultado difiere de lo que mandó el escritorio? Es decir, ¿el
    /// servidor tuvo un cambio propio desde la base que sobrevive en la
    /// combinación? false → el servidor no cambió nada (o solo cosas que el
    /// escritorio también dejó igual): el push se aplica como uno normal.
    pub servidor_aporta: bool,
}

/// Combinación a tres bandas de productos (SPEC2 R1.4), en `data` (puro):
///   - stock_actual = MAX(0, guardado + (entrante − base)): se suman las dos
///     ventas (web y escritorio) en vez de que una pise a la otra;
///   - otra columna: si el escritorio no la cambió desde la base, se queda la
///     guardada (sale de `data`); si la cambió y el servidor no, gana la del
///     escritorio; si los dos la cambiaron, gana la marca más nueva.
///
/// "Quién cambió qué" se decide por CONTENIDO (columna por columna contra la
/// base), nunca comparando marcas de dos relojes distintos (SPEC3 S2).
///
/// Las FK se comparan con su significado (crudas del escritorio contra la
/// base; las guardadas contra la base traducida por el mapa). Una FK que se
/// queda la guardada cuenta como aporte del servidor (no se compara con la
/// traducción de la entrante: a lo más se escribe una combinación igual).
pub fn combinar_tres_vias(tabla: &str, data: &mut Map<String, Value>, c: &Combinacion) -> ResultadoCombinacion {
    let mut omitir = HashSet::new();
    let mut aporta = false;
    let guardada_ts = c.guardada.get("updated_at").and_then(|v| v.as_str()).map(normalizar_ts).unwrap_or_default();
    // Empate → gana el escritorio (igual que el LWW del push).
    let gana_guardada = guardada_ts.as_str() > c.entrante_ts;
    let fks = fks_de(tabla);
    let columnas: Vec<String> = data.keys().cloned().collect();
    for col in columnas {
        if NO_SE_COMBINAN.contains(&col.as_str()) || col.starts_with("__") {
            continue;
        }
        let Some(info) = c.cols.get(&col) else { continue };
        if col == "stock_actual" {
            let (g, e, b) = (decimal_de(c.guardada.get(&col)), decimal_de(data.get(&col)), decimal_de(c.base.get(&col)));
            if let (Some(g), Some(e), Some(b)) = (g, e, b) {
                let nuevo = (g + (e - b)).max(Decimal::ZERO);
                // Como quedaría guardado (numeric(12,2)): sin falsos aportes
                // por decimales que el escritorio tiene y aquí no.
                if info.redondear(nuevo) != info.redondear(e) {
                    aporta = true;
                }
                data.insert(col, Value::String(nuevo.normalize().to_string()));
                continue;
            }
        }
        let fk = fks.iter().find(|(c2, _)| *c2 == col).map(|(_, k)| *k);
        if let Some(RefKind::Tabla(ref_tabla)) = fk {
            // Cruda del escritorio (entrante) contra cruda del escritorio (base).
            let escritorio_cambio = !c.base.contains_key(&col)
                || id_de_celda(data.get(&col)) != id_de_celda(c.base.get(&col));
            let servidor_cambio = if c.guardada_en_idioma_servidor {
                let base_aqui: Option<Option<i64>> = match id_de_celda(c.base.get(&col)) {
                    None => Some(None),
                    Some(raw) => match c.busq.por_mapa.get(&(ref_tabla.to_string(), raw)) {
                        Some((_, Some(id))) => Some(Some(*id)),
                        _ => None, // no se sabe
                    },
                };
                match base_aqui {
                    Some(b) => id_de_celda(c.guardada.get(&col)) != b,
                    None => false,
                }
            } else {
                c.celdas_web.contains(&col)
            };
            if servidor_cambio && (!escritorio_cambio || gana_guardada) {
                omitir.insert(col);
                aporta = true;
            }
            continue;
        }
        if !c.base.contains_key(&col) {
            continue; // sin base no se sabe: gana el escritorio
        }
        let escritorio_cambio = !mismo_valor(Some(info), data.get(&col), c.base.get(&col));
        let servidor_cambio = !mismo_valor(Some(info), c.guardada.get(&col), c.base.get(&col));
        if !escritorio_cambio || (servidor_cambio && gana_guardada) {
            // Se queda la guardada: aporta si es distinta de la entrante.
            if !mismo_valor(Some(info), c.guardada.get(&col), data.get(&col)) {
                aporta = true;
            }
            data.remove(&col);
        }
    }
    ResultadoCombinacion { omitir, servidor_aporta: aporta }
}

// =============================================================================
// Push idempotente (SPEC3 S1): huella del cambio
// =============================================================================

/// JSON canónico: llaves ordenadas en todos los niveles, sin espacios (no
/// depende de cómo serde_json guarde el orden de un objeto).
pub fn json_canonico(v: &Value) -> String {
    let mut s = String::new();
    escribir_canonico(v, &mut s);
    s
}

fn escribir_canonico(v: &Value, s: &mut String) {
    match v {
        Value::Object(m) => {
            let mut llaves: Vec<&String> = m.keys().collect();
            llaves.sort();
            s.push('{');
            for (i, k) in llaves.into_iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&Value::String(k.clone()).to_string());
                s.push(':');
                escribir_canonico(&m[k], s);
            }
            s.push('}');
        }
        Value::Array(a) => {
            s.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                escribir_canonico(x, s);
            }
            s.push(']');
        }
        otro => s.push_str(&otro.to_string()),
    }
}

/// Textos de los que sale la huella de un push de productos con `base`:
/// (fila entrante sin id/synced_at, base), ambos en JSON canónico. La huella
/// es md5(entrante) || md5(base) (se calcula en Postgres).
pub fn textos_huella(data: &Value, base: &Map<String, Value>) -> (String, String) {
    let entrante = match data.as_object() {
        Some(m) => {
            let mut m = m.clone();
            m.remove("id");
            m.remove("synced_at");
            Value::Object(m)
        }
        None => data.clone(),
    };
    (json_canonico(&entrante), json_canonico(&Value::Object(base.clone())))
}

// =============================================================================
// Celdas que escribió la web en filas del escritorio sin reparar
// (sync_fk_celda_web, migración 011)
// =============================================================================

/// Celdas marcadas de un lote de filas: (tabla, uuid) → columnas.
pub async fn cargar_celdas_web<'e, E>(ex: E, uuids: &[String]) -> ApiResult<HashMap<(String, String), HashSet<String>>>
where
    E: sqlx::PgExecutor<'e>,
{
    let mut out: HashMap<(String, String), HashSet<String>> = HashMap::new();
    if uuids.is_empty() {
        return Ok(out);
    }
    let rows = sqlx::query("SELECT tabla, uuid, columna FROM sync_fk_celda_web WHERE uuid = ANY($1)")
        .bind(uuids)
        .fetch_all(ex)
        .await?;
    for r in rows {
        out.entry((r.get(0), r.get(1))).or_default().insert(r.get(2));
    }
    Ok(out)
}

/// Entero de un valor ya tipado (None = NULL o no entero).
fn entero_de_valor(v: &ValorPg) -> Option<i64> {
    match v {
        ValorPg::Entero(x) => *x,
        ValorPg::Numerico(x) => x.filter(|d| d.fract().is_zero()).and_then(|d| d.to_i64()),
        ValorPg::Real(x) => x.filter(|f| f.fract() == 0.0).map(|f| f as i64),
        ValorPg::Texto(x) | ValorPg::Otro(x) => x.as_deref().and_then(|s| s.trim().parse().ok()),
        ValorPg::Booleano(x) => x.map(|b| b as i64),
    }
}

/// Celdas marcadas por la web que este push REESCRIBE con un valor distinto
/// del guardado (puro). La escritura del escritorio es más nueva y gana; su
/// significado ya lo cuida el estado de la fila (v2 → 'traducido') o la
/// reparación (crudo), así que la marca sobra y hay que quitarla: si se
/// quedara, la reparación nunca traduciría el id crudo del escritorio. Si el
/// valor es el mismo (un escritorio viejo devolviendo el id del servidor que
/// copió), la marca se queda.
pub fn celdas_web_reescritas(
    marcadas: Option<&HashSet<String>>,
    escritas: &[(String, ValorPg)],
    guardada: Option<&Map<String, Value>>,
) -> Vec<String> {
    let Some(marcadas) = marcadas else { return Vec::new() };
    let mut out: Vec<String> = escritas
        .iter()
        .filter(|(c, _)| marcadas.contains(c))
        .filter(|(c, v)| entero_de_valor(v) != guardada.and_then(|g| g.get(c)).and_then(valor_entero))
        .map(|(c, _)| c.clone())
        .collect();
    out.sort();
    out
}

/// Quita marcas de sync_fk_celda_web de una fila.
pub async fn borrar_celdas_web(
    tx: &mut Transaction<'_, Postgres>,
    tabla: &str,
    uuid: &str,
    columnas: &[String],
) -> ApiResult<()> {
    if columnas.is_empty() {
        return Ok(());
    }
    sqlx::query("DELETE FROM sync_fk_celda_web WHERE tabla = $1 AND uuid = $2 AND columna = ANY($3)")
        .bind(tabla)
        .bind(uuid)
        .bind(columnas)
        .execute(&mut **tx)
        .await?;
    tracing::info!("sync: {tabla} {uuid}: el escritorio reescribió {columnas:?}; se quita la marca de celda web");
    Ok(())
}

/// Hace las búsquedas de un lote de pedidos: una consulta por tabla y tipo.
pub async fn cargar_busquedas(
    tx: &mut Transaction<'_, Postgres>,
    device_uuid: &str,
    pedidos: &[Pedido],
) -> ApiResult<Busquedas> {
    let mut por_uuid: HashMap<&'static str, Vec<String>> = HashMap::new();
    let mut por_mapa: HashMap<&'static str, Vec<i64>> = HashMap::new();
    for p in pedidos {
        match p {
            Pedido::PorUuid(t, u) => por_uuid.entry(*t).or_default().push(u.clone()),
            Pedido::PorMapa(t, id) => por_mapa.entry(*t).or_default().push(*id),
        }
    }
    let mut b = Busquedas::default();
    for (t, mut uuids) in por_uuid {
        uuids.sort();
        uuids.dedup();
        let rows = sqlx::query(&format!("SELECT uuid, id FROM {t} WHERE uuid = ANY($1)"))
            .bind(&uuids)
            .fetch_all(&mut **tx)
            .await?;
        for r in rows {
            b.por_uuid.insert((t.to_string(), r.get::<String, _>(0)), r.get::<i64, _>(1));
        }
    }
    for (t, mut ids) in por_mapa {
        ids.sort();
        ids.dedup();
        let rows = sqlx::query(&format!(
            "SELECT m.local_id, m.uuid, r.id \
               FROM sync_id_map m LEFT JOIN {t} r ON r.uuid = m.uuid \
              WHERE m.device_uuid = $1 AND m.tabla = $2 AND m.local_id = ANY($3)"
        ))
        .bind(device_uuid)
        .bind(t)
        .bind(&ids)
        .fetch_all(&mut **tx)
        .await?;
        for r in rows {
            b.por_mapa.insert(
                (t.to_string(), r.get::<i64, _>(0)),
                (r.get::<String, _>(1), r.get::<Option<i64>, _>(2)),
            );
        }
    }
    Ok(b)
}

/// Borra las celdas pendientes de unas filas (se recalculan en cada push).
pub async fn borrar_pendientes(
    tx: &mut Transaction<'_, Postgres>,
    tabla: &str,
    uuids: &[String],
) -> ApiResult<()> {
    if uuids.is_empty() {
        return Ok(());
    }
    sqlx::query("DELETE FROM sync_fk_pendiente WHERE tabla = $1 AND fila_uuid = ANY($2)")
        .bind(tabla)
        .bind(uuids)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Borra los pendientes de unas celdas de una fila (el cambio las escribió).
pub async fn borrar_pendientes_celdas(
    tx: &mut Transaction<'_, Postgres>,
    tabla: &str,
    fila_uuid: &str,
    columnas: &[String],
) -> ApiResult<()> {
    if columnas.is_empty() {
        return Ok(());
    }
    sqlx::query("DELETE FROM sync_fk_pendiente WHERE tabla = $1 AND fila_uuid = $2 AND columna = ANY($3)")
        .bind(tabla)
        .bind(fila_uuid)
        .bind(columnas)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn guardar_pendientes(
    tx: &mut Transaction<'_, Postgres>,
    device_uuid: &str,
    pendientes: &[PendienteCelda],
) -> ApiResult<()> {
    for p in pendientes {
        sqlx::query(
            "INSERT INTO sync_fk_pendiente \
                 (device_uuid, tabla, fila_uuid, columna, ref_tabla, ref_uuid, raw_local_id, valor_puesto) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (tabla, fila_uuid, columna) DO UPDATE SET \
                 device_uuid = EXCLUDED.device_uuid, ref_tabla = EXCLUDED.ref_tabla, \
                 ref_uuid = EXCLUDED.ref_uuid, raw_local_id = EXCLUDED.raw_local_id, \
                 valor_puesto = EXCLUDED.valor_puesto",
        )
        .bind(device_uuid)
        .bind(&p.tabla)
        .bind(&p.fila_uuid)
        .bind(&p.columna)
        .bind(&p.ref_tabla)
        .bind(&p.ref_uuid)
        .bind(p.raw)
        .bind(p.valor_puesto)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// ¿Le quedan celdas pendientes a la fila o a sus hijos (los que hoy
/// cuelgan del id `pid`)?
pub async fn fila_con_pendientes(
    tx: &mut Transaction<'_, Postgres>,
    tabla: &str,
    uuid: &str,
    pid: i64,
) -> ApiResult<bool> {
    let mut sql = String::from(
        "SELECT EXISTS (SELECT 1 FROM sync_fk_pendiente WHERE tabla = $1 AND fila_uuid = $2)",
    );
    for (h, fk) in hijos_de(tabla) {
        sql.push_str(&format!(
            " OR EXISTS (SELECT 1 FROM sync_fk_pendiente p JOIN {h} c ON c.uuid = p.fila_uuid \
                          WHERE p.tabla = '{h}' AND c.{fk} = $3)"
        ));
    }
    let hay: bool = sqlx::query_scalar(&sql).bind(tabla).bind(uuid).bind(pid).fetch_one(&mut **tx).await?;
    Ok(hay)
}

/// Estado (sync_fk_estado) de una fila.
pub async fn estado_de_fila(
    tx: &mut Transaction<'_, Postgres>,
    tabla: &str,
    uuid: &str,
) -> ApiResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT estado FROM sync_fk_estado WHERE tabla = $1 AND uuid = $2")
        .bind(tabla)
        .bind(uuid)
        .fetch_optional(&mut **tx)
        .await?)
}

/// Celdas que la reparación ya tradujo (bitácora), por (tabla, uuid de fila).
pub async fn celdas_reparadas(
    tx: &mut Transaction<'_, Postgres>,
    uuids: &[String],
) -> ApiResult<HashMap<(String, String), HashSet<String>>> {
    let mut out: HashMap<(String, String), HashSet<String>> = HashMap::new();
    if uuids.is_empty() {
        return Ok(out);
    }
    let rows = sqlx::query(
        "SELECT tabla, fila_uuid, columna FROM sync_fk_repair_log \
          WHERE fila_uuid = ANY($1) AND columna <> '__estado'",
    )
    .bind(uuids)
    .fetch_all(&mut **tx)
    .await?;
    for r in rows {
        out.entry((r.get(0), r.get(1))).or_default().insert(r.get(2));
    }
    Ok(out)
}

/// Celdas pendientes de un lote de filas: (tabla, uuid) → columnas.
pub async fn cargar_pendientes_de(
    pool: &PgPool,
    uuids: &[String],
) -> ApiResult<HashMap<(String, String), HashSet<String>>> {
    let mut out: HashMap<(String, String), HashSet<String>> = HashMap::new();
    if uuids.is_empty() {
        return Ok(out);
    }
    let rows = sqlx::query("SELECT tabla, fila_uuid, columna FROM sync_fk_pendiente WHERE fila_uuid = ANY($1)")
        .bind(uuids)
        .fetch_all(pool)
        .await?;
    for r in rows {
        out.entry((r.get(0), r.get(1))).or_default().insert(r.get(2));
    }
    Ok(out)
}

/// Valor de sync_meta (None si no existe).
pub async fn meta(pool: &PgPool, clave: &str) -> ApiResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT valor FROM sync_meta WHERE clave = $1")
        .bind(clave)
        .fetch_optional(pool)
        .await?)
}

pub async fn escribir_estado(
    tx: &mut Transaction<'_, Postgres>,
    tabla: &str,
    uuid: &str,
    estado: &str,
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO sync_fk_estado (tabla, uuid, estado) VALUES ($1, $2, $3) \
         ON CONFLICT (tabla, uuid) DO UPDATE SET estado = EXCLUDED.estado, updated_at = now()",
    )
    .bind(tabla)
    .bind(uuid)
    .bind(estado)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// =============================================================================
// Mapa de ids (sync_id_map)
// =============================================================================

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RegistroIds {
    pub recibidos: usize,
    /// Ids locales que ya apuntaban a OTRO uuid (señal de un respaldo
    /// restaurado en el escritorio).
    pub remapeados: usize,
}

/// Registra pares (id local, uuid) de un dispositivo. Un id que ya apuntaba
/// al mismo uuid no se toca (conserva su fuente); uno que apuntaba a otro
/// uuid se reemplaza y se avisa en el log.
pub async fn registrar_pares(
    tx: &mut Transaction<'_, Postgres>,
    device_uuid: &str,
    tabla: &str,
    pares: &[(i64, String)],
    fuente: &str,
) -> ApiResult<RegistroIds> {
    // El último par gana si un id viene repetido (ON CONFLICT no acepta
    // tocar la misma fila dos veces).
    let mut unicos: HashMap<i64, &str> = HashMap::new();
    for (id, u) in pares {
        if !u.is_empty() {
            unicos.insert(*id, u.as_str());
        }
    }
    if unicos.is_empty() {
        return Ok(RegistroIds::default());
    }
    let ids: Vec<i64> = unicos.keys().copied().collect();
    let uuids: Vec<String> = ids.iter().map(|i| unicos[i].to_string()).collect();
    let previos = sqlx::query(
        "SELECT local_id, uuid FROM sync_id_map \
          WHERE device_uuid = $1 AND tabla = $2 AND local_id = ANY($3)",
    )
    .bind(device_uuid)
    .bind(tabla)
    .bind(&ids)
    .fetch_all(&mut **tx)
    .await?;
    let mut remapeados = 0;
    for r in &previos {
        let id: i64 = r.get(0);
        let viejo: String = r.get(1);
        if let Some(nuevo) = unicos.get(&id) {
            if *nuevo != viejo {
                remapeados += 1;
                tracing::warn!(
                    "sync_id_map: {device_uuid} {tabla} id {id} cambia de uuid {viejo} a {nuevo} \
                     (¿respaldo restaurado en el escritorio?)"
                );
            }
        }
    }
    sqlx::query(
        "INSERT INTO sync_id_map (device_uuid, tabla, local_id, uuid, fuente) \
         SELECT $1, $2, e.local_id, e.uuid, $5 FROM unnest($3::bigint[], $4::text[]) AS e(local_id, uuid) \
         ON CONFLICT (device_uuid, tabla, local_id) DO UPDATE \
            SET uuid = EXCLUDED.uuid, fuente = EXCLUDED.fuente, updated_at = now() \
          WHERE sync_id_map.uuid IS DISTINCT FROM EXCLUDED.uuid",
    )
    .bind(device_uuid)
    .bind(tabla)
    .bind(&ids)
    .bind(&uuids)
    .bind(fuente)
    .execute(&mut **tx)
    .await?;
    Ok(RegistroIds { recibidos: unicos.len(), remapeados })
}

// =============================================================================
// Reintento de celdas pendientes
// =============================================================================

/// SQL del reintento de un grupo (tabla, columna, ref_tabla) de pendientes
/// (puro, para poder probarlo). Compara-e-intercambia: la celda solo se
/// corrige si TODAVÍA tiene el valor que se puso (`valor_puesto`); si
/// alguien la cambió mientras tanto (p.ej. la web) o la fila ya no existe, el
/// pendiente está viejo y se borra sin tocar nada. $1 tabla, $2 columna,
/// $3 ref_tabla. Devuelve una fila por pendiente borrado.
pub fn sql_resolver_grupo(tabla: &str, columna: &str, ref_tabla: &str) -> String {
    // Pendientes anteriores a la migración 011 no tienen valor_puesto: su
    // celda quedó en NULL (o 0 si es NOT NULL).
    let mismo = format!(
        "(t.{columna} IS NOT DISTINCT FROM p.valor_puesto OR (p.valor_puesto IS NULL AND t.{columna} = 0))"
    );
    format!(
        "WITH res AS ( \
            SELECT p.id AS pid, p.fila_uuid, COALESCE(r1.id, r2.id) AS nuevo, \
                   (t.uuid IS NOT NULL AND {mismo}) AS vigente \
              FROM sync_fk_pendiente p \
              LEFT JOIN {tabla} t ON t.uuid = p.fila_uuid \
              LEFT JOIN {ref_tabla} r1 ON r1.uuid = p.ref_uuid \
              LEFT JOIN sync_id_map m ON p.ref_uuid IS NULL AND m.device_uuid = p.device_uuid \
                    AND m.tabla = p.ref_tabla AND m.local_id = p.raw_local_id \
              LEFT JOIN {ref_tabla} r2 ON r2.uuid = m.uuid \
             WHERE p.tabla = $1 AND p.columna = $2 AND p.ref_tabla = $3 \
         ), upd AS ( \
            UPDATE {tabla} t SET {columna} = res.nuevo FROM sync_fk_pendiente p, res \
             WHERE p.id = res.pid AND res.vigente AND res.nuevo IS NOT NULL \
               AND t.uuid = res.fila_uuid AND {mismo} \
            RETURNING t.id \
         ) \
         DELETE FROM sync_fk_pendiente p USING res \
          WHERE p.id = res.pid AND (res.nuevo IS NOT NULL OR NOT res.vigente) \
         RETURNING p.id"
    )
}

/// Reintenta las celdas de `sync_fk_pendiente` cuyo uuid ya existe aquí o
/// cuyo id local ya tiene entrada en el mapa; actualiza la celda (solo si
/// sigue con el valor que se puso), borra el pendiente (también los viejos) y
/// pasa la fila a 'traducido' cuando ya no le queda ninguno.
/// Devuelve cuántos pendientes se cerraron.
pub async fn resolver_pendientes(pool: &PgPool) -> ApiResult<u64> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(LLAVE_REPARACION)
        .execute(&mut *tx)
        .await?;
    let grupos = sqlx::query("SELECT DISTINCT tabla, columna, ref_tabla FROM sync_fk_pendiente")
        .fetch_all(&mut *tx)
        .await?;
    let mut resueltas = 0u64;
    for g in grupos {
        let tabla: String = g.get(0);
        let columna: String = g.get(1);
        let ref_tabla: String = g.get(2);
        // Los nombres vienen de la BD pero se validan contra el mapa antes de
        // pegarlos en SQL.
        let Some((_, kind)) = fks_de(&tabla).iter().find(|(c, _)| *c == columna) else {
            continue;
        };
        let ref_ok = match kind {
            RefKind::Tabla(r) => *r == ref_tabla,
            RefKind::Polimorfico(_) => tabla_de_auditoria(&ref_tabla).is_some(),
            _ => false,
        };
        if !ref_ok {
            continue;
        }
        // (Los nombres ya se validaron contra el mapa: es seguro pegarlos.)
        let sql = sql_resolver_grupo(&tabla, &columna, &ref_tabla);
        let n = sqlx::query(&sql)
            .persistent(false)
            .bind(&tabla)
            .bind(&columna)
            .bind(&ref_tabla)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        resueltas += n;
    }
    if resueltas > 0 {
        voltear_estados_resueltos(&mut tx).await?;
    }
    tx.commit().await?;
    Ok(resueltas)
}

/// Filas en 'pendiente' sin celdas pendientes (ni propias ni de sus hijos)
/// pasan a 'traducido'.
async fn voltear_estados_resueltos(tx: &mut Transaction<'_, Postgres>) -> ApiResult<()> {
    for (tabla, _) in FK_MAP {
        if padre_de(tabla).is_some() || !tiene_fks_traducibles(tabla) {
            continue;
        }
        let mut sql = format!(
            "UPDATE sync_fk_estado e SET estado = 'traducido', updated_at = now() \
              WHERE e.tabla = '{tabla}' AND e.estado = 'pendiente' \
                AND NOT EXISTS (SELECT 1 FROM sync_fk_pendiente p \
                                 WHERE p.tabla = '{tabla}' AND p.fila_uuid = e.uuid)"
        );
        for (h, fk) in hijos_de(tabla) {
            sql.push_str(&format!(
                " AND NOT EXISTS (SELECT 1 FROM sync_fk_pendiente p \
                                    JOIN {h} c ON c.uuid = p.fila_uuid \
                                    JOIN {tabla} x ON x.id = c.{fk} \
                                   WHERE p.tabla = '{h}' AND x.uuid = e.uuid)"
            ));
        }
        sqlx::query(&sql).execute(&mut **tx).await?;
    }
    Ok(())
}

// =============================================================================
// Referencias para el pull (__refs) y estados
// =============================================================================

/// ids que hay que convertir a uuid para armar las refs de una fila (puro).
pub fn pedidos_refs(tabla: &str, data: &Map<String, Value>) -> Vec<(&'static str, i64)> {
    fks_de(tabla)
        .iter()
        .filter_map(|(col, kind)| {
            let destino = tabla_destino(kind, data)?;
            let id = data.get(*col).and_then(valor_entero)?;
            Some((destino, id))
        })
        .collect()
}

/// Refs de una fila en significado del servidor: {columna: uuid | null}.
/// Si la fila referida no existe (o es el centinela 0), o la celda está
/// pendiente aquí (`pendientes`), la llave se omite ("no sé": el escritorio
/// conserva su valor; nunca null). (puro)
pub fn refs_de_fila(
    tabla: &str,
    data: &Map<String, Value>,
    uuids: &HashMap<(String, i64), String>,
    pendientes: Option<&HashSet<String>>,
) -> Map<String, Value> {
    let mut out = Map::new();
    for (col, kind) in fks_de(tabla) {
        let Some(destino) = tabla_destino(kind, data) else { continue };
        if pendientes.map(|p| p.contains(*col)).unwrap_or(false) {
            continue;
        }
        let Some(v) = data.get(*col) else { continue };
        if v.is_null() {
            out.insert(col.to_string(), Value::Null);
            continue;
        }
        if let Some(id) = valor_entero(v) {
            if let Some(u) = uuids.get(&(destino.to_string(), id)) {
                out.insert(col.to_string(), Value::String(u.clone()));
            }
        }
    }
    out
}

/// id → uuid por tabla, en una consulta por tabla.
pub async fn cargar_uuids_por_id(
    pool: &PgPool,
    pedidos: &[(&'static str, i64)],
) -> ApiResult<HashMap<(String, i64), String>> {
    let mut por_tabla: HashMap<&'static str, Vec<i64>> = HashMap::new();
    for (t, id) in pedidos {
        por_tabla.entry(*t).or_default().push(*id);
    }
    let mut out = HashMap::new();
    for (t, mut ids) in por_tabla {
        ids.sort();
        ids.dedup();
        let rows = sqlx::query(&format!("SELECT id, uuid FROM {t} WHERE id = ANY($1)"))
            .bind(&ids)
            .fetch_all(pool)
            .await?;
        for r in rows {
            out.insert((t.to_string(), r.get::<i64, _>(0)), r.get::<String, _>(1));
        }
    }
    Ok(out)
}

/// Estados (sync_fk_estado) de un lote de filas.
pub async fn cargar_estados(
    pool: &PgPool,
    pares: &[(String, String)],
) -> ApiResult<HashMap<(String, String), String>> {
    if pares.is_empty() {
        return Ok(HashMap::new());
    }
    let tablas: Vec<&str> = pares.iter().map(|(t, _)| t.as_str()).collect();
    let uuids: Vec<&str> = pares.iter().map(|(_, u)| u.as_str()).collect();
    let rows = sqlx::query(
        "SELECT e.tabla, e.uuid, e.estado FROM sync_fk_estado e \
           JOIN unnest($1::text[], $2::text[]) AS p(tabla, uuid) \
             ON p.tabla = e.tabla AND p.uuid = e.uuid",
    )
    .bind(&tablas)
    .bind(&uuids)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ((r.get::<String, _>(0), r.get::<String, _>(1)), r.get::<String, _>(2)))
        .collect())
}

// =============================================================================
// Reparación de FK de filas viejas del escritorio (SPEC §5)
// =============================================================================

/// Una celda reparable: (tabla, columna) → tabla referida. Las hijas se
/// juzgan por el alcance de su padre. Para audit_log.registro_id hay una por
/// cada tabla a la que puede apuntar (`tabla_afectada`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CeldaRep {
    pub tabla: &'static str,
    pub col: &'static str,
    pub ref_tabla: &'static str,
    /// Columna que elige la tabla destino (audit_log.tabla_afectada).
    pub col_poli: Option<&'static str>,
    /// (padre, fk) si la tabla es hija de un agregado.
    pub padre: Option<(&'static str, &'static str)>,
}

/// Todas las celdas que la reparación traduce (stock_sucursal salió del sync).
pub fn celdas_reparables() -> Vec<CeldaRep> {
    let mut out = Vec::new();
    for (tabla, cols) in FK_MAP {
        if *tabla == "stock_sucursal" {
            continue;
        }
        let padre = padre_de(tabla);
        let tabla: &'static str = tabla;
        for (col, kind) in *cols {
            let col: &'static str = col;
            match kind {
                RefKind::Tabla(r) => out.push(CeldaRep { tabla, col, ref_tabla: r, col_poli: None, padre }),
                RefKind::Polimorfico(cp) => {
                    for r in AUDIT_TABLAS_TRADUCIBLES {
                        out.push(CeldaRep { tabla, col, ref_tabla: r, col_poli: Some(cp), padre });
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Tablas "fila" de la reparación (no hijas) que reciben sync_fk_estado.
pub fn tablas_padre_reparables() -> Vec<&'static str> {
    let celdas = celdas_reparables();
    let mut out: Vec<&'static str> = Vec::new();
    for c in &celdas {
        let t = c.padre.map(|(p, _)| p).unwrap_or(c.tabla);
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

fn sql_escritorio(tabla: &str, alias: &str) -> String {
    if tabla == "audit_log" {
        format!("{alias}.origen = 'POS'")
    } else {
        format!("{alias}.uuid ~ {SQL_HEX32}")
    }
}

/// Fila del escritorio que empujó ESTE dispositivo ($1): sus celdas crudas
/// son ids de su mapa. (Hoy hay un solo escritorio; con dos, cada uno repara
/// solo lo que subió.)
fn sql_de_dispositivo(tabla: &str, alias: &str) -> String {
    format!(
        "{} AND EXISTS (SELECT 1 FROM sync_cursor sc WHERE sc.tabla = '{tabla}' \
           AND sc.uuid = {alias}.uuid AND sc.origen_device = $1)",
        sql_escritorio(tabla, alias)
    )
}

/// Fila en alcance: del escritorio, de este dispositivo y sin estado (o
/// 'sin_mapa', que se reintenta).
fn sql_en_alcance(tabla: &str, alias: &str) -> String {
    format!(
        "{} AND NOT EXISTS (SELECT 1 FROM sync_fk_estado e WHERE e.tabla = '{tabla}' \
           AND e.uuid = {alias}.uuid AND e.estado <> 'sin_mapa')",
        sql_de_dispositivo(tabla, alias)
    )
}

/// (FROM, WHERE, expresión del uuid de la fila dueña del estado).
fn sql_alcance(c: &CeldaRep) -> (String, String, &'static str) {
    let mut filtro = match c.padre {
        None => sql_en_alcance(c.tabla, "t"),
        Some((p, _)) => sql_en_alcance(p, "p"),
    };
    if let Some(cp) = c.col_poli {
        filtro.push_str(&format!(" AND t.{cp} = '{}'", c.ref_tabla));
    }
    filtro.push_str(&format!(
        " AND NOT EXISTS (SELECT 1 FROM sync_fk_repair_log l WHERE l.tabla = '{}' \
           AND l.columna = '{}' AND l.fila_uuid = t.uuid)",
        c.tabla, c.col
    ));
    // Celdas que la web ya escribió con ids de aquí (sync_fk_celda_web): no
    // son ids del escritorio, no se traducen.
    filtro.push_str(&format!(
        " AND NOT EXISTS (SELECT 1 FROM sync_fk_celda_web w WHERE w.tabla = '{}' \
           AND w.columna = '{}' AND w.uuid = t.uuid)",
        c.tabla, c.col
    ));
    match c.padre {
        None => (format!("{} t", c.tabla), filtro, "t.uuid"),
        Some((p, fk)) => (format!("{} t JOIN {p} p ON p.id = t.{fk}", c.tabla), filtro, "p.uuid"),
    }
}

/// Celdas en alcance cuyo id crudo no se puede traducir (sin entrada en el
/// mapa, o el uuid del mapa no existe aquí) → su fila queda 'sin_mapa'.
pub fn sql_sin_mapa(c: &CeldaRep) -> String {
    let (desde, filtro, dueno) = sql_alcance(c);
    let padre = c.padre.map(|(p, _)| p).unwrap_or(c.tabla);
    format!(
        "INSERT INTO _rep_sin_mapa (tabla, uuid) \
         SELECT DISTINCT '{padre}', {dueno} FROM {desde} \
           LEFT JOIN sync_id_map m ON m.device_uuid = $1 AND m.tabla = '{r}' AND m.local_id = t.{col} \
           LEFT JOIN {r} r ON r.uuid = m.uuid \
          WHERE {filtro} AND t.{col} IS NOT NULL AND r.id IS NULL",
        r = c.ref_tabla,
        col = c.col
    )
}

/// UNA sentencia por (tabla, columna[, tabla_afectada]): calcula el valor
/// nuevo desde el valor ORIGINAL (nunca encadena), actualiza solo si la celda
/// sigue teniendo ese valor y registra en la bitácora solo los cambios reales.
pub fn sql_traducir(c: &CeldaRep) -> String {
    let (desde, filtro, _) = sql_alcance(c);
    format!(
        "WITH cand AS ( \
            SELECT t.uuid AS fila, t.{col} AS viejo, r.id AS nuevo FROM {desde} \
              JOIN sync_id_map m ON m.device_uuid = $1 AND m.tabla = '{r}' AND m.local_id = t.{col} \
              JOIN {r} r ON r.uuid = m.uuid \
             WHERE {filtro} \
         ), upd AS ( \
            UPDATE {tabla} x SET {col} = c.nuevo FROM cand c \
             WHERE x.uuid = c.fila AND x.{col} = c.viejo AND c.viejo <> c.nuevo \
            RETURNING x.uuid, c.viejo, c.nuevo \
         ) \
         INSERT INTO sync_fk_repair_log (lote, tabla, fila_uuid, columna, valor_anterior, valor_nuevo) \
         SELECT $2, '{tabla}', uuid, '{col}', viejo, nuevo FROM upd",
        tabla = c.tabla,
        col = c.col,
        r = c.ref_tabla
    )
}

/// Estados de las filas procesadas: 'reparado' (aunque nada haya cambiado)
/// o 'sin_mapa'. Cada cambio de estado se anota en la bitácora con columna
/// '__estado' (anterior: NULL = sin fila, 0 = sin_mapa; nuevo: 1 = reparado,
/// 0 = sin_mapa) para poder revertir el lote. $1 = dispositivo, $2 = lote.
fn sql_estados(tabla: &str) -> String {
    format!(
        "WITH alcance AS ( \
            SELECT t.uuid, \
                   CASE WHEN s.uuid IS NULL THEN 'reparado' ELSE 'sin_mapa' END AS nuevo, \
                   e.estado AS previo \
              FROM {tabla} t \
              LEFT JOIN (SELECT DISTINCT uuid FROM _rep_sin_mapa WHERE tabla = '{tabla}') s ON s.uuid = t.uuid \
              LEFT JOIN sync_fk_estado e ON e.tabla = '{tabla}' AND e.uuid = t.uuid \
             WHERE {esc} AND (e.uuid IS NULL OR e.estado = 'sin_mapa') \
         ), ins AS ( \
            INSERT INTO sync_fk_estado (tabla, uuid, estado) \
            SELECT '{tabla}', uuid, nuevo FROM alcance WHERE previo IS DISTINCT FROM nuevo \
            ON CONFLICT (tabla, uuid) DO UPDATE SET estado = EXCLUDED.estado, updated_at = now() \
            RETURNING uuid \
         ) \
         INSERT INTO sync_fk_repair_log (lote, tabla, fila_uuid, columna, valor_anterior, valor_nuevo) \
         SELECT $2, '{tabla}', a.uuid, '__estado', \
                CASE WHEN a.previo IS NULL THEN NULL ELSE 0 END, \
                CASE WHEN a.nuevo = 'reparado' THEN 1 ELSE 0 END \
           FROM alcance a WHERE a.previo IS DISTINCT FROM a.nuevo",
        esc = sql_de_dispositivo(tabla, "t")
    )
}

fn nuevo_lote(device_uuid: &str) -> String {
    let corto: String = device_uuid.chars().take(8).collect();
    let azar: String = uuid::Uuid::new_v4().simple().to_string().chars().take(6).collect();
    format!("rep-{}-{corto}-{azar}", chrono::Utc::now().format("%Y%m%d%H%M%S"))
}

/// Resultado de la reparación.
#[derive(Debug)]
pub struct ResultadoReparacion {
    /// Pasó la verificación (y se confirmó, salvo en simulación).
    pub ok: bool,
    pub reporte: Value,
}

/// Repara las FK de las filas del escritorio que aún no tienen estado
/// (SPEC §5). Una transacción; verifica antes de confirmar; idempotente.
pub async fn reparar(pool: &PgPool, device_uuid: &str, simulacion: bool) -> ApiResult<ResultadoReparacion> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '30s'").execute(&mut *tx).await?;
    // Exclusivo contra los push (que toman la versión compartida).
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(LLAVE_REPARACION)
        .execute(&mut *tx)
        .await?;
    let lote = nuevo_lote(device_uuid);
    let celdas = celdas_reparables();
    let padres = tablas_padre_reparables();
    // Estadísticas frescas: el mapa recién subido y las tablas de control
    // crecen de golpe y, sin estadísticas, el planificador elige ciclos
    // anidados de minutos (medido: 4+ min contra ~0.2 s).
    analizar(&mut tx, &["sync_id_map", "sync_fk_estado", "sync_fk_repair_log"]).await?;

    // ── Línea base ──
    let colgadas_antes: i64 =
        sqlx::query_scalar(&sql_detalle_colgado()).bind(device_uuid).fetch_one(&mut *tx).await?;
    // Foto de todas las celdas FK reparables (+ updated_at) para verificar
    // después que solo cambió lo que quedó en la bitácora.
    sqlx::query(
        "CREATE TEMP TABLE _rep_antes (tabla TEXT, uuid TEXT, columna TEXT, valor BIGINT, updated_at TEXT) \
         ON COMMIT DROP",
    )
    .persistent(false)
    .execute(&mut *tx)
    .await?;
    let mut pares_tc: Vec<(&'static str, &'static str)> = Vec::new();
    for c in &celdas {
        if !pares_tc.contains(&(c.tabla, c.col)) {
            pares_tc.push((c.tabla, c.col));
        }
    }
    // Solo las filas que esta reparación puede tocar (las de este
    // dispositivo): lo que la web u otro equipo cambie en otras filas mientras
    // corre no es una falla de la reparación.
    for (t, col) in &pares_tc {
        let (desde, filtro) = match padre_de(t) {
            None => (format!("{t} t"), sql_de_dispositivo(t, "t")),
            Some((p, fk)) => (format!("{t} t JOIN {p} p ON p.id = t.{fk}"), sql_de_dispositivo(p, "p")),
        };
        sqlx::query(&format!(
            "INSERT INTO _rep_antes SELECT '{t}', t.uuid, '{col}', t.{col}, t.updated_at FROM {desde} WHERE {filtro}"
        ))
        .persistent(false)
        .bind(device_uuid)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("CREATE INDEX ON _rep_antes (tabla, columna, uuid)")
        .persistent(false)
        .execute(&mut *tx)
        .await?;
    analizar(&mut tx, &["_rep_antes"]).await?;

    // ── Filas con celdas sin mapa (desde los valores originales) ──
    sqlx::query("CREATE TEMP TABLE _rep_sin_mapa (tabla TEXT NOT NULL, uuid TEXT NOT NULL) ON COMMIT DROP")
        .persistent(false)
        .execute(&mut *tx)
        .await?;
    for c in &celdas {
        sqlx::query(&sql_sin_mapa(c))
            .persistent(false)
            .bind(device_uuid)
            .execute(&mut *tx)
            .await?;
    }
    analizar(&mut tx, &["_rep_sin_mapa"]).await?;

    // ── Traducción, una sentencia por celda ──
    let mut por_columna: Vec<(String, String, u64)> = Vec::new();
    for c in &celdas {
        let n = sqlx::query(&sql_traducir(c))
            .persistent(false)
            .bind(device_uuid)
            .bind(&lote)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        match por_columna.iter_mut().find(|(t, col, _)| t == c.tabla && col == c.col) {
            Some(x) => x.2 += n,
            None => por_columna.push((c.tabla.to_string(), c.col.to_string(), n)),
        }
    }
    let total_celdas: u64 = por_columna.iter().map(|x| x.2).sum();
    analizar(&mut tx, &["sync_fk_repair_log"]).await?;

    // ── Estados ──
    for p in &padres {
        sqlx::query(&sql_estados(p))
            .persistent(false)
            .bind(device_uuid)
            .bind(&lote)
            .execute(&mut *tx)
            .await?;
    }
    analizar(&mut tx, &["sync_fk_estado", "sync_fk_repair_log"]).await?;
    let filas_rows = sqlx::query(
        "SELECT l.tabla, \
                count(*) FILTER (WHERE l.valor_nuevo = 1) AS reparadas, \
                count(*) FILTER (WHERE l.valor_nuevo = 0) AS sin_mapa \
           FROM sync_fk_repair_log l WHERE l.lote = $1 AND l.columna = '__estado' GROUP BY l.tabla",
    )
    .bind(&lote)
    .fetch_all(&mut *tx)
    .await?;
    let mut filas = Map::new();
    for r in filas_rows {
        filas.insert(
            r.get::<String, _>(0),
            json!({ "reparadas": r.get::<i64, _>(1), "sin_mapa": r.get::<i64, _>(2) }),
        );
    }

    // ── Verificación ──
    let mut fallas: Vec<String> = Vec::new();
    // Solo ventas de este dispositivo: una fila ajena (otro escritorio, o
    // una venta que quedó cruda por una reversa) no bloquea esta reparación.
    let ventas_web_admin: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM ventas v JOIN usuarios u ON u.id = v.usuario_id \
          WHERE {} AND u.uuid LIKE 'web-admin-%'",
        sql_de_dispositivo("ventas", "v")
    ))
    .bind(device_uuid)
    .fetch_one(&mut *tx)
    .await?;
    if ventas_web_admin != 0 {
        fallas.push(format!(
            "{ventas_web_admin} ventas del escritorio siguen a nombre del usuario web-admin \
             (¿falta subir el mapa de usuarios?)"
        ));
    }
    let colgadas_despues: i64 =
        sqlx::query_scalar(&sql_detalle_colgado()).bind(device_uuid).fetch_one(&mut *tx).await?;
    if colgadas_despues > colgadas_antes {
        fallas.push(format!(
            "venta_detalle con producto inexistente subió de {colgadas_antes} a {colgadas_despues}"
        ));
    }
    let mut sin_bitacora = 0i64;
    let mut bitacora_distinta = 0i64;
    for (t, col) in &pares_tc {
        // Celdas que cambiaron y no están en la bitácora de este lote.
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM _rep_antes a JOIN {t} x ON x.uuid = a.uuid \
              WHERE a.tabla = '{t}' AND a.columna = '{col}' AND x.{col} IS DISTINCT FROM a.valor \
                AND NOT EXISTS (SELECT 1 FROM sync_fk_repair_log l WHERE l.lote = $1 \
                                  AND l.tabla = '{t}' AND l.columna = '{col}' AND l.fila_uuid = a.uuid)"
        ))
        .persistent(false)
        .bind(&lote)
        .fetch_one(&mut *tx)
        .await?;
        sin_bitacora += n;
        // Bitácora que no coincide con la foto ni con el valor actual.
        let m: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM sync_fk_repair_log l \
               LEFT JOIN _rep_antes a ON a.tabla = l.tabla AND a.columna = l.columna AND a.uuid = l.fila_uuid \
               LEFT JOIN {t} x ON x.uuid = l.fila_uuid \
              WHERE l.lote = $1 AND l.tabla = '{t}' AND l.columna = '{col}' \
                AND (a.valor IS DISTINCT FROM l.valor_anterior OR x.{col} IS DISTINCT FROM l.valor_nuevo \
                     OR x.updated_at IS DISTINCT FROM a.updated_at)"
        ))
        .persistent(false)
        .bind(&lote)
        .fetch_one(&mut *tx)
        .await?;
        bitacora_distinta += m;
    }
    if sin_bitacora != 0 {
        fallas.push(format!("{sin_bitacora} celdas cambiaron sin quedar en la bitácora"));
    }
    if bitacora_distinta != 0 {
        fallas.push(format!("{bitacora_distinta} renglones de bitácora no coinciden con los datos"));
    }
    let web_tocadas: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM sync_fk_repair_log l WHERE l.lote = $1 AND l.columna <> '__estado' \
            AND ((l.tabla <> 'audit_log' AND l.fila_uuid !~ {SQL_HEX32}) \
              OR (l.tabla = 'audit_log' AND EXISTS (SELECT 1 FROM audit_log a \
                                                    WHERE a.uuid = l.fila_uuid AND a.origen <> 'POS')))"
    ))
    .bind(&lote)
    .fetch_one(&mut *tx)
    .await?;
    if web_tocadas != 0 {
        fallas.push(format!("{web_tocadas} celdas de filas hechas en la web se tocaron"));
    }
    let cursor_escrito: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sync_cursor WHERE xmin = pg_current_xact_id()::xid")
            .fetch_one(&mut *tx)
            .await?;
    if cursor_escrito != 0 {
        fallas.push(format!("la reparación escribió {cursor_escrito} filas en sync_cursor"));
    }

    let ok = fallas.is_empty();
    if ok && !simulacion {
        sqlx::query(
            "INSERT INTO sync_meta (clave, valor) VALUES ('fk_reparado', '1') \
             ON CONFLICT (clave) DO UPDATE SET valor = '1'",
        )
        .execute(&mut *tx)
        .await?;
    }

    // Desde cuándo este servidor separa el dinero por caja (migración 011):
    // el escritorio lo usa como frontera para acomodar ventas web tardías.
    let caja_v2_desde: Option<String> =
        sqlx::query_scalar("SELECT valor FROM sync_meta WHERE clave = 'caja_v2_desde'")
            .fetch_optional(&mut *tx)
            .await?;

    let reporte = json!({
        "ok": ok,
        "lote": lote,
        "caja_v2_desde": caja_v2_desde,
        "device_uuid": device_uuid,
        "simulacion": simulacion,
        "confirmado": ok && !simulacion,
        "celdas_cambiadas": total_celdas,
        "por_columna": por_columna.iter().filter(|x| x.2 > 0)
            .map(|(t, c, n)| json!({ "tabla": t, "columna": c, "cambiadas": n }))
            .collect::<Vec<_>>(),
        "filas": filas,
        "verificacion": {
            "ventas_escritorio_con_web_admin": ventas_web_admin,
            "detalle_colgado_antes": colgadas_antes,
            "detalle_colgado_despues": colgadas_despues,
            "celdas_sin_bitacora": sin_bitacora,
            "bitacora_distinta": bitacora_distinta,
            "celdas_web_tocadas": web_tocadas,
            "sync_cursor_escritos": cursor_escrito,
            "fallas": fallas,
        },
    });

    if ok && !simulacion {
        tx.commit().await?;
        tracing::info!("reparación FK {lote}: {total_celdas} celdas cambiadas");
    } else {
        tx.rollback().await?;
        if !ok {
            tracing::warn!("reparación FK {lote} revertida: {:?}", reporte["verificacion"]["fallas"]);
        }
    }
    Ok(ResultadoReparacion { ok, reporte })
}

/// ANALYZE dentro de la transacción (ve sus propias filas nuevas).
async fn analizar(tx: &mut Transaction<'_, Postgres>, tablas: &[&str]) -> ApiResult<()> {
    for t in tablas {
        sqlx::query(&format!("ANALYZE {t}")).persistent(false).execute(&mut **tx).await?;
    }
    Ok(())
}

/// Renglones de venta (de ventas de este dispositivo, $1) cuyo producto no
/// existe aquí.
fn sql_detalle_colgado() -> String {
    format!(
        "SELECT count(*) FROM venta_detalle vd JOIN ventas v ON v.id = vd.venta_id \
          WHERE {} AND NOT EXISTS (SELECT 1 FROM productos p WHERE p.id = vd.producto_id)",
        sql_de_dispositivo("ventas", "v")
    )
}

/// Revierte un lote de la reparación: cada celda vuelve a su valor anterior
/// solo si todavía tiene el valor que puso la reparación Y su fila (la
/// dueña del estado: el padre para los hijos) sigue 'reparado'/'sin_mapa'.
/// Si un push la reescribió después (estado 'traducido'/'pendiente'), sus
/// celdas ya no son de la reparación: no se tocan y se reportan en
/// `no_restauradas_por_push`. Los estados vuelven a como estaban. Borra la
/// bitácora del lote.
pub async fn revertir_lote(pool: &PgPool, lote: &str) -> ApiResult<Value> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '30s'").execute(&mut *tx).await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(LLAVE_REPARACION)
        .execute(&mut *tx)
        .await?;
    analizar(&mut tx, &["sync_fk_estado", "sync_fk_repair_log"]).await?;
    let grupos = sqlx::query(
        "SELECT tabla, columna, count(*) FROM sync_fk_repair_log \
          WHERE lote = $1 AND columna <> '__estado' GROUP BY 1, 2",
    )
    .bind(lote)
    .fetch_all(&mut *tx)
    .await?;
    let mut restauradas = 0u64;
    let mut en_bitacora = 0i64;
    let mut por_push: Vec<Value> = Vec::new();
    let mut por_push_total = 0i64;
    for g in grupos {
        let tabla: String = g.get(0);
        let columna: String = g.get(1);
        en_bitacora += g.get::<i64, _>(2);
        // Validar nombres contra el mapa antes de pegarlos en SQL.
        let valida = fks_de(&tabla).iter().any(|(c, k)| *c == columna && es_traducible(k));
        if !valida {
            continue;
        }
        let sigue_reparada = sql_revertir_alcance(&tabla);
        restauradas += sqlx::query(&format!(
            "UPDATE {tabla} t SET {columna} = l.valor_anterior FROM sync_fk_repair_log l \
              WHERE l.lote = $1 AND l.tabla = '{tabla}' AND l.columna = '{columna}' \
                AND t.uuid = l.fila_uuid AND t.{columna} = l.valor_nuevo AND {sigue_reparada}"
        ))
        .persistent(false)
        .bind(lote)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        // Celdas cuya fila reescribió un push después de la reparación.
        let saltadas = sqlx::query(&format!(
            "SELECT l.fila_uuid, count(*) OVER () FROM sync_fk_repair_log l \
               JOIN {tabla} t ON t.uuid = l.fila_uuid \
              WHERE l.lote = $1 AND l.tabla = '{tabla}' AND l.columna = '{columna}' \
                AND NOT ({sigue_reparada}) \
              ORDER BY l.id LIMIT 200"
        ))
        .persistent(false)
        .bind(lote)
        .fetch_all(&mut *tx)
        .await?;
        if let Some(r) = saltadas.first() {
            por_push_total += r.get::<i64, _>(1);
        }
        for r in saltadas {
            if por_push.len() < 200 {
                por_push.push(json!({ "tabla": tabla, "fila_uuid": r.get::<String, _>(0), "columna": columna }));
            }
        }
    }
    if por_push_total > 0 {
        tracing::warn!(
            "reversa {lote}: {por_push_total} celdas no se restauraron porque un push reescribió su fila"
        );
    }
    let estados_borrados = sqlx::query(
        "DELETE FROM sync_fk_estado e USING sync_fk_repair_log l \
          WHERE l.lote = $1 AND l.columna = '__estado' AND l.valor_anterior IS NULL \
            AND e.tabla = l.tabla AND e.uuid = l.fila_uuid \
            AND e.estado = CASE WHEN l.valor_nuevo = 1 THEN 'reparado' ELSE 'sin_mapa' END",
    )
    .bind(lote)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let estados_restaurados = sqlx::query(
        "UPDATE sync_fk_estado e SET estado = 'sin_mapa', updated_at = now() \
           FROM sync_fk_repair_log l \
          WHERE l.lote = $1 AND l.columna = '__estado' AND l.valor_anterior = 0 \
            AND e.tabla = l.tabla AND e.uuid = l.fila_uuid \
            AND e.estado = CASE WHEN l.valor_nuevo = 1 THEN 'reparado' ELSE 'sin_mapa' END",
    )
    .bind(lote)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query("DELETE FROM sync_fk_repair_log WHERE lote = $1")
        .bind(lote)
        .execute(&mut *tx)
        .await?;
    let quedan: i64 = sqlx::query_scalar("SELECT count(*) FROM sync_fk_repair_log")
        .fetch_one(&mut *tx)
        .await?;
    if quedan == 0 {
        sqlx::query("DELETE FROM sync_meta WHERE clave = 'fk_reparado'")
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(json!({
        "lote": lote,
        "celdas_en_bitacora": en_bitacora,
        "celdas_restauradas": restauradas,
        "celdas_no_restauradas": en_bitacora - restauradas as i64,
        "no_restauradas_por_push_total": por_push_total,
        "no_restauradas_por_push": por_push,
        "estados_borrados": estados_borrados,
        "estados_restaurados": estados_restaurados,
    }))
}

/// Reversa: condición "la fila dueña del estado sigue 'reparado'/'sin_mapa'"
/// sobre la fila `t`. La dueña es el padre para las tablas hijas; la propia
/// fila para las demás (audit_log incluida). (puro)
pub fn sql_revertir_alcance(tabla: &str) -> String {
    match padre_de(tabla) {
        Some((p, fk)) => format!(
            "EXISTS (SELECT 1 FROM {p} pp JOIN sync_fk_estado e ON e.tabla = '{p}' AND e.uuid = pp.uuid \
               WHERE pp.id = t.{fk} AND e.estado IN ('reparado', 'sin_mapa'))"
        ),
        None => format!(
            "EXISTS (SELECT 1 FROM sync_fk_estado e WHERE e.tabla = '{tabla}' AND e.uuid = t.uuid \
               AND e.estado IN ('reparado', 'sin_mapa'))"
        ),
    }
}

#[cfg(test)]
#[path = "../tests/unit/sync_fk_tests.rs"]
mod tests;
