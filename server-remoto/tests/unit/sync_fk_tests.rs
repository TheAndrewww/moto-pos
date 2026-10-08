// Pruebas unitarias (sin BD) de src/sync_fk.rs: decisiones de traducción de
// FK, reloj LWW, filtro de columnas y limpieza del pull para clientes viejos.
// Se incluye como módulo desde sync_fk.rs (`#[path]`), por eso usa `super::*`.

use super::*;
use serde_json::json;

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

// ── Reloj ────────────────────────────────────────────────────────────────

#[test]
fn normaliza_marcas() {
    assert_eq!(normalizar_ts("2026-10-06T19:38:03.123-06:00"), "2026-10-06 19:38:03");
    assert_eq!(normalizar_ts("2026-10-06 19:38:03"), "2026-10-06 19:38:03");
    assert_eq!(normalizar_ts("2026-10-06"), "2026-10-06");
    assert_eq!(normalizar_ts(""), "");
}

#[test]
fn lww_igual_aplica_y_mas_viejo_rechaza() {
    let ahora = "2026-10-06 20:00:00";
    // Igual → aplicar (antes se rechazaba para siempre).
    assert_eq!(
        decidir_lww(Some("2026-10-06 19:00:00"), Some("2026-10-06 19:00:00"), ahora),
        Lww::Aplicar(Some("2026-10-06 19:00:00".into()))
    );
    // Más nuevo → aplicar.
    assert_eq!(
        decidir_lww(Some("2026-10-06 19:00:00"), Some("2026-10-06 19:00:01"), ahora),
        Lww::Aplicar(Some("2026-10-06 19:00:01".into()))
    );
    // Guardado estrictamente más nuevo → rechazar.
    assert_eq!(
        decidir_lww(Some("2026-10-06 19:00:01"), Some("2026-10-06 19:00:00"), ahora),
        Lww::Rechazar
    );
    // Sin fila guardada → aplicar.
    assert_eq!(
        decidir_lww(None, Some("2026-10-06 19:00:00"), ahora),
        Lww::Aplicar(Some("2026-10-06 19:00:00".into()))
    );
}

#[test]
fn lww_normaliza_formatos() {
    let ahora = "2026-10-06 20:00:00";
    // 'T' y fracciones no cuentan: es la misma marca → aplicar, y se escribe normalizada.
    assert_eq!(
        decidir_lww(Some("2026-10-06 19:00:00"), Some("2026-10-06T19:00:00.999Z"), ahora),
        Lww::Aplicar(Some("2026-10-06 19:00:00".into()))
    );
    assert_eq!(
        decidir_lww(Some("2026-10-06T19:00:05Z"), Some("2026-10-06 19:00:00"), ahora),
        Lww::Rechazar
    );
}

#[test]
fn lww_tope_en_ahora() {
    let ahora = "2026-10-06 20:00:00";
    // Marca en el futuro (escritorio en UTC, 6 h adelante) → se recorta a ahora.
    assert_eq!(
        decidir_lww(Some("2026-10-06 19:00:00"), Some("2026-10-07 02:00:00"), ahora),
        Lww::Aplicar(Some(ahora.into()))
    );
    // Recortada a ahora, sigue perdiendo contra algo guardado aún más adelante.
    assert_eq!(
        decidir_lww(Some("2026-10-06 21:00:00"), Some("2026-10-07 02:00:00"), ahora),
        Lww::Rechazar
    );
    // Recortada a ahora e igual a lo guardado → aplica.
    assert_eq!(
        decidir_lww(Some(ahora), Some("2099-01-01 00:00:00"), ahora),
        Lww::Aplicar(Some(ahora.into()))
    );
}

#[test]
fn lww_sin_marca_aplica_sin_tocar() {
    assert_eq!(decidir_lww(Some("2026-10-06 19:00:00"), None, "2026-10-06 20:00:00"), Lww::Aplicar(None));
    assert_eq!(decidir_lww(Some("2026-10-06 19:00:00"), Some(""), "2026-10-06 20:00:00"), Lww::Aplicar(None));
}

// ── Traducción de FK ─────────────────────────────────────────────────────

fn busquedas() -> Busquedas {
    let mut b = Busquedas::default();
    b.por_uuid.insert(("usuarios".into(), "u-duenio".into()), 2);
    b.por_uuid.insert(("productos".into(), "p-1".into()), 501);
    b.por_uuid.insert(("ventas".into(), "v-1".into()), 9001);
    // Mapa del escritorio: usuario local 1 → u-duenio (id 2 aquí).
    b.por_mapa.insert(("usuarios".into(), 1), ("u-duenio".into(), Some(2)));
    // Producto local 7002 → uuid conocido por el mapa pero aún no está aquí.
    b.por_mapa.insert(("productos".into(), 7002), ("p-nuevo".into(), None));
    b
}

fn columnas(defs: &[(&str, &str, bool, bool)]) -> ColumnasTabla {
    defs.iter()
        .map(|(c, t, nullable, def)| (c.to_string(), ColInfo::nueva(t, *nullable, *def)))
        .collect()
}

#[test]
fn celda_por_refs() {
    let b = busquedas();
    let data = obj(json!({"usuario_id": 1, "cliente_id": 5, "anulada_por": 3}));
    let refs = obj(json!({"usuario_id": "u-duenio", "cliente_id": null, "anulada_por": "u-no-existe"}));
    // uuid encontrado → id de aquí.
    assert_eq!(decidir_celda("usuarios", "usuario_id", &data, Some(&refs), &b), Some(Celda::Id(2)));
    // null en refs → NULL aunque la fila traiga un valor crudo.
    assert_eq!(decidir_celda("clientes", "cliente_id", &data, Some(&refs), &b), Some(Celda::Nula));
    // uuid que no existe aquí → pendiente con el uuid y el id crudo.
    assert_eq!(
        decidir_celda("usuarios", "anulada_por", &data, Some(&refs), &b),
        Some(Celda::Pendiente { ref_tabla: "usuarios", ref_uuid: Some("u-no-existe".into()), raw: Some(3) })
    );
}

#[test]
fn celda_por_mapa_sin_refs() {
    let b = busquedas();
    let data = obj(json!({"usuario_id": 1, "producto_id": 7002, "otro": 99, "nulo": null, "vacio": ""}));
    assert_eq!(decidir_celda("usuarios", "usuario_id", &data, None, &b), Some(Celda::Id(2)));
    // En el mapa pero la fila referida aún no llega → pendiente con su uuid.
    assert_eq!(
        decidir_celda("productos", "producto_id", &data, None, &b),
        Some(Celda::Pendiente { ref_tabla: "productos", ref_uuid: Some("p-nuevo".into()), raw: Some(7002) })
    );
    // Sin entrada en el mapa → pendiente solo con el id crudo.
    assert_eq!(
        decidir_celda("usuarios", "otro", &data, None, &b),
        Some(Celda::Pendiente { ref_tabla: "usuarios", ref_uuid: None, raw: Some(99) })
    );
    assert_eq!(decidir_celda("usuarios", "nulo", &data, None, &b), Some(Celda::Nula));
    assert_eq!(decidir_celda("usuarios", "vacio", &data, None, &b), Some(Celda::Nula));
    // La columna no viene ni en la fila ni en refs → no se toca.
    assert_eq!(decidir_celda("usuarios", "no_viene", &data, None, &b), None);
}

#[test]
fn celda_refs_sin_la_llave_usa_respaldo() {
    let b = busquedas();
    let data = obj(json!({"usuario_id": 1}));
    // refs presente pero sin la columna, o con un valor inservible → mapa.
    let refs = obj(json!({"otra": "x"}));
    assert_eq!(decidir_celda("usuarios", "usuario_id", &data, Some(&refs), &b), Some(Celda::Id(2)));
    let refs = obj(json!({"usuario_id": 5}));
    assert_eq!(decidir_celda("usuarios", "usuario_id", &data, Some(&refs), &b), Some(Celda::Id(2)));
    let refs = obj(json!({"usuario_id": ""}));
    assert_eq!(decidir_celda("usuarios", "usuario_id", &data, Some(&refs), &b), Some(Celda::Id(2)));
}

