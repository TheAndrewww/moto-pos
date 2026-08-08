// commands/auditoria_caja.rs — Auditoría de solo lectura del flujo de caja.
//
// Diagnostica por qué los cortes no cuadran. NO modifica nada: solo lee y
// reporta. Detecta las 5 fugas conocidas del módulo de cortes:
//
//   1. VENTAS SIN CORTE — ventas que no caen dentro del rango de ningún
//      corte. Ocurre cuando se salta un día de cierre: el período del
//      siguiente corte arranca después, y ese dinero nunca se concilia.
//
//   2. MOVIMIENTOS DESALINEADOS — movimientos asignados a un corte cuyo
//      rango de fechas NO contiene la fecha del movimiento. Consecuencia
//      de que la asignación es `WHERE corte_id IS NULL` sin filtro de
//      fecha: barre movimientos viejos hacia el corte actual.
//
//   3. CADENA DE FONDOS ROTA — el `fondo_siguiente` que declaró un cierre
//      vs el `fondo_declarado` de la apertura siguiente. Si no coinciden,
//      hay dinero que apareció o desapareció entre el cierre y la apertura
//      sin registro.
//
//   4. CORTES INCONSISTENTES — cortes donde la fórmula
//      (fondo_inicial + ventas_efectivo + entradas − retiros) no da el
//      `efectivo_esperado` guardado. Señal de que los totales se
//      calcularon en momentos distintos (race entre abrir el modal y
//      confirmar).
//
//   5. DÍAS CON VENTAS SIN CIERRE — el disparador del problema #1.

use serde::Serialize;
use tauri::State;
use super::auth::AppState;

#[derive(Serialize)]
pub struct VentaSinCorte {
    pub id: i64,
    pub folio: String,
    pub fecha: String,
    pub total: f64,
    pub metodo_pago: String,
}

#[derive(Serialize)]
pub struct MovimientoDesalineado {
    pub id: i64,
    pub tipo: String,
    pub monto: f64,
    pub concepto: String,
    pub fecha: String,
    pub corte_id: i64,
    pub corte_tipo: String,
    pub corte_inicio: String,
    pub corte_fin: String,
}

#[derive(Serialize)]
pub struct EslabonFondo {
    pub corte_id: i64,
    pub dia_cerrado: String,
    pub fondo_dejado: f64,
    /// Fondo que declaró la siguiente apertura. None si aún no hay
    /// apertura posterior a este cierre.
    pub fondo_declarado_siguiente: Option<f64>,
    pub fecha_apertura_siguiente: Option<String>,
    /// fondo_declarado_siguiente − fondo_dejado. Positivo = apareció
    /// dinero; negativo = desapareció.
    pub diferencia: Option<f64>,
}

#[derive(Serialize)]
pub struct CorteInconsistente {
    pub id: i64,
    pub tipo: String,
    pub created_at: String,
    pub fondo_inicial: f64,
    pub ventas_efectivo: f64,
    pub entradas: f64,
    pub retiros: f64,
    pub esperado_guardado: f64,
    pub esperado_recalculado: f64,
    pub descuadre: f64,
}

#[derive(Serialize)]
pub struct DiaSinCierre {
    pub dia: String,
    pub num_ventas: i64,
    pub total_ventas: f64,
    pub total_efectivo: f64,
}

#[derive(Serialize)]
pub struct ResumenAuditoria {
    pub ventas_sin_corte_count: i64,
    pub ventas_sin_corte_monto: f64,
    pub ventas_sin_corte_efectivo: f64,
    pub movimientos_desalineados_count: i64,
    pub movimientos_desalineados_monto: f64,
    pub eslabones_rotos_count: i64,
    pub eslabones_rotos_monto: f64,
    pub cortes_inconsistentes_count: i64,
    pub dias_sin_cierre_count: i64,
    /// Suma algebraica de todas las fugas detectadas. Es la mejor
    /// estimación de cuánto dinero "no cuadra" en total.
    pub impacto_total_estimado: f64,
}

#[derive(Serialize)]
pub struct AuditoriaCaja {
    pub resumen: ResumenAuditoria,
    pub ventas_sin_corte: Vec<VentaSinCorte>,
    pub movimientos_desalineados: Vec<MovimientoDesalineado>,
    pub cadena_fondos: Vec<EslabonFondo>,
    pub cortes_inconsistentes: Vec<CorteInconsistente>,
    pub dias_sin_cierre: Vec<DiaSinCierre>,
}

