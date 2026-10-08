// sync/worker.rs — Loop periódico de sincronización (push + pull).
//
// Arranca como tokio task al inicio de la app. Cada INTERVALO:
//   1. Si sync_state.activo == 1 y hay remote_url/token → procede.
//   2. Empuja hasta BATCH_PUSH filas del outbox (con refs por uuid, proto 2;
//      productos con su `base`). Un renglón solo se marca subido si la fila
//      no cambió durante la llamada. Hasta que termina la reparación de
//      referencias, las FK de las filas hechas en la web no viajan. Un
//      rechazo 'remoto_mas_nuevo' pide la versión del servidor (bandeja).
//      Un producto con código renombrado aquí ('-W'/'-D' + 8 del uuid) sube
//      con el código que tiene en el servidor (sin el sufijo).
//   3. Jala cambios del remoto desde last_pull_cursor mientras haya más
//      (máx. MAX_TANDAS_PULL tandas por ciclo).
//   4. Reintenta la bandeja de cambios recibidos que no se pudieron aplicar.
//   5. Tareas únicas del sync v2 (mapa de ids, reparación, re-descarga).
//
// Un solo ciclo a la vez (`CICLO`): el periódico, el de "Forzar
// sincronización" y la reparación manual nunca corren juntos (dos ciclos
// empujaban los mismos renglones y el servidor podía restar dos veces la
// misma venta del escritorio).
//
// Marcas del servidor: el push recibe `marcas` = la marca con la que el
// servidor guardó cada fila aceptada (con SU reloj: recorta las del futuro).
// Si la fila no cambió durante la llamada, su `updated_at` local pasa a ser
// esa marca (con la bandera de supresión: ni outbox ni otra marca) y la base
// de productos se guarda con ella. Así un reloj adelantado en esta PC ya no
// esconde las ediciones web posteriores (el pull las veía "más viejas").
//
// Nunca bloquea el hilo principal. Errores solo logean (no panic). Nunca se
// escribe un payload en el log (usuarios lleva pin/password_hash).

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use rusqlite::Connection;
use serde_json::{json, Value};

use super::{client::{RemoteClient, PushBody, CambioOut, PROTO}, outbox, state, apply, payload, inbox, tareas, fk, base};

const INTERVALO: Duration = Duration::from_secs(30);
const BATCH_PUSH: i64 = 100;
const BATCH_PULL_LIMITE_DIAS_CLEANUP: i64 = 7;
/// Tandas de pull por ciclo cuando el servidor dice `hay_mas`.
const MAX_TANDAS_PULL: usize = 10;
/// Espera antes de reintentar una tarea única que el servidor aún no
/// soporta o que falló (segundos).
const ESPERA_TAREAS_SEG: i64 = 15 * 60;
/// Espera si el servidor rechazó la reparación (su verificación falló).
const ESPERA_RECHAZO_SEG: i64 = 6 * 60 * 60;

/// Epoch (s) a partir del cual se vuelven a intentar las tareas únicas.
static PROXIMA_TAREA: AtomicI64 = AtomicI64::new(0);

/// Turno de sincronización: un solo ciclo (o reparación manual) a la vez. Se
/// guarda durante toda la llamada, incluidas las esperas de red.
static CICLO: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Cuánto espera "Forzar sincronización" (o la reparación manual) a que
/// termine el ciclo que ya está corriendo antes de rendirse.
pub const ESPERA_TURNO: Duration = Duration::from_secs(30);

/// Mensaje cuando ya hay un ciclo corriendo y no terminó a tiempo.
pub const MSG_EN_CURSO: &str =
    "Ya hay una sincronización en curso; espera unos segundos e intenta de nuevo.";

/// Toma el turno de sincronización: espera hasta `espera` a que termine el
/// ciclo en curso; si no termina, devuelve `MSG_EN_CURSO`.
pub async fn tomar_turno(espera: Duration) -> Result<tokio::sync::MutexGuard<'static, ()>, String> {
    tokio::time::timeout(espera, CICLO.lock())
        .await
        .map_err(|_| MSG_EN_CURSO.to_string())
}

