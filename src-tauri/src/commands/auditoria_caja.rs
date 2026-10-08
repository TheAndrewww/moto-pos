// commands/auditoria_caja.rs — Auditoría de solo lectura del flujo de caja.
//
// Diagnostica descuadres HISTÓRICOS del módulo de cortes. No modifica nada.
// Desde v0.1.39 el cálculo de cortes es una cadena continua (ver el
// encabezado de cortes.rs), así que estas fugas ya no se generan; la
// auditoría sirve para dimensionar lo que dejó el modelo anterior.
//
//   1. VENTAS SIN CORTE — ventas que no caen en el rango de ningún corte y
//      que ya no pueden entrar en uno (son anteriores al último corte).
//   2. MOVIMIENTOS DESALINEADOS — movimientos asignados a un corte cuyo rango
//      no contiene su fecha.
//   3. MOVIMIENTOS HUÉRFANOS — movimientos sin corte anteriores al último
//      corte: ningún corte los contó y ya ninguno los contará.
//   4. CADENA ROTA — cortes cuyo fondo inicial no es el fondo que dejó el
//      corte anterior. Es el síntoma de "al día siguiente no cuadra con lo
//      que se dejó".
//   5. CORTES INCONSISTENTES — fondo + ventas efectivo + entradas − retiros
//      ≠ esperado guardado.
//   6. DÍAS CON VENTAS SIN CIERRE — informativo.
//
// Solo se audita la caja de la tienda (caja 'principal'): las ventas de la
// caja web nunca entran en los cortes del escritorio. Ventas y movimientos se
// ubican por MOMENTO = COALESCE(registrado_at, fecha), igual que en cortes.rs:
// una venta web que llegó después de un corte cuenta en el siguiente y no es
// una fuga.
//
// Tampoco se auditan las ventas borradas (deleted_at, borrado lógico del
// sync) ni lo que la web registró ANTES de que existiera la cadena de esta PC
// (anterior a su primera apertura; p. ej. el historial web que trae el
// primer pull en una PC nueva): ese dinero nunca pasó por este cajón.

use serde::Serialize;
use tauri::State;
use super::auth::AppState;
use super::cortes::{
    inicio_y_fondo, origen_de_la_cadena, FIN_CORTE_SQL, MOMENTO_M_SQL, MOMENTO_SQL, MOMENTO_V_SQL,
};

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
pub struct MovimientoHuerfano {
    pub id: i64,
    pub tipo: String,
    pub monto: f64,
    pub concepto: String,
    pub fecha: String,
}

#[derive(Serialize)]
pub struct EslabonRoto {
    pub corte_id: i64,
    pub tipo: String,
    pub fecha: String,
    /// Fondo que dejó el corte anterior.
    pub fondo_anterior: f64,
    /// Fondo con el que arrancó este corte.
    pub fondo_inicial: f64,
    /// fondo_inicial − fondo_anterior. Negativo = desapareció dinero.
    pub diferencia: f64,
}

#[derive(Serialize)]
pub struct CorteInconsistente {
    pub id: i64,
    pub tipo: String,
    pub created_at: String,
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
    pub ventas_sin_corte_efectivo: f64,
    pub movimientos_desalineados_count: i64,
    pub movimientos_desalineados_monto: f64,
    pub movimientos_huerfanos_count: i64,
    pub movimientos_huerfanos_monto: f64,
    pub eslabones_rotos_count: i64,
    pub eslabones_rotos_monto: f64,
    pub cortes_inconsistentes_count: i64,
    pub dias_sin_cierre_count: i64,
    /// Suma con signo del efectivo que el modelo anterior dejó fuera de la
    /// conciliación. Positivo = la caja debería tener más de lo que los
    /// cortes esperaban; negativo = menos.
    pub impacto_total_estimado: f64,
}

#[derive(Serialize)]
pub struct AuditoriaCaja {
    pub resumen: ResumenAuditoria,
    pub ventas_sin_corte: Vec<VentaSinCorte>,
    pub movimientos_desalineados: Vec<MovimientoDesalineado>,
    pub movimientos_huerfanos: Vec<MovimientoHuerfano>,
    pub cadena_rota: Vec<EslabonRoto>,
    pub cortes_inconsistentes: Vec<CorteInconsistente>,
    pub dias_sin_cierre: Vec<DiaSinCierre>,
}

#[tauri::command]
pub fn auditar_caja(
    limite_dias: Option<i64>,
    state: State<'_, AppState>,
) -> Result<AuditoriaCaja, String> {
    let dias = limite_dias.unwrap_or(90).clamp(1, 3650);
    let db = state.db.lock().map_err(|e| e.to_string())?;
    auditar(&db, dias)
}