#[test]
fn traducir_venta_y_detalle() {
    let b = busquedas();
    let cols_v = columnas(&[
        ("usuario_id", "bigint", false, false),
        ("cliente_id", "bigint", true, false),
        ("anulada_por", "bigint", true, false),
        ("sucursal_id", "bigint", false, false),
    ]);
    let mut venta = obj(json!({"uuid": "v-x", "usuario_id": 1, "cliente_id": 8, "anulada_por": null, "sucursal_id": 1}));
    let refs = obj(json!({"cliente_id": "c-no-existe"}));
    let pend = traducir_fila("ventas", "v-x", &mut venta, Some(&refs), &b, Some(&cols_v), None);
    assert_eq!(venta["usuario_id"], json!(2)); // por mapa
    assert_eq!(venta["cliente_id"], Value::Null); // pendiente en columna que acepta NULL
    assert_eq!(venta["anulada_por"], Value::Null);
    assert_eq!(venta["sucursal_id"], json!(1)); // Paso: se copia
    assert_eq!(pend.len(), 1);
    assert_eq!(pend[0].columna, "cliente_id");
    assert_eq!(pend[0].ref_tabla, "clientes");
    assert_eq!(pend[0].ref_uuid.as_deref(), Some("c-no-existe"));
    assert_eq!(pend[0].raw, Some(8));

    let cols_d = columnas(&[
        ("venta_id", "bigint", false, false),
        ("producto_id", "bigint", false, false),
        ("autorizado_por", "bigint", true, false),
    ]);
    let mut det = obj(json!({"uuid": "d-1", "venta_id": 77, "producto_id": 7002, "autorizado_por": 1}));
    let pend = traducir_fila("venta_detalle", "d-1", &mut det, None, &b, Some(&cols_d), Some(9001));
    assert_eq!(det["venta_id"], json!(9001)); // Padre → id del padre aquí
    assert_eq!(det["producto_id"], json!(0)); // pendiente NOT NULL → centinela 0
    assert_eq!(det["autorizado_por"], json!(2));
    assert_eq!(pend.len(), 1);
    assert_eq!(pend[0].tabla, "venta_detalle");
    assert_eq!(pend[0].fila_uuid, "d-1");
    assert_eq!(pend[0].ref_uuid.as_deref(), Some("p-nuevo"));
}

#[test]
fn traducir_polimorfico_audit() {
    let b = busquedas();
    let mut a = obj(json!({"uuid": "a1", "origen": "POS", "usuario_id": 1, "tabla_afectada": "ventas", "registro_id": 44}));
    let refs = obj(json!({"registro_id": "v-1"}));
    let pend = traducir_fila("audit_log", "a1", &mut a, Some(&refs), &b, None, None);
    assert!(pend.is_empty());
    assert_eq!(a["registro_id"], json!(9001));
    assert_eq!(a["usuario_id"], json!(2));
    // tabla_afectada que no viaja (sesiones) → registro_id se copia tal cual.
    let mut s = obj(json!({"uuid": "a2", "usuario_id": 1, "tabla_afectada": "sesiones", "registro_id": 44}));
    let pend = traducir_fila("audit_log", "a2", &mut s, None, &b, None, None);
    assert!(pend.is_empty());
    assert_eq!(s["registro_id"], json!(44));
}

#[test]
fn recolecta_pedidos() {
    let data = obj(json!({"usuario_id": 1, "cliente_id": null, "anulada_por": 3, "sucursal_id": 1}));
    let refs = obj(json!({"anulada_por": "u-x"}));
    let mut p = recolectar_pedidos("ventas", &data, Some(&refs));
    p.sort_by_key(|x| format!("{x:?}"));
    assert_eq!(p, vec![Pedido::PorMapa("usuarios", 1), Pedido::PorUuid("usuarios", "u-x".into())]);
}

// ── Filtro de columnas y bind tipado ─────────────────────────────────────

fn cols_mov() -> ColumnasTabla {
    columnas(&[
        ("uuid", "text", false, false),
        ("concepto", "text", false, false),
        ("nota", "text", true, false),
        ("monto", "numeric", false, false),
        ("usuario_id", "bigint", false, false),
        ("corte_id", "bigint", true, false),
        ("activo", "integer", false, true),
        ("origen", "text", false, true),
        ("updated_at", "text", false, true),
    ])
}

#[test]
fn filtro_descarta_llaves_desconocidas_y_maliciosas() {
    let data = obj(json!({
        "id": 5, "synced_at": "x", "uuid": "m1", "concepto": "Retiro",
        "foo": 1, "nombre) VALUES (1)--": "x", "__refs": {}, "deleted": true
    }));
    let (cols, descartadas) = preparar_columnas(&cols_mov(), &data).unwrap();
    let nombres: Vec<&str> = cols.iter().map(|(c, _)| c.as_str()).collect();
    assert!(nombres.contains(&"uuid") && nombres.contains(&"concepto"));
    assert!(!nombres.contains(&"id") && !nombres.contains(&"synced_at") && !nombres.contains(&"__refs"));
    let mut d = descartadas.clone();
    d.sort();
    assert_eq!(d, vec!["deleted", "foo", "nombre) VALUES (1)--"]);
}

#[test]
fn filtro_tipos_y_nulos() {
    let data = obj(json!({
        "uuid": "m1",
        "concepto": "",          // NOT NULL texto: '' se queda ''
        "nota": "",              // texto que acepta NULL: '' → NULL
        "monto": 15.1,           // numeric exacto
        "usuario_id": 5.0,       // entero que llegó como REAL
        "corte_id": "",          // FK opcional como '' → NULL
        "activo": null,          // NOT NULL con default → se omite
        "origen": "desktop",
    }));
    let (cols, _) = preparar_columnas(&cols_mov(), &data).unwrap();
    let m: HashMap<&str, &ValorPg> = cols.iter().map(|(c, v)| (c.as_str(), v)).collect();
    assert_eq!(m["concepto"], &ValorPg::Texto(Some(String::new())));
    assert_eq!(m["nota"], &ValorPg::Texto(None));
    assert_eq!(m["monto"], &ValorPg::Numerico(Some(Decimal::from_str("15.1").unwrap())));
    assert_eq!(m["usuario_id"], &ValorPg::Entero(Some(5)));
    assert_eq!(m["corte_id"], &ValorPg::Entero(None));
    assert!(!m.contains_key("activo"));
    assert_eq!(m["origen"], &ValorPg::Texto(Some("desktop".into())));
}

#[test]
fn filtro_convierte_textos_numericos_y_rechaza_basura() {
    let info_num = ColInfo::nueva("numeric", false, true);
    assert_eq!(convertir_valor(&info_num, &json!("12.50")).unwrap(), ValorPg::Numerico(Some(Decimal::from_str("12.50").unwrap())));
    assert_eq!(convertir_valor(&info_num, &json!(1e-7)).unwrap(), ValorPg::Numerico(Some(Decimal::from_str("0.0000001").unwrap())));
    let info_int = ColInfo::nueva("integer", false, true);
    assert_eq!(convertir_valor(&info_int, &json!("7")).unwrap(), ValorPg::Entero(Some(7)));
    assert_eq!(convertir_valor(&info_int, &json!(true)).unwrap(), ValorPg::Entero(Some(1)));
    assert!(convertir_valor(&info_int, &json!("abc")).is_err());
    let info_txt = ColInfo::nueva("text", true, false);
    assert_eq!(convertir_valor(&info_txt, &json!(123)).unwrap(), ValorPg::Texto(Some("123".into())));
    assert_eq!(convertir_valor(&info_txt, &json!("a\0b")).unwrap(), ValorPg::Texto(Some("ab".into())));
    let info_otro = ColInfo::nueva("timestamp with time zone", true, false);
    assert_eq!(info_otro.tipo, TipoCol::Otro);
}