pub fn arrancar(db: Arc<Mutex<Connection>>) {
    tauri::async_runtime::spawn(async move {
        // Instalar el crypto provider de rustls para reqwest
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Jitter inicial para no arrancar exactamente al boot
        tokio::time::sleep(Duration::from_secs(5)).await;

        loop {
            {
                // El periódico espera su turno (sin límite) y lo suelta al
                // terminar, antes de dormir.
                let _turno = CICLO.lock().await;
                if let Err(e) = ciclo(&db).await {
                    log::warn!("sync ciclo falló: {}", e);
                }
            }
            tokio::time::sleep(INTERVALO).await;
        }
    });
}

/// Ejecuta un ciclo de sync (push + pull) inmediatamente, fuera del loop
/// periódico. Usado por el botón "Forzar sincronización" de la UI cuando
/// hay backlog acumulado y el usuario no quiere esperar el próximo intervalo
/// de 30s. Nunca corre junto con el ciclo periódico: si hay uno en curso
/// espera a que termine (hasta ESPERA_TURNO) y después corre el suyo; si no
/// termina a tiempo devuelve MSG_EN_CURSO. Devuelve `Ok(())` aunque haya
/// errores parciales — solo loguea.
pub async fn forzar_ciclo(db: Arc<Mutex<Connection>>) -> Result<(), String> {
    forzar_ciclo_con_espera(db, ESPERA_TURNO).await
}

pub(crate) async fn forzar_ciclo_con_espera(db: Arc<Mutex<Connection>>, espera: Duration) -> Result<(), String> {
    let _turno = tomar_turno(espera).await?;
    ciclo(&db).await
}

async fn ciclo(db: &Arc<Mutex<Connection>>) -> Result<(), String> {
    // 1. Leer configuración de sync
    let cfg = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        state::leer(&conn).map_err(|e| e.to_string())?
    };
    let Some(cfg) = cfg else { return Ok(()); };
    if !cfg.activo { return Ok(()); }
    let (Some(url), Some(token)) = (cfg.remote_url.clone(), cfg.remote_token.clone()) else {
        return Ok(());
    };

    let client = RemoteClient::new(&url, &token)?;

    // 2. Push pendientes
    if let Err(e) = push(db, &client, &cfg).await {
        log::warn!("sync push error: {}", e);
    }

    // 3. Pull nuevos
    if let Err(e) = pull(db, &client, &cfg).await {
        log::warn!("sync pull error: {}", e);
    }

    // 4. Bandeja de entrada
    if let Err(e) = inbox::reintentar(db, &client).await {
        log::warn!("sync inbox error: {}", e);
    }

    // 5. Tareas únicas
    tareas_unicas(db, &client, &cfg).await;

    // 6. Limpieza periódica del outbox
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let _ = outbox::limpiar_antiguos(&conn, BATCH_PULL_LIMITE_DIAS_CLEANUP);
    }

    Ok(())
}

/// Un renglón del outbox tal como se envió en este push.
struct Enviado {
    p: outbox::CambioPendiente,
    /// Payload completo al leerlo (antes de quitar nada). Si al volver del
    /// servidor la fila ya no da el mismo payload, cambió durante la llamada
    /// y el renglón sigue pendiente (aunque el trigger no haya movido
    /// `created_at` por caer en el mismo segundo).
    huella: Option<String>,
    /// `updated_at` local de la fila tal como viajó (se cambia por la marca
    /// del servidor si la fila no cambió durante la llamada).
    marca_enviada: Option<String>,
    /// Solo productos: la fila enviada; si el servidor la acepta, pasa a ser
    /// la base de la fusión de 3 vías.
    base_nueva: Option<Value>,
}

