// sync_fk_map.rs — Mapa compartido de llaves foráneas (FK) de las tablas que
// se sincronizan entre el POS de escritorio (SQLite) y el servidor (Postgres).
//
// POR QUÉ EXISTE
// Cada base numera sus filas por su cuenta (AUTOINCREMENT / BIGSERIAL), así
// que un `usuario_id = 1` significa "Dueño" en el escritorio y "Andrew Web"
// en el servidor. La identidad real de una fila es su `uuid`. Este mapa dice,
// para cada tabla, qué columnas enteras apuntan a otra tabla, para que cada
// lado traduzca `id local → uuid` al enviar y `uuid → id local` al recibir.
//
// ESTE ARCHIVO LO COMPARTEN LOS DOS CRATES
// - Servidor: `mod sync_fk_map;` en main.rs.
// - Escritorio: `include!("../../../server-remoto/src/sync_fk_map.rs")` en
//   src-tauri/src/sync/fk.rs.
// Por eso: sin dependencias, sin `use`, solo constantes y funciones puras.
// Vive en server-remoto/src porque el Dockerfile solo copia `src/`.

/// Qué significa una columna entera de una tabla sincronizada.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// Apunta a otra tabla sincronizada: se traduce por uuid.
    Tabla(&'static str),
    /// FK al padre de un agregado (venta_detalle.venta_id…): el receptor la
    /// fija con el id local del padre. Nunca viaja en `refs`.
    Padre(&'static str),
    /// Apunta a una tabla NO sincronizada que ambos lados siembran igual
    /// (roles, sucursales con la misma fila 1): se copia tal cual.
    Paso(&'static str),
    /// La tabla destino depende de otra columna de la misma fila
    /// (audit_log.registro_id según audit_log.tabla_afectada).
    Polimorfico(&'static str),
}

/// (tabla, &[(columna, tipo)]). Toda columna `*_id`, `autorizado_por` y
/// `anulada_por` de una tabla sincronizada DEBE aparecer aquí (hay un test
/// que lo verifica contra el esquema de cada lado).
#[allow(dead_code)]
pub const FK_MAP: &[(&str, &[(&str, RefKind)])] = &[
    // ── Catálogo ──
    ("sucursales", &[]),
    ("categorias", &[]),
    ("proveedores", &[]),
    ("clientes", &[]),
    ("usuarios", &[("rol_id", RefKind::Paso("roles"))]),
    ("productos", &[
        ("categoria_id", RefKind::Tabla("categorias")),
        ("proveedor_id", RefKind::Tabla("proveedores")),
    ]),
    ("stock_sucursal", &[
        ("producto_id", RefKind::Tabla("productos")),
        ("sucursal_id", RefKind::Paso("sucursales")),
    ]),
    // ── Caja y ventas ──
    ("cortes", &[
        ("usuario_id", RefKind::Tabla("usuarios")),
        ("sucursal_id", RefKind::Paso("sucursales")),
    ]),
    ("corte_denominaciones", &[("corte_id", RefKind::Padre("cortes"))]),
    ("corte_vendedores", &[
        ("corte_id", RefKind::Padre("cortes")),
        ("usuario_id", RefKind::Tabla("usuarios")),
    ]),
    ("ventas", &[
        ("usuario_id", RefKind::Tabla("usuarios")),
        ("cliente_id", RefKind::Tabla("clientes")),
        ("anulada_por", RefKind::Tabla("usuarios")),
        ("sucursal_id", RefKind::Paso("sucursales")),
    ]),
    ("venta_detalle", &[
        ("venta_id", RefKind::Padre("ventas")),
        ("producto_id", RefKind::Tabla("productos")),
        ("autorizado_por", RefKind::Tabla("usuarios")),
    ]),
    ("movimientos_caja", &[
        ("usuario_id", RefKind::Tabla("usuarios")),
        ("autorizado_por", RefKind::Tabla("usuarios")),
        ("corte_id", RefKind::Tabla("cortes")),
        ("sucursal_id", RefKind::Paso("sucursales")),
    ]),
    ("aperturas_caja", &[
        ("usuario_id", RefKind::Tabla("usuarios")),
        ("sucursal_id", RefKind::Paso("sucursales")),
    ]),
    ("devoluciones", &[
        ("venta_id", RefKind::Tabla("ventas")),
        ("usuario_id", RefKind::Tabla("usuarios")),
        ("autorizado_por", RefKind::Tabla("usuarios")),
        ("movimiento_caja_id", RefKind::Tabla("movimientos_caja")),
        ("sucursal_id", RefKind::Paso("sucursales")),
    ]),
    ("devolucion_detalle", &[
        ("devolucion_id", RefKind::Padre("devoluciones")),
        ("venta_detalle_id", RefKind::Tabla("venta_detalle")),
        ("producto_id", RefKind::Tabla("productos")),
    ]),
    // ── Compras / presupuestos / traspasos ──
    ("ordenes_pedido", &[
        ("usuario_id", RefKind::Tabla("usuarios")),
        ("proveedor_id", RefKind::Tabla("proveedores")),
        ("sucursal_id", RefKind::Paso("sucursales")),
    ]),
    ("orden_pedido_detalle", &[
        ("orden_id", RefKind::Padre("ordenes_pedido")),
        ("producto_id", RefKind::Tabla("productos")),
    ]),
    ("recepciones", &[
        ("orden_id", RefKind::Tabla("ordenes_pedido")),
        ("usuario_id", RefKind::Tabla("usuarios")),
        ("proveedor_id", RefKind::Tabla("proveedores")),
        ("sucursal_id", RefKind::Paso("sucursales")),
    ]),
    ("recepcion_detalle", &[
        ("recepcion_id", RefKind::Padre("recepciones")),
        ("producto_id", RefKind::Tabla("productos")),
    ]),
    ("presupuestos", &[
        ("usuario_id", RefKind::Tabla("usuarios")),
        ("cliente_id", RefKind::Tabla("clientes")),
        ("venta_id", RefKind::Tabla("ventas")),
        ("sucursal_id", RefKind::Paso("sucursales")),
    ]),
    ("presupuesto_detalle", &[
        ("presupuesto_id", RefKind::Padre("presupuestos")),
        ("producto_id", RefKind::Tabla("productos")),
    ]),
    ("transferencias", &[
        ("sucursal_origen_id", RefKind::Paso("sucursales")),
        ("sucursal_destino_id", RefKind::Paso("sucursales")),
        ("usuario_id", RefKind::Tabla("usuarios")),
    ]),
    ("transferencia_detalle", &[
        ("transferencia_id", RefKind::Padre("transferencias")),
        ("producto_id", RefKind::Tabla("productos")),
    ]),
    // ── Bitácora ──
    ("audit_log", &[
        ("usuario_id", RefKind::Tabla("usuarios")),
        ("registro_id", RefKind::Polimorfico("tabla_afectada")),
    ]),
];

/// Agregados: la fila padre viaja con sus hijos. (padre, &[(hijo, fk)]).
#[allow(dead_code)]
pub const AGGREGATES: &[(&str, &[(&str, &str)])] = &[
    ("ventas",         &[("venta_detalle",        "venta_id")]),
    ("presupuestos",   &[("presupuesto_detalle",  "presupuesto_id")]),
    ("ordenes_pedido", &[("orden_pedido_detalle", "orden_id")]),
    ("recepciones",    &[("recepcion_detalle",    "recepcion_id")]),
    ("cortes",         &[("corte_denominaciones", "corte_id"),
                         ("corte_vendedores",     "corte_id")]),
    ("devoluciones",   &[("devolucion_detalle",   "devolucion_id")]),
    ("transferencias", &[("transferencia_detalle","transferencia_id")]),
];

/// Orden en que conviene aplicar un lote para que las referencias ya existan
/// (no hay ciclos). Tablas que no aparecen van al final.
#[allow(dead_code)]
pub const ORDEN_APLICACION: &[&str] = &[
    "sucursales", "categorias", "proveedores", "usuarios", "clientes",
    "productos", "stock_sucursal", "cortes", "ventas", "movimientos_caja",
    "aperturas_caja", "ordenes_pedido", "recepciones", "presupuestos",
    "devoluciones", "transferencias", "audit_log",
];

/// Tablas a las que puede apuntar `audit_log.registro_id` (según
/// `tabla_afectada`). Cualquier otro valor (sesiones, pos_devices, config…)
/// se copia tal cual.
#[allow(dead_code)]
pub const AUDIT_TABLAS_TRADUCIBLES: &[&str] = &[
    "ventas", "productos", "cortes", "movimientos_caja", "devoluciones",
    "aperturas_caja", "recepciones", "presupuestos", "ordenes_pedido",
    "usuarios", "clientes", "proveedores", "categorias", "transferencias",
];

/// Columnas FK de una tabla (vacío si la tabla no tiene o no existe).
#[allow(dead_code)]
pub fn fks_de(tabla: &str) -> &'static [(&'static str, RefKind)] {
    FK_MAP.iter().find(|(t, _)| *t == tabla).map(|(_, c)| *c).unwrap_or(&[])
}

/// Hijos de un agregado (vacío si no es agregado).
#[allow(dead_code)]
pub fn hijos_de(tabla: &str) -> &'static [(&'static str, &'static str)] {
    AGGREGATES.iter().find(|(p, _)| *p == tabla).map(|(_, h)| *h).unwrap_or(&[])
}

