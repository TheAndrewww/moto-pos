// Pruebas de la ronda 3 del sync v2 (SPEC3 S) contra Postgres (copia fiel
// de producción). Submódulo de sync_v2_pg.rs: usa sus ayudas (ctx, cambio,
// body, producto_escritorio…) y las de la ronda 2. Igual que allá, solo
// corren con SYNC_PG_URL.
//
//   s1  push de productos con `base` idempotente: reenvío idéntico tras una
//       combinación, respuesta perdida tras un LWW + venta web, dos ciclos
//       encimados; poda de huellas viejas
//   s2  "¿el servidor cambió desde la base?" por contenido: reloj del
//       escritorio adelantado 5 min / 30 s, escritura web en el mismo
//       segundo; sin cambio del servidor → push normal
//   s3  `marcas` en la respuesta del push
//   s4  sync_fk_celda_web: el push quita la marca cuando el escritorio
//       reescribe la celda con otro valor; el pull viejo no manda la celda

use super::ronda2::{cursor_de, fila, subir_mapa_tienda, ultimo_origen, uuid_categoria};
use super::*;

/// Hora local del servidor + `segundos` (negativo = antes), con el formato de
/// updated_at. Sirve para simular el reloj del escritorio.
pub(super) async fn marca_rel(p: &PgPool, segundos: i64) -> String {
    sqlx::query_scalar(
        "SELECT to_char((now() AT TIME ZONE 'America/Mexico_City') + make_interval(secs => $1), \
                        'YYYY-MM-DD HH24:MI:SS')",
    )
    .bind(segundos as f64)
    .fetch_one(p)
    .await
    .unwrap()
}

pub(super) fn refs_nulas(pu: &str) -> Option<Value> {
    let mut m = Map::new();
    m.insert(pu.to_string(), json!({"categoria_id": null, "proveedor_id": null}));
    Some(Value::Object(m))
}

pub(super) fn con_base(data: &Value, base: &Value) -> CambioIn {
    let pu = data["uuid"].as_str().unwrap().to_string();
    let mut ch = cambio("productos", data.clone(), None, refs_nulas(&pu));
    ch.base = Some(base.clone());
    ch
}

pub(super) async fn precio_stock(p: &PgPool, pu: &str) -> String {
    texto(p, &format!("SELECT precio_venta || '|' || stock_actual FROM productos WHERE uuid = '{pu}'")).await.unwrap()
}

pub(super) async fn guardado_ts(p: &PgPool, tabla: &str, uuid: &str) -> String {
    texto(p, &format!("SELECT updated_at::text FROM {tabla} WHERE uuid = '{uuid}'")).await.unwrap()
}

pub(super) async fn cursores_de(p: &PgPool, uuid: &str) -> i64 {
    entero(p, &format!("SELECT count(*) FROM sync_cursor WHERE uuid = '{uuid}'")).await
}

pub(super) async fn huellas_de(p: &PgPool, uuid: &str) -> i64 {
    entero(p, &format!("SELECT count(*) FROM sync_push_aplicado WHERE device_uuid = '{DEV}' AND uuid = '{uuid}'")).await
}

/// Venta web del producto: stock − 1 (y precio nuevo si viene), con la hora
/// del servidor (o la marca dada) y su cursor.
pub(super) async fn venta_web(p: &PgPool, pu: &str, precio: Option<f64>, marca: Option<&str>) {
    let ts = match marca {
        Some(m) => m.to_string(),
        None => ahora(p).await,
    };
    sqlx::query(
        "UPDATE productos SET stock_actual = stock_actual - 1, precio_venta = COALESCE($2::numeric, precio_venta), \
                updated_at = $3 WHERE uuid = $1",
    )
    .bind(pu)
    .bind(precio)
    .bind(&ts)
    .execute(p)
    .await
    .unwrap();
    cursor_de(p, "productos", pu, "web-pos").await;
}

