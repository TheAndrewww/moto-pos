-- 003_cortes_origen.sql — distinguir el origen (web vs desktop) de cortes,
-- movimientos de caja y aperturas.
--
-- Antes de esta migración, todos los registros venían del desktop (vía sync).
-- Cuando el web POS empezó a poder hacer cortes y movimientos directamente,
-- necesitamos saber de dónde viene cada uno para:
--   1. Mostrar al admin web qué cortes generó cada caja (filtro de UI).
--   2. No mezclar accidentalmente las ventas/movimientos del web con los
--      del desktop al calcular el efectivo esperado del corte.
--
-- Default 'desktop' para que las filas existentes mantengan semántica.
-- El web siempre estampa origen='web' al insertar.

ALTER TABLE cortes
    ADD COLUMN IF NOT EXISTS origen TEXT NOT NULL DEFAULT 'desktop';

ALTER TABLE movimientos_caja
    ADD COLUMN IF NOT EXISTS origen TEXT NOT NULL DEFAULT 'desktop';

ALTER TABLE aperturas_caja
    ADD COLUMN IF NOT EXISTS origen TEXT NOT NULL DEFAULT 'desktop';

CREATE INDEX IF NOT EXISTS idx_cortes_origen_fecha
    ON cortes(origen, created_at);

CREATE INDEX IF NOT EXISTS idx_movimientos_caja_origen_corte
    ON movimientos_caja(origen, corte_id);
