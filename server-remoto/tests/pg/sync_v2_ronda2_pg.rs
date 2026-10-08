// Pruebas de la ronda 2 del sync v2 (SPEC2 R1) contra Postgres (copia fiel
// de producción). Submódulo de sync_v2_pg.rs: usa sus ayudas (ctx, cambio,
// body, venta_escritorio…). Igual que allá, solo corren con SYNC_PG_URL.
//
//   r1  push viejo: filas web sin tocar sus FK; fila 'sin_mapa'
//   r2  push v2: pendiente en fila existente conserva lo guardado;
//       resolver compara-e-intercambia; pull sin celdas pendientes
//   r3  LWW: mismo dispositivo aplica, otro dispositivo rechaza
//   r4  productos: combinación a tres bandas con `base`
//   r5  reparación: celdas web, verificación por dispositivo, reversa
//       que respeta pushes posteriores, caja_v2_desde en el reporte

use super::*;

pub(super) async fn fila(p: &PgPool, tabla: &str, uuid: &str) -> Map<String, Value> {
    serde_json::from_str(
        &texto(p, &format!("SELECT row_to_json(x)::text FROM {tabla} x WHERE uuid = '{uuid}'")).await.unwrap(),
    )
    .unwrap()
}

async fn filas_hijas(p: &PgPool, tabla: &str, fk: &str, pid: i64) -> Vec<Value> {
    let t = texto(
        p,
        &format!("SELECT COALESCE(json_agg(row_to_json(x) ORDER BY x.id), '[]')::text FROM {tabla} x WHERE {fk} = {pid}"),
    )
    .await
    .unwrap();
    serde_json::from_str(&t).unwrap()
}

pub(super) async fn cursor_de(p: &PgPool, tabla: &str, uuid: &str, origen: &str) {
    sqlx::query("INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) VALUES ($1, $2, 1, $3)")
        .bind(tabla)
        .bind(uuid)
        .bind(origen)
        .execute(p)
        .await
        .unwrap();
}

pub(super) async fn ultimo_origen(p: &PgPool, tabla: &str, uuid: &str) -> Option<String> {
    texto(
        p,
        &format!("SELECT origen_device FROM sync_cursor WHERE tabla = '{tabla}' AND uuid = '{uuid}' ORDER BY id DESC LIMIT 1"),
    )
    .await
}

pub(super) async fn pendientes_de(p: &PgPool, uuid: &str) -> i64 {
    entero(p, &format!("SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid = '{uuid}'")).await
}

const SEMILLA_CATEGORIAS: [&str; 10] = [
    "Frenos", "Cadenas y Piñones", "Aceites y Lubricantes", "Eléctrico", "Suspensión",
    "Motor", "Carrocería", "Transmisión", "Neumáticos", "Accesorios",
];

pub(super) async fn uuid_categoria(p: &PgPool, nombre: &str) -> String {
    sqlx::query_scalar("SELECT uuid FROM categorias WHERE nombre = $1").bind(nombre).fetch_one(p).await.unwrap()
}

/// Mapa del escritorio de la tienda (hechos de producción): usuarios 1..4,
/// categorías en el orden de la semilla, productos idénticos hasta 5948,
/// proveedores idénticos.
pub(super) async fn subir_mapa_tienda(p: &PgPool) {
    let provs: Vec<(i64, String)> = sqlx::query_as("SELECT id, uuid FROM proveedores WHERE uuid ~ '^[0-9a-f]{32}$'")
        .fetch_all(p)
        .await
        .unwrap();
    let pares = provs.iter().map(|(i, u)| json!([i, u])).collect();
    procesar_id_map(p, &IdMapBody { device_uuid: DEV.into(), tabla: "proveedores".into(), pares }).await.unwrap();
    let usuarios = vec![json!([1, DUENIO]), json!([2, NANCY]), json!([3, MARIANA]), json!([4, LAU])];
    procesar_id_map(p, &IdMapBody { device_uuid: DEV.into(), tabla: "usuarios".into(), pares: usuarios }).await.unwrap();
    let mut pares = Vec::new();
    for (i, n) in SEMILLA_CATEGORIAS.iter().enumerate() {
        pares.push(json!([i as i64 + 1, uuid_categoria(p, n).await]));
    }
    procesar_id_map(p, &IdMapBody { device_uuid: DEV.into(), tabla: "categorias".into(), pares }).await.unwrap();
    let prods: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, uuid FROM productos WHERE id <= 5948 AND uuid ~ '^[0-9a-f]{32}$' ORDER BY id")
            .fetch_all(p)
            .await
            .unwrap();
    for trozo in prods.chunks(5000) {
        let pares = trozo.iter().map(|(i, u)| json!([i, u])).collect();
        procesar_id_map(p, &IdMapBody { device_uuid: DEV.into(), tabla: "productos".into(), pares }).await.unwrap();
    }
}