/// Producto nuevo del escritorio (precio 25, stock 10) ya empujado con v2;
/// devuelve (fila empujada, respuesta).
pub(super) async fn producto_empujado(p: &PgPool, id: i64, pu: &str, marca: &str) -> (Value, PushResponse) {
    let mut d0 = producto_escritorio(id, pu, marca);
    d0["precio_venta"] = json!(25.0);
    d0["stock_actual"] = json!(10.0);
    let r = procesar_push(p, &body(Some(2), vec![cambio("productos", d0.clone(), None, refs_nulas(pu))])).await.unwrap();
    assert_eq!(r.aceptados, vec![pu.to_string()], "{:?}", r.rechazados);
    (d0, r)
}

// ─────────────────────────────────────────────────────────────────────────
// s1) Push de productos con `base` idempotente (huella en sync_push_aplicado).
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn s1_reenvio_identico_tras_combinacion_no_resta_dos_veces() {
    let Some(c) = ctx("s1").await else { return };
    let p = &c.pool;
    let pu = hex32();
    let (d0, _) = producto_empujado(p, 7301, &pu, &ahora(p).await).await;

    // Venta web (10 → 9) y venta del escritorio con base = d0 → 8 por combinación.
    venta_web(p, &pu, None, None).await;
    let mut d1 = d0.clone();
    d1["stock_actual"] = json!(9.0);
    d1["updated_at"] = json!(marca_rel(p, 1).await);
    let r = procesar_push(p, &body(Some(2), vec![con_base(&d1, &d0)])).await.unwrap();
    assert_eq!(r.aceptados, vec![pu.clone()], "{:?}", r.rechazados);
    assert_eq!(precio_stock(p, &pu).await, "25.00|8.00");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some("merge"));
    assert_eq!(huellas_de(p, &pu).await, 1);
    let marca_merge = guardado_ts(p, "productos", &pu).await;
    assert_eq!(r.marcas.get(&pu), Some(&marca_merge));

    // La respuesta se perdió: el escritorio reenvía EXACTAMENTE lo mismo
    // (misma fila, misma base). Antes: 8 + (9 − 10) = 7.
    let n_cursores = cursores_de(p, &pu).await;
    let r = procesar_push(p, &body(Some(2), vec![con_base(&d1, &d0)])).await.unwrap();
    assert_eq!(r.aceptados, vec![pu.clone()], "{:?}", r.rechazados);
    assert_eq!(precio_stock(p, &pu).await, "25.00|8.00", "sin segunda resta");
    assert_eq!(cursores_de(p, &pu).await, n_cursores, "sin escritura: sin cursor");
    assert_eq!(guardado_ts(p, "productos", &pu).await, marca_merge);
    assert_eq!(r.marcas.get(&pu), Some(&marca_merge), "la marca de lo guardado");
    assert_eq!(huellas_de(p, &pu).await, 1);
    // El mismo cambio con el `id` local distinto (o synced_at) es el mismo envío.
    let mut d1b = d1.clone();
    d1b["id"] = json!(999_999);
    d1b["synced_at"] = json!("2026-10-07 00:00:00");
    procesar_push(p, &body(Some(2), vec![con_base(&d1b, &d0)])).await.unwrap();
    assert_eq!(precio_stock(p, &pu).await, "25.00|8.00");

    // Dos ciclos encimados (sync forzado + periódico) empujan lo mismo a la
    // vez después de otra venta web (9 → … 8 − 1 = 7): una sola resta. Para
    // que de verdad coincidan, otra transacción tiene la fila bloqueada
    // mientras los dos arrancan; al soltarla, el segundo ve la huella del
    // primero.
    venta_web(p, &pu, None, None).await;
    let mut d2 = d1.clone();
    d2["stock_actual"] = json!(8.0);
    d2["updated_at"] = json!(marca_rel(p, 2).await);
    let n_cursores = cursores_de(p, &pu).await;
    let mut candado = p.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM productos WHERE uuid = $1 FOR UPDATE").bind(&pu).execute(&mut *candado).await.unwrap();
    let lanzar = || {
        let (pool, b) = (p.clone(), body(Some(2), vec![con_base(&d2, &d1)]));
        tokio::spawn(async move { procesar_push(&pool, &b).await })
    };
    let (h1, h2) = (lanzar(), lanzar());
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    candado.commit().await.unwrap();
    let (r1, r2) = (h1.await.unwrap().unwrap(), h2.await.unwrap().unwrap());
    assert_eq!(r1.aceptados, vec![pu.clone()], "{:?}", r1.rechazados);
    assert_eq!(r2.aceptados, vec![pu.clone()], "{:?}", r2.rechazados);
    assert_eq!(precio_stock(p, &pu).await, "25.00|6.00", "7 + (8 − 9) una sola vez");
    assert_eq!(cursores_de(p, &pu).await, n_cursores + 1);
    assert_eq!(r1.marcas.get(&pu), r2.marcas.get(&pu));
    assert_eq!(huellas_de(p, &pu).await, 2);
    c.cerrar().await;
}

