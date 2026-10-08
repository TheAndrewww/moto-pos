// Pruebas de la ronda 4 del sync v2 (SPEC4 S) contra Postgres (copia fiel
// de producción). Submódulo de sync_v2_pg.rs: usa sus ayudas y las de las
// rondas 2 y 3. Igual que allá, solo corren con SYNC_PG_URL.
//
//   s1  base efectiva: la respuesta de un push se pierde y antes del
//       reintento el escritorio vende OTRA vez (el reintento ya no es
//       idéntico y trae la base vieja): cada venta se resta una sola vez.
//       Tras una combinación (e2e r1c), tras un push normal + venta web
//       (e2e r1d), tras un push normal sin cambio del servidor, cadena de
//       respuestas perdidas sobre la misma base, y una base vieja que vuelve
//       después de que el escritorio avanzó (respaldo restaurado).
//   s2  la combinación compara numeric(12,2) redondeado como Postgres
//   s3  en el push normal la marca guardada nunca retrocede

use super::ronda2::{cursor_de, ultimo_origen};
use super::ronda3::{con_base, cursores_de, guardado_ts, huellas_de, marca_rel, precio_stock, producto_empujado, venta_web};
use super::*;

async fn stock(p: &PgPool, pu: &str) -> String {
    texto(p, &format!("SELECT stock_actual::text FROM productos WHERE uuid = '{pu}'")).await.unwrap()
}

/// Lo que mandó el escritorio con su stock local y su reloj.
async fn venta_local(p: &PgPool, anterior: &Value, stock_local: f64, segundos: i64) -> Value {
    let mut d = anterior.clone();
    d["stock_actual"] = json!(stock_local);
    d["updated_at"] = json!(marca_rel(p, segundos).await);
    d
}

/// Empuja `data` sobre `base`; debe aceptarse. Devuelve la marca.
async fn empujar(p: &PgPool, data: &Value, base: &Value) -> Option<String> {
    let pu = data["uuid"].as_str().unwrap().to_string();
    let r = procesar_push(p, &body(Some(2), vec![con_base(data, base)])).await.unwrap();
    assert_eq!(r.aceptados, vec![pu.clone()], "{:?}", r.rechazados);
    r.marcas.get(&pu).cloned()
}

/// La base como la guarda un escritorio nuevo tras una respuesta recibida:
/// la fila que empujó con la marca que devolvió el servidor.
fn base_confirmada(empujada: &Value, marca: &str) -> Value {
    let mut b = empujada.clone();
    b["updated_at"] = json!(marca);
    b
}

// ─────────────────────────────────────────────────────────────────────────
// s1) Base efectiva (lo que ya se aplicó de este equipo sobre esta base).
// ─────────────────────────────────────────────────────────────────────────

/// e2e r1c: el push perdido fue una COMBINACIÓN.
#[tokio::test]
async fn s1_combinacion_perdida_y_otra_venta_antes_del_reintento() {
    let Some(c) = ctx("s4r1c").await else { return };
    let p = &c.pool;
    let pu = hex32();
    let (d0, _) = producto_empujado(p, 7401, &pu, &marca_rel(p, -60).await).await;

    // Web vende 1 (10 → 9). El escritorio vende 1 (base d0) → combinación 8;
    // la respuesta se pierde (el escritorio se queda con base d0).
    venta_web(p, &pu, None, None).await;
    let a = venta_local(p, &d0, 9.0, -50).await;
    empujar(p, &a, &d0).await;
    assert_eq!(stock(p, &pu).await, "8.00");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some("merge"));
    // Anotado: md5 de la base y la fila aplicada.
    let anotado = texto(
        p,
        &format!(
            "SELECT (datos->>'stock_actual') || '|' || (base_md5 IS NOT NULL)::text FROM sync_push_aplicado \
              WHERE device_uuid = '{DEV}' AND uuid = '{pu}' ORDER BY id DESC LIMIT 1"
        ),
    )
    .await;
    assert_eq!(anotado.as_deref(), Some("9.0|true"));

    // La cajera vende otra antes del reintento: A' (stock 8) con base d0.
    // Antes: 8 + (8 − 10) = 6. Ahora la base efectiva es A: 8 + (8 − 9) = 7.
    let a2 = venta_local(p, &a, 8.0, -40).await;
    let marca = empujar(p, &a2, &d0).await.unwrap();
    assert_eq!(precio_stock(p, &pu).await, "25.00|7.00", "10 − 1 (web) − 2 (escritorio), cada venta una vez");
    assert_eq!(huellas_de(p, &pu).await, 2);
    // El reintento idéntico de A' sigue siendo un no-op.
    let n = cursores_de(p, &pu).await;
    empujar(p, &a2, &d0).await;
    assert_eq!(stock(p, &pu).await, "7.00");
    assert_eq!(cursores_de(p, &pu).await, n);

    // Esta vez la respuesta llegó (base = A' con la marca). Web vende 1 y el
    // escritorio vende 1 sobre su base nueva: la base efectiva ya no aplica.
    venta_web(p, &pu, None, None).await;
    let a3 = venta_local(p, &a2, 7.0, -30).await;
    empujar(p, &a3, &base_confirmada(&a2, &marca)).await;
    assert_eq!(stock(p, &pu).await, "5.00", "10 − 2 (web) − 3 (escritorio)");
    c.cerrar().await;
}

