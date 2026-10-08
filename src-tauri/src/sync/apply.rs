// sync/apply.rs — Aplicar cambios recibidos del remoto a la BD local.
//
// Reglas (sync v2):
//   - Todo el lote corre en UNA transacción. La bandera `sync_suppress_flag`
//     (tabla real: los triggers de outbox y de updated_at la consultan) se
//     pone y se quita DENTRO de esa transacción: si algo truena, el rollback
//     también la quita y los triggers nunca se quedan apagados.
//   - Un SAVEPOINT por cambio: si un cambio falla (incluido un hijo), se
//     deshace completo y se guarda en `sync_inbox` para reintentarlo; los
//     demás cambios del lote sí se aplican.
//   - Solo se escriben columnas que existen localmente (el servidor tiene
//     columnas extra). NULL en NOT NULL con default se omite; NULL en NOT NULL
//     TEXT sin default se guarda como ''.
//   - "El más nuevo gana" con marcas normalizadas: solo se omite si lo local
//     es ESTRICTAMENTE más nuevo; un empate aplica la versión del servidor
//     (el servidor también aplica en empate, y el pull v2 ya no devuelve los
//     ecos de este equipo: así los dos lados terminan iguales).
//   - Si este equipo tiene un cambio sin subir de esa fila, no se pisa: se
//     guarda en la bandeja ('pendiente_local') y se reintenta tras el push.
//     Si el servidor había rechazado ese cambio ('remoto_mas_nuevo') y aquí
//     se aplica su versión, el renglón del outbox se da por resuelto.
//   - Dinero: los cortes, sus hijos y las aperturas NUNCA se importan (la
//     cadena de la caja principal tiene un solo escritor: este equipo). Los
//     movimientos de caja solo se insertan si son de la web y de la caja
//     principal. Ventas/devoluciones/etc. hechas aquí nunca se modifican desde
//     el servidor; solo se aplican las hechas en la web.
//   - Las FK viajan como uuids (`__refs`) y se traducen a ids locales. Los
//     hijos se cuelgan del id LOCAL del padre. Un usuario 'web-admin-*' (solo
//     existe en el servidor) en una FK NOT NULL de una fila web se cambia por
//     el primer administrador activo de aquí: el dinero de esa venta cuenta.
//   - Producto NUEVO cuyo código ya es de otro producto aquí: se guarda como
//     '<código>-W<8 primeros del uuid>' (hecho en la web) o
//     '<código>-D<8 primeros del uuid>' (hecho en un escritorio: códigos
//     repetidos viejos del servidor que trae la re-descarga de una PC nueva)
//     en vez de quedarse atorado. El sufijo es solo de aquí: al subir, el
//     código viaja sin él (worker.rs).
//   - Borrados: siempre lógicos (deleted_at, activo=0). Nunca DELETE físico
//     de la fila recibida. Una fila de CATÁLOGO hecha en la web que llega ya
//     borrada y no existe aquí se crea borrada (sus ventas web la
//     referencian); las del escritorio y las transaccionales no se crean.
//   - Venta/movimiento web de la caja principal que llega cuando un corte ya
//     cubrió su fecha: se re-acomoda con `registrado_at` para que cuente en
//     el período abierto en vez de perderse, salvo que sea anterior a la
//     cadena de cortes de ESTA PC (PC nueva o de reemplazo: ese dinero nunca
//     pasó por este cajón). Un `registrado_at` local nunca se borra con el
//     NULL que manda el servidor.
//   - Productos: la fila aplicada queda como base de la fusión de 3 vías
//     (sync_base, ver base.rs).

use std::collections::HashMap;

use chrono::{Duration as ChronoDuration, NaiveDateTime};
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde::Serialize;
use serde_json::{Map, Value};

use super::fk::{self, RefKind};

/// Motivos con los que se guarda un cambio en `sync_inbox`.
pub const MOTIVO_ERROR: &str = "error";
pub const MOTIVO_PENDIENTE_LOCAL: &str = "pendiente_local";
pub const MOTIVO_SIN_FK: &str = "sin_fk";