/// Antes de la reparación de referencias, las FK de las filas hechas en la
/// web guardan aquí los ids CRUDOS del servidor (las versiones viejas los
/// copiaban tal cual). Traducirlos como si fueran ids de este equipo cambia
/// la categoría/usuario/producto en el servidor (p. ej. 'Accesorios' →
/// 'Cadenas y Piñones'). Hasta que la reparación termine, esas columnas no
/// viajan (ni en `data` ni en `refs` ni en `base`): el servidor conserva las
/// suyas. Los hijos hechos aquí (uuid de 32 hex) sí llevan sus FK: sus ids
/// son de este equipo y un renglón nuevo las necesita.
fn quitar_fks_de_fila_web(
    tabla: &str,
    uuid: &str,
    data: &mut Value,
    children: Option<&mut Value>,
    refs: Option<&mut Value>,
    base: Option<&mut Value>,
) {
    // (uuid, tabla) de cada fila web del agregado: padre e hijos web.
    let mut filas_web: Vec<(String, String)> = vec![(uuid.to_string(), tabla.to_string())];
    if let Some(o) = data.as_object_mut() {
        fk::quitar_fks_traducibles(tabla, o);
    }
    if let Some(b) = base.and_then(|b| b.as_object_mut()) {
        fk::quitar_fks_traducibles(tabla, b);
    }
    if let Some(ch) = children.and_then(|c| c.as_object_mut()) {
        for (tabla_hijo, filas) in ch.iter_mut() {
            let Some(filas) = filas.as_array_mut() else { continue };
            for f in filas {
                let Some(fo) = f.as_object_mut() else { continue };
                let hu = fo.get("uuid").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if hu.is_empty() || !fk::es_fila_web(tabla_hijo, &hu, fo) {
                    continue;
                }
                fk::quitar_fks_traducibles(tabla_hijo, fo);
                filas_web.push((hu, tabla_hijo.clone()));
            }
        }
    }
    if let Some(r) = refs.and_then(|r| r.as_object_mut()) {
        for (u, t) in filas_web {
            if let Some(m) = r.get_mut(&u).and_then(|m| m.as_object_mut()) {
                fk::quitar_fks_traducibles(&t, m);
            }
        }
    }
}

/// Producto guardado aquí como '<código>-W<8 del uuid>' o '-D<…>' porque su
/// código ya era de otro producto local (apply::ajustar_codigo_producto): en
/// el servidor se llama '<código>'. Viaja SIN el sufijo, en `data` y en
/// `base`. Si no, el servidor veía en cada push un código distinto al suyo:
/// la fusión de 3 vías lo tomaba como cambio del servidor y respondía con un
/// 'merge' (marca nueva y pull de vuelta) en CADA venta de ese producto, para
/// siempre; y un push normal le escribía el sufijo. Un código editado aquí
/// (sin ese sufijo exacto) viaja tal cual.
fn quitar_sufijo_codigo_local(uuid: &str, data: &mut Value, base: Option<&mut Value>) {
    for fila in std::iter::once(data).chain(base) {
        let Some(o) = fila.as_object_mut() else { continue };
        let servidor = o
            .get("codigo")
            .and_then(|c| c.as_str())
            .and_then(|c| apply::codigo_sin_sufijo_local(c, uuid))
            .map(String::from);
        if let Some(c) = servidor {
            o.insert("codigo".into(), Value::String(c));
        }
    }
}

/// Tras un rechazo 'remoto_mas_nuevo', ¿tiene caso pedir la versión del
/// servidor? Solo si aquí se aplicaría: catálogo (salvo productos: ahí el
/// servidor fusiona y la existencia del escritorio no se debe perder) y filas
/// hechas en la web de tablas transaccionales. Las del escritorio en tablas
/// de dinero/transaccionales nunca se modifican desde el servidor: pedirlas
/// solo repetiría una llamada inútil en cada ciclo.
fn pedir_tras_rechazo(tabla: &str, uuid: &str) -> bool {
    if tabla == "productos" {
        return false;
    }
    if fk::TABLAS_CATALOGO.contains(&tabla) {
        return true;
    }
    fk::TABLAS_TRANSACCIONALES.contains(&tabla) && !fk::es_uuid_escritorio(uuid)
}

