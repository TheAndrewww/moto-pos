-- 005_modo_caja.sql — Modo de operación del POS web (espejo vs individual).
--
-- CONTEXTO
-- Hasta ahora el web operaba siempre como "caja individual": filtraba todo
-- por origen='web' (ver migración 003) en aperturas, movimientos y cortes,
-- pero las ventas no llevaban marca de origen — quedaba un híbrido raro
-- donde el corte web veía SU fondo y SUS movimientos pero TODAS las ventas
-- (incluidas las del desktop).
--
-- ESTE PARCHE INTRODUCE
--   1. `pos_devices.modo_caja`     → cada dispositivo web declara cómo
--      quiere comportarse:
--        - 'espejo':     el web es otra ventana de la caja del desktop;
--                        comparte fondo, ventas y corte.
--        - 'individual': el web es su propia caja (segundo mostrador,
--                        tablet, celular del dueño).
--      Default 'individual' para mantener semántica actual (web aislado).
--
--   2. `ventas.origen`             → marca cada venta como 'web' o 'desktop'.
--      Sin esto, el modo individual no puede separar sus ventas y los
--      cortes nunca cuadran. Default 'desktop' para que las filas
--      preexistentes (que llegaron por sync del POS local) mantengan
--      su semántica.
--
-- COMPATIBILIDAD
-- - Ambas columnas son ADD COLUMN IF NOT EXISTS con default → no rompe
--   tokens JWT antiguos ni datos existentes.
-- - Los handlers backend leen `pos_devices.modo_caja` por device_uuid;
--   si el JWT no trae device_uuid (tokens emitidos antes del PR), se
--   asume 'individual' por defecto (comportamiento previo).

-- ─── 1. modo_caja en pos_devices ────────────────────────────────────────
ALTER TABLE pos_devices
    ADD COLUMN IF NOT EXISTS modo_caja TEXT NOT NULL DEFAULT 'individual';

-- Constraint para evitar valores inválidos. Lo creamos solo si no existe
-- (postgres no soporta ADD CONSTRAINT IF NOT EXISTS hasta v9.6+; usamos
-- DO block para idempotencia).
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'pos_devices_modo_caja_check'
    ) THEN
        ALTER TABLE pos_devices
            ADD CONSTRAINT pos_devices_modo_caja_check
            CHECK (modo_caja IN ('espejo', 'individual'));
    END IF;
END$$;

-- ─── 2. origen en ventas ────────────────────────────────────────────────
ALTER TABLE ventas
    ADD COLUMN IF NOT EXISTS origen TEXT NOT NULL DEFAULT 'desktop';

CREATE INDEX IF NOT EXISTS idx_ventas_origen_fecha
    ON ventas(origen, fecha);