/// `ultimo_error` del renglón del outbox que el servidor rechazó por tener
/// una versión más nueva, una vez que esa versión se aplicó aquí.
pub const RESUELTO_SERVIDOR: &str = "resuelto: gana la versión del servidor";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modo {
    /// Pull normal: inserta y actualiza (último-que-escribe-gana).
    Normal,
    /// Re-descarga desde el cursor 0: SOLO inserta filas que faltan. Nunca
    /// actualiza una fila existente (salvo re-colgar los hijos de un
    /// agregado web de su padre local).
    Replay,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ResultadoLote {
    pub aplicados: usize,
    pub omitidos: usize,
    pub aparcados: usize,
}

enum Resultado {
    Aplicado,
    /// Re-descarga: solo se re-colgaron los hijos de un agregado web que ya
    /// existía; la fila padre no se tocó (su entrada en la bandeja, si la
    /// hay, sigue pendiente).
    Recolgado,
    Omitido(&'static str),
    Aparcar { motivo: &'static str, detalle: String },
}

struct Contexto {
    modo: Modo,
    ahora: String,
    acepta_web_desde: Option<String>,
}

/// Hora local en el formato de todas las marcas: 'YYYY-MM-DD HH:MM:SS'.
pub fn ahora_local() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Aplica un lote de cambios del pull normal.
pub fn aplicar_lote(conn: &mut Connection, cambios: &[Value]) -> rusqlite::Result<ResultadoLote> {
    aplicar_lote_con(conn, cambios, Modo::Normal, &ahora_local())
}

/// Aplica un lote con modo y hora explícitos (la hora se inyecta en pruebas).
pub fn aplicar_lote_con(
    conn: &mut Connection,
    cambios: &[Value],
    modo: Modo,
    ahora: &str,
) -> rusqlite::Result<ResultadoLote> {
    let tx = conn.transaction()?;
    // Bandera DENTRO de la transacción: cualquier `?` de aquí en adelante
    // suelta la tx sin commit → rollback → la bandera desaparece con ella.
    tx.execute("INSERT OR IGNORE INTO sync_suppress_flag (id) VALUES (1)", [])?;

    let acepta_web_desde: Option<String> = tx
        .query_row("SELECT acepta_web_desde FROM sync_state WHERE id = 1", [], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()
        .unwrap_or(None)
        .flatten();
    let ctx = Contexto { modo, ahora: ahora.to_string(), acepta_web_desde };
    let mut esquema = Esquema::default();
    let mut res = ResultadoLote::default();

    // Orden estable por tabla para que las referencias existan antes que
    // quien las usa (ventas antes que devoluciones, etc.).
    let mut orden: Vec<&Value> = cambios.iter().collect();
    orden.sort_by_key(|c| fk::orden_de(c.get("__tabla").and_then(|v| v.as_str()).unwrap_or("")));

    for cambio in orden {
        let Some((tabla, uuid)) = identificar(cambio) else {
            log::warn!("sync apply: cambio sin __tabla/uuid, se ignora");
            res.omitidos += 1;
            continue;
        };
        tx.execute_batch("SAVEPOINT sync_cambio")?;
        match aplicar_uno(&tx, &mut esquema, &ctx, cambio) {
            Ok(Resultado::Aplicado) => {
                tx.execute_batch("RELEASE sync_cambio")?;
                quitar_de_bandeja(&tx, &tabla, &uuid)?;
                resolver_rechazo_remoto(&tx, &tabla, &uuid)?;
                res.aplicados += 1;
            }
            Ok(Resultado::Recolgado) => {
                tx.execute_batch("RELEASE sync_cambio")?;
                res.aplicados += 1;
            }
            Ok(Resultado::Omitido(por)) => {
                log::debug!("sync apply: {} {} omitido: {}", tabla, uuid, por);
                tx.execute_batch("ROLLBACK TO sync_cambio; RELEASE sync_cambio")?;
                // En la re-descarga "omitido" solo dice que la fila ya existe
                // (se trae una versión vieja del historial): lo que espera en
                // la bandeja sigue pendiente.
                if ctx.modo != Modo::Replay {
                    quitar_de_bandeja(&tx, &tabla, &uuid)?;
                }
                res.omitidos += 1;
            }
            Ok(Resultado::Aparcar { motivo, detalle }) => {
                tx.execute_batch("ROLLBACK TO sync_cambio; RELEASE sync_cambio")?;
                aparcar(&tx, &tabla, &uuid, motivo, &detalle, cambio, &ctx.ahora)?;
                res.aparcados += 1;
            }
            Err(e) => {
                tx.execute_batch("ROLLBACK TO sync_cambio; RELEASE sync_cambio")?;
                // Solo tabla/uuid/error: el payload puede traer pin/password_hash.
                log::warn!("sync apply: {} {} no se aplicó: {}", tabla, uuid, e);
                aparcar(&tx, &tabla, &uuid, MOTIVO_ERROR, &e, cambio, &ctx.ahora)?;
                res.aparcados += 1;
            }
        }
    }

    tx.execute("DELETE FROM sync_suppress_flag", [])?;
    tx.commit()?;
    Ok(res)
}

fn identificar(cambio: &Value) -> Option<(String, String)> {
    let tabla = cambio.get("__tabla")?.as_str()?.to_string();
    let uuid = cambio.get("__data")?.get("uuid")?.as_str()?.to_string();
    if uuid.is_empty() { return None; }
    Some((tabla, uuid))
}

// ─── Esquema local (PRAGMA table_info) ────────────────────

#[derive(Debug, Clone)]
pub(crate) struct Columna {
    pub nombre: String,
    pub tipo: String,
    pub notnull: bool,
    pub tiene_default: bool,
}

#[derive(Default)]
pub(crate) struct Esquema {
    tablas: HashMap<String, Vec<Columna>>,
}

impl Esquema {
    pub fn columnas(&mut self, conn: &Connection, tabla: &str) -> rusqlite::Result<Vec<Columna>> {
        if let Some(c) = self.tablas.get(tabla) {
            return Ok(c.clone());
        }
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({tabla})"))?;
        let cols: Vec<Columna> = stmt
            .query_map([], |r| {
                Ok(Columna {
                    nombre: r.get(1)?,
                    tipo: r.get::<_, Option<String>>(2)?.unwrap_or_default().to_uppercase(),
                    notnull: r.get::<_, i64>(3)? != 0,
                    tiene_default: r.get::<_, Option<String>>(4)?.is_some(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        self.tablas.insert(tabla.to_string(), cols.clone());
        Ok(cols)
    }
}

fn tiene_columna(cols: &[Columna], nombre: &str) -> bool {
    cols.iter().any(|c| c.nombre == nombre)
}

// ─── Aplicar un cambio ────────────────────────────────────

fn sql_err(e: rusqlite::Error) -> String {
    e.to_string()
}

fn texto<'a>(data: &'a Map<String, Value>, col: &str) -> Option<&'a str> {
    data.get(col).and_then(|v| v.as_str())
}

/// Valor no nulo y no vacío.
fn presente(data: &Map<String, Value>, col: &str) -> bool {
    match data.get(col) {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn aplicar_uno(
    tx: &Transaction,
    esq: &mut Esquema,
    ctx: &Contexto,
    cambio: &Value,
) -> Result<Resultado, String> {
    let obj = cambio.as_object().ok_or("cambio no es un objeto")?;
    let tabla = obj.get("__tabla").and_then(|v| v.as_str()).ok_or("cambio sin __tabla")?;
    let operacion = obj.get("__operacion").and_then(|v| v.as_str()).unwrap_or("UPSERT");
    let data = obj.get("__data").and_then(|v| v.as_object()).ok_or("cambio sin __data")?;
    let uuid = texto(data, "uuid").ok_or("cambio sin uuid")?;

    // Nombre de tabla que llega por la red: solo tablas sincronizadas
    // conocidas (se pega en SQL). Los hijos solo viajan dentro del padre.
    if !fk::es_tabla_sync(tabla) || fk::es_tabla_hija(tabla) {
        return Ok(Resultado::Omitido("tabla no sincronizable"));
    }
    if tabla == "stock_sucursal" {
        return Ok(Resultado::Omitido("stock_sucursal fuera de sync"));
    }
    // Cadena de cortes/aperturas: un solo escritor (este equipo).
    if fk::TABLAS_CAJA_NUNCA.contains(&tabla) {
        return Ok(Resultado::Omitido("caja: cortes/aperturas nunca se importan"));
    }
    // 'Andrew Web' es un login solo del servidor.
    if tabla == "usuarios" && uuid.starts_with("web-admin-") {
        return Ok(Resultado::Omitido("usuario solo web"));
    }

    let cols = esq.columnas(tx, tabla).map_err(sql_err)?;
    if cols.is_empty() {
        return Ok(Resultado::Omitido("tabla no existe localmente"));
    }
    let tiene_updated = tiene_columna(&cols, "updated_at");
    let local: Option<(i64, Option<String>)> = tx
        .query_row(
            &format!(
                "SELECT id, {} FROM {tabla} WHERE uuid = ? LIMIT 1",
                if tiene_updated { "updated_at" } else { "NULL" }
            ),
            [uuid],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(sql_err)?;
    let existe = local.is_some();
    let es_web = fk::es_fila_web(tabla, uuid, data);

    // ── Política por tabla ──
    match tabla {
        "movimientos_caja" => {
            if existe {
                return Ok(Resultado::Omitido("movimiento ya existe (solo inserción)"));
            }
            if texto(data, "origen") != Some("web") || texto(data, "caja") != Some("principal") {
                return Ok(Resultado::Omitido("movimiento que no es web/principal"));
            }
        }
        "audit_log" => {
            if !es_web {
                return Ok(Resultado::Omitido("bitácora del escritorio"));
            }
            if existe {
                return Ok(Resultado::Omitido("bitácora ya existe (solo inserción)"));
            }
        }
        t if fk::TABLAS_TRANSACCIONALES.contains(&t) => {
            if !es_web {
                return Ok(Resultado::Omitido("fila del escritorio: aquí manda el escritorio"));
            }
            if t == "ventas" && !data.contains_key("caja") {
                return Ok(Resultado::Omitido("venta sin caja (servidor sin actualizar)"));
            }
        }
        t if fk::TABLAS_CATALOGO.contains(&t) => {}
        _ => return Ok(Resultado::Omitido("tabla sin política")),
    }

    let refs_todas = obj.get("__refs").and_then(|v| v.as_object());
    let hijos = obj.get("__children").and_then(|v| v.as_object());

    // ── Re-descarga: solo inserta ──
    if ctx.modo == Modo::Replay && existe {
        // Agregado web ya presente: re-colgar sus hijos del padre local
        // (versiones viejas los dejaban con el id del servidor).
        if es_web && !fk::hijos_de(tabla).is_empty() {
            if let Some(h) = hijos {
                // Un cambio local sin subir (p. ej. renglones editados aquí)
                // no se pisa con la lista de hijos del servidor: espera en la
                // bandeja y se reintenta con la versión del servidor tras el
                // push, igual que en el pull normal.
                if hay_cambio_local_pendiente(tx, tabla, uuid).map_err(sql_err)? {
                    return Ok(Resultado::Aparcar {
                        motivo: MOTIVO_PENDIENTE_LOCAL,
                        detalle: "hay un cambio local sin subir (re-descarga)".into(),
                    });
                }
                let pid = local.as_ref().map(|l| l.0).unwrap_or(0);
                if let Some(r) = aplicar_hijos(tx, esq, tabla, pid, h, refs_todas, es_web)? {
                    return Ok(r);
                }
                return Ok(Resultado::Recolgado);
            }
        }
        return Ok(Resultado::Omitido("replay: la fila ya existe"));
    }

    let entrante_upd = texto(data, "updated_at").map(fk::normalizar_marca).unwrap_or_default();
    let borrado_op = operacion.eq_ignore_ascii_case("DELETE");
    let borrado = borrado_op || presente(data, "deleted_at");

    // ── Último-que-escribe-gana ──
    if let Some((_, local_upd)) = &local {
        if entrante_upd.is_empty() {
            // DELETE de una fila que ya no existe en el servidor (sin marca).
            if borrado_op {
                if hay_cambio_local_pendiente(tx, tabla, uuid).map_err(sql_err)? {
                    return Ok(Resultado::Aparcar {
                        motivo: MOTIVO_PENDIENTE_LOCAL,
                        detalle: "hay un cambio local sin subir".into(),
                    });
                }
                return borrado_logico(tx, &cols, tabla, uuid, None, &ctx.ahora);
            }
            return Ok(Resultado::Omitido("cambio sin updated_at"));
        }
        // Estricto: en empate gana el servidor (él también aplica en empate;
        // con '>=' un cambio web del mismo segundo quedaba solo allá).
        let local_norm = local_upd.as_deref().map(fk::normalizar_marca).unwrap_or_default();
        if local_norm > entrante_upd {
            return Ok(Resultado::Omitido("local más nuevo"));
        }
    } else if borrado {
        // Borrada en el servidor y nunca llegó aquí. Una fila de catálogo
        // hecha en la web se crea ya borrada: sus ventas web (p. ej. un
        // producto creado, vendido en espejo y borrado durante un corte de
        // internet) la necesitan para ligar su FK, si no esperarían en la
        // bandeja para siempre y su dinero nunca contaría. Las del escritorio,
        // las transaccionales y los DELETE sin fila no se crean.
        if borrado_op
            || !es_web
            || !fk::TABLAS_CATALOGO.contains(&tabla)
            || !tiene_columna(&cols, "deleted_at")
        {
            return Ok(Resultado::Omitido("borrada en el servidor y no existe aquí"));
        }
    }
    let crear_borrada = !existe && borrado;

    // ── Cambio local sin subir: no se pisa ──
    if hay_cambio_local_pendiente(tx, tabla, uuid).map_err(sql_err)? {
        return Ok(Resultado::Aparcar {
            motivo: MOTIVO_PENDIENTE_LOCAL,
            detalle: "hay un cambio local sin subir".into(),
        });
    }

    // ── Construir la fila con FK traducidas ──
    let refs_fila = refs_todas.and_then(|r| r.get(uuid)).and_then(|v| v.as_object());
    let mut fila = match construir_fila(tx, tabla, data, &cols, refs_fila, existe, es_web, None)? {
        Construida::Fila(f) => f,
        Construida::SinFk(d) => return Ok(Resultado::Aparcar { motivo: MOTIVO_SIN_FK, detalle: d }),
    };
    if borrado_op && tiene_columna(&cols, "deleted_at") && !presente(data, "deleted_at") {
        // DELETE legado con la fila completa: el borrado es la marca.
        let marca = if entrante_upd.is_empty() { ctx.ahora.clone() } else { entrante_upd.clone() };
        fila.retain(|(c, _)| c != "deleted_at");
        fila.push(("deleted_at".into(), rusqlite::types::Value::Text(marca)));
    }
    if existe {
        conservar_registrado_local(tx, tabla, uuid, &mut fila).map_err(sql_err)?;
    }
    if tabla == "productos" {
        ajustar_codigo_producto(tx, uuid, &mut fila, existe, es_web).map_err(sql_err)?;
    }

    if let Err(e) = escribir(tx, tabla, uuid, &fila, existe) {
        // Una fila borrada cuyo nombre/usuario único ya es de otra fila viva
        // de aquí: no se crea (como antes) en vez de atorarse en la bandeja.
        if crear_borrada && e.to_string().contains("UNIQUE") {
            log::info!("sync apply: {tabla} {uuid} borrada en el servidor choca con una fila de aquí ({e}); no se crea");
            return Ok(Resultado::Omitido("borrada en el servidor y choca con una fila de aquí"));
        }
        return Err(sql_err(e));
    }
    if crear_borrada {
        log::info!("sync apply: {tabla} {uuid} hecha en la web llegó ya borrada; se crea borrada");
    }
    if borrado && tiene_columna(&cols, "activo") {
        tx.execute(&format!("UPDATE {tabla} SET activo = 0 WHERE uuid = ?"), [uuid])
            .map_err(sql_err)?;
    }
    // Lo aplicado es ahora la versión común con el servidor (base de la
    // fusión de 3 vías de productos).
    if super::base::usa_base(tabla) {
        super::base::guardar_fila_local(tx, tabla, uuid).map_err(sql_err)?;
    }

    // ── Hijos: siempre del padre LOCAL ──
    if let Some(h) = hijos {
        if !fk::hijos_de(tabla).is_empty() {
            let pid = fk::id_local_de_uuid(tx, tabla, uuid).ok_or("padre no quedó guardado")?;
            if let Some(r) = aplicar_hijos(tx, esq, tabla, pid, h, refs_todas, es_web)? {
                return Ok(r);
            }
        }
    }

    // ── Venta/movimiento web que llegó tarde ──
    if !existe && (tabla == "ventas" || tabla == "movimientos_caja") {
        retimar_si_tardia(tx, tabla, uuid, data, ctx)?;
    }

    Ok(Resultado::Aplicado)
}

/// Borrado lógico de una fila local (nunca DELETE físico).
fn borrado_logico(
    tx: &Transaction,
    cols: &[Columna],
    tabla: &str,
    uuid: &str,
    marca: Option<&str>,
    ahora: &str,
) -> Result<Resultado, String> {
    if !tiene_columna(cols, "deleted_at") {
        return Ok(Resultado::Omitido("tabla sin deleted_at"));
    }
    let marca = marca.unwrap_or(ahora);
    tx.execute(
        &format!("UPDATE {tabla} SET deleted_at = COALESCE(deleted_at, ?) WHERE uuid = ?"),
        rusqlite::params![marca, uuid],
    )
    .map_err(sql_err)?;
    if tiene_columna(cols, "activo") {
        tx.execute(&format!("UPDATE {tabla} SET activo = 0 WHERE uuid = ?"), [uuid])
            .map_err(sql_err)?;
    }
    Ok(Resultado::Aplicado)
}

/// Un `registrado_at` local (venta/movimiento web re-acomodado después de un
/// corte) nunca se borra con el NULL que manda el servidor: se quita la
/// columna del UPDATE. Sin esto la venta volvería a su fecha original, ya
/// cubierta por el corte, y dejaría de contar.
fn conservar_registrado_local(
    tx: &Transaction,
    tabla: &str,
    uuid: &str,
    fila: &mut Vec<(String, rusqlite::types::Value)>,
) -> rusqlite::Result<()> {
    use rusqlite::types::Value as SqlV;
    let entrante_nulo = fila.iter().any(|(c, v)| {
        c == "registrado_at" && match v {
            SqlV::Null => true,
            SqlV::Text(s) => s.trim().is_empty(),
            _ => false,
        }
    });
    if !entrante_nulo {
        return Ok(());
    }
    let local: Option<String> = tx
        .query_row(
            &format!("SELECT registrado_at FROM {tabla} WHERE uuid = ?"),
            [uuid],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .filter(|s| !s.trim().is_empty());
    if local.is_some() {
        fila.retain(|(c, _)| c != "registrado_at");
    }
    Ok(())
}

/// Código de producto ocupado por OTRA fila local (borradas incluidas; el
/// UNIQUE de `productos.codigo` también las cuenta):
/// - producto NUEVO → se guarda como '<código>-W<8 primeros del uuid>' si lo
///   hizo la web, o '<código>-D<8 primeros del uuid>' si lo hizo un
///   escritorio (en el servidor hay productos de escritorio con el mismo
///   código desde hace tiempo; la re-descarga de una PC nueva los traía y uno
///   de cada par se quedaba en la bandeja con 'UNIQUE constraint failed', lo
///   que también atoraba las ventas de ese producto);
/// - producto que ya existe aquí → se conserva su código local.
fn ajustar_codigo_producto(
    tx: &Transaction,
    uuid: &str,
    fila: &mut Vec<(String, rusqlite::types::Value)>,
    existe: bool,
    es_web: bool,
) -> rusqlite::Result<()> {
    use rusqlite::types::Value as SqlV;
    let Some(pos) = fila.iter().position(|(c, _)| c == "codigo") else { return Ok(()) };
    let codigo = match &fila[pos].1 {
        SqlV::Text(s) => s.clone(),
        _ => return Ok(()),
    };
    let ocupado: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM productos WHERE codigo = ?1 AND (uuid IS NULL OR uuid <> ?2))",
        rusqlite::params![codigo, uuid],
        |r| r.get::<_, i64>(0),
    )? != 0;
    if !ocupado {
        return Ok(());
    }
    if existe {
        fila.remove(pos);
        log::warn!(
            "sync apply: productos {uuid}: el código '{codigo}' ya es de otro producto aquí; se conserva el código local"
        );
    } else {
        let nuevo = format!("{codigo}{}", sufijo_codigo_local(uuid, es_web));
        log::warn!(
            "sync apply: productos {uuid}: el código '{codigo}' ya es de otro producto aquí; se guarda como '{nuevo}'"
        );
        fila[pos].1 = SqlV::Text(nuevo);
    }
    Ok(())
}

/// Sufijo que se le pone aquí al código de un producto NUEVO cuyo código ya
/// es de otro producto local: '-W' (hecho en la web) o '-D' (hecho en un
/// escritorio) + los 8 primeros caracteres de su uuid.
fn sufijo_codigo_local(uuid: &str, es_web: bool) -> String {
    let corto: String = uuid.chars().take(8).collect();
    format!("-{}{corto}", if es_web { 'W' } else { 'D' })
}

/// Código con el que el SERVIDOR conoce un producto que aquí se guardó con
/// sufijo ('<código>-W<8 del uuid>' o '<código>-D<8 del uuid>', ver
/// `ajustar_codigo_producto`): `codigo` sin ese sufijo exacto. None si el
/// código no termina exactamente en '-W'/'-D' + los 8 primeros del uuid de
/// ESTA fila (p. ej. los '-W<id>' que puso la migración 011 del servidor son
/// el código real allá y viajan tal cual).
pub fn codigo_sin_sufijo_local<'a>(codigo: &'a str, uuid: &str) -> Option<&'a str> {
    if uuid.is_empty() {
        return None;
    }
    [true, false]
        .into_iter()
        .find_map(|es_web| codigo.strip_suffix(sufijo_codigo_local(uuid, es_web).as_str()))
}

/// Primer usuario administrador activo de este equipo: respaldo para una FK
/// NOT NULL de usuario que apunta a un login solo web ('web-admin-*').
fn usuario_respaldo(tx: &Transaction) -> Option<i64> {
    tx.query_row(
        "SELECT u.id FROM usuarios u JOIN roles r ON r.id = u.rol_id \
          WHERE r.es_admin = 1 AND u.activo = 1 AND u.deleted_at IS NULL \
          ORDER BY u.id LIMIT 1",
        [],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// El servidor rechazó un cambio local de esta fila por tener una versión
/// más nueva ('remoto_mas_nuevo') y esa versión ya se aplicó aquí: el
/// renglón del outbox queda resuelto (si no, se re-enviaría para siempre).
fn resolver_rechazo_remoto(tx: &Transaction, tabla: &str, uuid: &str) -> rusqlite::Result<()> {
    let n = tx.execute(
        "UPDATE sync_outbox SET synced_at = datetime('now'), ultimo_error = ?3 \
          WHERE tabla = ?1 AND uuid = ?2 AND synced_at IS NULL \
            AND COALESCE(ultimo_error, '') LIKE '%remoto_mas_nuevo%'",
        rusqlite::params![tabla, uuid, RESUELTO_SERVIDOR],
    )?;
    if n > 0 {
        log::info!("sync apply: {tabla} {uuid}: rechazo 'remoto_mas_nuevo' resuelto con la versión del servidor");
    }
    Ok(())
}

/// ¿Hay en el outbox un cambio de esta fila que aún no sube? Si el servidor
/// ya lo rechazó por tener una versión más nueva ('remoto_mas_nuevo'), no
/// cuenta: la versión del servidor gana y se aplica.
pub fn hay_cambio_local_pendiente(conn: &Connection, tabla: &str, uuid: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_outbox WHERE tabla = ? AND uuid = ? AND synced_at IS NULL \
           AND COALESCE(ultimo_error, '') NOT LIKE '%remoto_mas_nuevo%')",
        rusqlite::params![tabla, uuid],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n != 0)
}

enum Construida {
    Fila(Vec<(String, rusqlite::types::Value)>),
    SinFk(String),
}

/// Arma (columna, valor) solo con columnas locales, traduciendo FK.
///
/// - Paso: se copia tal cual.
/// - Padre: id LOCAL del padre (`padre`).
/// - Tabla/Polimórfico: con llave en `refs` → uuid → id local (si no existe
///   aquí: NULL si la columna acepta NULL; usuario 'web-admin-*' en NOT NULL
///   de una fila web → primer administrador activo; si no 'sin_fk'). Sin llave: fila
///   existente → se conserva el valor local (la columna no se toca); fila
///   nueva web → NULL o 'sin_fk'; fila nueva del escritorio (restauración) →
///   valor crudo.
#[allow(clippy::too_many_arguments)]
fn construir_fila(
    tx: &Transaction,
    tabla: &str,
    data: &Map<String, Value>,
    cols: &[Columna],
    refs: Option<&Map<String, Value>>,
    existe: bool,
    es_web: bool,
    padre: Option<(&str, i64)>,
) -> Result<Construida, String> {
    use rusqlite::types::Value as SqlV;
    let fks = fk::fks_de(tabla);
    let mut fila: Vec<(String, SqlV)> = Vec::new();

    for col in cols {
        let nombre = col.nombre.as_str();
        if nombre == "id" || nombre == "synced_at" {
            continue;
        }
        if let Some((fk_padre, pid)) = padre {
            if nombre == fk_padre {
                fila.push((nombre.to_string(), SqlV::Integer(pid)));
                continue;
            }
        }
        let kind = fks.iter().find(|(c, _)| *c == nombre).map(|(_, k)| *k);
        match kind {
            Some(RefKind::Padre(_)) => continue, // solo se fija con `padre`
            Some(k @ (RefKind::Tabla(_) | RefKind::Polimorfico(_))) => {
                if let Some(destino) = fk::tabla_destino(k, data) {
                    let sin_fk = |d: String| -> Result<Construida, String> { Ok(Construida::SinFk(d)) };
                    match refs.and_then(|r| r.get(nombre)) {
                        Some(Value::String(ref_uuid)) => {
                            match fk::id_local_de_uuid(tx, destino, ref_uuid) {
                                Some(id) => fila.push((nombre.to_string(), SqlV::Integer(id))),
                                None if !col.notnull => fila.push((nombre.to_string(), SqlV::Null)),
                                None => {
                                    // Login solo web ('Andrew Web'): nunca baja
                                    // a este equipo. Su venta/movimiento en la
                                    // caja de la tienda debe contar: queda a
                                    // nombre del primer administrador activo.
                                    let respaldo = if es_web
                                        && destino == "usuarios"
                                        && ref_uuid.starts_with("web-admin-")
                                    {
                                        usuario_respaldo(tx)
                                    } else {
                                        None
                                    };
                                    let Some(id) = respaldo else {
                                        return sin_fk(format!(
                                            "{tabla}.{nombre}: falta {destino} {ref_uuid}"
                                        ));
                                    };
                                    log::info!(
                                        "sync apply: {tabla}.{nombre}: usuario solo web {ref_uuid} → administrador local {id}"
                                    );
                                    fila.push((nombre.to_string(), SqlV::Integer(id)));
                                }
                            }
                        }
                        Some(Value::Null) => {
                            if !col.notnull {
                                fila.push((nombre.to_string(), SqlV::Null));
                            } else {
                                return sin_fk(format!("{tabla}.{nombre}: referencia nula"));
                            }
                        }
                        Some(_) | None => {
                            if existe {
                                continue; // se conserva el valor local
                            }
                            if es_web {
                                if col.notnull {
                                    return sin_fk(format!("{tabla}.{nombre}: sin referencia"));
                                }
                                fila.push((nombre.to_string(), SqlV::Null));
                            } else if let Some(v) = data.get(nombre) {
                                agregar_valor(&mut fila, col, v);
                            }
                        }
                    }
                    continue;
                }
                // Polimórfico hacia una tabla que no viaja: tal cual.
                if let Some(v) = data.get(nombre) {
                    agregar_valor(&mut fila, col, v);
                }
            }
            Some(RefKind::Paso(_)) | None => {
                if let Some(v) = data.get(nombre) {
                    agregar_valor(&mut fila, col, v);
                }
            }
        }
    }
    Ok(Construida::Fila(fila))
}

/// Agrega una columna normal aplicando las reglas de NULL/NOT NULL.
fn agregar_valor(fila: &mut Vec<(String, rusqlite::types::Value)>, col: &Columna, v: &Value) {
    use rusqlite::types::Value as SqlV;
    let mut sv = json_a_sqlite(v);
    // Marcas de sincronización siempre en el formato local único.
    if col.nombre == "updated_at" || col.nombre == "deleted_at" {
        if let SqlV::Text(s) = &sv {
            let n = fk::normalizar_marca(s);
            sv = if n.is_empty() && col.nombre == "deleted_at" { SqlV::Null } else { SqlV::Text(n) };
        }
    }
    if matches!(sv, SqlV::Null) && col.notnull {
        if col.tiene_default {
            return; // que use el default local
        }
        if col.tipo.contains("TEXT") || col.tipo.is_empty() {
            sv = SqlV::Text(String::new());
        }
    }
    fila.push((col.nombre.clone(), sv));
}

/// INSERT si no existe; UPDATE por uuid si existe (un UPSERT consumiría un
/// id AUTOINCREMENT en cada actualización).
fn escribir(
    tx: &Transaction,
    tabla: &str,
    uuid: &str,
    fila: &[(String, rusqlite::types::Value)],
    existe: bool,
) -> rusqlite::Result<()> {
    if existe {
        let sets: Vec<&(String, rusqlite::types::Value)> =
            fila.iter().filter(|(c, _)| c != "uuid").collect();
        if sets.is_empty() {
            return Ok(());
        }
        let set_sql = sets.iter().map(|(c, _)| format!("{c} = ?")).collect::<Vec<_>>().join(", ");
        let mut params: Vec<&dyn rusqlite::ToSql> =
            sets.iter().map(|(_, v)| v as &dyn rusqlite::ToSql).collect();
        params.push(&uuid);
        tx.execute(
            &format!("UPDATE {tabla} SET {set_sql} WHERE uuid = ?"),
            rusqlite::params_from_iter(params.iter()),
        )?;
    } else {
        let mut cols: Vec<&str> = fila.iter().map(|(c, _)| c.as_str()).collect();
        let mut params: Vec<&dyn rusqlite::ToSql> =
            fila.iter().map(|(_, v)| v as &dyn rusqlite::ToSql).collect();
        if !cols.contains(&"uuid") {
            cols.push("uuid");
            params.push(&uuid);
        }
        let ph = vec!["?"; cols.len()].join(", ");
        tx.execute(
            &format!("INSERT INTO {tabla} ({}) VALUES ({ph})", cols.join(", ")),
            rusqlite::params_from_iter(params.iter()),
        )?;
    }
    Ok(())
}

/// Aplica los hijos de un agregado: fk = id LOCAL del padre, FK traducidas,
/// upsert por uuid y borra los hijos locales de ese padre que ya no vienen.
/// Devuelve `Some(Aparcar)` si un hijo no se puede ligar.
fn aplicar_hijos(
    tx: &Transaction,
    esq: &mut Esquema,
    tabla: &str,
    pid: i64,
    hijos: &Map<String, Value>,
    refs_todas: Option<&Map<String, Value>>,
    es_web: bool,
) -> Result<Option<Resultado>, String> {
    for (tabla_hijo, fk_col) in fk::hijos_de(tabla) {
        let Some(arr) = hijos.get(*tabla_hijo).and_then(|v| v.as_array()) else { continue };
        let hcols = esq.columnas(tx, tabla_hijo).map_err(sql_err)?;
        if hcols.is_empty() {
            continue;
        }
        let mut entrantes: Vec<String> = Vec::with_capacity(arr.len());
        for h in arr {
            let ho = h.as_object().ok_or_else(|| format!("{tabla_hijo}: hijo no es objeto"))?;
            let hu = texto(ho, "uuid")
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("{tabla_hijo}: hijo sin uuid"))?;
            entrantes.push(hu.to_string());
            let hexiste = fk::id_local_de_uuid(tx, tabla_hijo, hu).is_some();
            let hrefs = refs_todas.and_then(|r| r.get(hu)).and_then(|v| v.as_object());
            match construir_fila(tx, tabla_hijo, ho, &hcols, hrefs, hexiste, es_web, Some((fk_col, pid)))? {
                Construida::Fila(f) => escribir(tx, tabla_hijo, hu, &f, hexiste).map_err(sql_err)?,
                Construida::SinFk(d) => {
                    return Ok(Some(Resultado::Aparcar { motivo: MOTIVO_SIN_FK, detalle: d }));
                }
            }
        }
        // Hijos locales de este padre que ya no vienen.
        let mut sql = format!("DELETE FROM {tabla_hijo} WHERE {fk_col} = ?");
        if !entrantes.is_empty() {
            sql.push_str(&format!(
                " AND (uuid IS NULL OR uuid NOT IN ({}))",
                vec!["?"; entrantes.len()].join(", ")
            ));
        }
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&pid];
        for u in &entrantes {
            params.push(u);
        }
        tx.execute(&sql, rusqlite::params_from_iter(params.iter())).map_err(sql_err)?;
    }
    Ok(None)
}

/// Fin del último corte de la caja principal (misma definición de fin que
/// usa la cadena en commands/cortes.rs).
pub(crate) fn fin_ultimo_corte_principal(conn: &Connection) -> Option<String> {
    let f = crate::commands::cortes::FIN_CORTE_SQL;
    conn.query_row(
        &format!(
            "SELECT {f} FROM cortes WHERE deleted_at IS NULL AND caja = 'principal' \
             ORDER BY {f} DESC, id DESC LIMIT 1"
        ),
        [],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .ok()
    .flatten()
    .flatten()
}

/// Venta o movimiento web de la caja principal recién insertado cuya fecha
/// ya quedó cubierta por un corte: se le pone `registrado_at` = max(ahora,
/// fin del último corte + 1 s) para que cuente en el período abierto. Corre
/// con la bandera puesta: no va al outbox ni mueve updated_at.
///
/// Límite: max(acepta_web_desde, origen de la cadena de cortes de ESTA PC).
/// En una PC nueva o de reemplazo la historia web anterior a su primera
/// apertura/corte nunca pasó por este cajón: re-acomodarla al período
/// abierto mostraría un faltante que no existe. Se calcula aquí (no al
/// guardar acepta_web_desde) porque el origen cambia con la primera
/// apertura o el primer corte.
fn retimar_si_tardia(
    tx: &Transaction,
    tabla: &str,
    uuid: &str,
    data: &Map<String, Value>,
    ctx: &Contexto,
) -> Result<(), String> {
    if texto(data, "origen") != Some("web") || texto(data, "caja") != Some("principal") {
        return Ok(());
    }
    if presente(data, "registrado_at") {
        return Ok(());
    }
    let Some(fecha) = texto(data, "fecha").map(fk::normalizar_marca) else { return Ok(()) };
    let Some(desde) = ctx.acepta_web_desde.as_deref().map(fk::normalizar_marca) else { return Ok(()) };
    let Some(fin) = fin_ultimo_corte_principal(tx) else { return Ok(()) };
    let fin = fk::normalizar_marca(&fin);
    if fecha > fin {
        return Ok(()); // cae en el período abierto: cuenta por su fecha
    }
    let origen = fk::normalizar_marca(&crate::commands::cortes::origen_de_la_cadena(tx));
    let limite = std::cmp::max(desde, origen);
    if fecha < limite {
        return Ok(()); // historia previa o anterior a esta caja: nunca se re-acomoda
    }
    let fin_mas_1 = NaiveDateTime::parse_from_str(&fin, "%Y-%m-%d %H:%M:%S")
        .map(|d| (d + ChronoDuration::seconds(1)).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or(fin);
    let ahora = fk::normalizar_marca(&ctx.ahora);
    let registrado = if ahora > fin_mas_1 { ahora } else { fin_mas_1 };
    tx.execute(
        &format!("UPDATE {tabla} SET registrado_at = ? WHERE uuid = ?"),
        rusqlite::params![registrado, uuid],
    )
    .map_err(sql_err)?;
    log::info!("sync apply: {tabla} {uuid} llegó después del corte; registrado_at = {registrado}");
    Ok(())
}

// ─── Bandeja de entrada (sync_inbox) ──────────────────────

/// Espera antes del siguiente reintento: 5 min, 10, 20, … hasta 6 h.
pub fn espera_reintento(intentos: i64) -> ChronoDuration {
    let n = intentos.clamp(1, 10) as u32;
    let minutos = (5i64 << (n - 1)).min(360);
    ChronoDuration::minutes(minutos)
}

fn sumar(ahora: &str, d: ChronoDuration) -> String {
    NaiveDateTime::parse_from_str(&fk::normalizar_marca(ahora), "%Y-%m-%d %H:%M:%S")
        .map(|t| (t + d).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|_| ahora.to_string())
}

/// Guarda (o actualiza) un cambio que no se pudo aplicar.
fn aparcar(
    tx: &Transaction,
    tabla: &str,
    uuid: &str,
    motivo: &str,
    error: &str,
    cambio: &Value,
    ahora: &str,
) -> rusqlite::Result<()> {
    let previos: i64 = tx
        .query_row(
            "SELECT intentos FROM sync_inbox WHERE tabla = ? AND uuid = ?",
            rusqlite::params![tabla, uuid],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let intentos = previos + 1;
    let proximo = if motivo == MOTIVO_PENDIENTE_LOCAL {
        ahora.to_string()
    } else {
        sumar(ahora, espera_reintento(intentos))
    };
    tx.execute(
        "INSERT INTO sync_inbox (tabla, uuid, motivo, error, payload, intentos, proximo_intento, ultimo_intento, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8) \
         ON CONFLICT(tabla, uuid) DO UPDATE SET \
            motivo = excluded.motivo, error = excluded.error, payload = excluded.payload, \
            intentos = excluded.intentos, proximo_intento = excluded.proximo_intento, \
            ultimo_intento = excluded.ultimo_intento",
        rusqlite::params![tabla, uuid, motivo, error, cambio.to_string(), intentos, proximo, ahora],
    )?;
    Ok(())
}

fn quitar_de_bandeja(tx: &Transaction, tabla: &str, uuid: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM sync_inbox WHERE tabla = ? AND uuid = ?",
        rusqlite::params![tabla, uuid],
    )?;
    Ok(())
}

fn json_a_sqlite(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as SqlV;
    match v {
        Value::Null => SqlV::Null,
        Value::Bool(b) => SqlV::Integer(if *b { 1 } else { 0 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() { SqlV::Integer(i) }
            else if let Some(f) = n.as_f64() { SqlV::Real(f) }
            else { SqlV::Null }
        }
        Value::String(s) => SqlV::Text(s.clone()),
        Value::Array(_) | Value::Object(_) => SqlV::Text(v.to_string()),
    }
}

// ─── Arreglo espejo de referencias (tarea única) ──────────

/// Tablas cuyas filas web locales pueden tener FK con ids del servidor.
/// Nunca cortes ni aperturas (historial de caja intocable).
pub const TABLAS_ESPEJO: &[&str] = &[
    "productos", "ventas", "movimientos_caja", "devoluciones", "recepciones",
    "ordenes_pedido", "presupuestos", "transferencias", "audit_log",
];

/// uuids de las filas locales hechas en la web (uuid que no es 32 hex; en
/// audit_log, origen 'WEB').
pub fn uuids_web_locales(conn: &Connection, tabla: &str) -> rusqlite::Result<Vec<String>> {
    let sql = if tabla == "audit_log" {
        "SELECT uuid FROM audit_log WHERE origen = 'WEB' AND uuid IS NOT NULL ORDER BY id".to_string()
    } else {
        format!(
            "SELECT uuid FROM {tabla} WHERE uuid IS NOT NULL \
               AND NOT (length(uuid) = 32 AND uuid NOT GLOB '*[^0-9a-f]*') ORDER BY id"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let v = stmt.query_map([], |r| r.get::<_, String>(0))?.collect();
    v
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct EspejoTabla {
    pub tabla: String,
    pub filas: i64,
    pub celdas_cambiadas: i64,
    pub sin_resolver: i64,
}

/// Aplica las referencias que mandó el servidor (`{uuid: {col: uuid}}`) a
/// filas web locales de `tabla` y de sus hijos. Solo cambia columnas FK
/// (nunca montos, nunca corte_id, nunca inserta) y corre con la bandera
/// puesta: no va al outbox ni mueve updated_at.
pub fn aplicar_refs_espejo(
    conn: &mut Connection,
    tabla: &str,
    refs: &Map<String, Value>,
) -> rusqlite::Result<EspejoTabla> {
    let mut rep = EspejoTabla { tabla: tabla.to_string(), ..Default::default() };
    if !TABLAS_ESPEJO.contains(&tabla) {
        return Ok(rep);
    }
    let tx = conn.transaction()?;
    tx.execute("INSERT OR IGNORE INTO sync_suppress_flag (id) VALUES (1)", [])?;

    for (fila_uuid, mapa) in refs {
        let Some(mapa) = mapa.as_object() else { continue };
        // ¿Es el padre o uno de sus hijos?
        let mut destino_tabla: Option<&str> = None;
        if fk::id_local_de_uuid(&tx, tabla, fila_uuid).is_some() {
            destino_tabla = Some(tabla);
        } else {
            for (hijo, _) in fk::hijos_de(tabla) {
                if fk::id_local_de_uuid(&tx, hijo, fila_uuid).is_some() {
                    destino_tabla = Some(hijo);
                    break;
                }
            }
        }
        let Some(t) = destino_tabla else { continue };
        let fila_json = super::payload::fila_a_json(&tx, t, fila_uuid)?
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        // Solo filas web (los hijos siguen al padre web que se pidió).
        if t == tabla && !fk::es_fila_web(t, fila_uuid, &fila_json) {
            continue;
        }
        rep.filas += 1;
        for (col, kind) in fk::fks_de(t) {
            if *col == "corte_id" {
                continue;
            }
            let Some(destino) = fk::tabla_destino(*kind, &fila_json) else { continue };
            let Some(ref_uuid) = mapa.get(*col).and_then(|v| v.as_str()) else { continue };
            match fk::id_local_de_uuid(&tx, destino, ref_uuid) {
                Some(id) => {
                    let n = tx.execute(
                        &format!("UPDATE {t} SET {col} = ?1 WHERE uuid = ?2 AND ({col} IS NULL OR {col} <> ?1)"),
                        rusqlite::params![id, fila_uuid],
                    )?;
                    rep.celdas_cambiadas += n as i64;
                }
                None => rep.sin_resolver += 1,
            }
        }
    }

    tx.execute("DELETE FROM sync_suppress_flag", [])?;
    tx.commit()?;
    Ok(rep)
}