#[tokio::test]
async fn s1_lww_respuesta_perdida_y_venta_web_no_resta_dos_veces() {
    let Some(c) = ctx("s1b").await else { return };
    let p = &c.pool;
    let pu = hex32();
    let (d0, _) = producto_empujado(p, 7302, &pu, &ahora(p).await).await;

    // El escritorio vende 1 con base = d0; lo último de aquí es suyo → LWW
    // normal (10 → 9). La huella queda anotada.
    let mut d1 = d0.clone();
    d1["stock_actual"] = json!(9.0);
    d1["updated_at"] = json!(marca_rel(p, 1).await);
    let r = procesar_push(p, &body(Some(2), vec![con_base(&d1, &d0)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(precio_stock(p, &pu).await, "25.00|9.00");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some(DEV));
    assert_eq!(huellas_de(p, &pu).await, 1);

    // La respuesta se perdió y la web vende 1 antes del reintento (9 → 8).
    venta_web(p, &pu, Some(30.0), None).await;
    let marca_web = guardado_ts(p, "productos", &pu).await;
    let n_cursores = cursores_de(p, &pu).await;
    // Reintento idéntico: lo último ya es de la web, pero el envío ya se
    // aplicó → no-op. Antes: combinación 8 + (9 − 10) = 7.
    let r = procesar_push(p, &body(Some(2), vec![con_base(&d1, &d0)])).await.unwrap();
    assert_eq!(r.aceptados, vec![pu.clone()], "{:?}", r.rechazados);
    assert_eq!(precio_stock(p, &pu).await, "30.00|8.00");
    assert_eq!(cursores_de(p, &pu).await, n_cursores);
    assert_eq!(r.marcas.get(&pu), Some(&marca_web));

    // El escritorio ya sabe que se aceptó (base = d1) y vende otra: 8 − 1 = 7,
    // y el precio de la web se queda.
    let mut d2 = d1.clone();
    d2["stock_actual"] = json!(8.0);
    d2["updated_at"] = json!(marca_rel(p, 2).await);
    let r = procesar_push(p, &body(Some(2), vec![con_base(&d2, &d1)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(precio_stock(p, &pu).await, "30.00|7.00");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some("merge"));
    c.cerrar().await;
}

#[tokio::test]
async fn s1_poda_de_huellas_viejas() {
    let Some(c) = ctx("s1c").await else { return };
    let p = &c.pool;
    sqlx::raw_sql(&format!(
        "INSERT INTO sync_push_aplicado (device_uuid, tabla, uuid, huella, created_at) VALUES \
           ('{DEV}', 'productos', 'u-vieja', 'h', now() - interval '31 days'), \
           ('{DEV}', 'productos', 'u-reciente', 'h', now() - interval '29 days')"
    ))
    .execute(p)
    .await
    .unwrap();
    let cu = hex32();
    let cli = json!({"id": 601, "uuid": cu, "nombre": "PODA", "activo": 1, "updated_at": ahora(p).await, "deleted_at": null});
    let r = procesar_push(p, &body(Some(2), vec![cambio("clientes", cli, None, None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(entero(p, "SELECT count(*) FROM sync_push_aplicado WHERE uuid = 'u-vieja'").await, 0);
    assert_eq!(entero(p, "SELECT count(*) FROM sync_push_aplicado WHERE uuid = 'u-reciente'").await, 1);
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// s2) "¿El servidor cambió desde la base?" por contenido, no por marcas de
//     dos relojes.
// ─────────────────────────────────────────────────────────────────────────

/// Escritorio con el reloj `adelanto` segundos adelantado: empuja el producto
/// (el servidor recorta su marca), la web vende 1 y sube el precio a 35 con
/// la hora del servidor (dentro de la ventana del adelanto), y el escritorio
/// vende 1 con base = la fila que empujó. Esperado: 8 / 35 por combinación.
async fn caso_reloj(p: &PgPool, id: i64, adelanto: i64, mismo_segundo: bool, base_con_marca: bool) {
    let pu = hex32();
    let e0 = marca_rel(p, adelanto).await;
    let (d0, r) = producto_empujado(p, id, &pu, &e0).await;
    let guardada = guardado_ts(p, "productos", &pu).await;
    assert_eq!(r.marcas.get(&pu), Some(&guardada), "marca = lo guardado aquí");
    if adelanto > 0 {
        assert!(guardada < e0, "marca del futuro recortada: {guardada} vs {e0}");
    }
    // La web (reloj del servidor): en el mismo segundo que el push, o ahora.
    let tw = if mismo_segundo { guardada.clone() } else { ahora(p).await };
    venta_web(p, &pu, Some(35.0), Some(&tw)).await;
    // El escritorio vende 1 dos minutos después (por su reloj).
    let mut d1 = d0.clone();
    d1["stock_actual"] = json!(9.0);
    d1["updated_at"] = json!(marca_rel(p, adelanto + 120).await);
    // Base: la fila que empujó; un escritorio viejo la guarda con su marca
    // sin recortar, uno nuevo con la marca que devolvió el servidor.
    let mut base = d0.clone();
    if base_con_marca {
        base["updated_at"] = json!(guardada);
    }
    let r = procesar_push(p, &body(Some(2), vec![con_base(&d1, &base)])).await.unwrap();
    assert_eq!(r.aceptados, vec![pu.clone()], "{:?}", r.rechazados);
    assert_eq!(
        precio_stock(p, &pu).await,
        "35.00|8.00",
        "adelanto {adelanto} s, mismo segundo {mismo_segundo}: venta y precio de la web se conservan"
    );
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some("merge"));
    assert_eq!(r.marcas.get(&pu), Some(&guardado_ts(p, "productos", &pu).await));
}

#[tokio::test]
async fn s2_reloj_adelantado_y_mismo_segundo_se_combinan() {
    let Some(c) = ctx("s2").await else { return };
    let p = &c.pool;
    // Antes: 9 / 25 (LWW aplicaba la fila completa del escritorio).
    caso_reloj(p, 7311, 300, false, false).await;
    caso_reloj(p, 7312, 30, false, false).await;
    caso_reloj(p, 7313, 0, true, false).await;
    // Escritorio nuevo (base con la marca del servidor): igual.
    caso_reloj(p, 7314, 300, false, true).await;
    caso_reloj(p, 7315, 0, true, true).await;

    // La web SOLO cambia el precio (sin venta) en el mismo segundo del push;
    // el escritorio vende 1: el stock resultante es el suyo, pero el precio
    // de la web es un cambio propio del servidor → combinación, no LWW.
    let pu = hex32();
    let (d0, _) = producto_empujado(p, 7316, &pu, &ahora(p).await).await;
    let guardada = guardado_ts(p, "productos", &pu).await;
    sqlx::query("UPDATE productos SET precio_venta = 35, updated_at = $2 WHERE uuid = $1")
        .bind(&pu)
        .bind(&guardada)
        .execute(p)
        .await
        .unwrap();
    cursor_de(p, "productos", &pu, "web-pos").await;
    let mut d1 = d0.clone();
    d1["stock_actual"] = json!(9.0);
    d1["updated_at"] = json!(marca_rel(p, 60).await);
    let r = procesar_push(p, &body(Some(2), vec![con_base(&d1, &d0)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(precio_stock(p, &pu).await, "35.00|9.00", "el precio de la web no se pierde");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some("merge"));
    c.cerrar().await;
}

#[tokio::test]
async fn s2_sin_cambio_del_servidor_es_push_normal() {
    let Some(c) = ctx("s2b").await else { return };
    let p = &c.pool;
    let pu = hex32();
    let (d0, _) = producto_empujado(p, 7321, &pu, &ahora(p).await).await;
    // La web guarda el formulario sin cambiar nada: solo marca y cursor nuevos.
    let marca_web = marca_rel(p, 5).await;
    sqlx::query("UPDATE productos SET updated_at = $2 WHERE uuid = $1")
        .bind(&pu)
        .bind(&marca_web)
        .execute(p)
        .await
        .unwrap();
    cursor_de(p, "productos", &pu, "web-pos").await;
    // El escritorio (reloj 10 min atrasado) vende 1 con base = d0: el
    // servidor no tuvo cambios propios → push normal: su fila y su cursor.
    // Antes: remoto_mas_nuevo → solo stock. La marca guardada no retrocede
    // (SPEC4 S3): se queda la de la web, y `marcas` la devuelve.
    let mut d1 = d0.clone();
    d1["stock_actual"] = json!(9.0);
    d1["nombre"] = json!("Renombrado en el escritorio");
    let atrasada = marca_rel(p, -600).await;
    d1["updated_at"] = json!(atrasada);
    let r = procesar_push(p, &body(Some(2), vec![con_base(&d1, &d0)])).await.unwrap();
    assert_eq!(r.aceptados, vec![pu.clone()], "{:?}", r.rechazados);
    assert_eq!(precio_stock(p, &pu).await, "25.00|9.00");
    assert_eq!(texto(p, &format!("SELECT nombre FROM productos WHERE uuid = '{pu}'")).await.as_deref(), Some("Renombrado en el escritorio"));
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some(DEV));
    assert_eq!(guardado_ts(p, "productos", &pu).await, marca_web, "max(entrante recortada, guardada)");
    assert_eq!(r.marcas.get(&pu), Some(&marca_web));
    // También queda su huella: reenviarlo no cambia nada.
    let n = cursores_de(p, &pu).await;
    procesar_push(p, &body(Some(2), vec![con_base(&d1, &d0)])).await.unwrap();
    assert_eq!(cursores_de(p, &pu).await, n);

    // Otra vez sin cambio propio del servidor (la web solo vuelve a guardar),
    // ahora con el reloj del escritorio 10 min ADELANTADO: push normal con su
    // marca recortada a la hora de aquí.
    sqlx::query("UPDATE productos SET updated_at = $2 WHERE uuid = $1")
        .bind(&pu)
        .bind(ahora(p).await)
        .execute(p)
        .await
        .unwrap();
    cursor_de(p, "productos", &pu, "web-pos").await;
    let mut d2 = d1.clone();
    d2["stock_actual"] = json!(8.0);
    let futura = marca_rel(p, 600).await;
    d2["updated_at"] = json!(futura);
    let r = procesar_push(p, &body(Some(2), vec![con_base(&d2, &d1)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(precio_stock(p, &pu).await, "25.00|8.00");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some(DEV));
    let g = guardado_ts(p, "productos", &pu).await;
    assert!(g < futura && g <= ahora(p).await, "recortada: {g} vs {futura}");
    assert_eq!(r.marcas.get(&pu), Some(&g));
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// s3) `marcas` en la respuesta: una por fila padre aceptada.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn s3_marcas_en_la_respuesta() {
    let Some(c) = ctx("s3").await else { return };
    let p = &c.pool;
    let ts = ahora(p).await;
    let (cu, cw) = (hex32(), hex32());
    let cli = |u: &str, nombre: &str, marca: &str| {
        json!({"id": 701, "uuid": u, "nombre": nombre, "activo": 1, "updated_at": marca, "deleted_at": null})
    };
    // Cliente con marca del futuro: se recorta y la marca dice la recortada.
    // Cliente que la web editó después: rechazado → sin marca.
    procesar_push(p, &body(Some(2), vec![cambio("clientes", cli(&cw, "W", &ts), None, None)])).await.unwrap();
    sqlx::query("UPDATE clientes SET nombre = 'WEB', updated_at = $2 WHERE uuid = $1")
        .bind(&cw)
        .bind(marca_rel(p, 60).await)
        .execute(p)
        .await
        .unwrap();
    cursor_de(p, "clientes", &cw, "web-pos").await;
    // Venta con renglón: marca solo del padre.
    let (_, prod_uuid) = producto_existente(p, 100).await;
    let (vu, du) = (hex32(), hex32());
    let mut rf = Map::new();
    rf.insert(vu.clone(), json!({"usuario_id": DUENIO, "cliente_id": null, "anulada_por": null}));
    rf.insert(du.clone(), json!({"producto_id": prod_uuid, "autorizado_por": null}));
    let r = procesar_push(
        p,
        &body(
            Some(2),
            vec![
                cambio("clientes", cli(&cu, "FUTURO", "2099-01-01 00:00:00"), None, None),
                cambio("clientes", cli(&cw, "VIEJO", &ts), None, None),
                cambio("ventas", venta_escritorio(99301, &vu, 1, &ts), Some(json!({"venta_detalle": [detalle(88301, &du, 99301, 7777, &ts)]})), Some(Value::Object(rf))),
                cambio("stock_sucursal", json!({"uuid": hex32()}), None, None),
            ],
        ),
    )
    .await
    .unwrap();
    assert_eq!(r.rechazados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(r.rechazados[0].uuid, cw);
    assert_eq!(r.aceptados.len(), 3);
    let g_cu = guardado_ts(p, "clientes", &cu).await;
    assert!(g_cu.as_str() < "2099" && g_cu.len() == 19, "{g_cu}");
    assert_eq!(r.marcas.get(&cu), Some(&g_cu));
    assert_eq!(r.marcas.get(&vu), Some(&guardado_ts(p, "ventas", &vu).await));
    assert!(!r.marcas.contains_key(&cw), "rechazado: sin marca");
    assert!(!r.marcas.contains_key(&du), "hijos: sin marca");
    assert_eq!(r.marcas.len(), 2, "{:?}", r.marcas);
    // Forma JSON: objeto {uuid: 'YYYY-MM-DD HH:MM:SS'}.
    let j = serde_json::to_value(&r).unwrap();
    assert_eq!(j["marcas"][&cu], json!(g_cu));
    // DELETE: la marca del borrado lógico.
    let mut del = cambio("clientes", json!({"uuid": cu}), None, None);
    del.operacion = "DELETE".into();
    let r = procesar_push(p, &body(Some(2), vec![del])).await.unwrap();
    assert_eq!(r.aceptados, vec![cu.clone()]);
    assert_eq!(r.marcas.get(&cu), Some(&guardado_ts(p, "clientes", &cu).await));
    assert!(texto(p, &format!("SELECT deleted_at FROM clientes WHERE uuid = '{cu}'")).await.is_some());
    // Sin filas aceptadas: objeto vacío (los escritorios nuevos lo esperan).
    let r = procesar_push(p, &body(Some(2), vec![])).await.unwrap();
    assert_eq!(serde_json::to_value(&r).unwrap()["marcas"], json!({}));
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// s4) sync_fk_celda_web: (a) el push quita la marca cuando el escritorio
//     reescribe la celda con OTRO valor (la reparación la traduce después);
//     con el mismo valor se queda. (b) El pull viejo no manda las celdas
//     marcadas de filas del escritorio sin estado.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn s4_marcas_de_celda_web() {
    let Some(c) = ctx("s4").await else { return };
    let p = &c.pool;
    let c0 = entero(p, "SELECT max(id) FROM sync_cursor").await;
    let id_cat = |n: &'static str| async move { id_de(p, "categorias", &uuid_categoria(p, n).await).await };
    let (frenos, accesorios, aceites) = (id_cat("Frenos").await, id_cat("Accesorios").await, id_cat("Aceites y Lubricantes").await);
    let marcas_de = |u: String| async move {
        entero(p, &format!("SELECT count(*) FROM sync_fk_celda_web WHERE tabla = 'productos' AND uuid = '{u}' AND columna = 'categoria_id'")).await
    };
    // La web pone 'Frenos' (id de aquí) a productos del escritorio sin reparar
    // (lo que hace rpc actualizar_producto).
    let web_cambia_categoria = |u: String, cat: i64| async move {
        sqlx::raw_sql(&format!(
            "UPDATE productos SET categoria_id = {cat}, updated_at = '2026-06-01 00:00:00' WHERE uuid = '{u}'; \
             INSERT INTO sync_fk_celda_web (tabla, uuid, columna) VALUES ('productos', '{u}', 'categoria_id') ON CONFLICT DO NOTHING; \
             INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) VALUES ('productos', '{u}', 1, 'web-pos');"
        ))
        .execute(p)
        .await
        .unwrap();
    };
    let (_, p1) = producto_existente(p, 1).await;
    let (_, p2) = producto_existente(p, 2).await;
    web_cambia_categoria(p1.clone(), frenos).await;
    // Otro producto del escritorio con su categoría cruda (3 = Aceites allá), sin marca.
    sqlx::query("UPDATE productos SET categoria_id = 3 WHERE uuid = $1").bind(&p2).execute(p).await.unwrap();
    cursor_de(p, "productos", &p2, "web-pos").await;

    // (b) Pull viejo: sin la celda marcada (el escritorio viejo la tomaría
    //     como id suyo: 5 = 'Suspensión' allá); lo demás sí viaja.
    let r = procesar_pull(p, &PullQuery { cursor: c0.to_string(), sucursal_id: 1, device_uuid: None, proto: None }).await.unwrap();
    let buscar = |cs: &Vec<Value>, u: &str| cs.iter().rev().find(|x| x["__data"]["uuid"] == json!(u)).cloned();
    let x1 = buscar(&r.cambios, &p1).expect("producto marcado en el pull viejo");
    assert!(x1["__data"].get("categoria_id").is_none(), "{}", x1["__data"]);
    assert!(x1["__data"].get("codigo").is_some() && x1["__data"].get("proveedor_id").is_some());
    let x2 = buscar(&r.cambios, &p2).expect("producto sin marca en el pull viejo");
    assert_eq!(x2["__data"]["categoria_id"], json!(3), "sin marca: cruda como siempre");
    // El pull v2 no cambia (sin __refs: el escritorio conserva lo suyo).
    let r = procesar_pull(p, &PullQuery { cursor: c0.to_string(), sucursal_id: 1, device_uuid: Some("otro".into()), proto: Some(2) }).await.unwrap();
    let x1 = buscar(&r.cambios, &p1).unwrap();
    assert_eq!(x1["__data"]["categoria_id"], json!(frenos));
    assert!(x1.get("__refs").is_none());

    // (a) Escritorio viejo (sin proto) devuelve el mismo id que copió: la marca se queda.
    let ts = ahora(p).await;
    let mut d = fila(p, "productos", &p1).await;
    d.insert("updated_at".into(), json!(ts));
    let r = procesar_push(p, &body(None, vec![cambio("productos", Value::Object(d.clone()), None, None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(marcas_de(p1.clone()).await, 1, "mismo valor: la marca se queda");
    // … y luego cambia la categoría en el escritorio (3 = Aceites allá): se
    // guarda cruda, la marca se va y la reparación la traduce por el mapa.
    d.insert("categoria_id".into(), json!(3));
    let r = procesar_push(p, &body(None, vec![cambio("productos", Value::Object(d), None, None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(entero(p, &format!("SELECT categoria_id FROM productos WHERE uuid = '{p1}'")).await, 3);
    assert_eq!(marcas_de(p1.clone()).await, 0, "el escritorio reescribió la celda: sin marca");
    assert_eq!(estado(p, "productos", &p1).await, None);

    // v2: otro valor → la marca se va; mismo valor → se queda. (Los
    // productos 3–5 tienen el proveedor 1, igual en los dos lados.)
    let prov1 = texto(p, "SELECT uuid FROM proveedores WHERE id = 1").await.unwrap();
    let (_, p3) = producto_existente(p, 3).await;
    let (_, p4) = producto_existente(p, 4).await;
    web_cambia_categoria(p3.clone(), frenos).await;
    web_cambia_categoria(p4.clone(), frenos).await;
    for (u, cat) in [(&p3, "Accesorios"), (&p4, "Frenos")] {
        let mut d = fila(p, "productos", u).await;
        d.insert("updated_at".into(), json!(ts));
        d.insert("categoria_id".into(), json!(1)); // id local, viaja por refs
        let mut rf = Map::new();
        rf.insert(u.clone(), json!({"categoria_id": uuid_categoria(p, cat).await, "proveedor_id": prov1}));
        let r = procesar_push(p, &body(Some(2), vec![cambio("productos", Value::Object(d), None, Some(Value::Object(rf)))])).await.unwrap();
        assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    }
    assert_eq!(entero(p, &format!("SELECT categoria_id FROM productos WHERE uuid = '{p3}'")).await, accesorios);
    assert_eq!(marcas_de(p3.clone()).await, 0);
    assert_eq!(entero(p, &format!("SELECT categoria_id FROM productos WHERE uuid = '{p4}'")).await, frenos);
    assert_eq!(marcas_de(p4.clone()).await, 1);

    // Combinación a tres bandas sobre una fila sin reparar con marca: el
    // escritorio no tocó la categoría → se queda la de la web (y su marca);
    // el stock por diferencia.
    let (_, p5) = producto_existente(p, 5).await;
    sqlx::query("UPDATE productos SET stock_actual = 10, updated_at = '2026-05-01 00:00:00' WHERE uuid = $1").bind(&p5).execute(p).await.unwrap();
    let mut base5 = fila(p, "productos", &p5).await;
    base5.insert("id".into(), json!(5));
    web_cambia_categoria(p5.clone(), frenos).await;
    sqlx::query("UPDATE productos SET stock_actual = 9 WHERE uuid = $1").bind(&p5).execute(p).await.unwrap();
    let mut d5 = base5.clone();
    d5.insert("stock_actual".into(), json!(9.0));
    d5.insert("updated_at".into(), json!(ts));
    let mut rf = Map::new();
    rf.insert(p5.clone(), json!({"categoria_id": null, "proveedor_id": prov1}));
    let mut ch = cambio("productos", Value::Object(d5), None, Some(Value::Object(rf)));
    ch.base = Some(Value::Object(base5));
    let r = procesar_push(p, &body(Some(2), vec![ch])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(
        texto(p, &format!("SELECT categoria_id || '|' || stock_actual FROM productos WHERE uuid = '{p5}'")).await,
        Some(format!("{frenos}|8.00"))
    );
    assert_eq!(ultimo_origen(p, "productos", &p5).await.as_deref(), Some("merge"));
    assert_eq!(marcas_de(p5.clone()).await, 1, "la celda no se reescribió");

    // La reparación traduce la cruda de p1 (3 → Aceites aquí); p4 (marca)
    // ya no está en su alcance (v2 la dejó 'traducido').
    subir_mapa_tienda(p).await;
    let rep = sync_fk::reparar(p, DEV, false).await.unwrap();
    assert!(rep.ok, "{}", rep.reporte);
    assert_eq!(entero(p, &format!("SELECT categoria_id FROM productos WHERE uuid = '{p1}'")).await, aceites);
    assert_eq!(estado(p, "productos", &p1).await.as_deref(), Some("reparado"));
    assert_eq!(entero(p, &format!("SELECT categoria_id FROM productos WHERE uuid = '{p4}'")).await, frenos);
    c.cerrar().await;
}