/// La fila sigue tal como viajó en el push: su `updated_at` pasa a ser la
/// marca con la que el servidor la guardó. Solo si la marca local sigue
/// siendo la que viajó. Con la bandera de supresión (ni outbox ni otra
/// marca); corre dentro de la transacción del push. Devuelve true si la
/// cambió.
fn adoptar_marca_servidor(
    conn: &Connection,
    tabla: &str,
    uuid: &str,
    enviada: &str,
    marca: &str,
) -> rusqlite::Result<bool> {
    // Nombre de tabla del outbox (se pega en SQL): solo tablas sincronizadas.
    if enviada == marca || !fk::es_tabla_sync(tabla) || fk::es_tabla_hija(tabla) {
        return Ok(false);
    }
    conn.execute("INSERT OR IGNORE INTO sync_suppress_flag (id) VALUES (1)", [])?;
    let r = conn.execute(
        &format!("UPDATE {tabla} SET updated_at = ?1 WHERE uuid = ?2 AND updated_at = ?3"),
        rusqlite::params![marca, uuid, enviada],
    );
    conn.execute("DELETE FROM sync_suppress_flag", [])?;
    Ok(r? > 0)
}

pub(super) async fn push(
    db: &Arc<Mutex<Connection>>,
    client: &RemoteClient,
    cfg: &state::SyncConfig,
) -> Result<(), String> {
    // Leer pendientes + construir payloads (bajo lock corto)
    let (enviados, cambios_out) = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let pendientes = outbox::pendientes(&conn, BATCH_PUSH).map_err(|e| e.to_string())?;
        if pendientes.is_empty() { return Ok(()); }
        let reparadas = state::leer_tareas(&conn)
            .map(|t| t.referencias_reparadas_at.is_some())
            .unwrap_or(false);

        let mut cambios = Vec::with_capacity(pendientes.len());
        let mut enviados: Vec<Enviado> = Vec::with_capacity(pendientes.len());
        let mut fuera_de_sync: Vec<i64> = Vec::new();
        for p in pendientes {
            // stock_sucursal salió del sync (nadie la lee): se descarta.
            if p.tabla == "stock_sucursal" {
                fuera_de_sync.push(p.id);
                continue;
            }
            let mut huella = None;
            let mut marca_enviada = None;
            let mut base_nueva = None;
            let (data, children, refs, base) = if p.operacion == "DELETE" {
                (json!({ "uuid": p.uuid, "deleted": true }), None, None, None)
            } else {
                match payload::construir_payload(&conn, &p.tabla, &p.uuid) {
                    Ok(Some(mut v)) => {
                        huella = Some(v.to_string());
                        marca_enviada = v.get("updated_at").and_then(|x| x.as_str()).map(String::from);
                        let (mut children, mut refs) = if let Some(obj) = v.as_object_mut() {
                            (obj.remove("__children"), obj.remove("__refs"))
                        } else { (None, None) };
                        let mut base = None;
                        if base::usa_base(&p.tabla) {
                            // La base guarda la fila LOCAL (con el sufijo, si
                            // lo hay): se le quita al enviarla, igual que a
                            // `data`.
                            base_nueva = Some(v.clone());
                            base = base::leer(&conn, &p.tabla, &p.uuid).unwrap_or(None);
                        }
                        if p.tabla == "productos" {
                            quitar_sufijo_codigo_local(&p.uuid, &mut v, base.as_mut());
                        }
                        let es_web = v.as_object().map(|o| fk::es_fila_web(&p.tabla, &p.uuid, o)).unwrap_or(false);
                        if !reparadas && es_web {
                            quitar_fks_de_fila_web(
                                &p.tabla, &p.uuid, &mut v, children.as_mut(), refs.as_mut(), base.as_mut(),
                            );
                        }
                        (v, children, refs, base)
                    }
                    Ok(None) => continue,  // fila ya no existe — nada que enviar
                    Err(e) => {
                        let _ = outbox::marcar_error_si(&conn, p.id, &p.created_at, &format!("payload: {}", e));
                        continue;
                    }
                }
            };
            cambios.push(CambioOut {
                tabla: p.tabla.clone(),
                operacion: p.operacion.clone(),
                data,
                children,
                refs,
                base,
            });
            enviados.push(Enviado { p, huella, marca_enviada, base_nueva });
        }
        if !fuera_de_sync.is_empty() {
            let _ = outbox::descartar(&conn, &fuera_de_sync);
        }
        (enviados, cambios)
    };

    if cambios_out.is_empty() { return Ok(()); }

    let body = PushBody {
        device_uuid: cfg.device_uuid.clone(),
        sucursal_id: cfg.sucursal_id,
        proto: PROTO,
        cambios: cambios_out,
    };

    match client.push(&body).await {
        Ok(resp) => {
            let aceptados_set: HashSet<&String> = resp.aceptados.iter().collect();
            let marcas = resp.marcas();
            let mut conn = db.lock().map_err(|e| e.to_string())?;
            let ahora = apply::ahora_local();
            // Todo lo que deja la respuesta (marcas, bases, outbox, bandeja)
            // en UNA transacción: si algo truena no queda a medias y la
            // bandera de supresión se va con el rollback.
            let tx = conn.transaction().map_err(|e| e.to_string())?;
            let mut ok = 0usize;
            let mut cambiaron = 0usize;
            let mut remarcadas = 0usize;
            for e in enviados.iter().filter(|e| aceptados_set.contains(&e.p.uuid)) {
                // Marca con la que el servidor guardó la fila (servidores
                // viejos: ninguna → se conserva la local, como antes).
                let marca = marcas.get(&e.p.uuid);
                // ¿Cambió la fila mientras viajaba? Entonces sigue pendiente.
                let igual = e.p.operacion == "DELETE"
                    || payload::construir_payload(&tx, &e.p.tabla, &e.p.uuid)
                        .ok()
                        .flatten()
                        .map(|v| v.to_string())
                        == e.huella;
                // Lo que el servidor aceptó es la nueva versión común, con la
                // marca con la que él la guardó (mismo reloj que sus otras
                // escrituras).
                if let Some(b) = &e.base_nueva {
                    let mut b = b.clone();
                    if let (Some(m), Some(o)) = (marca, b.as_object_mut()) {
                        o.insert("updated_at".into(), Value::String(m.clone()));
                    }
                    if let Err(err) = base::guardar(&tx, &e.p.tabla, &e.p.uuid, &b) {
                        log::warn!("sync push: no se guardó la base de {} {}: {}", e.p.tabla, e.p.uuid, err);
                    }
                }
                // La fila local sigue siendo lo que viajó: toma la marca del
                // servidor. Si cambió en el camino, conserva la suya (el
                // cambio nuevo sube en el siguiente ciclo).
                if igual && e.p.operacion != "DELETE" {
                    if let (Some(m), Some(enviada)) = (marca, e.marca_enviada.as_deref()) {
                        match adoptar_marca_servidor(&tx, &e.p.tabla, &e.p.uuid, enviada, m) {
                            Ok(true) => remarcadas += 1,
                            Ok(false) => {}
                            Err(err) => log::warn!(
                                "sync push: no se pudo poner la marca del servidor a {} {}: {}",
                                e.p.tabla, e.p.uuid, err
                            ),
                        }
                    }
                }
                if igual && outbox::marcar_sincronizado_si(&tx, e.p.id, &e.p.created_at).map_err(|e| e.to_string())? {
                    ok += 1;
                } else {
                    cambiaron += 1;
                }
            }

            for r in &resp.rechazados {
                let Some(e) = enviados.iter().find(|e| e.p.uuid == r.uuid) else { continue };
                let marcado = outbox::marcar_error_si(&tx, e.p.id, &e.p.created_at, &r.motivo).unwrap_or(false);
                // El servidor tiene una versión más nueva: se pide y se
                // aplica aquí.
                if marcado && r.motivo.contains("remoto_mas_nuevo") && pedir_tras_rechazo(&e.p.tabla, &e.p.uuid) {
                    if let Err(err) = inbox::pedir_version_servidor(&tx, &e.p.tabla, &e.p.uuid, &ahora) {
                        log::warn!("sync push: no se pudo anotar {} {} en la bandeja: {}", e.p.tabla, e.p.uuid, err);
                    }
                }
            }
            state::actualizar_push_at(&tx).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            log::info!(
                "sync push: {} aceptados, {} rechazados{}{}{}",
                ok,
                resp.rechazados.len(),
                if cambiaron == 0 { String::new() }
                else { format!(", {} cambiaron durante el envío (se re-envían)", cambiaron) },
                if remarcadas == 0 { String::new() }
                else { format!(", {} con la marca del servidor", remarcadas) },
                if resp.faltantes.is_empty() { String::new() }
                else { format!(", {} referencias que el servidor aún no tiene", resp.faltantes.len()) }
            );
        }
        Err(e) => {
            let conn = db.lock().map_err(|e| e.to_string())?;
            for env in &enviados {
                let _ = outbox::marcar_error_si(&conn, env.p.id, &env.p.created_at, &e);
            }
            return Err(e);
        }
    }
    Ok(())
}

