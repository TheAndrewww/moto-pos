// venta_calc.rs — Recálculo y validación de importes de una venta (POS web).
//
// El servidor NO confía en los importes que manda el navegador. Con los datos
// que sí controla (precio_venta actual del producto, descuento del cliente,
// límite del vendedor, autorizaciones firmadas) recalcula cada partida y el
// total, y rechaza la venta si lo que el navegador mostró al cajero no cuadra.
//
// La aritmética replica EXACTAMENTE src/store/ventaStore.ts (mismas
// operaciones f64 y en el mismo orden):
//   descuento_monto = precio_original * (pct / 100)
//   precio_final    = precio_original - descuento_monto
//   subtotal        = precio_final * cantidad
//   raw             = Σ subtotal                    (ventas.subtotal, YA neto)
//   descuento       = Σ descuento_monto * cantidad  (informativo)
//   total           = raw redondeado HACIA ARRIBA al peso (ventaStore.total())
//   cambio          = monto_recibido - total        (solo efectivo)
//
// Módulo puro (sin BD) para poder probarlo con `cargo test` sin Postgres.

/// Tolerancia para comparar dinero (medio centavo).
pub const EPS: f64 = 0.005;
/// Topes de cordura. numeric(12,2) aguanta 9_999_999_999.99; arriba de eso
/// Postgres truena con "numeric field overflow" (500). Con estos topes el
/// error es un 400 legible. Máximos reales en producción (oct-2026):
/// cantidad 40, precio 1627, ticket 2494.
pub const MAX_CANTIDAD: f64 = 10_000.0;
pub const MAX_IMPORTE: f64 = 1_000_000.0;
pub const MAX_PARTIDAS: usize = 300;
/// Cambio máximo aceptable en efectivo. En producción hay 20 ventas desktop
/// con cambio > $2,000 y todas son errores de captura (p. ej. recibido
/// 8,081,512,402 = código de barras escaneado en el campo de efectivo); los
/// cambios legítimos más altos rondan $1,000 (pago con billete de 1000).
pub const MAX_CAMBIO: f64 = 5_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetodoPago {
    Efectivo,
    Tarjeta,
    Transferencia,
}

impl MetodoPago {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "efectivo" => Some(Self::Efectivo),
            "tarjeta" => Some(Self::Tarjeta),
            "transferencia" => Some(Self::Transferencia),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Efectivo => "efectivo",
            Self::Tarjeta => "tarjeta",
            Self::Transferencia => "transferencia",
        }
    }
}

/// Partida tal como la mandó el navegador.
#[derive(Debug, Clone)]
pub struct LineaCliente {
    pub cantidad: f64,
    pub precio_original: f64,
    pub descuento_porcentaje: f64,
    pub descuento_monto: f64,
    pub precio_final: f64,
    pub subtotal: f64,
}

/// Reglas que el servidor resolvió en BD para esa partida.
#[derive(Debug, Clone)]
pub struct ReglaLinea {
    /// Para mensajes de error.
    pub nombre: String,
    /// Precios aceptables para `precio_original`. El primero es el precio de
    /// venta actual del producto (el que se reporta si no coincide).
    pub precios_validos: Vec<f64>,
    /// Descuento que se acepta sin autorización: 100 si el cajero es admin;
    /// si no, max(descuento_max_vendedor_pct, descuento del cliente).
    pub pct_max_sin_autorizacion: f64,
    /// Porcentaje autorizado por un dueño (token firmado válido), si hay.
    pub pct_autorizado: Option<f64>,
}

/// Totales del encabezado tal como los mandó el navegador.
#[derive(Debug, Clone)]
pub struct TotalesCliente {
    pub subtotal: f64,
    pub descuento: f64,
    pub total: f64,
    pub monto_recibido: f64,
    /// Informativo: el servidor recalcula el cambio (recibido - total).
    #[allow(dead_code)]
    pub cambio: f64,
}

