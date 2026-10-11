-- 014_solicitudes_eliminar_venta.sql — "Eliminar venta de prueba" desde la web.
--
-- La eliminación de una venta de prueba la EJECUTA el POS de escritorio (dueño
-- de las ventas, el inventario y los cortes): ajusta el corte, regresa las
-- piezas y luego purga aquí (POST /sync/purgar). Desde la web solo se PIDE: la
-- solicitud queda pendiente y el escritorio la toma en su siguiente ciclo de
-- sincronización (GET /sync/solicitudes), la aplica y la confirma.

-- Permiso individual (desktop migración 017). Con la columna aquí, el sync
-- la lleva y la trae como cualquier otra columna de usuarios.
ALTER TABLE usuarios ADD COLUMN IF NOT EXISTS puede_eliminar_ventas INTEGER NOT NULL DEFAULT 0;
-- Daniela Bravo (usuario "Dani"): la única autorizada (pedido del dueño, 2026-10-10).
UPDATE usuarios SET puede_eliminar_ventas = 1 WHERE uuid = 'c088a77266563a58935e396a1cb55d27';

-- Una solicitud por venta. Al aplicarse se BORRA (la venta de prueba no deja
-- rastro); si el escritorio la rechaza queda con el motivo para mostrarlo.
CREATE TABLE IF NOT EXISTS solicitudes_eliminar_venta (
    id                  BIGSERIAL   PRIMARY KEY,
    uuid                TEXT        NOT NULL UNIQUE DEFAULT gen_random_uuid()::text,
    venta_uuid          TEXT        NOT NULL UNIQUE,
    venta_folio         TEXT,
    solicitado_por_uuid TEXT        NOT NULL,
    estado              TEXT        NOT NULL DEFAULT 'pendiente'
                        CHECK (estado IN ('pendiente', 'rechazada')),
    error               TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
