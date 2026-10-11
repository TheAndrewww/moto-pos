// Pruebas de POST /sync/purgar ("Eliminar venta de prueba" del escritorio)
// contra Postgres (copia fiel de producción). Submódulo de sync_v2_pg.rs:
// usa sus ayudas. Igual que allá, solo corren con SYNC_PG_URL.
//
//   p1  borra venta + detalle, devolución + detalle, movimiento de caja y una
//       venta web, con todo su rastro del sync (cursor, estado y pendientes
//       de FK, huellas, celdas web, mapa de ids) y las líneas de bitácora
//       que apuntan a lo borrado (son ventas de PRUEBA: no deben dejar
//       rastro); el resto de la bitácora queda idéntico, fila por fila; lo
//       ajeno no se toca; un pull ya no manda nada de lo borrado; repetir la
//       llamada no hace nada.
//   p2  rechaza tablas fuera de la lista (sin borrar nada), filas sin uuid,
//       falta de device_uuid y lotes de más de 500; una línea de bitácora
//       mandada por uuid se borra sola sin tocar las demás.
//   (La autorización HTTP está en g_autorizacion_sync, con los demás /sync/*.)

use super::*;

/// Tablas del rastro del sync por (tabla, uuid de la fila). Lista propia (no
/// RASTRO_SYNC) para que la prueba note si alguien quita una.
const RASTRO: &[(&str, &str)] = &[
    ("sync_cursor", "uuid"),
    ("sync_fk_estado", "uuid"),
    ("sync_fk_pendiente", "fila_uuid"),
    ("sync_push_aplicado", "uuid"),
    ("sync_fk_celda_web", "uuid"),
    ("sync_id_map", "uuid"),
];

/// Cuántas filas de rastro tiene (tabla, uuid), por tabla del rastro.
async fn rastro(p: &PgPool, tabla: &str, uuid: &str) -> Vec<(&'static str, i64)> {
    let mut out = Vec::new();
    for (t, col) in RASTRO {
        let n = entero(p, &format!("SELECT count(*) FROM {t} WHERE tabla = '{tabla}' AND {col} = '{uuid}'")).await;
        out.push((*t, n));
    }
    out
}