#[test]
fn upsert_no_toca_origen_ni_caja() {
    let cols = columnas(&[
        ("uuid", "text", false, false),
        ("total", "numeric", false, true),
        ("origen", "text", false, true),
        ("caja", "text", false, true),
    ]);
    let filas = vec![
        ("uuid".to_string(), ValorPg::Texto(Some("v".into()))),
        ("total".to_string(), ValorPg::Numerico(Some(Decimal::from(3)))),
        ("origen".to_string(), ValorPg::Texto(Some("web".into()))),
        ("caja".to_string(), ValorPg::Texto(Some("web".into()))),
    ];
    let sql = sql_upsert("ventas", &filas, &cols);
    assert!(sql.contains("ON CONFLICT (uuid) DO UPDATE SET \"total\"=EXCLUDED.\"total\" RETURNING id"), "{sql}");
    assert!(!sql.contains("\"origen\"=EXCLUDED"));
    assert!(!sql.contains("\"caja\"=EXCLUDED"));
    assert!(!sql.contains("\"uuid\"=EXCLUDED"));
    // Solo columnas inmutables → DO NOTHING.
    let sql = sql_upsert("ventas", &filas[..1], &cols);
    assert!(sql.contains("DO NOTHING RETURNING id"), "{sql}");
    // Tipo no manejado → CAST.
    let cols2 = columnas(&[("uuid", "text", false, false), ("t", "timestamp with time zone", true, false)]);
    let sql = sql_upsert("x", &[("uuid".into(), ValorPg::Texto(Some("u".into()))), ("t".into(), ValorPg::Otro(Some("2026-01-01".into())))], &cols2);
    assert!(sql.contains("CAST($2 AS timestamp with time zone)"), "{sql}");
}

// ── Pull: idioma de las celdas y limpieza para clientes viejos ───────────

#[test]
fn significado_y_limpieza_legacy() {
    // Filas de la web: siempre en idioma del servidor.
    assert!(en_significado_servidor(false, None));
    // Del escritorio: solo traducidas o reparadas.
    assert!(en_significado_servidor(true, Some("traducido")));
    assert!(en_significado_servidor(true, Some("reparado")));
    assert!(!en_significado_servidor(true, Some("pendiente")));
    assert!(!en_significado_servidor(true, Some("sin_mapa")));
    assert!(!en_significado_servidor(true, None));
    // Cliente viejo: se quitan las FK de filas del escritorio con cualquier estado.
    assert!(quitar_fks_para_legacy(true, Some("traducido")));
    assert!(quitar_fks_para_legacy(true, Some("pendiente")));
    assert!(!quitar_fks_para_legacy(true, None));
    assert!(!quitar_fks_para_legacy(false, Some("traducido")));
}

#[test]
fn caja_web_para_legacy() {
    let web = "019dd079-a8fb-7bd0-88ab-999a2d1be862";
    let esc = "88f9dc7342f620bcb56e301e436fb727";
    for t in ["ventas", "cortes", "movimientos_caja", "aperturas_caja"] {
        assert!(es_caja_web_legacy(t, web), "{t}");
        assert!(!es_caja_web_legacy(t, esc), "{t}");
    }
    assert!(!es_caja_web_legacy("productos", web));
    assert!(!es_caja_web_legacy("devoluciones", web));
}

#[test]
fn quita_fks_de_venta_y_audit() {
    let mut v = obj(json!({"uuid": "v", "usuario_id": 2, "cliente_id": null, "anulada_por": 3, "sucursal_id": 1, "total": 5}));
    quitar_fks("ventas", &mut v);
    assert!(!v.contains_key("usuario_id") && !v.contains_key("cliente_id") && !v.contains_key("anulada_por"));
    assert_eq!(v["sucursal_id"], json!(1));
    assert_eq!(v["total"], json!(5));
    let mut d = obj(json!({"uuid": "d", "venta_id": 9, "producto_id": 4, "autorizado_por": null}));
    quitar_fks("venta_detalle", &mut d);
    assert_eq!(d["venta_id"], json!(9)); // Padre se queda
    assert!(!d.contains_key("producto_id") && !d.contains_key("autorizado_por"));
    let mut a = obj(json!({"uuid": "a", "usuario_id": 1, "tabla_afectada": "sesiones", "registro_id": 7}));
    quitar_fks("audit_log", &mut a);
    assert!(!a.contains_key("usuario_id"));
    assert_eq!(a["registro_id"], json!(7));
    let mut a = obj(json!({"uuid": "a", "usuario_id": 1, "tabla_afectada": "ventas", "registro_id": 7}));
    quitar_fks("audit_log", &mut a);
    assert!(!a.contains_key("registro_id"));
}

#[test]
fn refs_para_pull() {
    let mut ids = HashMap::new();
    ids.insert(("usuarios".to_string(), 2i64), "u-duenio".to_string());
    let v = obj(json!({"usuario_id": 2, "cliente_id": null, "anulada_por": 0, "sucursal_id": 1}));
    let r = refs_de_fila("ventas", &v, &ids, None);
    assert_eq!(r.get("usuario_id"), Some(&json!("u-duenio")));
    assert_eq!(r.get("cliente_id"), Some(&Value::Null));
    // Centinela 0 / fila inexistente → la llave se omite ("no sé").
    assert!(!r.contains_key("anulada_por"));
    // Paso nunca viaja en refs.
    assert!(!r.contains_key("sucursal_id"));
    assert_eq!(pedidos_refs("ventas", &v), vec![("usuarios", 2), ("usuarios", 0)]);
}

#[test]
fn fila_escritorio_por_uuid_o_origen() {
    let vacio = Map::new();
    assert!(es_fila_escritorio("ventas", "88f9dc7342f620bcb56e301e436fb727", &vacio));
    assert!(!es_fila_escritorio("ventas", "019dd079-a8fb-7bd0-88ab-999a2d1be862", &vacio));
    // audit_log: el servidor también genera 32 hex → manda `origen`.
    let pos = obj(json!({"origen": "POS"}));
    let web = obj(json!({"origen": "WEB"}));
    assert!(es_fila_escritorio("audit_log", "019dd079-a8fb-7bd0-88ab-999a2d1be862", &pos));
    assert!(!es_fila_escritorio("audit_log", "88f9dc7342f620bcb56e301e436fb727", &web));
}

#[test]
fn tablas_con_fks_traducibles() {
    assert!(!tiene_fks_traducibles("categorias"));
    assert!(!tiene_fks_traducibles("usuarios")); // rol_id es Paso
    assert!(tiene_fks_traducibles("ventas"));
    assert!(tiene_fks_traducibles("productos"));
    assert!(tiene_fks_traducibles("audit_log"));
    assert_eq!(padre_de("venta_detalle"), Some(("ventas", "venta_id")));
    assert_eq!(padre_de("ventas"), None);
}

// ── Reparación: forma de las sentencias ──────────────────────────────────

#[test]
fn celdas_de_la_reparacion() {
    let c = celdas_reparables();
    assert!(c.iter().any(|x| x.tabla == "ventas" && x.col == "usuario_id" && x.ref_tabla == "usuarios" && x.padre.is_none()));
    assert!(c.iter().any(|x| x.tabla == "venta_detalle" && x.col == "producto_id" && x.padre == Some(("ventas", "venta_id"))));
    assert!(c.iter().any(|x| x.tabla == "devoluciones" && x.col == "movimiento_caja_id" && x.ref_tabla == "movimientos_caja"));
    assert!(!c.iter().any(|x| x.tabla == "stock_sucursal"));
    // Padre (venta_detalle.venta_id) y Paso (sucursal_id) nunca se reparan.
    assert!(!c.iter().any(|x| x.col == "venta_id" && x.tabla == "venta_detalle"));
    assert!(!c.iter().any(|x| x.col == "sucursal_id"));
    let reg: Vec<_> = c.iter().filter(|x| x.tabla == "audit_log" && x.col == "registro_id").collect();
    assert_eq!(reg.len(), AUDIT_TABLAS_TRADUCIBLES.len());
    let padres = tablas_padre_reparables();
    assert!(padres.contains(&"ventas") && padres.contains(&"audit_log") && padres.contains(&"cortes"));
    assert!(!padres.contains(&"venta_detalle") && !padres.contains(&"usuarios"));
}

