-- 002_audit_secuencia.sql — soporte para mutaciones de inventario desde web.
--
-- El POS desktop tiene tablas locales `audit_log` y `codigo_secuencia` que no
-- viajan al servidor remoto (no están en TABLAS_SYNC porque son metadata
-- de instancia). Cuando el web hace mutaciones (crear_producto, ajustar_stock,
-- etc.) necesitamos:
--
--   1. `codigo_secuencia` — generador atómico de códigos `MR-#####`
--      compartido entre instancias web (cada caja remota saca su próximo
--      código sin colisionar).
--
--   2. `audit_log` — bitácora de cambios (alimenta `historial_precios_producto`
--      y la futura página de bitácora). Solo recibe entradas web; el desktop
--      tiene su propia copia local. No se sincroniza.

CREATE TABLE IF NOT EXISTS codigo_secuencia (
    id            INTEGER PRIMARY KEY DEFAULT 1,
    ultimo_valor  BIGINT  NOT NULL DEFAULT 0,
    CHECK (id = 1)
);

INSERT INTO codigo_secuencia (id, ultimo_valor)
VALUES (1, 0)
ON CONFLICT (id) DO NOTHING;

-- Sembrar `ultimo_valor` con el máximo MR-##### existente para que las nuevas
-- claves no choquen con productos creados desde el desktop antes de habilitar
-- el web.
UPDATE codigo_secuencia
   SET ultimo_valor = GREATEST(
       ultimo_valor,
       COALESCE(
           (SELECT MAX(NULLIF(regexp_replace(codigo, '^MR-', ''), '')::BIGINT)
              FROM productos
             WHERE codigo ~ '^MR-[0-9]+$'),
           0
       )
   )
 WHERE id = 1;

CREATE TABLE IF NOT EXISTS audit_log (
    id                  BIGSERIAL PRIMARY KEY,
    usuario_id          BIGINT,
    accion              TEXT NOT NULL,
    tabla_afectada      TEXT,
    registro_id         BIGINT,
    datos_anteriores    TEXT,
    datos_nuevos        TEXT,
    descripcion_legible TEXT,
    origen              TEXT NOT NULL DEFAULT 'WEB',
    fecha               TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS')
);

CREATE INDEX IF NOT EXISTS idx_audit_accion_registro
    ON audit_log(accion, tabla_afectada, registro_id);
CREATE INDEX IF NOT EXISTS idx_audit_fecha
    ON audit_log(fecha);