/// Partida recalculada, redondeada a centavos para guardarse.
#[derive(Debug, Clone, PartialEq)]
pub struct LineaCalculada {
    pub cantidad: f64,
    pub precio_original: f64,
    pub descuento_porcentaje: f64,
    pub descuento_monto: f64,
    pub precio_final: f64,
    pub subtotal: f64,
    /// true si el descuento excede lo permitido sin PIN y se aceptó por un
    /// token de autorización.
    pub uso_autorizacion: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VentaCalculada {
    pub lineas: Vec<LineaCalculada>,
    pub subtotal: f64,
    pub descuento: f64,
    pub total: f64,
    pub monto_recibido: f64,
    pub cambio: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ErrorVenta {
    SinItems,
    DemasiadasPartidas(usize),
    Invalido(String),
    PrecioCambio { nombre: String, enviado: f64, actual: f64 },
    DescuentoNoAutorizado { nombre: String, pct: f64, max: f64 },
    Inconsistente(String),
    PagoInsuficiente { total: f64, recibido: f64 },
    CambioFueraDeRango { cambio: f64 },
}

impl ErrorVenta {
    /// Mensaje para el cajero. Empieza con un código estable para que el
    /// frontend pueda reaccionar (p. ej. PRECIO_CAMBIO → refrescar precios).
    pub fn mensaje(&self) -> String {
        match self {
            Self::SinItems => "VENTA_INVALIDA: La venta no tiene productos".into(),
            Self::DemasiadasPartidas(n) => format!(
                "VENTA_INVALIDA: La venta tiene {} partidas (máximo {})", n, MAX_PARTIDAS),
            Self::Invalido(m) => format!("VENTA_INVALIDA: {}", m),
            Self::PrecioCambio { nombre, enviado, actual } => format!(
                "PRECIO_CAMBIO: El precio de '{}' cambió de ${:.2} a ${:.2}. \
                 Se actualizaron los precios; revisa el carrito y cobra de nuevo.",
                nombre, enviado, actual),
            Self::DescuentoNoAutorizado { nombre, pct, max } => format!(
                "DESCUENTO_NO_AUTORIZADO: El descuento de {}% en '{}' excede el \
                 límite de {}% y no tiene autorización vigente del dueño. \
                 Vuelve a aplicar el descuento con el PIN.",
                pct, nombre, max),
            Self::Inconsistente(m) => format!(
                "TOTALES_INCONSISTENTES: {}. Recarga la página y vuelve a capturar la venta.", m),
            Self::PagoInsuficiente { total, recibido } => format!(
                "PAGO_INSUFICIENTE: Recibido ${:.2} es menor al total ${:.2}", recibido, total),
            Self::CambioFueraDeRango { cambio } => format!(
                "MONTO_RECIBIDO_INVALIDO: El cambio calculado (${:.2}) es demasiado alto. \
                 ¿Se escaneó un código en el campo de efectivo?", cambio),
        }
    }
}

/// Redondeo a centavos, mitad lejos de cero (igual que numeric de Postgres).
pub fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Réplica de `ventaStore.total()`: redondea HACIA ARRIBA al peso; si `raw`
/// ya es entero (al centavo) lo deja igual.
pub fn total_a_cobrar(raw: f64) -> f64 {
    if !(raw > 0.0) {
        return 0.0;
    }
    // JS: Math.round(raw * 100). Para positivos f64::round es idéntico.
    let cents = (raw * 100.0).round();
    if cents % 100.0 == 0.0 {
        cents / 100.0
    } else {
        (cents / 100.0).ceil()
    }
}

fn tiene_max_2_decimales(x: f64) -> bool {
    let c = x * 100.0;
    (c - c.round()).abs() < 1e-6
}

fn cerca(a: f64, b: f64) -> bool {
    (a - b).abs() <= EPS
}

/// Partida recalculada SIN redondear (bit a bit lo mismo que el frontend).
fn aritmetica_linea(precio_original: f64, pct: f64, cantidad: f64) -> (f64, f64, f64) {
    let dm = precio_original * (pct / 100.0);
    let pf = precio_original - dm;
    let sub = pf * cantidad;
    (dm, pf, sub)
}

pub fn calcular(
    lineas: &[LineaCliente],
    reglas: &[ReglaLinea],
    totales: &TotalesCliente,
    metodo: MetodoPago,
) -> Result<VentaCalculada, ErrorVenta> {
    if lineas.is_empty() {
        return Err(ErrorVenta::SinItems);
    }
    if lineas.len() > MAX_PARTIDAS {
        return Err(ErrorVenta::DemasiadasPartidas(lineas.len()));
    }
    assert_eq!(lineas.len(), reglas.len(), "una regla por partida");

    let mut out = Vec::with_capacity(lineas.len());
    let mut raw = 0.0_f64;
    let mut desc_total = 0.0_f64;

    for (l, r) in lineas.iter().zip(reglas) {
        // 1. Cordura de entradas (JSON no trae NaN/Inf, pero por si acaso).
        let todos = [l.cantidad, l.precio_original, l.descuento_porcentaje,
                     l.descuento_monto, l.precio_final, l.subtotal];
        if todos.iter().any(|v| !v.is_finite()) {
            return Err(ErrorVenta::Invalido(format!("'{}': importe no numérico", r.nombre)));
        }
        if !(l.cantidad > 0.0) || l.cantidad > MAX_CANTIDAD || !tiene_max_2_decimales(l.cantidad) {
            return Err(ErrorVenta::Invalido(format!(
                "'{}': cantidad inválida ({})", r.nombre, l.cantidad)));
        }
        if l.precio_original < 0.0 || l.precio_original > MAX_IMPORTE {
            return Err(ErrorVenta::Invalido(format!(
                "'{}': precio inválido ({})", r.nombre, l.precio_original)));
        }
        let pct = l.descuento_porcentaje;
        if !(0.0..=100.0).contains(&pct) || !tiene_max_2_decimales(pct) {
            return Err(ErrorVenta::Invalido(format!(
                "'{}': descuento inválido ({}%)", r.nombre, pct)));
        }

        // 2. Precio: debe ser el precio de venta vigente (o uno permitido).
        //    Desde aquí se usa el precio del SERVIDOR (`po`), no el del
        //    navegador: la tolerancia EPS es para ruido de flotantes, no para
        //    que una diferencia de 0.004 se multiplique por la cantidad.
        let po = match r.precios_validos.iter().copied().find(|p| cerca(*p, l.precio_original)) {
            Some(p) => p,
            None => {
                return Err(ErrorVenta::PrecioCambio {
                    nombre: r.nombre.clone(),
                    enviado: l.precio_original,
                    actual: r.precios_validos.first().copied().unwrap_or(0.0),
                });
            }
        };

        // 3. Descuento: dentro del límite, o autorizado por un dueño.
        let excede = pct > r.pct_max_sin_autorizacion + 1e-9;
        if excede {
            let ok = r.pct_autorizado.map(|a| pct <= a + 1e-9).unwrap_or(false);
            if !ok {
                return Err(ErrorVenta::DescuentoNoAutorizado {
                    nombre: r.nombre.clone(),
                    pct,
                    max: r.pct_max_sin_autorizacion,
                });
            }
        }

        // 4. Aritmética de la partida con el precio del servidor; lo que
        //    mostró el navegador debe coincidir.
        let (dm, pf, sub) = aritmetica_linea(po, pct, l.cantidad);
        if !cerca(dm, l.descuento_monto) || !cerca(pf, l.precio_final) || !cerca(sub, l.subtotal) {
            return Err(ErrorVenta::Inconsistente(format!(
                "la partida '{}' no cuadra (esperado ${:.2} × {} = ${:.2})",
                r.nombre, pf, l.cantidad, sub)));
        }
        raw += sub;
        desc_total += dm * l.cantidad;

        out.push(LineaCalculada {
            cantidad: l.cantidad,
            precio_original: round2(po),
            descuento_porcentaje: pct,
            descuento_monto: round2(dm),
            precio_final: round2(pf),
            subtotal: round2(sub),
            uso_autorizacion: excede,
        });
    }

    // 5. Encabezado.
    if !totales.subtotal.is_finite() || !cerca(totales.subtotal, raw) {
        return Err(ErrorVenta::Inconsistente(format!(
            "el subtotal enviado (${:.2}) no coincide con las partidas (${:.2})",
            totales.subtotal, raw)));
    }
    if !totales.descuento.is_finite() || !cerca(totales.descuento, desc_total) {
        return Err(ErrorVenta::Inconsistente(format!(
            "el descuento enviado (${:.2}) no coincide con las partidas (${:.2})",
            totales.descuento, desc_total)));
    }
    // El total es el monto que el cajero cobró. Debe ser el redondeo al peso
    // del subtotal. Se aceptan los candidatos de raw ± 1e-6 para tolerar el
    // ruido de 1 ULP del parseo JSON justo en la frontera de medio centavo.
    let candidatos = [total_a_cobrar(raw - 1e-6), total_a_cobrar(raw), total_a_cobrar(raw + 1e-6)];
    if !totales.total.is_finite() || !candidatos.iter().any(|c| cerca(*c, totales.total)) {
        return Err(ErrorVenta::Inconsistente(format!(
            "el total enviado (${:.2}) no corresponde al subtotal (${:.2} → ${:.2})",
            totales.total, raw, total_a_cobrar(raw))));
    }
    let total = round2(totales.total);

    // 6. Pago. En tarjeta/transferencia se normaliza (el frontend ya manda
    //    recibido = total y cambio = 0). En efectivo se valida.
    let (monto_recibido, cambio) = match metodo {
        MetodoPago::Efectivo => {
            let rec = totales.monto_recibido;
            if !rec.is_finite() || rec < 0.0 || rec > MAX_IMPORTE {
                return Err(ErrorVenta::Invalido(format!("monto recibido inválido ({})", rec)));
            }
            if rec + EPS < total {
                return Err(ErrorVenta::PagoInsuficiente { total, recibido: rec });
            }
            let cambio = round2((rec - total).max(0.0));
            if cambio > MAX_CAMBIO {
                return Err(ErrorVenta::CambioFueraDeRango { cambio });
            }
            (round2(rec), cambio)
        }
        _ => (total, 0.0),
    };

    Ok(VentaCalculada {
        lineas: out,
        subtotal: round2(raw),
        descuento: round2(desc_total),
        total,
        monto_recibido,
        cambio,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regla(precio: f64, max: f64) -> ReglaLinea {
        ReglaLinea { nombre: "X".into(), precios_validos: vec![precio], pct_max_sin_autorizacion: max, pct_autorizado: None }
    }

    /// Construye la partida exactamente como lo hace ventaStore.ts.
    fn linea_js(precio: f64, pct: f64, cant: f64) -> LineaCliente {
        let dm = precio * (pct / 100.0);
        let pf = precio - dm;
        LineaCliente { cantidad: cant, precio_original: precio, descuento_porcentaje: pct,
                       descuento_monto: dm, precio_final: pf, subtotal: pf * cant }
    }

    fn totales_js(ls: &[LineaCliente], recibido: f64) -> TotalesCliente {
        let raw: f64 = ls.iter().map(|l| l.subtotal).sum();
        let desc: f64 = ls.iter().map(|l| l.descuento_monto * l.cantidad).sum();
        let total = total_a_cobrar(raw);
        TotalesCliente { subtotal: raw, descuento: desc, total, monto_recibido: recibido,
                         cambio: (recibido - total).max(0.0) }
    }

    #[test]
    fn venta_852_de_produccion_cuadra() {
        // V-20260505-0001: cliente 12%, 60 + 15 → subtotal 66, desc 9, total 66, recibe 100.
        let ls = vec![linea_js(60.0, 12.0, 1.0), linea_js(15.0, 12.0, 1.0)];
        let t = totales_js(&ls, 100.0);
        let r = calcular(&ls, &[regla(60.0, 15.0), regla(15.0, 15.0)], &t, MetodoPago::Efectivo).unwrap();
        assert_eq!((r.subtotal, r.descuento, r.total, r.cambio), (66.0, 9.0, 66.0, 34.0));
        assert_eq!(r.lineas[0].descuento_monto, 7.2);
        assert_eq!(r.lineas[0].precio_final, 52.8);
    }

    #[test]
    fn redondeo_al_peso_hacia_arriba() {
        assert_eq!(total_a_cobrar(65.12), 66.0);
        assert_eq!(total_a_cobrar(66.0), 66.0);
        assert_eq!(total_a_cobrar(66.000000000001), 66.0);
        assert_eq!(total_a_cobrar(0.0), 0.0);
        assert_eq!(total_a_cobrar(84.9951), 85.0);
        assert_eq!(total_a_cobrar(85.006), 86.0);
    }

    #[test]
    fn total_manipulado_se_rechaza() {
        let ls = vec![linea_js(300.0, 0.0, 1.0)];
        let mut t = totales_js(&ls, 300.0);
        t.total = 30.0;
        let e = calcular(&ls, &[regla(300.0, 15.0)], &t, MetodoPago::Efectivo).unwrap_err();
        assert!(matches!(e, ErrorVenta::Inconsistente(_)));
    }

    #[test]
    fn precio_rebajado_por_el_cliente_se_rechaza() {
        let ls = vec![linea_js(1.0, 0.0, 1.0)];
        let t = totales_js(&ls, 1.0);
        let e = calcular(&ls, &[regla(296.0, 15.0)], &t, MetodoPago::Efectivo).unwrap_err();
        assert!(matches!(e, ErrorVenta::PrecioCambio { actual, .. } if actual == 296.0));
        assert!(e.mensaje().starts_with("PRECIO_CAMBIO:"));
    }

    #[test]
    fn precio_de_presupuesto_es_alternativa_valida() {
        let ls = vec![linea_js(300.0, 0.0, 1.0)];
        let t = totales_js(&ls, 300.0);
        let mut r = regla(296.0, 15.0);
        r.precios_validos.push(300.0);
        assert!(calcular(&ls, &[r], &t, MetodoPago::Efectivo).is_ok());
    }

    #[test]
    fn descuento_sobre_limite_requiere_autorizacion() {
        let ls = vec![linea_js(100.0, 30.0, 1.0)];
        let t = totales_js(&ls, 70.0);
        let e = calcular(&ls, &[regla(100.0, 15.0)], &t, MetodoPago::Efectivo).unwrap_err();
        assert!(matches!(e, ErrorVenta::DescuentoNoAutorizado { .. }));

        let mut r = regla(100.0, 15.0);
        r.pct_autorizado = Some(30.0);
        let ok = calcular(&ls, &[r], &t, MetodoPago::Efectivo).unwrap();
        assert!(ok.lineas[0].uso_autorizacion);

        let mut r = regla(100.0, 15.0);
        r.pct_autorizado = Some(20.0); // autorizó 20, se aplicó 30
        assert!(calcular(&ls, &[r], &t, MetodoPago::Efectivo).is_err());
    }

    #[test]
    fn descuento_de_cliente_20_sin_pin_es_valido() {
        // Cliente LUIS Y ESTHER (20%) en producción: límite = max(15, 20).
        let ls = vec![linea_js(99.0, 20.0, 3.0)];
        let t = totales_js(&ls, 300.0);
        assert!(calcular(&ls, &[regla(99.0, 20.0)], &t, MetodoPago::Efectivo).is_ok());
    }

    #[test]
    fn admin_puede_dar_100() {
        let ls = vec![linea_js(50.0, 100.0, 1.0)];
        let t = totales_js(&ls, 0.0);
        let r = calcular(&ls, &[regla(50.0, 100.0)], &t, MetodoPago::Efectivo).unwrap();
        assert_eq!(r.total, 0.0);
    }

    #[test]
    fn producto_con_precio_cero_es_valido() {
        // 4 partidas así en producción (V-000377, V-003950, V-006948).
        let ls = vec![linea_js(0.0, 0.0, 2.0)];
        let t = totales_js(&ls, 0.0);
        assert!(calcular(&ls, &[regla(0.0, 15.0)], &t, MetodoPago::Efectivo).is_ok());
    }

    #[test]
    fn cantidades_y_porcentajes_invalidos() {
        for (p, pct, c) in [(10.0, 0.0, 0.0), (10.0, 0.0, -1.0), (10.0, -5.0, 1.0),
                            (10.0, 101.0, 1.0), (-10.0, 0.0, 1.0), (10.0, 0.0, 0.333)] {
            let ls = vec![linea_js(p, pct, c)];
            let t = totales_js(&ls, 1000.0);
            let e = calcular(&ls, &[regla(p, 100.0)], &t, MetodoPago::Efectivo).unwrap_err();
            assert!(matches!(e, ErrorVenta::Invalido(_)), "{p} {pct} {c}: {e:?}");
        }
    }

    #[test]
    fn efectivo_insuficiente_y_cambio_absurdo() {
        let ls = vec![linea_js(135.0, 0.0, 1.0)];
        let t = totales_js(&ls, 100.0);
        assert!(matches!(calcular(&ls, &[regla(135.0, 15.0)], &t, MetodoPago::Efectivo),
                         Err(ErrorVenta::PagoInsuficiente { .. })));
        let t = totales_js(&ls, 8_081_512.0);
        assert!(calcular(&ls, &[regla(135.0, 15.0)], &t, MetodoPago::Efectivo).is_err());
    }

    #[test]
    fn tarjeta_normaliza_recibido_y_cambio() {
        let ls = vec![linea_js(1440.0, 0.0, 1.0)];
        let mut t = totales_js(&ls, 0.0);
        t.monto_recibido = 99999.0;
        t.cambio = 5.0;
        let r = calcular(&ls, &[regla(1440.0, 15.0)], &t, MetodoPago::Tarjeta).unwrap();
        assert_eq!((r.monto_recibido, r.cambio), (1440.0, 0.0));
    }

    #[test]
    fn partida_alterada_se_rechaza() {
        let mut l = linea_js(60.0, 12.0, 2.0);
        l.subtotal = 1.0;
        let t = TotalesCliente { subtotal: 1.0, descuento: 14.4, total: 1.0, monto_recibido: 1.0, cambio: 0.0 };
        assert!(matches!(calcular(&[l], &[regla(60.0, 15.0)], &t, MetodoPago::Efectivo),
                         Err(ErrorVenta::Inconsistente(_))));
    }

    #[test]
    fn barrido_de_combinaciones_reales_siempre_cuadra() {
        // Precios enteros y con centavos × descuentos usados en producción
        // (0/10/12/15/20) y algunos raros × cantidades 1..40: lo que produce
        // el frontend siempre debe pasar.
        let precios = [0.0, 7.0, 15.0, 29.0, 52.5, 60.0, 99.99, 296.0, 1627.0];
        let pcts = [0.0, 10.0, 12.0, 12.5, 15.0, 20.0, 33.33, 100.0];
        for &p in &precios { for &pct in &pcts { for c in 1..=40 {
            let ls = vec![linea_js(p, pct, c as f64), linea_js(15.0, 12.0, 1.0)];
            let t = totales_js(&ls, 1_000_000.0_f64.min(total_a_cobrar(ls.iter().map(|l| l.subtotal).sum()) + 500.0));
            let rs = [regla(p, 100.0), regla(15.0, 100.0)];
            let r = calcular(&ls, &rs, &t, MetodoPago::Efectivo);
            assert!(r.is_ok(), "p={p} pct={pct} c={c}: {r:?}");
        }}}
    }

    #[test]
    fn importes_salen_del_precio_del_servidor() {
        // Precio del navegador 100.004 vs servidor 100 (dentro de EPS).
        // Con cantidad 1 se acepta, pero lo guardado es 100.00, no 100.004.
        let ls = vec![linea_js(100.004, 0.0, 1.0)];
        let t = totales_js(&ls, 100.0);
        let r = calcular(&ls, &[regla(100.0, 15.0)], &t, MetodoPago::Efectivo).unwrap();
        assert_eq!(r.lineas[0].precio_original, 100.0);
        assert_eq!(r.lineas[0].subtotal, 100.0);
        assert_eq!(r.subtotal, 100.0);

        // Con cantidad 40 la diferencia (0.16) ya no cuadra: se rechaza en
        // vez de guardar 4000.16.
        let ls = vec![linea_js(100.004, 0.0, 40.0)];
        let t = totales_js(&ls, 5000.0);
        match calcular(&ls, &[regla(100.0, 15.0)], &t, MetodoPago::Efectivo) {
            Err(ErrorVenta::Inconsistente(_)) => {}
            Ok(v) => {
                // Si algún día se aceptara, tendría que ser con 100.00 × 40.
                assert_eq!(v.lineas[0].subtotal, 4000.0);
                assert_ne!(v.subtotal, 4000.16);
            }
            Err(e) => panic!("error inesperado: {e:?}"),
        }
    }

    #[test]
    fn porcentaje_con_mas_de_dos_decimales() {
        let ls = vec![linea_js(100.0, 33.33, 1.0)];
        let t = totales_js(&ls, 100.0);
        assert!(calcular(&ls, &[regla(100.0, 100.0)], &t, MetodoPago::Efectivo).is_ok());

        let ls = vec![linea_js(100.0, 33.333, 1.0)];
        let t = totales_js(&ls, 100.0);
        let e = calcular(&ls, &[regla(100.0, 100.0)], &t, MetodoPago::Efectivo).unwrap_err();
        assert!(matches!(e, ErrorVenta::Invalido(_)), "{e:?}");
        assert!(e.mensaje().starts_with("VENTA_INVALIDA:"));
    }

    #[test]
    fn json_roundtrip_no_rompe_la_validacion() {
        // El navegador serializa con JSON.stringify y serde_json parsea sin
        // `float_roundtrip`: puede haber 1 ULP de diferencia.
        let ls = vec![linea_js(99.99, 12.5, 3.0), linea_js(52.5, 33.33, 7.0)];
        let t = totales_js(&ls, 1000.0);
        let j = serde_json::json!({
            "l": ls.iter().map(|l| [l.cantidad, l.precio_original, l.descuento_porcentaje,
                                     l.descuento_monto, l.precio_final, l.subtotal]).collect::<Vec<_>>(),
            "t": [t.subtotal, t.descuento, t.total, t.monto_recibido, t.cambio],
        });
        let txt = serde_json::to_string(&j).unwrap();
        let v: serde_json::Value = serde_json::from_str(&txt).unwrap();
        let ls2: Vec<LineaCliente> = v["l"].as_array().unwrap().iter().map(|a| {
            let f = |i: usize| a[i].as_f64().unwrap();
            LineaCliente { cantidad: f(0), precio_original: f(1), descuento_porcentaje: f(2),
                           descuento_monto: f(3), precio_final: f(4), subtotal: f(5) }
        }).collect();
        let f = |i: usize| v["t"][i].as_f64().unwrap();
        let t2 = TotalesCliente { subtotal: f(0), descuento: f(1), total: f(2), monto_recibido: f(3), cambio: f(4) };
        assert!(calcular(&ls2, &[regla(99.99, 100.0), regla(52.5, 100.0)], &t2, MetodoPago::Efectivo).is_ok());
    }
}