// ─────────────────────────────────────────────────────────────────────────
// r1) Push viejo (sin proto): las FK de filas web no se tocan (antes: NULL +
//     pendiente sin mapa, o 'Accesorios' → 'Cadenas' con el mapa); hijo
//     nuevo de una fila web se traduce; fila 'sin_mapa': solo lo ya reparado
//     se traduce, lo demás sigue crudo.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn r1_push_viejo_filas_web_y_sin_mapa() {
    let Some(c) = ctx("r1").await else { return };
    let p = &c.pool;
    let ts = ahora(p).await;

    // ── Producto web 11907 (Accesorios = 2 aquí, proveedor 15) ──
    let wu = "019ef19c-5df7-7542-8344-68c5aae190e1";
    let mut w = fila(p, "productos", wu).await;
    assert_eq!((w["categoria_id"].clone(), w["proveedor_id"].clone()), (json!(2), json!(15)));
    w.insert("id".into(), json!(6100)); // id local en el escritorio
    w.insert("stock_actual".into(), json!(2.0));
    w.insert("updated_at".into(), json!(ts));
    // Sin mapa todavía.
    let r = procesar_push(p, &body(None, vec![cambio("productos", Value::Object(w.clone()), None, None)])).await.unwrap();
    assert_eq!(r.aceptados, vec![wu.to_string()], "{:?}", r.rechazados);
    assert_eq!(
        texto(p, &format!("SELECT categoria_id || '|' || proveedor_id || '|' || stock_actual FROM productos WHERE uuid = '{wu}'")).await.as_deref(),
        Some("2|15|2.00"),
        "sin mapa: antes quedaba NULL + pendiente"
    );
    // Con el mapa del escritorio (2 = 'Cadenas y Piñones' allá; proveedor 15 → otro).
    subir_mapa_tienda(p).await;
    let prov3: String = texto(p, "SELECT uuid FROM proveedores WHERE id = 3").await.unwrap();
    procesar_id_map(p, &IdMapBody { device_uuid: DEV.into(), tabla: "proveedores".into(), pares: vec![json!([15, prov3])] }).await.unwrap();
    w.insert("stock_actual".into(), json!(1.0));
    let r = procesar_push(p, &body(None, vec![cambio("productos", Value::Object(w), None, None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(
        texto(p, &format!("SELECT categoria_id || '|' || proveedor_id || '|' || stock_actual FROM productos WHERE uuid = '{wu}'")).await.as_deref(),
        Some("2|15|1.00"),
        "con mapa: antes pasaba a 4 (Cadenas y Piñones)"
    );
    assert_eq!(estado(p, "productos", wu).await, None);
    assert_eq!(pendientes_de(p, wu).await, 0);

    // ── Venta web con renglones: el escritorio viejo la reenvía (anulación)
    //    con ids crudos del servidor + un renglón NUEVO hecho allá ──
    let vw = texto(p, "SELECT uuid FROM ventas WHERE origen = 'web' AND uuid !~ '^[0-9a-f]{32}$' \
                        AND EXISTS (SELECT 1 FROM venta_detalle d WHERE d.venta_id = ventas.id) ORDER BY id LIMIT 1")
        .await
        .unwrap();
    let vid = id_de(p, "ventas", &vw).await;
    let antes_v = fila(p, "ventas", &vw).await;
    let antes_hijos = filas_hijas(p, "venta_detalle", "venta_id", vid).await;
    let mut v = antes_v.clone();
    v.insert("id".into(), json!(4321));
    v.insert("usuario_id".into(), json!(1)); // crudo (= 'Andrew Web' aquí)
    v.insert("anulada".into(), json!(1));
    v.insert("updated_at".into(), json!(ts));
    let mut hijos: Vec<Value> = antes_hijos
        .iter()
        .map(|h| {
            let mut h = h.clone();
            h["producto_id"] = json!(1);
            h["venta_id"] = json!(4321);
            h
        })
        .collect();
    let (_, p100) = producto_existente(p, 100).await;
    procesar_id_map(p, &IdMapBody { device_uuid: DEV.into(), tabla: "productos".into(), pares: vec![json!([7777, p100])] }).await.unwrap();
    let nuevo = hex32();
    hijos.push(detalle(99, &nuevo, 4321, 7777, &ts));
    let r = procesar_push(p, &body(None, vec![cambio("ventas", Value::Object(v), Some(json!({"venta_detalle": hijos})), None)])).await.unwrap();
    assert_eq!(r.aceptados, vec![vw.clone()], "{:?}", r.rechazados);
    let despues = fila(p, "ventas", &vw).await;
    assert_eq!(despues["usuario_id"], antes_v["usuario_id"], "FK de la fila web intacta");
    assert_eq!(despues["anulada"], json!(1), "lo demás sí se aplica");
    for h in &antes_hijos {
        let hu = h["uuid"].as_str().unwrap();
        assert_eq!(fila(p, "venta_detalle", hu).await["producto_id"], h["producto_id"], "renglón web intacto");
    }
    assert_eq!(entero(p, &format!("SELECT producto_id FROM venta_detalle WHERE uuid = '{nuevo}'")).await, 100, "renglón nuevo por el mapa");
    assert_eq!(entero(p, &format!("SELECT venta_id FROM venta_detalle WHERE uuid = '{nuevo}'")).await, vid);
    assert_eq!(estado(p, "ventas", &vw).await, None);

    // ── Fila 'sin_mapa': la reparación ya tradujo usuario_id (2 → 3) pero no cliente_id ──
    let vs = texto(
        p,
        &format!("SELECT uuid FROM ventas v WHERE v.uuid ~ '^[0-9a-f]{{32}}$' AND v.usuario_id = 2 AND v.cliente_id IS NULL \
                  AND EXISTS (SELECT 1 FROM sync_cursor sc WHERE sc.tabla = 'ventas' AND sc.uuid = v.uuid AND sc.origen_device = '{DEV}') \
                  ORDER BY id LIMIT 1"),
    )
    .await
    .unwrap();
    sqlx::raw_sql(&format!(
        "UPDATE ventas SET usuario_id = 3 WHERE uuid = '{vs}'; \
         INSERT INTO sync_fk_repair_log (lote, tabla, fila_uuid, columna, valor_anterior, valor_nuevo) VALUES ('prueba', 'ventas', '{vs}', 'usuario_id', 2, 3); \
         INSERT INTO sync_fk_estado (tabla, uuid, estado) VALUES ('ventas', '{vs}', 'sin_mapa');"
    ))
    .execute(p)
    .await
    .unwrap();
    let mut s = fila(p, "ventas", &vs).await;
    s.insert("usuario_id".into(), json!(2)); // crudo del escritorio (Nancy)
    s.insert("cliente_id".into(), json!(77)); // crudo, sin reparar todavía
    s.insert("updated_at".into(), json!(ts));
    let r = procesar_push(p, &body(None, vec![cambio("ventas", Value::Object(s), None, None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(
        texto(p, &format!("SELECT usuario_id || '|' || cliente_id FROM ventas WHERE uuid = '{vs}'")).await.as_deref(),
        Some("3|77"),
        "usuario_id (ya reparado) por el mapa; cliente_id sigue crudo para la reparación"
    );
    assert_eq!(estado(p, "ventas", &vs).await.as_deref(), Some("sin_mapa"));
    assert_eq!(pendientes_de(p, &vs).await, 0);
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// r2) Push v2 con una referencia que todavía no llega: en una fila que ya
//     existe se conserva lo guardado (pendiente con valor_puesto); el pull
//     no manda esa celda (ni null en __refs, ni en __data del pull viejo);
//     resolver_pendientes solo escribe si la celda sigue igual.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn r2_pendiente_conserva_lo_guardado_y_cas() {
    let Some(c) = ctx("r2").await else { return };
    let p = &c.pool;
    let ts = ahora(p).await;
    let c0 = entero(p, "SELECT max(id) FROM sync_cursor").await;

    // Producto del escritorio 100 (proveedor 3). El escritorio lo cambia a un
    // proveedor que el servidor todavía no tiene.
    let (_, pu) = producto_existente(p, 100).await;
    let prov_nuevo = hex32();
    let mut d = fila(p, "productos", &pu).await;
    d.insert("stock_actual".into(), json!(5));
    d.insert("proveedor_id".into(), json!(40)); // id local del escritorio
    d.insert("updated_at".into(), json!(ts));
    let mut rf = Map::new();
    rf.insert(pu.clone(), json!({"proveedor_id": prov_nuevo, "categoria_id": null}));
    let r = procesar_push(p, &body(Some(2), vec![cambio("productos", Value::Object(d), None, Some(Value::Object(rf)))])).await.unwrap();
    assert_eq!(r.aceptados, vec![pu.clone()], "{:?}", r.rechazados);
    assert_eq!(r.faltantes, vec![prov_nuevo.clone()]);
    assert_eq!(
        texto(p, &format!("SELECT proveedor_id || '|' || stock_actual FROM productos WHERE uuid = '{pu}'")).await.as_deref(),
        Some("3|5.00"),
        "se conserva el proveedor guardado (antes: NULL)"
    );
    assert_eq!(
        texto(p, &format!("SELECT valor_puesto || '|' || raw_local_id || '|' || ref_uuid FROM sync_fk_pendiente WHERE fila_uuid = '{pu}' AND columna = 'proveedor_id'")).await,
        Some(format!("3|40|{prov_nuevo}"))
    );
    assert_eq!(estado(p, "productos", &pu).await.as_deref(), Some("pendiente"));

    // Producto web 11907: la misma situación, para ver los dos pulls.
    let wu = "019ef19c-5df7-7542-8344-68c5aae190e1";
    let mut w = fila(p, "productos", wu).await;
    w.insert("updated_at".into(), json!(ts));
    let mut rf = Map::new();
    rf.insert(wu.to_string(), json!({"proveedor_id": prov_nuevo, "categoria_id": uuid_categoria(p, "Accesorios").await}));
    let r = procesar_push(p, &body(Some(2), vec![cambio("productos", Value::Object(w), None, Some(Value::Object(rf)))])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(texto(p, &format!("SELECT categoria_id || '|' || proveedor_id FROM productos WHERE uuid = '{wu}'")).await.as_deref(), Some("2|15"));
    // Lo "publica" la web para que lo vea otro equipo.
    cursor_de(p, "productos", wu, "web-pos").await;

    let buscar = |cs: &Vec<Value>, u: &str| cs.iter().find(|x| x["__data"]["uuid"] == json!(u)).cloned();
    let r = procesar_pull(p, &PullQuery { cursor: c0.to_string(), sucursal_id: 1, device_uuid: Some("otro-equipo".into()), proto: Some(2) }).await.unwrap();
    let x = buscar(&r.cambios, wu).expect("producto web en el pull v2");
    let refs = x["__refs"][wu].as_object().unwrap();
    assert!(!refs.contains_key("proveedor_id"), "celda pendiente: sin llave (nunca null): {refs:?}");
    assert_eq!(refs["categoria_id"], json!(uuid_categoria(p, "Accesorios").await));
    let x = buscar(&r.cambios, &pu).expect("producto del escritorio en el pull v2");
    assert!(x.get("__refs").is_none(), "fila del escritorio 'pendiente': sin __refs");
    let r = procesar_pull(p, &PullQuery { cursor: c0.to_string(), sucursal_id: 1, device_uuid: None, proto: None }).await.unwrap();
    let x = buscar(&r.cambios, wu).expect("producto web en el pull viejo");
    assert!(x["__data"].get("proveedor_id").is_none(), "pull viejo: sin la celda pendiente");
    assert_eq!(x["__data"]["categoria_id"], json!(2));
    let m = procesar_refs(p, &RefsBody { tabla: "productos".into(), uuids: vec![wu.to_string()] }).await.unwrap();
    assert!(m[wu].get("proveedor_id").is_none() && m[wu].get("categoria_id").is_some(), "{m}");
    let f = procesar_fila(p, &FilaQuery { tabla: "productos".into(), uuid: wu.to_string() }).await.unwrap();
    assert!(f["__refs"][wu].get("proveedor_id").is_none(), "{f}");

    // Otro producto del escritorio (200) espera un proveedor que nunca llega;
    // la web le cambia el proveedor → el pendiente queda viejo y se borra en
    // el siguiente reintento aunque no se pueda resolver.
    let (_, pu200) = producto_existente(p, 200).await;
    let mut d200 = fila(p, "productos", &pu200).await;
    d200.insert("updated_at".into(), json!(ts));
    let mut rf = Map::new();
    rf.insert(pu200.clone(), json!({"proveedor_id": hex32(), "categoria_id": null}));
    procesar_push(p, &body(Some(2), vec![cambio("productos", Value::Object(d200), None, Some(Value::Object(rf)))])).await.unwrap();
    assert_eq!(pendientes_de(p, &pu200).await, 1);
    sqlx::query("UPDATE productos SET proveedor_id = 9 WHERE uuid = $1").bind(&pu200).execute(p).await.unwrap();

    // CAS: la web cambia el proveedor del producto 100 antes de que llegue el
    // proveedor nuevo → al llegar, el pendiente está viejo y se borra sin
    // pisar la edición de la web.
    sqlx::query("UPDATE productos SET proveedor_id = 7, updated_at = $2 WHERE uuid = $1").bind(&pu).bind(&ts).execute(p).await.unwrap();
    let prov = json!({"id": 40, "uuid": prov_nuevo, "nombre": "PROV NUEVO", "updated_at": ts, "deleted_at": null});
    let r = procesar_push(p, &body(Some(2), vec![cambio("proveedores", prov, None, None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    let id_prov = id_de(p, "proveedores", &prov_nuevo).await;
    assert_eq!(entero(p, &format!("SELECT proveedor_id FROM productos WHERE uuid = '{pu}'")).await, 7, "gana la edición de la web");
    assert_eq!(pendientes_de(p, &pu).await, 0, "pendiente viejo borrado");
    assert_eq!(estado(p, "productos", &pu).await.as_deref(), Some("traducido"));
    // El del producto web sí seguía igual (15) → se corrige al proveedor nuevo.
    assert_eq!(entero(p, &format!("SELECT proveedor_id FROM productos WHERE uuid = '{wu}'")).await, id_prov);
    assert_eq!(pendientes_de(p, wu).await, 0);
    assert_eq!(pendientes_de(p, &pu200).await, 0, "viejo y sin resolver: igual se borra");
    assert_eq!(entero(p, &format!("SELECT proveedor_id FROM productos WHERE uuid = '{pu200}'")).await, 9);
    assert_eq!(estado(p, "productos", &pu200).await.as_deref(), Some("traducido"));

    // Fila nueva: centinela con valor_puesto (0 en NOT NULL).
    let (vu, du, pfalta) = (hex32(), hex32(), hex32());
    let mut rf = Map::new();
    rf.insert(vu.clone(), json!({"usuario_id": NANCY, "cliente_id": null, "anulada_por": null}));
    rf.insert(du.clone(), json!({"producto_id": pfalta, "autorizado_por": null}));
    procesar_push(p, &body(Some(2), vec![cambio("ventas", venta_escritorio(99100, &vu, 2, &ts), Some(json!({"venta_detalle": [detalle(88100, &du, 99100, 7100, &ts)]})), Some(Value::Object(rf)))])).await.unwrap();
    assert_eq!(entero(p, &format!("SELECT producto_id FROM venta_detalle WHERE uuid = '{du}'")).await, 0);
    assert_eq!(entero(p, &format!("SELECT count(*) FROM sync_fk_pendiente WHERE fila_uuid = '{du}' AND valor_puesto = 0")).await, 1);
    // Re-push v2 sin el renglón: el renglón y su pendiente se van.
    let mut rf = Map::new();
    rf.insert(vu.clone(), json!({"usuario_id": NANCY, "cliente_id": null, "anulada_por": null}));
    procesar_push(p, &body(Some(2), vec![cambio("ventas", venta_escritorio(99100, &vu, 2, &ts), Some(json!({"venta_detalle": []})), Some(Value::Object(rf)))])).await.unwrap();
    assert_eq!(pendientes_de(p, &du).await, 0);
    assert_eq!(estado(p, "ventas", &vu).await.as_deref(), Some("traducido"));
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// r3) LWW: si lo más nuevo de aquí lo escribió el MISMO escritorio, su push
//     (más viejo por reloj) se aplica; si fue otro (la web), se rechaza.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn r3_lww_mismo_dispositivo() {
    let Some(c) = ctx("r3").await else { return };
    let p = &c.pool;
    let ts = ahora(p).await;
    let cu = hex32();
    let cli = |nombre: &str, marca: &str| {
        json!({"id": 501, "uuid": cu, "nombre": nombre, "telefono": null, "email": null,
               "descuento_porcentaje": 0, "activo": 1, "notas": null, "updated_at": marca, "deleted_at": null})
    };
    // El escritorio con reloj adelantado (marca futura → se recorta a ahora).
    let r = procesar_push(p, &body(Some(2), vec![cambio("clientes", cli("ADELANTADO", "2099-01-01 00:00:00"), None, None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1);
    let guardada = texto(p, &format!("SELECT updated_at FROM clientes WHERE uuid = '{cu}'")).await.unwrap();
    assert!(guardada >= ts);
    // Corrige su reloj y edita: su marca es más vieja que la guardada, pero la
    // guardada es suya → se aplica (antes: remoto_mas_nuevo para siempre).
    let r = procesar_push(p, &body(Some(2), vec![cambio("clientes", cli("CORREGIDO", "2026-01-02 03:04:05"), None, None)])).await.unwrap();
    assert_eq!(r.aceptados, vec![cu.clone()], "{:?}", r.rechazados);
    assert_eq!(
        texto(p, &format!("SELECT nombre || '|' || updated_at FROM clientes WHERE uuid = '{cu}'")).await.as_deref(),
        Some("CORREGIDO|2026-01-02 03:04:05")
    );
    // Push viejo (sin proto): misma regla.
    let r = procesar_push(p, &body(None, vec![cambio("clientes", cli("VIEJO MISMO EQUIPO", "2026-01-01 00:00:00"), None, None)])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    // La web edita después → un push más viejo del escritorio se rechaza.
    sqlx::query("UPDATE clientes SET nombre = 'WEB', updated_at = $2 WHERE uuid = $1").bind(&cu).bind(&ts).execute(p).await.unwrap();
    cursor_de(p, "clientes", &cu, "web-pos").await;
    let r = procesar_push(p, &body(Some(2), vec![cambio("clientes", cli("ESCRITORIO", "2026-01-03 00:00:00"), None, None)])).await.unwrap();
    assert_eq!(r.rechazados.len(), 1);
    assert_eq!(r.rechazados[0].motivo, "remoto_mas_nuevo");
    assert_eq!(texto(p, &format!("SELECT nombre FROM clientes WHERE uuid = '{cu}'")).await.as_deref(), Some("WEB"));
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// r4) productos con `base`: combinación a tres bandas.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn r4_productos_tres_vias() {
    let Some(c) = ctx("r4").await else { return };
    let p = &c.pool;
    subir_mapa_tienda(p).await;
    let frenos = uuid_categoria(p, "Frenos").await;
    let id_frenos = id_de(p, "categorias", &frenos).await;
    let id_accesorios = id_de(p, "categorias", &uuid_categoria(p, "Accesorios").await).await;

    // Producto nuevo del escritorio: precio 25, stock 10, categoría local 1 (Frenos).
    let pu = hex32();
    let t0 = "2026-01-01 10:00:00";
    let mut d0 = producto_escritorio(7200, &pu, t0);
    d0["precio_venta"] = json!(25.0);
    d0["stock_actual"] = json!(10.0);
    d0["categoria_id"] = json!(1);
    let refs_p = || {
        let mut m = Map::new();
        m.insert(pu.clone(), json!({"categoria_id": frenos, "proveedor_id": null}));
        Some(Value::Object(m))
    };
    let r = procesar_push(p, &body(Some(2), vec![cambio("productos", d0.clone(), None, refs_p())])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(entero(p, &format!("SELECT categoria_id FROM productos WHERE uuid = '{pu}'")).await, id_frenos);
    let precio_stock_cat = || async {
        texto(p, &format!("SELECT precio_venta || '|' || stock_actual || '|' || categoria_id FROM productos WHERE uuid = '{pu}'")).await.unwrap()
    };

    // 1) La web sube el precio a 35 y cambia la categoría a Accesorios; el
    //    escritorio vende 1 en el mismo ciclo (base = d0).
    sqlx::query("UPDATE productos SET precio_venta = 35, categoria_id = $2, updated_at = '2026-01-01 10:01:00' WHERE uuid = $1")
        .bind(&pu).bind(id_accesorios).execute(p).await.unwrap();
    cursor_de(p, "productos", &pu, "web-pos").await;
    let mut d1 = d0.clone();
    d1["stock_actual"] = json!(9.0);
    d1["updated_at"] = json!("2026-01-01 10:02:00");
    let c_antes = entero(p, "SELECT max(id) FROM sync_cursor").await;
    let mut ch = cambio("productos", d1.clone(), None, refs_p());
    ch.base = Some(d0.clone());
    let r = procesar_push(p, &body(Some(2), vec![ch])).await.unwrap();
    assert_eq!(r.aceptados, vec![pu.clone()], "{:?}", r.rechazados);
    assert_eq!(precio_stock_cat().await, format!("35.00|9.00|{id_accesorios}"), "precio y categoría de la web, stock del escritorio");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some("merge"));
    let marca = texto(p, &format!("SELECT updated_at FROM productos WHERE uuid = '{pu}'")).await.unwrap();
    assert!(marca.as_str() > "2026-01-01 10:02:00", "hora del servidor: {marca}");
    // El escritorio baja la fila combinada (no es su eco).
    let pr = procesar_pull(p, &PullQuery { cursor: c_antes.to_string(), sucursal_id: 1, device_uuid: Some(DEV.into()), proto: Some(2) }).await.unwrap();
    let bajado = pr.cambios.iter().find(|x| x["__data"]["uuid"] == json!(pu)).expect("merge en el pull");
    assert_eq!(bajado["__data"]["precio_venta"], json!(35.0));
    assert_eq!(bajado["__refs"][&pu]["categoria_id"], json!(uuid_categoria(p, "Accesorios").await));

    // 2) Venta en la web y venta en el escritorio del mismo producto: las dos
    //    descuentan (base = lo que empujó en 1: stock 9).
    sqlx::query("UPDATE productos SET stock_actual = stock_actual - 1, updated_at = '2026-01-01 10:03:00' WHERE uuid = $1")
        .bind(&pu).execute(p).await.unwrap();
    cursor_de(p, "productos", &pu, "web-pos").await;
    let mut d2 = d1.clone();
    d2["stock_actual"] = json!(8.0);
    d2["updated_at"] = json!("2026-01-01 10:04:00");
    let mut ch = cambio("productos", d2.clone(), None, refs_p());
    ch.base = Some(d1.clone());
    procesar_push(p, &body(Some(2), vec![ch])).await.unwrap();
    assert_eq!(precio_stock_cat().await, format!("35.00|7.00|{id_accesorios}"), "10 − 2 (escritorio) − 1 (web)");

    // 3) La web ajusta el stock a 20; el escritorio vende 3 → 17.
    sqlx::query("UPDATE productos SET stock_actual = 20, updated_at = '2026-01-01 10:05:00' WHERE uuid = $1")
        .bind(&pu).execute(p).await.unwrap();
    cursor_de(p, "productos", &pu, "web-pos").await;
    let mut d3 = d2.clone();
    d3["stock_actual"] = json!(5.0);
    d3["updated_at"] = json!("2026-01-01 10:06:00");
    let mut ch = cambio("productos", d3.clone(), None, refs_p());
    ch.base = Some(d2.clone());
    procesar_push(p, &body(Some(2), vec![ch])).await.unwrap();
    assert_eq!(precio_stock_cat().await, format!("35.00|17.00|{id_accesorios}"), "ajuste de la web − venta del escritorio");

    // 4) Los dos cambian el precio: gana la marca más nueva (la del escritorio).
    sqlx::query("UPDATE productos SET precio_venta = 40, updated_at = '2026-01-01 10:07:00' WHERE uuid = $1")
        .bind(&pu).execute(p).await.unwrap();
    cursor_de(p, "productos", &pu, "web-pos").await;
    let mut d4 = d3.clone();
    d4["precio_venta"] = json!(50.0);
    d4["updated_at"] = json!("2026-01-01 10:08:00");
    let mut ch = cambio("productos", d4.clone(), None, refs_p());
    ch.base = Some(d3.clone());
    procesar_push(p, &body(Some(2), vec![ch])).await.unwrap();
    assert_eq!(precio_stock_cat().await, format!("50.00|17.00|{id_accesorios}"));

    // 5) El escritorio ya bajó la fila (base = lo guardado): camino normal,
    //    su categoría local 1 vuelve a Frenos y queda su cursor.
    let mut base5 = fila(p, "productos", &pu).await;
    base5.insert("categoria_id".into(), json!(10)); // local: Accesorios = 10 en la semilla
    base5.insert("id".into(), json!(7200));
    let mut d5 = Value::Object(base5.clone());
    d5["categoria_id"] = json!(1);
    d5["stock_actual"] = json!(16.0);
    let mut ch = cambio("productos", d5, None, refs_p());
    ch.base = Some(Value::Object(base5));
    procesar_push(p, &body(Some(2), vec![ch])).await.unwrap();
    assert_eq!(precio_stock_cat().await, format!("50.00|16.00|{id_frenos}"));
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some(DEV));

    // 6) Lo más nuevo de aquí es del mismo escritorio (su base se perdió): sin
    //    combinación, su fila se aplica tal cual (no se resta dos veces).
    let mut d6 = d0.clone();
    d6["stock_actual"] = json!(15.0);
    d6["precio_venta"] = json!(50.0);
    d6["updated_at"] = json!("2026-01-01 10:09:00");
    let mut ch = cambio("productos", d6, None, refs_p());
    ch.base = Some(d0.clone());
    let r = procesar_push(p, &body(Some(2), vec![ch])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(precio_stock_cat().await, format!("50.00|15.00|{id_frenos}"));
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some(DEV));

    // 7) Escritorio viejo (sin proto) con `base`: se ignora (camino de siempre).
    sqlx::query("UPDATE productos SET precio_venta = 60, updated_at = '2026-01-01 10:10:00' WHERE uuid = $1")
        .bind(&pu).execute(p).await.unwrap();
    cursor_de(p, "productos", &pu, "web-pos").await;
    let mut d7 = d0.clone();
    d7["stock_actual"] = json!(14.0);
    d7["updated_at"] = json!("2026-01-01 10:11:00");
    let mut ch = cambio("productos", d7, None, None);
    ch.base = Some(d0.clone());
    procesar_push(p, &body(None, vec![ch])).await.unwrap();
    assert_eq!(
        texto(p, &format!("SELECT precio_venta || '|' || stock_actual FROM productos WHERE uuid = '{pu}'")).await.as_deref(),
        Some("25.00|14.00"),
        "LWW normal: la fila del escritorio es más nueva"
    );
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// r5) Reparación: celdas que escribió la web no se traducen; la verificación
//     es por dispositivo; la reversa no pisa filas que un push reescribió y
//     las reporta; caja_v2_desde en el reporte.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn r5_reparacion_ajustes() {
    let Some(c) = ctx("r5").await else { return };
    let p = &c.pool;
    let caja_v2 = texto(p, "SELECT valor FROM sync_meta WHERE clave = 'caja_v2_desde'").await;
    assert!(caja_v2.is_some());

    // (b) Un equipo nuevo sin filas propias: la reparación pasa aunque las
    //     ventas viejas de la tienda sigan a nombre de 'Andrew Web'.
    let r = sync_fk::reparar(p, "equipo-nuevo", false).await.unwrap();
    assert!(r.ok, "{}", r.reporte);
    assert_eq!(r.reporte["celdas_cambiadas"], json!(0));
    assert_eq!(r.reporte["caja_v2_desde"], json!(caja_v2), "(d) caja_v2_desde en el reporte");
    // La tienda sin mapa sí falla (sus ventas siguen en web-admin).
    let r = sync_fk::reparar(p, DEV, true).await.unwrap();
    assert!(!r.ok);
    assert_eq!(r.reporte["caja_v2_desde"], json!(caja_v2));

    // (a) Dos productos del escritorio con categoría 5: el 1 la puso la web
    //     (Frenos aquí, anotado en sync_fk_celda_web), el 2 es crudo del
    //     escritorio (5 = Suspensión allá).
    let (_, p1) = producto_existente(p, 1).await;
    let (_, p2) = producto_existente(p, 2).await;
    sqlx::raw_sql(&format!(
        "UPDATE productos SET categoria_id = 5 WHERE uuid IN ('{p1}', '{p2}'); \
         INSERT INTO sync_fk_celda_web (tabla, uuid, columna) VALUES ('productos', '{p1}', 'categoria_id');"
    ))
    .execute(p)
    .await
    .unwrap();

    // (c) Venta del escritorio (Dueño crudo = 1) con un renglón autorizado por 1.
    let vu = texto(
        p,
        &format!("SELECT uuid FROM ventas v WHERE v.uuid ~ '^[0-9a-f]{{32}}$' AND v.usuario_id = 1 AND v.cliente_id IS NULL \
                  AND v.anulada_por IS NULL AND EXISTS (SELECT 1 FROM venta_detalle d WHERE d.venta_id = v.id) \
                  AND EXISTS (SELECT 1 FROM sync_cursor sc WHERE sc.tabla = 'ventas' AND sc.uuid = v.uuid AND sc.origen_device = '{DEV}') \
                  ORDER BY id LIMIT 1"),
    )
    .await
    .unwrap();
    let vid = id_de(p, "ventas", &vu).await;
    let du = texto(p, &format!("SELECT uuid FROM venta_detalle WHERE venta_id = {vid} ORDER BY id LIMIT 1")).await.unwrap();
    sqlx::query("UPDATE venta_detalle SET autorizado_por = 1 WHERE uuid = $1").bind(&du).execute(p).await.unwrap();
    let raw_antes = entero(p, &format!(
        "SELECT count(*) FROM ventas v WHERE v.uuid ~ '^[0-9a-f]{{32}}$' AND v.usuario_id = 1 \
          AND EXISTS (SELECT 1 FROM sync_cursor sc WHERE sc.tabla = 'ventas' AND sc.uuid = v.uuid AND sc.origen_device = '{DEV}')"
    )).await;

    subir_mapa_tienda(p).await;
    let r = sync_fk::reparar(p, DEV, false).await.unwrap();
    assert!(r.ok, "{}", r.reporte);
    let lote = r.reporte["lote"].as_str().unwrap().to_string();
    assert_eq!(entero(p, &format!("SELECT categoria_id FROM productos WHERE uuid = '{p1}'")).await, 5, "celda web: no se traduce");
    let id_susp = id_de(p, "categorias", &uuid_categoria(p, "Suspensión").await).await;
    assert_eq!(entero(p, &format!("SELECT categoria_id FROM productos WHERE uuid = '{p2}'")).await, id_susp, "cruda: 5 del escritorio = Suspensión");
    assert_eq!(estado(p, "productos", &p1).await.as_deref(), Some("reparado"));
    assert_eq!(
        entero(p, &format!("SELECT count(*) FROM sync_fk_repair_log WHERE fila_uuid = '{p1}' AND columna = 'categoria_id'")).await,
        0
    );
    assert_eq!(entero(p, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{vu}'")).await, 2);
    assert_eq!(entero(p, &format!("SELECT autorizado_por FROM venta_detalle WHERE uuid = '{du}'")).await, 2);

    // Un push v2 reescribe esa venta después de la reparación (anulación).
    let ts = ahora(p).await;
    let mut v = fila(p, "ventas", &vu).await;
    v.insert("usuario_id".into(), json!(1)); // local del escritorio
    v.insert("updated_at".into(), json!(ts));
    let mut rf = Map::new();
    rf.insert(vu.clone(), json!({"usuario_id": DUENIO, "cliente_id": null, "anulada_por": null}));
    let r = procesar_push(p, &body(Some(2), vec![cambio("ventas", Value::Object(v), None, Some(Value::Object(rf)))])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(estado(p, "ventas", &vu).await.as_deref(), Some("traducido"));

    // Reversa: esa venta (y su renglón) NO vuelven a los ids crudos; se reportan.
    let rv = sync_fk::revertir_lote(p, &lote).await.unwrap();
    assert_eq!(entero(p, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{vu}'")).await, 2, "{rv}");
    assert_eq!(entero(p, &format!("SELECT autorizado_por FROM venta_detalle WHERE uuid = '{du}'")).await, 2, "{rv}");
    assert_eq!(rv["no_restauradas_por_push_total"], json!(2), "{rv}");
    let saltadas: Vec<(String, String)> = rv["no_restauradas_por_push"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["tabla"].as_str().unwrap().to_string(), x["fila_uuid"].as_str().unwrap().to_string()))
        .collect();
    assert!(saltadas.contains(&("ventas".to_string(), vu.clone())), "{rv}");
    assert!(saltadas.contains(&("venta_detalle".to_string(), du.clone())), "{rv}");
    assert_eq!(rv["celdas_no_restauradas"], json!(2), "{rv}");
    // Todo lo demás sí volvió (las ventas crudas de Dueño, menos la reescrita).
    assert_eq!(
        entero(p, &format!(
            "SELECT count(*) FROM ventas v WHERE v.uuid ~ '^[0-9a-f]{{32}}$' AND v.usuario_id = 1 \
              AND EXISTS (SELECT 1 FROM sync_cursor sc WHERE sc.tabla = 'ventas' AND sc.uuid = v.uuid AND sc.origen_device = '{DEV}')"
        )).await,
        raw_antes - 1
    );
    assert_eq!(entero(p, &format!("SELECT categoria_id FROM productos WHERE uuid = '{p2}'")).await, 5);
    assert_eq!(estado(p, "ventas", &vu).await.as_deref(), Some("traducido"));
    // Y la siguiente reparación pasa (antes: 409 para siempre).
    let r = sync_fk::reparar(p, DEV, false).await.unwrap();
    assert!(r.ok, "{}", r.reporte);
    assert_eq!(entero(p, &format!("SELECT usuario_id FROM ventas WHERE uuid = '{vu}'")).await, 2);
    c.cerrar().await;
}
