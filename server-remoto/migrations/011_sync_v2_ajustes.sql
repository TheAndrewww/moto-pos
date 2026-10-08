-- 011_sync_v2_ajustes.sql — Ajustes del sync v2 tras la revisión.

-- 1. Valor que se escribió en una celda pendiente (NULL o 0). resolver_pendientes
--    solo la corrige si la celda TODAVÍA tiene ese valor: si alguien la editó
--    mientras tanto (p.ej. la web), no se pisa esa edición.
ALTER TABLE sync_fk_pendiente ADD COLUMN IF NOT EXISTS valor_puesto BIGINT;

-- 2. Celdas FK que la web cambió en filas hechas en el escritorio que todavía no
--    se reparan (sus FKs siguen siendo ids del escritorio). Esas celdas ya están
--    en ids de ESTE servidor: la reparación no debe traducirlas otra vez.
CREATE TABLE IF NOT EXISTS sync_fk_celda_web (
    tabla      TEXT        NOT NULL,
    uuid       TEXT        NOT NULL,
    columna    TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tabla, uuid, columna)
);

-- 3. Momento en que este servidor empezó a separar el dinero por caja. Lo usa:
--    - el escritorio, como frontera para acomodar en el periodo abierto las
--      ventas web (espejo) que bajen tarde, incluidas las hechas mientras la
--      tienda seguía con la versión anterior;
--    - la caja web: las aperturas web anteriores (las del modal diario, casi
--      todas en $0) no definen el fondo de su primer corte.
INSERT INTO sync_meta (clave, valor)
VALUES ('caja_v2_desde', to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'))
ON CONFLICT (clave) DO NOTHING;

-- 4. Productos creados en la web con un código que ya usa otro producto. El
--    escritorio exige código único, así que nunca pudo guardarlos y cualquier
--    venta de ellos se quedaría sin bajar. Se les agrega '-W<id>' al código y se
--    publican de nuevo para que el escritorio los reciba. El dueño puede luego
--    unirlos o corregir el código.
WITH dup AS (
    UPDATE productos p
       SET codigo = p.codigo || '-W' || p.id,
           updated_at = to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS')
     WHERE p.uuid !~ '^[0-9a-f]{32}$'
       AND EXISTS (SELECT 1 FROM productos q
                    WHERE q.codigo = p.codigo AND q.id <> p.id
                      AND (q.uuid ~ '^[0-9a-f]{32}$' OR q.id < p.id))
    RETURNING p.uuid
)
INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device)
SELECT 'productos', uuid, 1, 'web-pos' FROM dup;