#[test]
fn sentencia_de_reparacion_no_encadena() {
    let c = celdas_reparables().into_iter().find(|x| x.tabla == "ventas" && x.col == "usuario_id").unwrap();
    let sql = sql_traducir(&c);
    // Valor nuevo desde el ORIGINAL, y solo si la celda sigue igual.
    assert!(sql.contains("t.usuario_id AS viejo"), "{sql}");
    assert!(sql.contains("x.usuario_id = c.viejo AND c.viejo <> c.nuevo"), "{sql}");
    assert!(sql.contains("m.tabla = 'usuarios'"), "{sql}");
    assert!(sql.contains("t.uuid ~ '^[0-9a-f]{32}$'"), "{sql}");
    assert!(sql.contains("e.estado <> 'sin_mapa'"), "{sql}");
    assert!(sql.contains("sync_fk_repair_log l WHERE l.tabla = 'ventas'"), "{sql}");
    assert!(!sql.contains("updated_at"), "{sql}");
    // Hijas: alcance por el padre.
    let h = celdas_reparables().into_iter().find(|x| x.tabla == "venta_detalle" && x.col == "producto_id").unwrap();
    let sql = sql_traducir(&h);
    assert!(sql.contains("venta_detalle t JOIN ventas p ON p.id = t.venta_id"), "{sql}");
    assert!(sql.contains("p.uuid ~ '^[0-9a-f]{32}$'"), "{sql}");
    // audit_log: por origen y por tabla_afectada.
    let a = celdas_reparables()
        .into_iter()
        .find(|x| x.tabla == "audit_log" && x.col == "registro_id" && x.ref_tabla == "ventas")
        .unwrap();
    let sql = sql_traducir(&a);
    assert!(sql.contains("t.origen = 'POS'") && sql.contains("t.tabla_afectada = 'ventas'"), "{sql}");
}

// ═════════════════════════════════════════════════════════════════════════
// Ronda 2 (SPEC2 R1)
// ═════════════════════════════════════════════════════════════════════════

fn cols_venta() -> ColumnasTabla {
    columnas(&[
        ("uuid", "text", false, false),
        ("usuario_id", "bigint", false, false),
        ("cliente_id", "bigint", true, false),
        ("anulada_por", "bigint", true, false),
        ("sucursal_id", "bigint", false, false),
    ])
}

#[test]
fn marca_efectiva_normaliza_y_recorta() {
    let ahora = "2026-10-06 20:00:00";
    assert_eq!(marca_efectiva(Some("2026-10-06T19:00:00.5Z"), ahora).as_deref(), Some("2026-10-06 19:00:00"));
    assert_eq!(marca_efectiva(Some("2099-01-01 00:00:00"), ahora).as_deref(), Some(ahora));
    assert_eq!(marca_efectiva(Some(""), ahora), None);
    assert_eq!(marca_efectiva(None, ahora), None);
}

#[test]
fn politica_por_protocolo_origen_y_estado() {
    let traducir = Politica::Traducir { pendiente_en_existente: true };
    // v2: siempre traducir (con pendiente en filas existentes).
    assert_eq!(politica_de(true, true, None), traducir);
    assert_eq!(politica_de(true, false, None), traducir);
    // Escritorio viejo.
    assert_eq!(politica_de(false, false, None), Politica::Quitar, "fila web");
    assert_eq!(politica_de(false, false, Some("traducido")), Politica::Quitar);
    assert_eq!(politica_de(false, true, None), Politica::Crudo, "escritorio sin estado");
    assert_eq!(politica_de(false, true, Some("sin_mapa")), Politica::SinMapa { traducidas: HashSet::new() });
    for e in ["traducido", "reparado", "pendiente"] {
        assert_eq!(politica_de(false, true, Some(e)), Politica::Traducir { pendiente_en_existente: false }, "{e}");
    }
    // Hijos.
    assert_eq!(politica_hijo(&Politica::Quitar, true, HashSet::new()), Politica::Quitar);
    assert_eq!(
        politica_hijo(&Politica::Quitar, false, HashSet::new()),
        Politica::Traducir { pendiente_en_existente: false },
        "hijo nuevo de fila web: no hay nada guardado que conservar"
    );
    assert_eq!(politica_hijo(&Politica::Crudo, false, HashSet::new()), Politica::Crudo);
    let t: HashSet<String> = ["producto_id".to_string()].into();
    assert_eq!(
        politica_hijo(&Politica::SinMapa { traducidas: HashSet::new() }, true, t.clone()),
        Politica::SinMapa { traducidas: t }
    );
}

#[test]
fn crudo_no_traduce_ni_deja_pendientes() {
    let b = busquedas();
    let mut v = obj(json!({"uuid": "v", "usuario_id": 1, "cliente_id": 8, "anulada_por": null, "sucursal_id": 1}));
    let r = traducir_fila_con("ventas", "v", &mut v, None, &b, Some(&cols_venta()), None, None, &Politica::Crudo, &HashSet::new());
    assert!(r.pendientes.is_empty());
    assert_eq!(v["usuario_id"], json!(1), "crudo aunque el mapa lo conozca");
    assert_eq!(v["cliente_id"], json!(8));
    let mut esc = r.escritas.clone();
    esc.sort();
    assert_eq!(esc, vec!["anulada_por", "cliente_id", "usuario_id"]);
    // Padre igual se fija con el id de aquí.
    let mut d = obj(json!({"uuid": "d", "venta_id": 77, "producto_id": 7002}));
    traducir_fila_con("venta_detalle", "d", &mut d, None, &b, None, Some(9001), None, &Politica::Crudo, &HashSet::new());
    assert_eq!(d["venta_id"], json!(9001));
    assert_eq!(d["producto_id"], json!(7002));
}

#[test]
fn quitar_fks_de_fila_web_existente() {
    let b = busquedas();
    let guardada = obj(json!({"uuid": "w", "usuario_id": 3, "cliente_id": null}));
    let mut v = obj(json!({"uuid": "w", "usuario_id": 1, "cliente_id": 8, "total": 5, "sucursal_id": 1}));
    let r = traducir_fila_con("ventas", "w", &mut v, None, &b, Some(&cols_venta()), None, Some(&guardada), &Politica::Quitar, &HashSet::new());
    assert!(r.pendientes.is_empty() && r.escritas.is_empty());
    assert!(!v.contains_key("usuario_id") && !v.contains_key("cliente_id"), "se queda lo del servidor");
    assert_eq!(v["total"], json!(5));
    assert_eq!(v["sucursal_id"], json!(1));
    // Aunque traiga refs: un cliente viejo no las manda, y si llegaran no se usan.
    let refs = obj(json!({"usuario_id": "u-duenio"}));
    let mut v = obj(json!({"uuid": "w", "usuario_id": 1}));
    traducir_fila_con("ventas", "w", &mut v, Some(&refs), &b, None, None, Some(&guardada), &Politica::Quitar, &HashSet::new());
    assert!(!v.contains_key("usuario_id"));
}