/// e2e r1d: el push perdido fue un push NORMAL (lo último era del mismo
/// escritorio) y la web vendió antes del reintento.
#[tokio::test]
async fn s1_push_normal_perdido_venta_web_y_otra_venta() {
    let Some(c) = ctx("s4r1d").await else { return };
    let p = &c.pool;
    let pu = hex32();
    let (d0, _) = producto_empujado(p, 7402, &pu, &marca_rel(p, -60).await).await;

    // Lo último es de este escritorio → LWW normal (10 → 9); respuesta perdida.
    let a = venta_local(p, &d0, 9.0, -50).await;
    empujar(p, &a, &d0).await;
    assert_eq!(stock(p, &pu).await, "9.00");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some(DEV));
    // La web vende 1 (9 → 8) y el escritorio vende otra antes del reintento.
    venta_web(p, &pu, Some(30.0), None).await;
    let a2 = venta_local(p, &a, 8.0, -40).await;
    empujar(p, &a2, &d0).await;
    // Antes: 8 + (8 − 10) = 6.
    assert_eq!(precio_stock(p, &pu).await, "30.00|7.00", "10 − 1 (web) − 2 (escritorio); el precio de la web se queda");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some("merge"));

    // Mismo caso cuando el push perdido fue un push NORMAL por contenido
    // (lo último era de la web pero sin cambios propios: solo re-guardó).
    let pu = hex32();
    let (d0, _) = producto_empujado(p, 7403, &pu, &marca_rel(p, -60).await).await;
    sqlx::query("UPDATE productos SET updated_at = $2 WHERE uuid = $1").bind(&pu).bind(marca_rel(p, -55).await).execute(p).await.unwrap();
    cursor_de(p, "productos", &pu, "web-pos").await;
    let a = venta_local(p, &d0, 9.0, -50).await;
    empujar(p, &a, &d0).await;
    assert_eq!(stock(p, &pu).await, "9.00");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some(DEV), "sin aporte del servidor: push normal");
    venta_web(p, &pu, None, None).await;
    let a2 = venta_local(p, &a, 8.0, -40).await;
    empujar(p, &a2, &d0).await;
    assert_eq!(stock(p, &pu).await, "7.00");

    // Dos pushes del mismo escritorio perdidos seguidos (lo último es suyo:
    // LWW, su fila tal cual), luego la web vende y llega el tercero: se
    // combina contra el segundo, que también quedó anotado.
    let pu = hex32();
    let (d0, _) = producto_empujado(p, 7404, &pu, &marca_rel(p, -60).await).await;
    let a = venta_local(p, &d0, 9.0, -50).await;
    empujar(p, &a, &d0).await;
    let a2 = venta_local(p, &a, 8.0, -45).await;
    empujar(p, &a2, &d0).await;
    assert_eq!(stock(p, &pu).await, "8.00");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some(DEV));
    venta_web(p, &pu, None, None).await; // 7
    let a3 = venta_local(p, &a2, 7.0, -40).await;
    empujar(p, &a3, &d0).await;
    assert_eq!(stock(p, &pu).await, "6.00", "10 − 1 (web) − 3 (escritorio)");
    c.cerrar().await;
}