fn total(r: &[(&'static str, i64)]) -> i64 {
    r.iter().map(|(_, n)| n).sum()
}

fn cuenta(r: &[(&'static str, i64)], t: &str) -> i64 {
    r.iter().find(|(x, _)| *x == t).map(|(_, n)| *n).unwrap_or(0)
}

/// Huella de la bitácora SIN ciertas filas (las que la purga debe borrar).
async fn huella_bitacora_sin(p: &PgPool, fuera: &[&str]) -> (i64, String) {
    let lista = fuera.iter().map(|u| format!("'{u}'")).collect::<Vec<_>>().join(",");
    let filtro = if lista.is_empty() { String::new() } else { format!("WHERE uuid NOT IN ({lista})") };
    let n = entero(p, &format!("SELECT count(*) FROM audit_log {filtro}")).await;
    let h = texto(p, &format!("SELECT md5(string_agg(row_to_json(a)::text, '|' ORDER BY id)) FROM audit_log a {filtro}"))
        .await
        .unwrap_or_default();
    (n, h)
}

async fn existe(p: &PgPool, tabla: &str, uuid: &str) -> bool {
    entero(p, &format!("SELECT count(*) FROM {tabla} WHERE uuid = '{uuid}'")).await > 0
}

fn fila(tabla: &str, uuid: &str) -> FilaPurga {
    FilaPurga { tabla: tabla.into(), uuid: uuid.into() }
}

fn purga(filas: Vec<FilaPurga>) -> PurgarBody {
    PurgarBody { device_uuid: DEV.into(), filas }
}

/// Venta del escritorio con un renglón, empujada con v2 (traducida por refs).
async fn venta_empujada(p: &PgPool, id_local: i64, vu: &str, du: &str, prod_uuid: &str, ts: &str) {
    let mut rf = Map::new();
    rf.insert(vu.into(), json!({"usuario_id": DUENIO, "cliente_id": null, "anulada_por": null}));
    rf.insert(du.into(), json!({"producto_id": prod_uuid, "autorizado_por": null}));
    let r = procesar_push(
        p,
        &body(
            Some(2),
            vec![cambio(
                "ventas",
                venta_escritorio(id_local, vu, 1, ts),
                Some(json!({ "venta_detalle": [detalle(id_local + 500, du, id_local, 7777, ts)] })),
                Some(Value::Object(rf)),
            )],
        ),
    )
    .await
    .unwrap();
    assert_eq!(r.aceptados, vec![vu.to_string()], "{:?}", r.rechazados);
}

// ─────────────────────────────────────────────────────────────────────────
// p1) Purga completa: filas, hijos y rastro; bitácora intacta; idempotente.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn p1_purga_venta_devolucion_movimiento_y_su_rastro() {
    let Some(c) = ctx("purga1").await else { return };
    let p = &c.pool;
    let ts = ahora(p).await;
    let (_, prod_uuid) = producto_existente(p, 100).await;
    let c0 = entero(p, "SELECT max(id) FROM sync_cursor").await;

    // Venta de prueba V (con renglón) y venta ajena C que NO se borra.
    let (vu, du) = (hex32(), hex32());
    let (cu, cdu) = (hex32(), hex32());
    venta_empujada(p, 99101, &vu, &du, &prod_uuid, &ts).await;
    venta_empujada(p, 99102, &cu, &cdu, &prod_uuid, &ts).await;
    let vid = id_de(p, "ventas", &vu).await;

    // Retiro de caja de su reembolso (M) y la devolución (D) que lo usa. La
    // devolución lleva un `autorizado_por` que nunca llega → celda pendiente.
    let (mu, dvu, ddu) = (hex32(), hex32(), hex32());
    let mut rf = Map::new();
    rf.insert(mu.clone(), json!({"usuario_id": DUENIO, "autorizado_por": null, "corte_id": null}));
    let mov = json!({
        "id": 5101, "uuid": mu, "tipo": "RETIRO", "usuario_id": 1, "monto": 50.0,
        "concepto": "Reembolso devolución D-T1", "autorizado_por": null, "corte_id": null,
        "fecha": ts, "updated_at": ts, "deleted_at": null, "caja": "principal"
    });
    let r = procesar_push(p, &body(Some(2), vec![cambio("movimientos_caja", mov, None, Some(Value::Object(rf)))]))
        .await
        .unwrap();
    assert_eq!(r.aceptados, vec![mu.clone()], "{:?}", r.rechazados);
    let usuario_que_no_llega = hex32();
    let mut rf = Map::new();
    rf.insert(
        dvu.clone(),
        json!({"venta_id": vu, "usuario_id": DUENIO, "autorizado_por": usuario_que_no_llega, "movimiento_caja_id": mu}),
    );
    rf.insert(ddu.clone(), json!({"venta_detalle_id": du, "producto_id": prod_uuid}));
    let dev = json!({
        "id": 301, "uuid": dvu, "folio": "D-T1", "venta_id": 99101, "usuario_id": 1, "autorizado_por": 9,
        "motivo": "prueba", "total_devuelto": 50.0, "movimiento_caja_id": 5101,
        "fecha": ts, "updated_at": ts, "deleted_at": null
    });
    let dev_det = json!({
        "id": 401, "uuid": ddu, "devolucion_id": 301, "venta_detalle_id": 99601, "producto_id": 7777,
        "cantidad": 1.0, "precio_unitario": 50.0, "subtotal": 50.0, "updated_at": ts, "deleted_at": null
    });
    let r = procesar_push(
        p,
        &body(
            Some(2),
            vec![cambio("devoluciones", dev, Some(json!({"devolucion_detalle": [dev_det]})), Some(Value::Object(rf)))],
        ),
    )
    .await
    .unwrap();
    assert_eq!(r.aceptados, vec![dvu.clone()], "{:?}", r.rechazados);
    assert_eq!(r.faltantes, vec![usuario_que_no_llega.clone()]);
    assert_eq!(entero(p, &format!("SELECT venta_id FROM devoluciones WHERE uuid = '{dvu}'")).await, vid);

    // Venta web W (uuid de la web) con su renglón y su cursor.
    let (wu, wdu) = (format!("prueba-purga-{}", hex32()), format!("prueba-purga-{}", hex32()));
    let wid: i64 = sqlx::query_scalar(
        "INSERT INTO ventas (uuid, sucursal_id, folio, usuario_id, subtotal, total, metodo_pago, fecha, updated_at) \
         VALUES ($1, 1, 'W-PURGA', 2, 10, 10, 'efectivo', $2, $2) RETURNING id",
    )
    .bind(&wu)
    .bind(&ts)
    .fetch_one(p)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO venta_detalle (uuid, venta_id, producto_id, cantidad, precio_original, precio_final, subtotal) \
         VALUES ($1, $2, 100, 1, 10, 10, 10)",
    )
    .bind(&wdu)
    .bind(wid)
    .execute(p)
    .await
    .unwrap();
    sqlx::query("INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device) VALUES ('ventas', $1, 1, 'web-pos')")
        .bind(&wu)
        .execute(p)
        .await
        .unwrap();

    // Bitácora: una nota del escritorio que apunta a V (traducida por refs) y
    // una de la web; ninguna se debe tocar.
    let au = hex32();
    let mut rf = Map::new();
    rf.insert(au.clone(), json!({"usuario_id": DUENIO, "registro_id": vu}));
    let nota = json!({
        "id": 777101, "uuid": au, "usuario_id": 1, "accion": "VENTA", "tabla_afectada": "ventas",
        "registro_id": 99101, "datos_anteriores": null, "datos_nuevos": null,
        "descripcion_legible": "Venta de prueba para purgar", "origen": "POS", "fecha": ts,
        "updated_at": ts, "deleted_at": null
    });
    let r = procesar_push(p, &body(Some(2), vec![cambio("audit_log", nota, None, Some(Value::Object(rf)))]))
        .await
        .unwrap();
    assert_eq!(r.aceptados, vec![au.clone()], "{:?}", r.rechazados);
    assert_eq!(entero(p, &format!("SELECT registro_id FROM audit_log WHERE uuid = '{au}'")).await, vid);
    sqlx::query(
        "INSERT INTO audit_log (usuario_id, accion, tabla_afectada, registro_id, descripcion_legible, origen) \
         VALUES (2, 'VENTA', 'ventas', $1, 'Venta web para purgar', 'WEB')",
    )
    .bind(wid)
    .execute(p)
    .await
    .unwrap();

    // Rastro que el push no deja solo: celdas web y huellas (V, su renglón y C).
    for (t, u, col) in [("ventas", &vu, "cliente_id"), ("venta_detalle", &du, "autorizado_por"), ("ventas", &cu, "cliente_id")] {
        sqlx::query("INSERT INTO sync_fk_celda_web (tabla, uuid, columna) VALUES ($1, $2, $3)")
            .bind(t)
            .bind(u)
            .bind(col)
            .execute(p)
            .await
            .unwrap();
    }
    for u in [&vu, &cu] {
        sqlx::query("INSERT INTO sync_push_aplicado (device_uuid, tabla, uuid, huella) VALUES ($1, 'ventas', $2, 'huella-prueba')")
            .bind(DEV)
            .bind(u)
            .execute(p)
            .await
            .unwrap();
    }

    // ── Antes: todo existe y tiene rastro ──
    let rv = rastro(p, "ventas", &vu).await;
    for t in ["sync_cursor", "sync_fk_estado", "sync_push_aplicado", "sync_fk_celda_web", "sync_id_map"] {
        assert!(cuenta(&rv, t) > 0, "venta sin {t} antes de purgar: {rv:?}");
    }
    assert!(cuenta(&rastro(p, "venta_detalle", &du).await, "sync_id_map") > 0);
    assert!(cuenta(&rastro(p, "venta_detalle", &du).await, "sync_fk_celda_web") > 0);
    let rd = rastro(p, "devoluciones", &dvu).await;
    assert!(cuenta(&rd, "sync_fk_pendiente") > 0, "{rd:?}");
    assert_eq!(estado(p, "devoluciones", &dvu).await.as_deref(), Some("pendiente"));
    assert!(cuenta(&rastro(p, "devolucion_detalle", &ddu).await, "sync_id_map") > 0);
    assert!(cuenta(&rastro(p, "movimientos_caja", &mu).await, "sync_cursor") > 0);
    assert!(cuenta(&rastro(p, "ventas", &wu).await, "sync_cursor") > 0);
    let rastro_c = rastro(p, "ventas", &cu).await;
    let rastro_cd = rastro(p, "venta_detalle", &cdu).await;
    assert!(total(&rastro_c) > 0 && total(&rastro_cd) > 0);
    let au_web: String = texto(
        p,
        &format!("SELECT uuid FROM audit_log WHERE origen = 'WEB' AND tabla_afectada = 'ventas' AND registro_id = {wid}"),
    )
    .await
    .unwrap();
    let bitacora = huella_bitacora_sin(p, &[&au, &au_web]).await;
    let c_antes = texto(p, &format!("SELECT row_to_json(v)::text FROM ventas v WHERE uuid = '{cu}'")).await;
    let cd_antes = texto(p, &format!("SELECT row_to_json(d)::text FROM venta_detalle d WHERE uuid = '{cdu}'")).await;

    // ── Purga (con V repetida: cuenta una vez) ──
    let filas = || {
        vec![
            fila("ventas", &vu),
            fila("devoluciones", &dvu),
            fila("movimientos_caja", &mu),
            fila("ventas", &wu),
            fila("ventas", &vu),
        ]
    };
    let r = procesar_purgar(p, &purga(filas())).await.unwrap();
    assert_eq!(r["borrados"], json!(4), "{r}");
    assert_eq!(r["hijos_borrados"], json!(3), "{r}");
    assert_eq!(r["recibidos"], json!(4), "{r}");
    assert_eq!(
        r["por_tabla"],
        json!({"ventas": 2, "venta_detalle": 2, "devoluciones": 1, "devolucion_detalle": 1, "movimientos_caja": 1, "audit_log": 2}),
        "{r}"
    );

    // ── Después: ni filas ni rastro ──
    for (t, u) in [
        ("ventas", &vu),
        ("venta_detalle", &du),
        ("devoluciones", &dvu),
        ("devolucion_detalle", &ddu),
        ("movimientos_caja", &mu),
        ("ventas", &wu),
        ("venta_detalle", &wdu),
    ] {
        assert!(!existe(p, t, u).await, "{t} {u} sigue existiendo");
        let rs = rastro(p, t, u).await;
        assert_eq!(total(&rs), 0, "{t} {u} conserva rastro: {rs:?}");
    }
    assert_eq!(entero(p, &format!("SELECT count(*) FROM venta_detalle WHERE venta_id IN ({vid}, {wid})")).await, 0);

    // Las líneas de bitácora de lo borrado desaparecen (con su rastro); el
    // resto de la bitácora queda idéntico.
    for u in [&au, &au_web] {
        assert!(!existe(p, "audit_log", u).await, "la línea {u} sigue en la bitácora");
        assert_eq!(total(&rastro(p, "audit_log", u).await), 0, "la línea {u} conserva rastro");
    }
    assert_eq!(huella_bitacora_sin(p, &[]).await, bitacora, "cambió otra línea de la bitácora");

    // Lo ajeno no se toca.
    assert_eq!(texto(p, &format!("SELECT row_to_json(v)::text FROM ventas v WHERE uuid = '{cu}'")).await, c_antes);
    assert_eq!(texto(p, &format!("SELECT row_to_json(d)::text FROM venta_detalle d WHERE uuid = '{cdu}'")).await, cd_antes);
    assert_eq!(rastro(p, "ventas", &cu).await, rastro_c);
    assert_eq!(rastro(p, "venta_detalle", &cdu).await, rastro_cd);

    // Un pull (de otro equipo, que sí vería lo del escritorio) ya no manda
    // nada de lo borrado, ni como fila ni como DELETE; la venta ajena sí baja.
    let purgados = [&vu, &du, &dvu, &ddu, &mu, &wu, &wdu];
    let mut cursor = c0;
    let mut vistos = 0;
    let mut vio_c = false;
    loop {
        let pr = procesar_pull(
            p,
            &PullQuery { cursor: cursor.to_string(), sucursal_id: 1, device_uuid: Some("otro-equipo".into()), proto: Some(2) },
        )
        .await
        .unwrap();
        for cambio in &pr.cambios {
            let s = cambio.to_string();
            for u in purgados {
                assert!(!s.contains(u.as_str()), "el pull trae {u}: {s}");
            }
            vio_c |= cambio["__data"]["uuid"] == json!(cu);
            vistos += 1;
        }
        cursor = pr.next_cursor.parse().unwrap();
        if !pr.hay_mas {
            break;
        }
    }
    assert!(vio_c && vistos > 0, "el pull debía traer la venta ajena");

    // Idempotente: repetir no borra nada más ni toca la bitácora.
    let r = procesar_purgar(p, &purga(filas())).await.unwrap();
    assert_eq!((r["borrados"].clone(), r["hijos_borrados"].clone()), (json!(0), json!(0)), "{r}");
    assert_eq!(r["recibidos"], json!(4), "{r}");
    assert_eq!(huella_bitacora_sin(p, &[]).await, bitacora);
    assert_eq!(texto(p, &format!("SELECT row_to_json(v)::text FROM ventas v WHERE uuid = '{cu}'")).await, c_antes);
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// p2) Validación: nada fuera de la lista, nada sin uuid, máximo 500.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn p2_purga_rechaza_tablas_ajenas_y_lotes_invalidos() {
    let Some(c) = ctx("purga2").await else { return };
    let p = &c.pool;
    let ts = ahora(p).await;
    let (_, prod_uuid) = producto_existente(p, 100).await;
    let (vu, du) = (hex32(), hex32());
    venta_empujada(p, 99201, &vu, &du, &prod_uuid, &ts).await;
    let au: String = texto(p, "SELECT uuid FROM audit_log ORDER BY id DESC LIMIT 1").await.unwrap();

    let es_400 = |r: ApiResult<Value>, que: &str| match r {
        Err(ApiError::BadRequest(m)) => m,
        otro => panic!("{que}: se esperaba 400, llegó {otro:?}"),
    };

    // Una tabla ajena rechaza TODA la llamada: la venta válida tampoco se borra.
    for t in ["productos", "venta_detalle", "devolucion_detalle", "cortes", "usuarios", "sync_cursor"] {
        let m = es_400(procesar_purgar(p, &purga(vec![fila("ventas", &vu), fila(t, &du)])).await, t);
        assert!(m.contains("tabla no purgable"), "{m}");
        assert!(existe(p, "ventas", &vu).await && existe(p, "venta_detalle", &du).await, "{t}");
    }
    assert!(existe(p, "audit_log", &au).await);
    assert!(total(&rastro(p, "ventas", &vu).await) > 0);

    es_400(procesar_purgar(p, &purga(vec![fila("ventas", &vu), fila("ventas", "  ")])).await, "uuid vacío");
    es_400(
        procesar_purgar(p, &PurgarBody { device_uuid: " ".into(), filas: vec![fila("ventas", &vu)] }).await,
        "sin device_uuid",
    );
    let muchas: Vec<FilaPurga> = (0..501).map(|_| fila("ventas", &hex32())).collect();
    es_400(procesar_purgar(p, &purga(muchas)).await, "501 filas");
    assert!(existe(p, "ventas", &vu).await);

    // 500 exactas pasan (las que no existen cuentan como hechas).
    let mut quinientas: Vec<FilaPurga> = (0..499).map(|_| fila("movimientos_caja", &hex32())).collect();
    quinientas.push(fila("ventas", &vu));
    let r = procesar_purgar(p, &purga(quinientas)).await.unwrap();
    assert_eq!((r["borrados"].clone(), r["hijos_borrados"].clone(), r["recibidos"].clone()), (json!(1), json!(1), json!(500)));
    assert!(!existe(p, "ventas", &vu).await && !existe(p, "venta_detalle", &du).await);

    // Lote vacío: nada que hacer.
    let r = procesar_purgar(p, &purga(vec![])).await.unwrap();
    assert_eq!(r["borrados"], json!(0));

    // Una línea de bitácora mandada por uuid (el escritorio manda las de la
    // venta eliminada) se borra sola; las demás quedan idénticas.
    let otra: String = texto(p, &format!("SELECT uuid FROM audit_log WHERE uuid <> '{au}' ORDER BY id DESC LIMIT 1")).await.unwrap();
    let resto = huella_bitacora_sin(p, &[&otra]).await;
    let r = procesar_purgar(p, &purga(vec![fila("audit_log", &otra)])).await.unwrap();
    assert_eq!(r["borrados"], json!(1), "{r}");
    assert!(!existe(p, "audit_log", &otra).await);
    assert_eq!(huella_bitacora_sin(p, &[]).await, resto, "cambió otra línea de la bitácora");
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// p3) Eliminaciones pedidas desde la web: solo el escritorio las lee y las
//     confirma; aplicada o purgada → desaparece; rechazada → queda el motivo.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn p3_solicitudes_de_eliminar_venta_de_la_web() {
    let Some(c) = ctx("purga3").await else { return };
    let p = &c.pool;
    let hash = bcrypt::hash("clave-de-prueba", 4).unwrap();
    sqlx::query("INSERT INTO admin_users (email, password_hash, nombre, sucursal_id) VALUES ('p3@prueba.local', $1, 'Prueba', 1)")
        .bind(&hash)
        .execute(p)
        .await
        .unwrap();
    let a = app(p.clone());
    let (st, login) = llamar(&a, "POST", "/auth/login", None, Some(json!({"email": "p3@prueba.local", "password": "clave-de-prueba"}))).await;
    assert_eq!(st, StatusCode::OK, "{login}");
    let tok = login["token"].as_str().unwrap().to_string();

    let ts = ahora(p).await;
    let (_, prod_uuid) = producto_existente(p, 100).await;
    let (vu, du) = (hex32(), hex32());
    venta_empujada(p, 99301, &vu, &du, &prod_uuid, &ts).await;
    let (vu2, du2) = (hex32(), hex32());
    venta_empujada(p, 99302, &vu2, &du2, &prod_uuid, &ts).await;
    let fantasma = hex32();
    for (venta, folio) in [(&vu, "V-P3-1"), (&vu2, "V-P3-2"), (&fantasma, "V-NO-EXISTE")] {
        sqlx::query(
            "INSERT INTO solicitudes_eliminar_venta (venta_uuid, venta_folio, solicitado_por_uuid) \
             VALUES ($1, $2, 'c088a77266563a58935e396a1cb55d27')",
        )
        .bind(venta)
        .bind(folio)
        .execute(p)
        .await
        .unwrap();
    }

    // Un token web no las puede leer ni confirmar.
    let web = crate::auth::emitir_token_con_device(SECRETO, 4, "Mariana", "device", 1, Some("web-dev-1".into()), chrono::Duration::days(7)).unwrap();
    assert_eq!(llamar(&a, "GET", "/sync/solicitudes", Some(&web), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        llamar(&a, "POST", "/sync/solicitudes/resultado", Some(&web), Some(json!({"resultados": []}))).await.0,
        StatusCode::FORBIDDEN
    );

    // El escritorio las lee; la de una venta que ya no existe se descarta sola.
    let (st, r) = llamar(&a, "GET", "/sync/solicitudes", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let lista = r["solicitudes"].as_array().unwrap().clone();
    let ventas: Vec<&str> = lista.iter().map(|s| s["venta_uuid"].as_str().unwrap()).collect();
    assert_eq!(ventas, vec![vu.as_str(), vu2.as_str()], "{r}");
    assert_eq!(lista[0]["solicitado_por_uuid"], json!("c088a77266563a58935e396a1cb55d27"));
    assert_eq!(entero(p, &format!("SELECT count(*) FROM solicitudes_eliminar_venta WHERE venta_uuid = '{fantasma}'")).await, 0);
    let s1 = lista[0]["uuid"].as_str().unwrap().to_string();
    let s2 = lista[1]["uuid"].as_str().unwrap().to_string();

    // Rechazada: queda con el motivo y ya no se ofrece. Aplicada: se borra.
    let (st, r) = llamar(
        &a,
        "POST",
        "/sync/solicitudes/resultado",
        Some(&tok),
        Some(json!({"resultados": [
            {"uuid": s1, "aplicada": false, "error": "motivo de prueba"},
            {"uuid": s2, "aplicada": true}
        ]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!((r["aplicadas"].clone(), r["rechazadas"].clone()), (json!(1), json!(1)), "{r}");
    assert_eq!(
        texto(p, &format!("SELECT estado || '|' || error FROM solicitudes_eliminar_venta WHERE uuid = '{s1}'")).await.as_deref(),
        Some("rechazada|motivo de prueba")
    );
    assert_eq!(entero(p, &format!("SELECT count(*) FROM solicitudes_eliminar_venta WHERE uuid = '{s2}'")).await, 0);
    let (_, r) = llamar(&a, "GET", "/sync/solicitudes", Some(&tok), None).await;
    assert_eq!(r["solicitudes"], json!([]), "la rechazada ya no se ofrece");

    // Otra vez pendiente y el escritorio la aplica: al purgar la venta, la
    // solicitud desaparece aunque no llegue la confirmación.
    sqlx::query("UPDATE solicitudes_eliminar_venta SET estado = 'pendiente', error = NULL WHERE uuid = $1")
        .bind(&s1)
        .execute(p)
        .await
        .unwrap();
    procesar_purgar(p, &purga(vec![fila("ventas", &vu)])).await.unwrap();
    assert_eq!(entero(p, &format!("SELECT count(*) FROM solicitudes_eliminar_venta WHERE uuid = '{s1}'")).await, 0);
    c.cerrar().await;
}