#[test]
fn pendiente_en_fila_existente_conserva_lo_guardado() {
    let b = busquedas();
    let guardada = obj(json!({"uuid": "v", "usuario_id": 3, "cliente_id": 12}));
    // v2: la celda sale del cambio y se anota pendiente con valor_puesto = lo guardado.
    let refs = obj(json!({"cliente_id": "c-no-existe", "usuario_id": "u-duenio"}));
    let mut v = obj(json!({"uuid": "v", "usuario_id": 1, "cliente_id": 8}));
    let pol = Politica::Traducir { pendiente_en_existente: true };
    let r = traducir_fila_con("ventas", "v", &mut v, Some(&refs), &b, Some(&cols_venta()), None, Some(&guardada), &pol, &HashSet::new());
    assert!(!v.contains_key("cliente_id"), "se conserva lo guardado");
    assert_eq!(v["usuario_id"], json!(2));
    assert_eq!(r.pendientes.len(), 1);
    assert_eq!(r.pendientes[0].columna, "cliente_id");
    assert_eq!(r.pendientes[0].valor_puesto, Some(12));
    assert_eq!(r.pendientes[0].raw, Some(8));
    assert_eq!(r.escritas, vec!["usuario_id"]);
    // Escritorio viejo con estado: sin entrada en el mapa → se conserva, sin pendiente nuevo.
    let mut v = obj(json!({"uuid": "v", "usuario_id": 1, "cliente_id": 8}));
    let pol = Politica::Traducir { pendiente_en_existente: false };
    let r = traducir_fila_con("ventas", "v", &mut v, None, &b, Some(&cols_venta()), None, Some(&guardada), &pol, &HashSet::new());
    assert!(r.pendientes.is_empty());
    assert!(!v.contains_key("cliente_id"));
    assert_eq!(v["usuario_id"], json!(2), "por el mapa");
    // Fila nueva: centinela + pendiente con ese valor puesto.
    let mut v = obj(json!({"uuid": "v", "usuario_id": 99, "cliente_id": 8}));
    let r = traducir_fila_con("ventas", "v", &mut v, None, &b, Some(&cols_venta()), None, None, &pol, &HashSet::new());
    assert_eq!(v["usuario_id"], json!(0));
    assert_eq!(v["cliente_id"], Value::Null);
    let mut puestos: Vec<(String, Option<i64>)> = r.pendientes.iter().map(|p| (p.columna.clone(), p.valor_puesto)).collect();
    puestos.sort();
    assert_eq!(puestos, vec![("cliente_id".to_string(), None), ("usuario_id".to_string(), Some(0))]);
}

#[test]
fn sin_mapa_traduce_solo_lo_ya_reparado_y_omitir_respeta_refs() {
    let b = busquedas();
    let guardada = obj(json!({"uuid": "v", "usuario_id": 2, "cliente_id": 8, "anulada_por": 1}));
    let traducidas: HashSet<String> = ["usuario_id".to_string()].into();
    let mut v = obj(json!({"uuid": "v", "usuario_id": 1, "cliente_id": 8, "anulada_por": 1}));
    let r = traducir_fila_con(
        "ventas", "v", &mut v, None, &b, Some(&cols_venta()), None, Some(&guardada),
        &Politica::SinMapa { traducidas }, &HashSet::new(),
    );
    assert_eq!(v["usuario_id"], json!(2), "la reparación ya la tradujo: por el mapa");
    assert_eq!(v["anulada_por"], json!(1), "sigue cruda: la reparación la toma después");
    assert_eq!(v["cliente_id"], json!(8));
    assert!(r.pendientes.is_empty());
    // omitir: la columna sale aunque refs la traiga.
    let refs = obj(json!({"usuario_id": "u-duenio"}));
    let omitir: HashSet<String> = ["usuario_id".to_string()].into();
    let mut v = obj(json!({"uuid": "v", "usuario_id": 1}));
    let r = traducir_fila_con(
        "ventas", "v", &mut v, Some(&refs), &b, None, None, Some(&guardada),
        &Politica::Traducir { pendiente_en_existente: true }, &omitir,
    );
    assert!(!v.contains_key("usuario_id"));
    assert!(r.escritas.is_empty() && r.pendientes.is_empty());
}

#[test]
fn refs_del_pull_omiten_pendientes() {
    let mut ids = HashMap::new();
    ids.insert(("usuarios".to_string(), 2i64), "u-duenio".to_string());
    let v = obj(json!({"usuario_id": 2, "cliente_id": null, "anulada_por": null}));
    let pend: HashSet<String> = ["cliente_id".to_string(), "usuario_id".to_string()].into();
    let r = refs_de_fila("ventas", &v, &ids, Some(&pend));
    assert!(!r.contains_key("cliente_id"), "pendiente: nunca null");
    assert!(!r.contains_key("usuario_id"));
    assert_eq!(r.get("anulada_por"), Some(&Value::Null));
    let mut d = obj(json!({"uuid": "x", "cliente_id": null, "total": 1}));
    quitar_columnas(&mut d, Some(&pend));
    assert!(!d.contains_key("cliente_id"));
    assert_eq!(d["total"], json!(1));
    quitar_columnas(&mut d, None);
}

#[test]
fn resolver_compara_e_intercambia() {
    let sql = sql_resolver_grupo("ventas", "cliente_id", "clientes");
    assert!(sql.contains("t.cliente_id IS NOT DISTINCT FROM p.valor_puesto"), "{sql}");
    assert!(sql.contains("res.vigente AND res.nuevo IS NOT NULL"), "{sql}");
    // Viejos (celda cambió o fila ya no existe) se borran aunque no se resuelvan.
    assert!(sql.contains("res.nuevo IS NOT NULL OR NOT res.vigente"), "{sql}");
    assert!(sql.contains("LEFT JOIN ventas t ON t.uuid = p.fila_uuid"), "{sql}");
}

#[test]
fn reparacion_excluye_celdas_web_y_reversa_mira_el_estado() {
    let c = celdas_reparables().into_iter().find(|x| x.tabla == "productos" && x.col == "categoria_id").unwrap();
    let sql = sql_traducir(&c);
    assert!(sql.contains("sync_fk_celda_web w WHERE w.tabla = 'productos'"), "{sql}");
    assert!(sql.contains("w.columna = 'categoria_id' AND w.uuid = t.uuid"), "{sql}");
    assert!(sql_sin_mapa(&c).contains("sync_fk_celda_web"));
    let cond = sql_revertir_alcance("ventas");
    assert!(cond.contains("e.tabla = 'ventas' AND e.uuid = t.uuid") && cond.contains("('reparado', 'sin_mapa')"), "{cond}");
    let cond = sql_revertir_alcance("venta_detalle");
    assert!(cond.contains("FROM ventas pp JOIN sync_fk_estado e ON e.tabla = 'ventas' AND e.uuid = pp.uuid"), "{cond}");
    assert!(cond.contains("pp.id = t.venta_id"), "{cond}");
    let cond = sql_revertir_alcance("audit_log");
    assert!(cond.contains("e.tabla = 'audit_log' AND e.uuid = t.uuid"), "{cond}");
}

#[test]
fn mismo_valor_por_tipo() {
    let num = ColInfo::nueva("numeric", false, true);
    assert!(mismo_valor(Some(&num), Some(&json!(10)), Some(&json!(10.0))));
    assert!(mismo_valor(Some(&num), Some(&json!("10.00")), Some(&json!(10))));
    assert!(!mismo_valor(Some(&num), Some(&json!(10.5)), Some(&json!(10))));
    let txt = ColInfo::nueva("text", true, false);
    assert!(mismo_valor(Some(&txt), Some(&json!("")), Some(&Value::Null)));
    assert!(mismo_valor(Some(&txt), Some(&json!("")), None));
    assert!(!mismo_valor(Some(&txt), Some(&json!("a")), Some(&json!("b"))));
    let ent = ColInfo::nueva("integer", false, true);
    assert!(mismo_valor(Some(&ent), Some(&json!(1)), Some(&json!(true))));
}

