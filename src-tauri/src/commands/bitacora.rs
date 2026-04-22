// commands/bitacora.rs — Visor de auditoría

use serde::Serialize;
use tauri::State;
use crate::commands::auth::AppState;

#[derive(Serialize, Clone)]
pub struct EntradaBitacora {
    pub id: i64,
    pub usuario_nombre: Option<String>,
    pub accion: String,
    pub tabla_afectada: Option<String>,
    pub registro_id: Option<i64>,
    pub descripcion_legible: String,
    pub origen: String,
    pub fecha: String,
}

#[tauri::command]
pub fn listar_bitacora(
    limite: Option<i64>,
    accion_filtro: Option<String>,
    state: State<'_, AppState>,
) -> Vec<EntradaBitacora> {
    let db = state.db.lock().unwrap();
    let lim = limite.unwrap_or(200);

    let query = if let Some(ref filtro) = accion_filtro {
        format!(
            r#"SELECT a.id, u.nombre_completo, a.accion, a.tabla_afectada,
                      a.registro_id, a.descripcion_legible, a.origen, a.fecha
               FROM audit_log a
               LEFT JOIN usuarios u ON u.id = a.usuario_id
               WHERE a.accion LIKE '%{}%'
               ORDER BY a.fecha DESC
               LIMIT {}"#,
            filtro.replace('\'', ""), lim
        )
    } else {
        format!(
            r#"SELECT a.id, u.nombre_completo, a.accion, a.tabla_afectada,
                      a.registro_id, a.descripcion_legible, a.origen, a.fecha
               FROM audit_log a
               LEFT JOIN usuarios u ON u.id = a.usuario_id
               ORDER BY a.fecha DESC
               LIMIT {}"#,
            lim
        )
    };

    let mut stmt = db.prepare(&query).unwrap();
    stmt.query_map([], |row| {
        Ok(EntradaBitacora {
            id: row.get(0)?,
            usuario_nombre: row.get(1)?,
            accion: row.get(2)?,
            tabla_afectada: row.get(3)?,
            registro_id: row.get(4)?,
            descripcion_legible: row.get(5)?,
            origen: row.get(6)?,
            fecha: row.get(7)?,
        })
    }).unwrap()
    .filter_map(|r| r.ok())
    .collect()
}
