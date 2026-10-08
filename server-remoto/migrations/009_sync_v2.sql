-- 009_sync_v2.sql — Infraestructura para que las llaves foráneas viajen por uuid.
--
-- CONTEXTO
-- El escritorio (SQLite) y este servidor numeran sus filas por separado. El
-- sync emparejaba filas por uuid pero copiaba las columnas *_id tal cual, así
-- que "usuario_id = 1" (Dueño en el escritorio) se leía aquí como el usuario 1
-- del servidor ("Andrew Web"); igual con productos nuevos, categorías,
-- devoluciones, etc. Desde el protocolo v2 cada FK viaja también como uuid y
-- cada lado la traduce a su id local.
--
-- ESTA MIGRACIÓN SOLO CREA TABLAS E ÍNDICES y normaliza marcas de tiempo en el
-- futuro. No cambia ninguna FK existente: eso lo hace la reparación
-- (POST /sync/reparar) cuando el escritorio ya subió su mapa de ids.

-- id local de cada dispositivo → uuid. Se llena con cada push (fuente 'push')
-- y con la subida completa del escritorio (fuente 'bootstrap').
CREATE TABLE IF NOT EXISTS sync_id_map (
    device_uuid TEXT        NOT NULL,
    tabla       TEXT        NOT NULL,
    local_id    BIGINT      NOT NULL,
    uuid        TEXT        NOT NULL,
    fuente      TEXT        NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (device_uuid, tabla, local_id)
);
CREATE INDEX IF NOT EXISTS idx_sync_id_map_uuid ON sync_id_map (tabla, uuid);

-- Celdas FK que llegaron sin poder traducirse (la fila referida aún no
-- existe aquí). Se reintentan después de cada push.
CREATE TABLE IF NOT EXISTS sync_fk_pendiente (
    id           BIGSERIAL   PRIMARY KEY,
    device_uuid  TEXT,
    tabla        TEXT        NOT NULL,
    fila_uuid    TEXT        NOT NULL,
    columna      TEXT        NOT NULL,
    ref_tabla    TEXT        NOT NULL,
    ref_uuid     TEXT,
    raw_local_id BIGINT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tabla, fila_uuid, columna)
);

-- En qué "idioma" están las FK de cada fila hecha en el escritorio:
--   traducido  → escrita por un push v2 / id-map, FKs ya son ids de aquí
--   pendiente  → como traducido pero con alguna celda en sync_fk_pendiente
--   reparado   → fila vieja corregida por la reparación
--   sin_mapa   → la reparación no encontró el id en el mapa (se reintenta)
-- Filas del escritorio que NO aparecen aquí tienen FKs crudas (ids del escritorio).
CREATE TABLE IF NOT EXISTS sync_fk_estado (
    tabla      TEXT        NOT NULL,
    uuid       TEXT        NOT NULL,
    estado     TEXT        NOT NULL
               CHECK (estado IN ('traducido', 'pendiente', 'reparado', 'sin_mapa')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tabla, uuid)
);

-- Bitácora celda por celda de la reparación (permite revertir un lote).
CREATE TABLE IF NOT EXISTS sync_fk_repair_log (
    id             BIGSERIAL   PRIMARY KEY,
    lote           TEXT        NOT NULL,
    tabla          TEXT        NOT NULL,
    fila_uuid      TEXT        NOT NULL,
    columna        TEXT        NOT NULL,
    valor_anterior BIGINT,
    valor_nuevo    BIGINT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sync_fk_repair_log_celda ON sync_fk_repair_log (tabla, columna, fila_uuid);
CREATE INDEX IF NOT EXISTS idx_sync_fk_repair_log_lote ON sync_fk_repair_log (lote);

-- Banderas sueltas del sync (p.ej. 'fk_reparado' = '1').
CREATE TABLE IF NOT EXISTS sync_meta (
    clave TEXT PRIMARY KEY,
    valor TEXT NOT NULL
);

-- Índices de hijos por padre (el push reemplaza hijos por padre y el pull los
-- carga por padre; sin índice eran recorridos completos).
CREATE INDEX IF NOT EXISTS idx_venta_detalle_venta ON venta_detalle (venta_id);
CREATE INDEX IF NOT EXISTS idx_devolucion_detalle_devolucion ON devolucion_detalle (devolucion_id);
CREATE INDEX IF NOT EXISTS idx_recepcion_detalle_recepcion ON recepcion_detalle (recepcion_id);
CREATE INDEX IF NOT EXISTS idx_corte_denominaciones_corte ON corte_denominaciones (corte_id);
CREATE INDEX IF NOT EXISTS idx_corte_vendedores_corte ON corte_vendedores (corte_id);
CREATE INDEX IF NOT EXISTS idx_presupuesto_detalle_presupuesto ON presupuesto_detalle (presupuesto_id);
CREATE INDEX IF NOT EXISTS idx_orden_pedido_detalle_orden ON orden_pedido_detalle (orden_id);
CREATE INDEX IF NOT EXISTS idx_transferencia_detalle_transferencia ON transferencia_detalle (transferencia_id);
CREATE INDEX IF NOT EXISTS idx_sync_cursor_tabla_uuid ON sync_cursor (tabla, uuid, id);

-- Reloj único (hora local de la tienda). El escritorio escribía updated_at en
-- UTC (6 h adelante), así que hay filas con marca "en el futuro" que hacían
-- que el servidor rechazara cambios legítimos posteriores. Se traen a la hora
-- actual. No se escribe sync_cursor: los datos no cambian.
DO $$
DECLARE
    t     TEXT;
    ahora TEXT := to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS');
BEGIN
    FOREACH t IN ARRAY ARRAY[
        'productos', 'proveedores', 'clientes', 'usuarios', 'categorias', 'sucursales',
        'stock_sucursal', 'ventas', 'presupuestos', 'ordenes_pedido', 'recepciones',
        'cortes', 'devoluciones', 'transferencias', 'movimientos_caja', 'aperturas_caja',
        'audit_log'
    ] LOOP
        IF EXISTS (SELECT 1 FROM information_schema.columns
                    WHERE table_schema = 'public' AND table_name = t
                      AND column_name = 'updated_at' AND data_type = 'text') THEN
            EXECUTE format(
                'UPDATE %I SET updated_at = $1 WHERE replace(substr(updated_at, 1, 19), ''T'', '' '') > $1',
                t) USING ahora;
        END IF;
    END LOOP;
END $$;