pub(super) async fn pull(
    db: &Arc<Mutex<Connection>>,
    client: &RemoteClient,
    cfg: &state::SyncConfig,
) -> Result<(), String> {
    for _ in 0..MAX_TANDAS_PULL {
        let cursor = {
            let conn = db.lock().map_err(|e| e.to_string())?;
            state::leer(&conn).map_err(|e| e.to_string())?
                .and_then(|c| c.last_pull_cursor)
                .unwrap_or_default()
        };
        let resp = client.pull(&cursor, cfg.sucursal_id, &cfg.device_uuid).await?;

        let mut conn = db.lock().map_err(|e| e.to_string())?;
        // Antes de aplicar: el límite para re-acomodar ventas web tardías.
        state::ajustar_desde_respuesta(&conn, resp.caja_v2_desde.as_deref());
        if !resp.cambios.is_empty() {
            // Errores por cambio quedan en la bandeja; un error aquí es de
            // todo el lote (p. ej. la BD bloqueada) y NO avanza el cursor.
            let r = apply::aplicar_lote(&mut conn, &resp.cambios).map_err(|e| e.to_string())?;
            log::info!(
                "sync pull: {} aplicados, {} omitidos, {} a la bandeja, cursor={}",
                r.aplicados, r.omitidos, r.aparcados, resp.next_cursor
            );
        }
        state::actualizar_pull(&conn, &resp.next_cursor).map_err(|e| e.to_string())?;
        if !resp.hay_mas || resp.next_cursor == cursor {
            break;
        }
    }
    Ok(())
}