/// Ejecuta la auditoría completa del flujo de caja.
///
/// `limite_dias`: cuántos días hacia atrás analizar (default 90). Limitar
/// evita traer años de historia en negocios con mucho volumen.
#[tauri::command]
pub fn auditar_caja(
    limite_dias: Option<i64>,
    state: State<'_, AppState>,
) -> Result<AuditoriaCaja, String> {
    let dias = limite_dias.unwrap_or(90).clamp(1, 3650);
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let desde: String = db.query_row(
        "SELECT date('now', 'localtime', ?)",
        rusqlite::params![format!("-{} days", dias)],
        |r| r.get(0),
    ).unwrap_or_else(|_| "2000-01-01".to_string());

    // ── 1. Ventas que no caen en el rango de ningún corte ──
    // El cálculo del corte usa `fecha > inicio AND fecha <= fin`, así que
    // replicamos esa semántica exacta para detectar los huecos reales.
    let mut stmt = db.prepare(
        r#"SELECT v.id, v.folio, v.fecha, v.total, v.metodo_pago
           FROM ventas v
           WHERE v.anulada = 0
             AND date(v.fecha) >= ?
             AND NOT EXISTS (
                 SELECT 1 FROM cortes c
                 WHERE v.fecha > c.fecha_inicio AND v.fecha <= c.fecha_fin
             )
           ORDER BY v.fecha ASC"#,
    ).map_err(|e| e.to_string())?;

    let ventas_sin_corte: Vec<VentaSinCorte> = stmt.query_map(
        rusqlite::params![desde],
        |row| Ok(VentaSinCorte {
            id: row.get(0)?,
            folio: row.get(1)?,
            fecha: row.get(2)?,
            total: row.get(3)?,
            metodo_pago: row.get(4)?,
        }),
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    let ventas_sin_corte_monto: f64 = ventas_sin_corte.iter().map(|v| v.total).sum();
    let ventas_sin_corte_efectivo: f64 = ventas_sin_corte.iter()
        .filter(|v| v.metodo_pago == "efectivo")
        .map(|v| v.total)
        .sum();

    // ── 2. Movimientos cuya fecha cae fuera del rango de su propio corte ──
    let mut stmt = db.prepare(
        r#"SELECT m.id, m.tipo, m.monto, m.concepto, m.fecha,
                  c.id, c.tipo, c.fecha_inicio, c.fecha_fin
           FROM movimientos_caja m
           JOIN cortes c ON c.id = m.corte_id
           WHERE date(m.fecha) >= ?
             AND NOT (m.fecha > c.fecha_inicio AND m.fecha <= c.fecha_fin)
           ORDER BY m.fecha ASC"#,
    ).map_err(|e| e.to_string())?;

    let movimientos_desalineados: Vec<MovimientoDesalineado> = stmt.query_map(
        rusqlite::params![desde],
        |row| Ok(MovimientoDesalineado {
            id: row.get(0)?,
            tipo: row.get(1)?,
            monto: row.get(2)?,
            concepto: row.get(3)?,
            fecha: row.get(4)?,
            corte_id: row.get(5)?,
            corte_tipo: row.get(6)?,
            corte_inicio: row.get(7)?,
            corte_fin: row.get(8)?,
        }),
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    // Impacto con signo: un RETIRO desalineado resta de más (falta dinero),
    // una ENTRADA desalineada suma de más (sobra dinero).
    let movimientos_desalineados_monto: f64 = movimientos_desalineados.iter()
        .map(|m| if m.tipo == "RETIRO" { -m.monto } else { m.monto })
        .sum();

    // ── 3. Cadena de fondos: cierre N → apertura N+1 ──
    let mut stmt = db.prepare(
        r#"SELECT c.id, date(c.fecha_fin), c.fondo_siguiente, c.created_at,
                  (SELECT a.fondo_declarado FROM aperturas_caja a
                    WHERE a.fecha > c.created_at ORDER BY a.fecha ASC LIMIT 1),
                  (SELECT a.fecha FROM aperturas_caja a
                    WHERE a.fecha > c.created_at ORDER BY a.fecha ASC LIMIT 1)
           FROM cortes c
           WHERE c.tipo = 'DIA' AND date(c.fecha_fin) >= ?
           ORDER BY c.fecha_fin DESC"#,
    ).map_err(|e| e.to_string())?;

    let cadena_fondos: Vec<EslabonFondo> = stmt.query_map(
        rusqlite::params![desde],
        |row| {
            let fondo_dejado: f64 = row.get(2)?;
            let siguiente: Option<f64> = row.get(4)?;
            Ok(EslabonFondo {
                corte_id: row.get(0)?,
                dia_cerrado: row.get(1)?,
                fondo_dejado,
                fondo_declarado_siguiente: siguiente,
                fecha_apertura_siguiente: row.get(5)?,
                diferencia: siguiente.map(|s| s - fondo_dejado),
            })
        },
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    // Un eslabón está roto si hay apertura posterior y difiere más de 1 centavo.
    let eslabones_rotos: Vec<&EslabonFondo> = cadena_fondos.iter()
        .filter(|e| e.diferencia.map(|d| d.abs() > 0.01).unwrap_or(false))
        .collect();
    let eslabones_rotos_monto: f64 = eslabones_rotos.iter()
        .filter_map(|e| e.diferencia)
        .sum();
    let eslabones_rotos_count = eslabones_rotos.len() as i64;

    // ── 4. Cortes donde la fórmula no da el esperado guardado ──
    let mut stmt = db.prepare(
        r#"SELECT id, tipo, created_at, fondo_inicial, total_ventas_efectivo,
                  total_entradas_efectivo, total_retiros_efectivo, efectivo_esperado
           FROM cortes
           WHERE date(created_at) >= ?
             AND ABS((fondo_inicial + total_ventas_efectivo
                      + total_entradas_efectivo - total_retiros_efectivo)
                     - efectivo_esperado) > 0.01
           ORDER BY created_at DESC"#,
    ).map_err(|e| e.to_string())?;

    let cortes_inconsistentes: Vec<CorteInconsistente> = stmt.query_map(
        rusqlite::params![desde],
        |row| {
            let fondo_inicial: f64 = row.get(3)?;
            let ventas_efectivo: f64 = row.get(4)?;
            let entradas: f64 = row.get(5)?;
            let retiros: f64 = row.get(6)?;
            let esperado_guardado: f64 = row.get(7)?;
            let recalculado = fondo_inicial + ventas_efectivo + entradas - retiros;
            Ok(CorteInconsistente {
                id: row.get(0)?,
                tipo: row.get(1)?,
                created_at: row.get(2)?,
                fondo_inicial,
                ventas_efectivo,
                entradas,
                retiros,
                esperado_guardado,
                esperado_recalculado: recalculado,
                descuadre: recalculado - esperado_guardado,
            })
        },
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    // ── 5. Días con ventas pero sin cierre ──
    // Excluimos HOY: aún puede cerrarse, no es un hueco todavía.
    let mut stmt = db.prepare(
        r#"SELECT date(v.fecha) AS dia,
                  COUNT(*),
                  COALESCE(SUM(v.total), 0),
                  COALESCE(SUM(CASE WHEN v.metodo_pago = 'efectivo' THEN v.total ELSE 0 END), 0)
           FROM ventas v
           WHERE v.anulada = 0
             AND date(v.fecha) >= ?
             AND date(v.fecha) < date('now', 'localtime')
             AND NOT EXISTS (
                 SELECT 1 FROM cortes c
                 WHERE c.tipo = 'DIA' AND date(c.fecha_fin) = date(v.fecha)
             )
           GROUP BY dia
           ORDER BY dia DESC"#,
    ).map_err(|e| e.to_string())?;

    let dias_sin_cierre: Vec<DiaSinCierre> = stmt.query_map(
        rusqlite::params![desde],
        |row| Ok(DiaSinCierre {
            dia: row.get(0)?,
            num_ventas: row.get(1)?,
            total_ventas: row.get(2)?,
            total_efectivo: row.get(3)?,
        }),
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    // ── Resumen ──
    // El impacto total suma las fugas que afectan efectivo:
    //   - ventas en efectivo que nunca se conciliaron (dinero que entró
    //     a la caja pero ningún corte lo esperaba)
    //   - movimientos aplicados al corte equivocado (con signo)
    //   - saltos en la cadena de fondos
    let impacto_total_estimado =
        ventas_sin_corte_efectivo + movimientos_desalineados_monto + eslabones_rotos_monto;

    let resumen = ResumenAuditoria {
        ventas_sin_corte_count: ventas_sin_corte.len() as i64,
        ventas_sin_corte_monto,
        ventas_sin_corte_efectivo,
        movimientos_desalineados_count: movimientos_desalineados.len() as i64,
        movimientos_desalineados_monto,
        eslabones_rotos_count,
        eslabones_rotos_monto,
        cortes_inconsistentes_count: cortes_inconsistentes.len() as i64,
        dias_sin_cierre_count: dias_sin_cierre.len() as i64,
        impacto_total_estimado,
    };

    Ok(AuditoriaCaja {
        resumen,
        ventas_sin_corte,
        movimientos_desalineados,
        cadena_fondos,
        cortes_inconsistentes,
        dias_sin_cierre,
    })
}