pub(crate) fn auditar(db: &rusqlite::Connection, dias: i64) -> Result<AuditoriaCaja, String> {
    let desde: String = db.query_row(
        "SELECT date('now', 'localtime', ?)",
        rusqlite::params![format!("-{} days", dias)],
        |r| r.get(0),
    ).unwrap_or_else(|_| "2000-01-01".to_string());

    // Todo lo posterior al último corte pertenece al período abierto: el
    // próximo corte lo incluirá, no es una fuga.
    let (inicio_abierto, _) = inicio_y_fondo(db);
    // Lo que la web registró antes de esto no pasó por este cajón.
    let origen_cadena = origen_de_la_cadena(db);

    // ── 1. Ventas fuera de todo corte (y ya no recuperables) ──
    let mut stmt = db.prepare(&format!(
        r#"SELECT v.id, v.folio, v.fecha, v.total, v.metodo_pago
           FROM ventas v
           WHERE v.anulada = 0 AND v.caja = 'principal' AND v.deleted_at IS NULL
             AND substr({mo}, 1, 10) >= ?1
             AND {mo} <= ?2
             AND NOT (v.origen = 'web' AND {mo} <= ?3)
             AND NOT EXISTS (
                 SELECT 1 FROM cortes c
                 WHERE c.deleted_at IS NULL AND c.caja = 'principal'
                   AND {mo} > c.fecha_inicio AND {mo} <= c.fecha_fin
             )
           ORDER BY {mo} ASC"#,
        mo = MOMENTO_V_SQL
    )).map_err(|e| e.to_string())?;
    let ventas_sin_corte: Vec<VentaSinCorte> = stmt.query_map(
        rusqlite::params![desde, inicio_abierto, origen_cadena],
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
    let ventas_sin_corte_efectivo: f64 = ventas_sin_corte.iter()
        .filter(|v| v.metodo_pago == "efectivo")
        .map(|v| v.total)
        .sum();

    // ── 2. Movimientos asignados fuera del rango de su corte ──
    // Se excluye el retiro propio de un corte de turno del modelo anterior:
    // se creaba un instante después de calcular (fuera del rango) pero ya
    // estaba reflejado en el fondo que dejó ese corte. No es una fuga.
    let mut stmt = db.prepare(&format!(
        r#"SELECT m.id, m.tipo, m.monto, m.concepto, m.fecha,
                  c.id, c.tipo, c.fecha_inicio, c.fecha_fin
           FROM movimientos_caja m
           JOIN cortes c ON c.id = m.corte_id
           WHERE substr({mo}, 1, 10) >= ?
             AND m.caja = 'principal' AND c.caja = 'principal'
             AND m.deleted_at IS NULL AND c.deleted_at IS NULL
             AND NOT ({mo} > c.fecha_inicio AND {mo} <= c.fecha_fin)
             AND NOT (c.tipo = 'PARCIAL' AND m.tipo = 'RETIRO'
                      AND ABS(m.monto - (c.efectivo_contado - c.fondo_siguiente)) < 0.005)
           ORDER BY {mo} ASC"#,
        mo = MOMENTO_M_SQL
    )).map_err(|e| e.to_string())?;
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
    // Con signo: un retiro aplicado de más hizo que se esperara menos.
    let movimientos_desalineados_monto: f64 = movimientos_desalineados.iter()
        .map(|m| if m.tipo == "RETIRO" { -m.monto } else { m.monto })
        .sum();

    // ── 3. Movimientos sin corte anteriores al período abierto ──
    let mut stmt = db.prepare(&format!(
        r#"SELECT id, tipo, monto, concepto, fecha FROM movimientos_caja
           WHERE caja = 'principal' AND corte_id IS NULL AND deleted_at IS NULL
             AND substr({mo}, 1, 10) >= ?1 AND {mo} <= ?2
             AND NOT (origen = 'web' AND {mo} <= ?3)
           ORDER BY {mo} ASC"#,
        mo = MOMENTO_SQL
    )).map_err(|e| e.to_string())?;
    let movimientos_huerfanos: Vec<MovimientoHuerfano> = stmt.query_map(
        rusqlite::params![desde, inicio_abierto, origen_cadena],
        |row| Ok(MovimientoHuerfano {
            id: row.get(0)?,
            tipo: row.get(1)?,
            monto: row.get(2)?,
            concepto: row.get(3)?,
            fecha: row.get(4)?,
        }),
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();
    // Un retiro que ningún corte contó: el dinero salió pero no se esperaba
    // que saliera → la caja tiene menos de lo que los cortes creyeron.
    let movimientos_huerfanos_monto: f64 = movimientos_huerfanos.iter()
        .map(|m| if m.tipo == "RETIRO" { -m.monto } else { m.monto })
        .sum();

    // ── 4. Continuidad de la cadena de cortes ──
    let mut stmt = db.prepare(&format!(
        r#"SELECT id, tipo, {f}, fondo_inicial, fondo_siguiente
           FROM cortes WHERE deleted_at IS NULL AND caja = 'principal'
           ORDER BY {f} ASC, id ASC"#,
        f = FIN_CORTE_SQL
    )).map_err(|e| e.to_string())?;
    let cadena: Vec<(i64, String, String, f64, f64)> = stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    let mut cadena_rota: Vec<EslabonRoto> = cadena.windows(2)
        .filter_map(|w| {
            let (_, _, _, _, fondo_anterior) = &w[0];
            let (id, tipo, fecha, fondo_inicial, _) = &w[1];
            let diferencia = fondo_inicial - fondo_anterior;
            (diferencia.abs() > 0.005 && fecha.as_str() >= desde.as_str()).then(|| EslabonRoto {
                corte_id: *id,
                tipo: tipo.clone(),
                fecha: fecha.clone(),
                fondo_anterior: *fondo_anterior,
                fondo_inicial: *fondo_inicial,
                diferencia,
            })
        })
        .collect();
    cadena_rota.reverse(); // más recientes primero
    let eslabones_rotos_monto: f64 = cadena_rota.iter().map(|e| e.diferencia).sum();

    // ── 5. Aritmética interna de cada corte ──
    let mut stmt = db.prepare(
        r#"SELECT id, tipo, created_at, efectivo_esperado,
                  fondo_inicial + total_ventas_efectivo + total_entradas_efectivo - total_retiros_efectivo
           FROM cortes
           WHERE deleted_at IS NULL AND caja = 'principal' AND substr(created_at, 1, 10) >= ?
             AND ABS((fondo_inicial + total_ventas_efectivo
                      + total_entradas_efectivo - total_retiros_efectivo)
                     - efectivo_esperado) > 0.01
           ORDER BY created_at DESC"#,
    ).map_err(|e| e.to_string())?;
    let cortes_inconsistentes: Vec<CorteInconsistente> = stmt.query_map(
        rusqlite::params![desde],
        |row| {
            let guardado: f64 = row.get(3)?;
            let recalculado: f64 = row.get(4)?;
            Ok(CorteInconsistente {
                id: row.get(0)?,
                tipo: row.get(1)?,
                created_at: row.get(2)?,
                esperado_guardado: guardado,
                esperado_recalculado: recalculado,
                descuadre: recalculado - guardado,
            })
        },
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    // ── 6. Días con ventas sin cierre del día (informativo) ──
    let mut stmt = db.prepare(&format!(
        r#"SELECT substr({mo}, 1, 10) AS dia, COUNT(*), COALESCE(SUM(v.total), 0),
                  COALESCE(SUM(CASE WHEN v.metodo_pago = 'efectivo' THEN v.total ELSE 0 END), 0)
           FROM ventas v
           WHERE v.anulada = 0 AND v.caja = 'principal' AND v.deleted_at IS NULL
             AND substr({mo}, 1, 10) >= ?1
             AND substr({mo}, 1, 10) < date('now', 'localtime')
             AND NOT (v.origen = 'web' AND {mo} <= ?2)
             AND NOT EXISTS (
                 SELECT 1 FROM cortes c
                 WHERE c.tipo = 'DIA' AND c.deleted_at IS NULL AND c.caja = 'principal'
                   AND substr({f}, 1, 10) = substr({mo}, 1, 10)
             )
           GROUP BY dia
           ORDER BY dia DESC"#,
        f = FIN_CORTE_SQL.replace("fecha_fin", "c.fecha_fin").replace("created_at", "c.created_at"),
        mo = MOMENTO_V_SQL
    )).map_err(|e| e.to_string())?;
    let dias_sin_cierre: Vec<DiaSinCierre> = stmt.query_map(
        rusqlite::params![desde, origen_cadena],
        |row| Ok(DiaSinCierre {
            dia: row.get(0)?,
            num_ventas: row.get(1)?,
            total_ventas: row.get(2)?,
            total_efectivo: row.get(3)?,
        }),
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    let resumen = ResumenAuditoria {
        ventas_sin_corte_count: ventas_sin_corte.len() as i64,
        ventas_sin_corte_efectivo,
        movimientos_desalineados_count: movimientos_desalineados.len() as i64,
        movimientos_desalineados_monto,
        movimientos_huerfanos_count: movimientos_huerfanos.len() as i64,
        movimientos_huerfanos_monto,
        eslabones_rotos_count: cadena_rota.len() as i64,
        eslabones_rotos_monto,
        cortes_inconsistentes_count: cortes_inconsistentes.len() as i64,
        dias_sin_cierre_count: dias_sin_cierre.len() as i64,
        impacto_total_estimado: ventas_sin_corte_efectivo
            + movimientos_desalineados_monto
            + movimientos_huerfanos_monto
            + eslabones_rotos_monto,
    };

    Ok(AuditoriaCaja {
        resumen,
        ventas_sin_corte,
        movimientos_desalineados,
        movimientos_huerfanos,
        cadena_rota,
        cortes_inconsistentes,
        dias_sin_cierre,
    })
}