fn epoch_ahora() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Tareas únicas del sync v2, en orden: mapa de ids + reparación del
/// servidor + arreglo espejo, y después la re-descarga. Si el servidor aún
/// no las soporta (404) se espera ESPERA_TAREAS_SEG sin llenar el log.
async fn tareas_unicas(db: &Arc<Mutex<Connection>>, client: &RemoteClient, cfg: &state::SyncConfig) {
    if epoch_ahora() < PROXIMA_TAREA.load(Ordering::Relaxed) {
        return;
    }
    let t = {
        let Ok(conn) = db.lock() else { return };
        match state::leer_tareas(&conn) {
            Ok(t) => t,
            Err(_) => return,
        }
    };

    if t.referencias_reparadas_at.is_none() {
        match tareas::reparar_referencias(db, client, &cfg.device_uuid, false).await {
            Ok(r) => log::info!(
                "sync: referencias reparadas (espejo: {} celdas corregidas)",
                r.espejo.celdas_cambiadas
            ),
            Err(tareas::ErrorTarea::NoSoportado) => {
                log::debug!("sync: el servidor aún no soporta la reparación de referencias");
                PROXIMA_TAREA.store(epoch_ahora() + ESPERA_TAREAS_SEG, Ordering::Relaxed);
            }
            Err(tareas::ErrorTarea::Rechazado(_)) => {
                // La verificación del servidor falló: no tiene caso repetir
                // la reparación cada 15 min; queda el botón manual.
                log::warn!("sync: el servidor rechazó la reparación de referencias (verificación); se reintenta en 6 h");
                PROXIMA_TAREA.store(epoch_ahora() + ESPERA_RECHAZO_SEG, Ordering::Relaxed);
            }
            Err(tareas::ErrorTarea::Fallo(e)) => {
                log::warn!("sync: reparación de referencias falló: {}", e);
                PROXIMA_TAREA.store(epoch_ahora() + ESPERA_TAREAS_SEG, Ordering::Relaxed);
            }
        }
        return;
    }

    if t.replay_pendiente() {
        if let Err(e) = tareas::correr_replay(db, client, cfg).await {
            log::warn!("sync replay: {}", e);
            PROXIMA_TAREA.store(epoch_ahora() + ESPERA_TAREAS_SEG, Ordering::Relaxed);
        }
    }
}