/// Cadena de respuestas perdidas sobre la misma base (con ventas web en
/// medio): cada push aplicado es la base del siguiente. Y una base vieja que
/// vuelve DESPUÉS de que el escritorio avanzó (restauró un respaldo) se usa
/// tal cual llega: no se toma lo aplicado hace tiempo sobre esa base.
#[tokio::test]
async fn s1_cadena_de_respuestas_perdidas_sobre_la_misma_base() {
    let Some(c) = ctx("s4cad").await else { return };
    let p = &c.pool;
    let pu = hex32();
    let mut d0 = producto_escritorio(7405, &pu, &marca_rel(p, -120).await);
    d0["precio_venta"] = json!(25.0);
    d0["stock_actual"] = json!(20.0);
    let r = procesar_push(p, &body(Some(2), vec![cambio("productos", d0.clone(), None, super::ronda3::refs_nulas(&pu))])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);

    venta_web(p, &pu, None, None).await; // 19
    let a1 = venta_local(p, &d0, 19.0, -100).await;
    empujar(p, &a1, &d0).await; // combinación 18, perdida
    assert_eq!(stock(p, &pu).await, "18.00");
    venta_web(p, &pu, None, None).await; // 17
    let a2 = venta_local(p, &a1, 18.0, -90).await;
    empujar(p, &a2, &d0).await; // 17 + (18 − 19) = 16, perdida
    assert_eq!(stock(p, &pu).await, "16.00");
    let a3 = venta_local(p, &a2, 17.0, -80).await;
    empujar(p, &a3, &d0).await; // 16 + (17 − 18) = 15, perdida
    assert_eq!(stock(p, &pu).await, "15.00");
    venta_web(p, &pu, None, None).await; // 14
    let a4 = venta_local(p, &a3, 16.0, -70).await;
    let marca = empujar(p, &a4, &d0).await.unwrap(); // 14 + (16 − 17) = 13, esta sí llega
    assert_eq!(stock(p, &pu).await, "13.00", "20 − 3 (web) − 4 (escritorio)");
    assert_eq!(huellas_de(p, &pu).await, 4);
    // Reenvíos idénticos de cualquier eslabón: no-op.
    for (d, b) in [(&a4, &d0), (&a2, &d0)] {
        let n = cursores_de(p, &pu).await;
        empujar(p, d, b).await;
        assert_eq!(stock(p, &pu).await, "13.00");
        assert_eq!(cursores_de(p, &pu).await, n);
    }

    // El escritorio avanza (base = a4 con la marca) y vende 1: 13 − 1 = 12.
    let a5 = venta_local(p, &a4, 15.0, -60).await;
    empujar(p, &a5, &base_confirmada(&a4, &marca)).await;
    assert_eq!(stock(p, &pu).await, "12.00");

    // Respaldo restaurado: el escritorio vuelve a tener la fila y la base d0
    // (stock 20), la web vende 1 (11) y el escritorio vende 1 sobre d0. El
    // último push de este equipo fue sobre OTRA base → se usa d0 tal cual:
    // 11 + (19 − 20) = 10. (Tomar a4, lo último aplicado sobre d0, daría
    // 11 + (19 − 16) = 14: devolvería tres piezas que sí se vendieron.)
    venta_web(p, &pu, None, None).await;
    let r1 = venta_local(p, &d0, 19.0, -50).await;
    empujar(p, &r1, &d0).await;
    assert_eq!(stock(p, &pu).await, "10.00");
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// s2) numeric(12,2): la base del escritorio (REAL) con más decimales no
//     cuenta como cambio del servidor.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn s2_costo_con_mas_decimales_no_revierte_la_edicion() {
    let Some(c) = ctx("s4esc").await else { return };
    let p = &c.pool;
    let costo_stock = |pu: String| async move {
        texto(p, &format!("SELECT precio_costo || '|' || stock_actual FROM productos WHERE uuid = '{pu}'")).await.unwrap()
    };

    // Base: costo 33.335 (aquí quedó 33.34). El escritorio sube el costo a 40
    // y vende 1; la web vende 1 DESPUÉS (marca más nueva). Antes: 33.34|8.00.
    let pu = hex32();
    let mut d0 = producto_escritorio(7411, &pu, &marca_rel(p, -60).await);
    d0["precio_costo"] = json!(33.335);
    d0["stock_actual"] = json!(10.0);
    let r = procesar_push(p, &body(Some(2), vec![cambio("productos", d0.clone(), None, super::ronda3::refs_nulas(&pu))])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    assert_eq!(costo_stock(pu.clone()).await, "33.34|10.00");
    let mut d1 = venta_local(p, &d0, 9.0, -30).await;
    d1["precio_costo"] = json!(40.0);
    venta_web(p, &pu, None, None).await;
    empujar(p, &d1, &d0).await;
    assert_eq!(costo_stock(pu.clone()).await, "40.00|8.00", "la edición del escritorio y las dos ventas");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some("merge"));

    // Sin edición del escritorio y la web solo re-guardó: 33.335 contra 33.34
    // no es un cambio del servidor → push normal (antes: 'merge' de más).
    let pu = hex32();
    let mut d0 = producto_escritorio(7412, &pu, &marca_rel(p, -60).await);
    d0["precio_costo"] = json!(33.335);
    d0["stock_actual"] = json!(10.0);
    let r = procesar_push(p, &body(Some(2), vec![cambio("productos", d0.clone(), None, super::ronda3::refs_nulas(&pu))])).await.unwrap();
    assert_eq!(r.aceptados.len(), 1, "{:?}", r.rechazados);
    sqlx::query("UPDATE productos SET updated_at = $2 WHERE uuid = $1").bind(&pu).bind(ahora(p).await).execute(p).await.unwrap();
    cursor_de(p, "productos", &pu, "web-pos").await;
    let d1 = venta_local(p, &d0, 9.0, -30).await;
    empujar(p, &d1, &d0).await;
    assert_eq!(costo_stock(pu.clone()).await, "33.34|9.00");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some(DEV));
    c.cerrar().await;
}