fn cols_producto() -> ColumnasTabla {
    columnas(&[
        ("uuid", "text", false, false),
        ("nombre", "text", false, false),
        ("precio_venta", "numeric", false, true),
        ("stock_actual", "numeric", false, true),
        ("categoria_id", "bigint", true, false),
        ("proveedor_id", "bigint", true, false),
        ("updated_at", "text", false, true),
        ("created_at", "text", false, true),
    ])
}

fn combinar(
    data: &mut Map<String, Value>,
    guardada: &Map<String, Value>,
    base: &Map<String, Value>,
    b: &Busquedas,
    en_idioma: bool,
    web: &HashSet<String>,
    entrante_ts: &str,
) -> HashSet<String> {
    combinar_r(data, guardada, base, b, en_idioma, web, entrante_ts).omitir
}

fn combinar_r(
    data: &mut Map<String, Value>,
    guardada: &Map<String, Value>,
    base: &Map<String, Value>,
    b: &Busquedas,
    en_idioma: bool,
    web: &HashSet<String>,
    entrante_ts: &str,
) -> ResultadoCombinacion {
    let cols = cols_producto();
    combinar_tres_vias(
        "productos",
        data,
        &Combinacion {
            guardada,
            base,
            cols: &cols,
            busq: b,
            guardada_en_idioma_servidor: en_idioma,
            celdas_web: web,
            entrante_ts,
        },
    )
}

