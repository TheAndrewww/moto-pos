-- 010_caja.sql — A qué cajón pertenece cada movimiento de dinero.
--
-- CONTEXTO
-- `origen` (migraciones 003 y 005) dice quién CREÓ la fila ('desktop'/'web'),
-- pero se usaba como si fuera la caja, y no sirve para eso: una venta hecha en
-- la web en modo espejo entra al cajón de la tienda. Desde ahora:
--   caja = 'principal' → cajón físico de la tienda. Su cadena de cortes y
--                        aperturas la escribe SOLO el POS de escritorio; las
--                        ventas/movimientos de la web en modo espejo entran
--                        aquí, bajan al escritorio y cuentan en su corte.
--   caja = 'web'       → caja aparte de los equipos web en modo individual;
--                        su cadena de cortes la calcula este servidor.
-- `registrado_at`: momento en que una venta/movimiento llegó tarde (después de
-- un corte que ya cubría su fecha); el corte usa COALESCE(registrado_at, fecha).

ALTER TABLE ventas           ADD COLUMN IF NOT EXISTS caja TEXT NOT NULL DEFAULT 'principal';
ALTER TABLE movimientos_caja ADD COLUMN IF NOT EXISTS caja TEXT NOT NULL DEFAULT 'principal';
ALTER TABLE cortes           ADD COLUMN IF NOT EXISTS caja TEXT NOT NULL DEFAULT 'principal';
ALTER TABLE aperturas_caja   ADD COLUMN IF NOT EXISTS caja TEXT NOT NULL DEFAULT 'principal';

ALTER TABLE ventas           ADD COLUMN IF NOT EXISTS registrado_at TEXT;
ALTER TABLE movimientos_caja ADD COLUMN IF NOT EXISTS registrado_at TEXT;

DO $$
DECLARE t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY['ventas', 'movimientos_caja', 'cortes', 'aperturas_caja'] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = t || '_caja_check') THEN
            EXECUTE format(
                'ALTER TABLE %I ADD CONSTRAINT %I CHECK (caja IN (''principal'', ''web''))',
                t, t || '_caja_check');
        END IF;
    END LOOP;
END $$;

CREATE INDEX IF NOT EXISTS idx_ventas_caja_fecha ON ventas (caja, fecha);
CREATE INDEX IF NOT EXISTS idx_movimientos_caja_caja ON movimientos_caja (caja, corte_id, fecha);
CREATE INDEX IF NOT EXISTS idx_cortes_caja ON cortes (caja);
CREATE INDEX IF NOT EXISTS idx_aperturas_caja_caja ON aperturas_caja (caja, fecha);

-- Equipos web nuevos entran en modo espejo (la web se usa como respaldo del
-- mismo cajón cuando falla el escritorio). Los existentes conservan su modo.
ALTER TABLE pos_devices ALTER COLUMN modo_caja SET DEFAULT 'espejo';
-- Versión de protocolo de sync que reporta cada escritorio (NULL = anterior a v2).
ALTER TABLE pos_devices ADD COLUMN IF NOT EXISTS protocolo INTEGER;

-- Procedencia real de filas viejas, tomada de sync_cursor (quién las escribió).
-- Las aperturas hechas en la web fueron las del modal diario de los equipos
-- web (60 de 61 con $0): no son del cajón de la tienda.
UPDATE aperturas_caja SET caja = 'web', origen = 'web'
 WHERE origen = 'web'
    OR uuid IN (SELECT uuid FROM sync_cursor
                 WHERE tabla = 'aperturas_caja' AND origen_device IN ('web-pos', 'web-admin'));
UPDATE cortes SET caja = 'web' WHERE origen = 'web';
-- Ventas y movimientos web de antes de las migraciones 003/005 quedaron con
-- origen 'desktop' por el default; su dinero sí entró al cajón de la tienda,
-- así que la caja se queda en 'principal'.
UPDATE ventas SET origen = 'web'
 WHERE origen = 'desktop'
   AND uuid IN (SELECT uuid FROM sync_cursor
                 WHERE tabla = 'ventas' AND origen_device IN ('web-pos', 'web-admin'));
UPDATE movimientos_caja SET origen = 'web'
 WHERE origen = 'desktop'
   AND uuid IN (SELECT uuid FROM sync_cursor
                 WHERE tabla = 'movimientos_caja' AND origen_device IN ('web-pos', 'web-admin'));

-- Equipos web que quedaron en modo 'individual': nunca hicieron un corte web
-- (0 cortes web; 60 de 61 aperturas web en $0, forzadas por el modal diario) y
-- la evidencia muestra que la web se usó como respaldo del MISMO cajón cuando
-- falló el escritorio (2026-06-23, 2026-08-02). Se pasan a espejo para que lo
-- que cobren cuente en el corte de la tienda. Desde la web se puede volver a
-- elegir "caja propia" en cualquier momento.
UPDATE pos_devices SET modo_caja = 'espejo'
 WHERE modo_caja = 'individual'
   AND NOT EXISTS (SELECT 1 FROM cortes WHERE caja = 'web');