// ─────────────────────────────────────────────────────────────────────────
// s3) Push normal (sin cambio propio del servidor): la marca guardada nunca
//     va hacia atrás; el contenido es el del escritorio.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn s3_marca_guardada_no_retrocede_en_push_normal() {
    let Some(c) = ctx("s4marca").await else { return };
    let p = &c.pool;
    let pu = hex32();
    let (d0, _) = producto_empujado(p, 7421, &pu, &marca_rel(p, -600).await).await;

    // La web vuelve a guardar sin cambios (marca tW, cursor web). El
    // escritorio (venta hecha antes de tW, o reloj atrasado) empuja con una
    // marca más vieja: contenido suyo, marca guardada = tW (antes: la suya).
    let tw = ahora(p).await;
    sqlx::query("UPDATE productos SET updated_at = $2 WHERE uuid = $1").bind(&pu).bind(&tw).execute(p).await.unwrap();
    cursor_de(p, "productos", &pu, "web-pos").await;
    let mut d1 = venta_local(p, &d0, 9.0, -300).await;
    d1["nombre"] = json!("Nombre del escritorio");
    let marca = empujar(p, &d1, &d0).await;
    assert_eq!(precio_stock(p, &pu).await, "25.00|9.00");
    assert_eq!(
        texto(p, &format!("SELECT nombre FROM productos WHERE uuid = '{pu}'")).await.as_deref(),
        Some("Nombre del escritorio")
    );
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some(DEV));
    let g = guardado_ts(p, "productos", &pu).await;
    assert!(g >= tw, "no retrocede: {g} vs {tw}");
    assert_eq!(g, tw);
    assert_eq!(marca.as_deref(), Some(g.as_str()), "marcas = lo guardado");

    // Si la marca del escritorio es la más nueva, se queda la suya.
    let tw2 = marca_rel(p, -200).await;
    sqlx::query("UPDATE productos SET updated_at = $2 WHERE uuid = $1").bind(&pu).bind(&tw2).execute(p).await.unwrap();
    cursor_de(p, "productos", &pu, "web-pos").await;
    let d2 = venta_local(p, &d1, 8.0, -100).await;
    let marca = empujar(p, &d2, &base_confirmada(&d1, &g)).await;
    assert_eq!(stock(p, &pu).await, "8.00");
    assert_eq!(ultimo_origen(p, "productos", &pu).await.as_deref(), Some(DEV));
    let g2 = guardado_ts(p, "productos", &pu).await;
    assert_eq!(Some(g2.clone()), d2["updated_at"].as_str().map(String::from));
    assert_eq!(marca, Some(g2));
    c.cerrar().await;
}
