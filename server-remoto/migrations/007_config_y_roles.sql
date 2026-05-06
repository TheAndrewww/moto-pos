-- 007_config_y_roles.sql — Tablas de configuración y permisos.
--
-- Hasta hoy `obtener_config_negocio`, `obtener_config_descuentos`,
-- `listar_roles` y `rol_es_admin` estaban hardcodeados en rpc.rs (ver
-- comentarios "// El POS local hay tabla X. En web se hardcodea."). Eso
-- significa:
--   - El usuario web NO podía editar la información del negocio para tickets.
--   - Si se creaba un rol nuevo en SQLite, el web no lo veía.
--   - Las constantes de descuentos máximos no eran ajustables.
--
-- Esta migración crea las 4 tablas que faltaban y siembra los mismos
-- defaults que el desktop SQLite (ver schema.rs líneas 92-112 y SEED_DATA).
-- Los handlers de rpc.rs cambian a leer de aquí en este mismo PR.

-- ─── 1. config_negocio ─────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS config_negocio (
    id                    INTEGER PRIMARY KEY DEFAULT 1,
    nombre                TEXT NOT NULL DEFAULT 'Moto Refaccionaria',
    direccion             TEXT NOT NULL DEFAULT '',
    telefono              TEXT NOT NULL DEFAULT '',
    rfc                   TEXT NOT NULL DEFAULT '',
    mensaje_pie           TEXT NOT NULL DEFAULT '¡Gracias por su compra!',
    respaldo_auto_activo  INTEGER NOT NULL DEFAULT 1,
    respaldo_auto_hora    TEXT NOT NULL DEFAULT '23:00',
    impresora_termica     TEXT NOT NULL DEFAULT '',
    updated_at            TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    CHECK (id = 1)
);

INSERT INTO config_negocio (id) VALUES (1)
ON CONFLICT (id) DO NOTHING;

-- ─── 2. config_descuentos ──────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS config_descuentos (
    id                            INTEGER PRIMARY KEY DEFAULT 1,
    descuento_max_vendedor_pct    NUMERIC(6,2) NOT NULL DEFAULT 15.0,
    descuento_max_total_pct       NUMERIC(6,2) NOT NULL DEFAULT 10.0,
    precio_minimo_global_margen   NUMERIC(6,2) NOT NULL DEFAULT 5.0,
    updated_at                    TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    CHECK (id = 1)
);

INSERT INTO config_descuentos (id) VALUES (1)
ON CONFLICT (id) DO NOTHING;

-- ─── 3. roles ──────────────────────────────────────────────────────────
-- Mismos ids que el desktop SQLite (1=dueño, 2=vendedor, 3=almacenista).
-- Mantenemos esa numeración porque `rol_es_admin` y handlers existentes
-- la asumen en muchos lugares.
CREATE TABLE IF NOT EXISTS roles (
    id          BIGSERIAL PRIMARY KEY,
    nombre      TEXT NOT NULL UNIQUE,
    descripcion TEXT,
    es_admin    INTEGER NOT NULL DEFAULT 0
);

INSERT INTO roles (id, nombre, descripcion, es_admin) VALUES
    (1, 'dueño',       'Acceso total al sistema',         1),
    (2, 'vendedor',    'Realizar ventas y cobrar',        0),
    (3, 'almacenista', 'Gestión de inventario y pedidos', 0)
ON CONFLICT (id) DO NOTHING;

-- Importante: ajustar la secuencia para que el próximo INSERT con id auto
-- generado no choque con los seed (postgres no avanza el counter al hacer
-- INSERT con id explícito).
SELECT setval(pg_get_serial_sequence('roles', 'id'),
              GREATEST((SELECT MAX(id) FROM roles), 1));

-- ─── 4. permisos ───────────────────────────────────────────────────────
-- Misma matriz que el desktop SEED_DATA. Cuando el web aprenda a
-- evaluar permisos por rol (hoy `tienePermiso` solo distingue admin
-- vs no-admin), bastará con leer de aquí.
CREATE TABLE IF NOT EXISTS permisos (
    id          BIGSERIAL PRIMARY KEY,
    rol_id      BIGINT NOT NULL REFERENCES roles(id),
    modulo      TEXT NOT NULL,
    accion      TEXT NOT NULL,
    permitido   INTEGER NOT NULL DEFAULT 0,
    UNIQUE (rol_id, modulo, accion)
);

-- DUEÑO (rol_id=1): acceso total
INSERT INTO permisos (rol_id, modulo, accion, permitido) VALUES
    (1, 'ventas',       'ver',      1), (1, 'ventas',       'crear',    1),
    (1, 'ventas',       'editar',   1), (1, 'ventas',       'eliminar', 1),
    (1, 'ventas',       'anular',   1),
    (1, 'devoluciones', 'ver',      1), (1, 'devoluciones', 'crear',    1),
    (1, 'inventario',   'ver',      1), (1, 'inventario',   'crear',    1),
    (1, 'inventario',   'editar',   1), (1, 'inventario',   'eliminar', 1),
    (1, 'precios',      'ver',      1), (1, 'precios',      'editar',   1),
    (1, 'pedidos',      'ver',      1), (1, 'pedidos',      'crear',    1),
    (1, 'pedidos',      'editar',   1),
    (1, 'reportes',     'ver',      1),
    (1, 'usuarios',     'ver',      1), (1, 'usuarios',     'crear',    1),
    (1, 'usuarios',     'editar',   1), (1, 'usuarios',     'eliminar', 1),
    (1, 'bitacora',     'ver',      1)
ON CONFLICT (rol_id, modulo, accion) DO NOTHING;

-- VENDEDOR (rol_id=2)
INSERT INTO permisos (rol_id, modulo, accion, permitido) VALUES
    (2, 'ventas',       'ver',      1), (2, 'ventas',       'crear',    1),
    (2, 'ventas',       'anular',   0),
    (2, 'devoluciones', 'ver',      1), (2, 'devoluciones', 'crear',    1),
    (2, 'inventario',   'ver',      1), (2, 'inventario',   'crear',    1),
    (2, 'inventario',   'editar',   1),
    (2, 'precios',      'ver',      0), (2, 'precios',      'editar',   0),
    (2, 'pedidos',      'ver',      1), (2, 'pedidos',      'crear',    1),
    (2, 'reportes',     'ver',      0),
    (2, 'usuarios',     'ver',      0),
    (2, 'bitacora',     'ver',      0)
ON CONFLICT (rol_id, modulo, accion) DO NOTHING;

-- ALMACENISTA (rol_id=3)
INSERT INTO permisos (rol_id, modulo, accion, permitido) VALUES
    (3, 'ventas',       'ver',      0), (3, 'ventas',       'crear',    0),
    (3, 'inventario',   'ver',      1), (3, 'inventario',   'crear',    1),
    (3, 'inventario',   'editar',   1),
    (3, 'pedidos',      'ver',      1), (3, 'pedidos',      'crear',    1),
    (3, 'precios',      'ver',      0), (3, 'reportes',     'ver',      0),
    (3, 'usuarios',     'ver',      0), (3, 'bitacora',     'ver',      0)
ON CONFLICT (rol_id, modulo, accion) DO NOTHING;

CREATE INDEX IF NOT EXISTS idx_permisos_rol ON permisos(rol_id);