#[test]
fn tres_vias_precio_web_y_venta_del_escritorio() {
    let b = Busquedas::default();
    let base = obj(json!({"nombre": "Balata", "precio_venta": 25, "stock_actual": 10, "updated_at": "2026-10-07 10:00:00"}));
    // La web subió el precio; el escritorio vendió 1 (bajó el stock).
    let guardada = obj(json!({"nombre": "Balata", "precio_venta": 35.0, "stock_actual": 10.0, "updated_at": "2026-10-07 10:05:00"}));
    let mut data = obj(json!({"uuid": "p", "nombre": "Balata", "precio_venta": 25, "stock_actual": 9, "updated_at": "2026-10-07 10:06:00", "created_at": "x"}));
    let omitir = combinar(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:06:00");
    assert!(omitir.is_empty());
    assert!(!data.contains_key("precio_venta"), "el escritorio no lo cambió: se queda el de la web");
    assert!(!data.contains_key("nombre"));
    assert_eq!(data["stock_actual"], json!("9"));
    assert_eq!(data["created_at"], json!("x"), "no se combina");
}

#[test]
fn tres_vias_stock_suma_las_dos_ventas_y_no_baja_de_cero() {
    let b = Busquedas::default();
    let base = obj(json!({"stock_actual": 10, "updated_at": "2026-10-07 10:00:00"}));
    // Web vendió 1 (10 → 9); escritorio vendió 1 (10 → 9) → 8.
    let guardada = obj(json!({"stock_actual": 9.0, "updated_at": "2026-10-07 10:01:00"}));
    let mut data = obj(json!({"stock_actual": 9, "updated_at": "2026-10-07 10:02:00"}));
    combinar(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:02:00");
    assert_eq!(data["stock_actual"], json!("8"));
    // Web ajustó a 20; escritorio vendió 3 → 17.
    let guardada = obj(json!({"stock_actual": "20.00", "updated_at": "2026-10-07 10:01:00"}));
    let mut data = obj(json!({"stock_actual": 7, "updated_at": "2026-10-07 10:02:00"}));
    combinar(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:02:00");
    assert_eq!(data["stock_actual"], json!("17"));
    // Nunca negativo.
    let guardada = obj(json!({"stock_actual": 1, "updated_at": "2026-10-07 10:01:00"}));
    let mut data = obj(json!({"stock_actual": 5.5, "updated_at": "2026-10-07 10:02:00"}));
    combinar(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:02:00");
    assert_eq!(data["stock_actual"], json!("0"));
    // Fracciones exactas.
    let guardada = obj(json!({"stock_actual": 2.5, "updated_at": "2026-10-07 10:01:00"}));
    let mut data = obj(json!({"stock_actual": 9.75, "updated_at": "2026-10-07 10:02:00"}));
    combinar(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:02:00");
    assert_eq!(data["stock_actual"], json!("2.25"));
}

#[test]
fn tres_vias_los_dos_cambiaron_gana_la_marca_mas_nueva() {
    let b = Busquedas::default();
    let base = obj(json!({"nombre": "A", "precio_venta": 10, "updated_at": "2026-10-07 10:00:00"}));
    let guardada = obj(json!({"nombre": "WEB", "precio_venta": 12, "updated_at": "2026-10-07 10:05:00"}));
    // Escritorio más nuevo → gana el escritorio en lo que los dos cambiaron.
    let mut data = obj(json!({"nombre": "ESC", "precio_venta": 10, "updated_at": "2026-10-07 10:06:00"}));
    combinar(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:06:00");
    assert_eq!(data["nombre"], json!("ESC"));
    assert!(!data.contains_key("precio_venta"), "solo la web lo cambió");
    // Web más nueva → gana la web.
    let mut data = obj(json!({"nombre": "ESC", "updated_at": "2026-10-07 10:04:00"}));
    combinar(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:04:00");
    assert!(!data.contains_key("nombre"));
    // Solo el escritorio lo cambió → gana el escritorio aunque sea más viejo.
    let guardada2 = obj(json!({"nombre": "A", "updated_at": "2026-10-07 10:05:00"}));
    let mut data = obj(json!({"nombre": "ESC", "updated_at": "2026-10-07 10:04:00"}));
    combinar(&mut data, &guardada2, &base, &b, true, &HashSet::new(), "2026-10-07 10:04:00");
    assert_eq!(data["nombre"], json!("ESC"));
}

#[test]
fn tres_vias_fk_con_su_significado() {
    // Mapa del escritorio: categoría local 1 → id 5 aquí; local 2 → id 4.
    let mut b = Busquedas::default();
    b.por_mapa.insert(("categorias".into(), 1), ("cat-frenos".into(), Some(5)));
    b.por_mapa.insert(("categorias".into(), 2), ("cat-cadenas".into(), Some(4)));
    let base = obj(json!({"categoria_id": 1, "proveedor_id": null, "updated_at": "2026-10-07 10:00:00"}));
    // La web cambió la categoría (5 → 2 aquí); el escritorio no (sigue 1).
    let guardada = obj(json!({"categoria_id": 2, "proveedor_id": null, "updated_at": "2026-10-07 10:05:00"}));
    let mut data = obj(json!({"categoria_id": 1, "proveedor_id": null, "updated_at": "2026-10-07 10:06:00"}));
    let omitir = combinar(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:06:00");
    assert!(omitir.contains("categoria_id"), "se queda la de la web");
    assert!(!omitir.contains("proveedor_id"));
    // El escritorio la cambió (1 → 2 local) y la web no (sigue 5 aquí) → se traduce la del escritorio.
    let guardada = obj(json!({"categoria_id": 5, "updated_at": "2026-10-07 10:05:00"}));
    let mut data = obj(json!({"categoria_id": 2, "updated_at": "2026-10-07 10:06:00"}));
    let omitir = combinar(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:06:00");
    assert!(omitir.is_empty());
    assert_eq!(data["categoria_id"], json!(2), "cruda: la traducción viene después");
    // Fila del escritorio sin reparar: solo cuenta lo que la web marcó.
    let guardada = obj(json!({"categoria_id": 7, "updated_at": "2026-10-07 10:05:00"}));
    let web: HashSet<String> = ["categoria_id".to_string()].into();
    let mut data = obj(json!({"categoria_id": 1, "updated_at": "2026-10-07 10:06:00"}));
    let omitir = combinar(&mut data, &guardada, &base, &b, false, &web, "2026-10-07 10:06:00");
    assert!(omitir.contains("categoria_id"));
    let mut data = obj(json!({"categoria_id": 1, "updated_at": "2026-10-07 10:06:00"}));
    let omitir = combinar(&mut data, &guardada, &base, &b, false, &HashSet::new(), "2026-10-07 10:06:00");
    assert!(omitir.is_empty(), "sin marca web: se traduce la del escritorio");
}

// ── SPEC3 S2: "¿el servidor cambió desde la base?" por contenido ──────────

#[test]
fn tres_vias_sin_cambio_del_servidor_no_aporta() {
    let b = Busquedas::default();
    let base = obj(json!({"nombre": "Balata", "precio_venta": 25, "stock_actual": 10, "categoria_id": null,
                          "updated_at": "2026-10-07 10:00:00"}));
    // Lo guardado es la base (otra marca y otros tipos JSON, mismo contenido):
    // aunque la guardada sea "más nueva" por reloj, no hay aporte del servidor.
    let guardada = obj(json!({"nombre": "Balata", "precio_venta": "25.00", "stock_actual": 10.0, "categoria_id": null,
                              "updated_at": "2026-10-07 10:09:00"}));
    let mut data = obj(json!({"nombre": "Balata 2", "precio_venta": 25, "stock_actual": 9, "categoria_id": null,
                              "updated_at": "2026-10-07 10:01:00"}));
    let r = combinar_r(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:01:00");
    assert!(!r.servidor_aporta, "el servidor no tuvo cambios propios: push normal");
    assert!(r.omitir.is_empty());
    // Con un empate de contenido por recorte a 0 tampoco aporta (mismo resultado).
    let guardada = obj(json!({"stock_actual": 0, "updated_at": "2026-10-07 10:09:00"}));
    let base = obj(json!({"stock_actual": 5, "updated_at": "2026-10-07 10:00:00"}));
    let mut data = obj(json!({"stock_actual": 0, "updated_at": "2026-10-07 10:01:00"}));
    let r = combinar_r(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:01:00");
    assert!(!r.servidor_aporta);
}

#[test]
fn tres_vias_cambio_del_servidor_aporta_aunque_su_marca_sea_vieja() {
    let b = Busquedas::default();
    // Reloj del escritorio adelantado: la base trae una marca POSTERIOR a la
    // venta y al precio de la web. Por contenido igual se detecta.
    let base = obj(json!({"precio_venta": 25, "stock_actual": 10, "updated_at": "2026-10-07 10:05:00"}));
    let guardada = obj(json!({"precio_venta": 35, "stock_actual": 9, "updated_at": "2026-10-07 10:01:00"}));
    let mut data = obj(json!({"precio_venta": 25, "stock_actual": 9, "updated_at": "2026-10-07 10:07:00"}));
    let r = combinar_r(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:02:00");
    assert!(r.servidor_aporta);
    assert_eq!(data["stock_actual"], json!("8"));
    assert!(!data.contains_key("precio_venta"), "precio de la web");
    // Solo el stock (venta web): aporta.
    let guardada = obj(json!({"precio_venta": 25, "stock_actual": 9, "updated_at": "2026-10-07 10:01:00"}));
    let mut data = obj(json!({"precio_venta": 25, "stock_actual": 10, "updated_at": "2026-10-07 10:07:00"}));
    assert!(combinar_r(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:02:00").servidor_aporta);
    // Los dos cambiaron el precio y gana el escritorio: lo guardado no sobrevive.
    let guardada = obj(json!({"precio_venta": 30, "stock_actual": 10, "updated_at": "2026-10-07 10:01:00"}));
    let mut data = obj(json!({"precio_venta": 40, "stock_actual": 10, "updated_at": "2026-10-07 10:07:00"}));
    let r = combinar_r(&mut data, &guardada, &base, &b, true, &HashSet::new(), "2026-10-07 10:02:00");
    assert!(!r.servidor_aporta, "el resultado es la fila del escritorio");
    assert_eq!(data["precio_venta"], json!(40));
    // FK que se queda la del servidor: aporta.
    let mut bm = Busquedas::default();
    bm.por_mapa.insert(("categorias".into(), 1), ("cat-frenos".into(), Some(5)));
    let base = obj(json!({"categoria_id": 1, "updated_at": "2026-10-07 10:05:00"}));
    let guardada = obj(json!({"categoria_id": 2, "updated_at": "2026-10-07 10:01:00"}));
    let mut data = obj(json!({"categoria_id": 1, "updated_at": "2026-10-07 10:07:00"}));
    let r = combinar_r(&mut data, &guardada, &base, &bm, true, &HashSet::new(), "2026-10-07 10:02:00");
    assert!(r.servidor_aporta && r.omitir.contains("categoria_id"));
    // Misma FK que la base (traducida): no aporta.
    let guardada = obj(json!({"categoria_id": 5, "updated_at": "2026-10-07 10:01:00"}));
    let mut data = obj(json!({"categoria_id": 1, "updated_at": "2026-10-07 10:07:00"}));
    assert!(!combinar_r(&mut data, &guardada, &base, &bm, true, &HashSet::new(), "2026-10-07 10:02:00").servidor_aporta);
}

// ── SPEC3 S1: huella del push ─────────────────────────────────────────────

#[test]
fn json_canonico_ordena_llaves_en_todos_los_niveles() {
    let a = json!({"b": 1, "a": {"y": [3, {"q": null, "p": "x"}], "x": 2.5}, "c": "ñ\""});
    assert_eq!(json_canonico(&a), r#"{"a":{"x":2.5,"y":[3,{"p":"x","q":null}]},"b":1,"c":"ñ\""}"#);
    // Mismo contenido con otro orden de llaves → mismo texto.
    let mut m = Map::new();
    m.insert("c".into(), json!("ñ\""));
    m.insert("a".into(), json!({"x": 2.5, "y": [3, {"p": "x", "q": null}]}));
    m.insert("b".into(), json!(1));
    assert_eq!(json_canonico(&Value::Object(m)), json_canonico(&a));
}

#[test]
fn huella_ignora_id_y_synced_at_pero_no_lo_demas() {
    let base = obj(json!({"stock_actual": 10, "updated_at": "2026-10-07 10:00:00"}));
    let d1 = json!({"id": 1, "uuid": "p", "stock_actual": 9, "synced_at": null, "updated_at": "2026-10-07 10:01:00"});
    let d2 = json!({"id": 77, "uuid": "p", "stock_actual": 9, "synced_at": "2026-10-07 10:02:00", "updated_at": "2026-10-07 10:01:00"});
    assert_eq!(textos_huella(&d1, &base), textos_huella(&d2, &base));
    let d3 = json!({"id": 1, "uuid": "p", "stock_actual": 8, "updated_at": "2026-10-07 10:01:00"});
    assert_ne!(textos_huella(&d1, &base).0, textos_huella(&d3, &base).0);
    let otra_base = obj(json!({"stock_actual": 10, "updated_at": "2026-10-07 10:00:01"}));
    assert_ne!(textos_huella(&d1, &base).1, textos_huella(&d1, &otra_base).1);
    assert!(!textos_huella(&d1, &base).0.contains("\"id\""));
}

// ── SPEC3 S4: marcas de celda web ─────────────────────────────────────────

#[test]
fn celdas_web_que_el_escritorio_reescribe() {
    let marcadas: HashSet<String> = ["categoria_id".to_string(), "proveedor_id".to_string()].into();
    let guardada = obj(json!({"categoria_id": 5, "proveedor_id": 9, "nombre": "x"}));
    let escritas = vec![
        ("categoria_id".to_string(), ValorPg::Entero(Some(3))), // distinto → se quita
        ("proveedor_id".to_string(), ValorPg::Entero(Some(9))), // igual → se queda
        ("nombre".to_string(), ValorPg::Texto(Some("y".into()))), // sin marca
    ];
    assert_eq!(celdas_web_reescritas(Some(&marcadas), &escritas, Some(&guardada)), vec!["categoria_id".to_string()]);
    // NULL contra un id: distinto.
    let escritas = vec![("proveedor_id".to_string(), ValorPg::Entero(None))];
    assert_eq!(celdas_web_reescritas(Some(&marcadas), &escritas, Some(&guardada)), vec!["proveedor_id".to_string()]);
    // Columna que el cambio no escribe (quedó la guardada): la marca se queda.
    assert!(celdas_web_reescritas(Some(&marcadas), &[], Some(&guardada)).is_empty());
    // Sin marcas: nada.
    let escritas = vec![("categoria_id".to_string(), ValorPg::Entero(Some(3)))];
    assert!(celdas_web_reescritas(None, &escritas, Some(&guardada)).is_empty());
}

#[test]
fn update_de_fila_existente_sin_inmutables() {
    let cols = columnas(&[
        ("uuid", "text", false, false),
        ("total", "numeric", false, true),
        ("caja", "text", false, true),
        ("t", "timestamp with time zone", true, false),
    ]);
    let filas = vec![
        ("uuid".to_string(), ValorPg::Texto(Some("v".into()))),
        ("total".to_string(), ValorPg::Numerico(Some(Decimal::from(3)))),
        ("caja".to_string(), ValorPg::Texto(Some("web".into()))),
        ("t".to_string(), ValorPg::Otro(Some("2026-01-01".into()))),
    ];
    let (sql, idx) = sql_update("ventas", &filas, &cols).unwrap();
    assert_eq!(sql, "UPDATE ventas SET \"total\"=$1,\"t\"=CAST($2 AS timestamp with time zone) WHERE uuid = $3 RETURNING id");
    assert_eq!(idx, vec![1, 3]);
    assert!(sql_update("ventas", &filas[..1], &cols).is_none());
}

// ── SPEC4 S2: comparación numérica con la escala de la columna ────────────

#[test]
fn mismo_valor_con_escala_como_postgres() {
    let dos = ColInfo::nueva("numeric", false, true).con_escala(Some(2));
    assert_eq!(dos.escala, Some(2));
    // Lo que Postgres guardaría en numeric(12,2): punto medio lejos del cero.
    assert!(mismo_valor(Some(&dos), Some(&json!(33.335)), Some(&json!("33.34"))));
    assert!(mismo_valor(Some(&dos), Some(&json!(33.334)), Some(&json!(33.33))));
    assert!(mismo_valor(Some(&dos), Some(&json!(-1.005)), Some(&json!("-1.01"))));
    assert!(mismo_valor(Some(&dos), Some(&json!(2.675)), Some(&json!("2.68"))));
    assert!(mismo_valor(Some(&dos), Some(&json!(0.30000000000000004)), Some(&json!(0.3))));
    assert!(!mismo_valor(Some(&dos), Some(&json!(33.335)), Some(&json!(33.33))));
    assert!(!mismo_valor(Some(&dos), Some(&json!(10.5)), Some(&json!(10))));
    assert!(!mismo_valor(Some(&dos), Some(&json!(1)), Some(&Value::Null)));
    assert!(mismo_valor(Some(&dos), Some(&Value::Null), None));
    assert_eq!(dos.redondear(Decimal::from_str("33.335").unwrap()), Decimal::from_str("33.34").unwrap());
    // Sin escala declarada (numeric a secas): exacto, como antes.
    let libre = ColInfo::nueva("numeric", false, true);
    assert!(!mismo_valor(Some(&libre), Some(&json!(33.335)), Some(&json!("33.34"))));
    assert_eq!(ColInfo::nueva("numeric", false, true).con_escala(None).escala, None);
    // La escala solo cuenta en numeric (information_schema da 0 en bigint).
    assert_eq!(ColInfo::nueva("bigint", true, false).con_escala(Some(0)).escala, None);
    assert_eq!(ColInfo::nueva("double precision", true, false).con_escala(Some(2)).escala, None);
}

fn cols_producto_con_escala() -> ColumnasTabla {
    let mut cols = cols_producto();
    for c in ["precio_venta", "stock_actual"] {
        let info = cols.remove(c).unwrap().con_escala(Some(2));
        cols.insert(c.into(), info);
    }
    cols.insert("precio_costo".into(), ColInfo::nueva("numeric", false, true).con_escala(Some(2)));
    cols
}

fn combinar_escala(
    data: &mut Map<String, Value>,
    guardada: &Map<String, Value>,
    base: &Map<String, Value>,
    entrante_ts: &str,
) -> ResultadoCombinacion {
    let cols = cols_producto_con_escala();
    combinar_tres_vias(
        "productos",
        data,
        &Combinacion {
            guardada,
            base,
            cols: &cols,
            busq: &Busquedas::default(),
            guardada_en_idioma_servidor: true,
            celdas_web: &HashSet::new(),
            entrante_ts,
        },
    )
}

#[test]
fn tres_vias_base_con_mas_decimales_no_es_cambio_del_servidor() {
    // Base del escritorio (REAL): costo 33.335; aquí quedó 33.34. El
    // escritorio sube el costo a 40 y vende 1; la web vendió 1 DESPUÉS (su
    // marca es más nueva). Antes: 33.34 contaba como cambio del servidor y,
    // por marca, se quedaba (la edición del escritorio se perdía).
    let base = obj(json!({"precio_costo": 33.335, "precio_venta": 50, "stock_actual": 10, "updated_at": "2026-10-07 10:00:00"}));
    let guardada = obj(json!({"precio_costo": "33.34", "precio_venta": "50.00", "stock_actual": "9.00", "updated_at": "2026-10-07 10:09:00"}));
    let mut data = obj(json!({"precio_costo": 40, "precio_venta": 50, "stock_actual": 9, "updated_at": "2026-10-07 10:05:00"}));
    let r = combinar_escala(&mut data, &guardada, &base, "2026-10-07 10:05:00");
    assert_eq!(data["precio_costo"], json!(40), "la edición del escritorio se queda");
    assert_eq!(data["stock_actual"], json!("8"));
    assert!(r.servidor_aporta, "la venta web sí es aporte");

    // Sin edición del escritorio y la web solo volvió a guardar: 33.335 (base
    // y entrante) contra 33.34 guardado no es aporte → push normal.
    let guardada = obj(json!({"precio_costo": "33.34", "precio_venta": "50.00", "stock_actual": "10.00", "updated_at": "2026-10-07 10:09:00"}));
    let mut data = obj(json!({"precio_costo": 33.335, "precio_venta": 50, "stock_actual": 9, "updated_at": "2026-10-07 10:05:00"}));
    let r = combinar_escala(&mut data, &guardada, &base, "2026-10-07 10:05:00");
    assert!(!r.servidor_aporta, "mismo contenido redondeado: sin aporte");
    assert!(!data.contains_key("precio_costo"), "se queda la guardada (es la misma)");

    // Un cambio real del servidor (33.34 → 35) sigue contando, y por marca gana.
    let guardada = obj(json!({"precio_costo": "35.00", "stock_actual": "10.00", "updated_at": "2026-10-07 10:09:00"}));
    let mut data = obj(json!({"precio_costo": 40, "stock_actual": 10, "updated_at": "2026-10-07 10:05:00"}));
    let r = combinar_escala(&mut data, &guardada, &base, "2026-10-07 10:05:00");
    assert!(r.servidor_aporta);
    assert!(!data.contains_key("precio_costo"));

    // Stock con fracciones de centavo: lo que quedaría guardado es lo mismo.
    let base = obj(json!({"stock_actual": 0.335, "updated_at": "2026-10-07 10:00:00"}));
    let guardada = obj(json!({"stock_actual": "0.34", "updated_at": "2026-10-07 10:09:00"}));
    let mut data = obj(json!({"stock_actual": 0.335, "updated_at": "2026-10-07 10:05:00"}));
    assert!(!combinar_escala(&mut data, &guardada, &base, "2026-10-07 10:05:00").servidor_aporta);
}