/// Posición en ORDEN_APLICACION (las desconocidas al final).
#[allow(dead_code)]
pub fn orden_de(tabla: &str) -> usize {
    ORDEN_APLICACION.iter().position(|t| *t == tabla).unwrap_or(ORDEN_APLICACION.len())
}

/// ¿El uuid lo generó el POS de escritorio? Sus uuids son 32 hex en
/// minúsculas (`lower(hex(randomblob(16)))`); la web usa Uuid v7 con guiones
/// o 'web-admin-…'. OJO: en audit_log el servidor también genera 32 hex por
/// default — ahí usa `origen` ('POS' = escritorio, 'WEB' = web).
#[allow(dead_code)]
pub fn es_uuid_escritorio(uuid: &str) -> bool {
    uuid.len() == 32 && uuid.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Para audit_log.registro_id: tabla destino según `tabla_afectada`, o None
/// si no se traduce (se copia tal cual).
#[allow(dead_code)]
pub fn tabla_de_auditoria(tabla_afectada: &str) -> Option<&'static str> {
    AUDIT_TABLAS_TRADUCIBLES.iter().copied().find(|t| *t == tabla_afectada)
}

#[cfg(test)]
mod tests_fk_map {
    use super::*;

    #[test]
    fn uuid_escritorio() {
        assert!(es_uuid_escritorio("88f9dc7342f620bcb56e301e436fb727"));
        assert!(!es_uuid_escritorio("019dd079-a8fb-7bd0-88ab-999a2d1be862"));
        assert!(!es_uuid_escritorio("web-admin-2bc80cfd-fe3a-45e9-8178-83edbc372aa7"));
        assert!(!es_uuid_escritorio("88F9DC7342F620BCB56E301E436FB727"));
    }

    #[test]
    fn padres_coinciden_con_agregados() {
        for (padre, hijos) in AGGREGATES {
            for (hijo, fk) in *hijos {
                let k = fks_de(hijo).iter().find(|(c, _)| c == fk).map(|(_, k)| *k);
                assert_eq!(k, Some(RefKind::Padre(padre)), "{hijo}.{fk}");
            }
        }
    }

    #[test]
    fn referencias_apuntan_a_tablas_del_mapa() {
        for (t, cols) in FK_MAP {
            for (c, k) in *cols {
                if let RefKind::Tabla(r) = k {
                    assert!(FK_MAP.iter().any(|(x, _)| x == r), "{t}.{c} → {r}");
                }
            }
        }
    }
}
