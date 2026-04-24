-- Schema del servidor remoto Moto Refaccionaria (Fase 3.2)
-- Espejo de las tablas del POS pero con tipos Postgres y una tabla extra
-- `admin_users` para el login email+password del panel web.
--
-- IMPORTANTE: todas las columnas de fecha sync-tracked son TEXT con formato
-- 'YYYY-MM-DD HH24:MI:SS' idéntico al que emite SQLite del POS. Esto hace
-- que LWW sea una comparación de strings byte-por-byte sin conversiones.
-- Solo las tablas internas (admin_users, pos_devices, sync_cursor) usan
-- TIMESTAMPTZ porque nunca se sincronizan con el POS.

-- ============================================================
-- ADMINISTRADORES DEL PANEL WEB (interno, no sync)
-- ============================================================
CREATE TABLE IF NOT EXISTS admin_users (
    id              BIGSERIAL PRIMARY KEY,
    uuid            UUID NOT NULL UNIQUE DEFAULT gen_random_uuid(),
    email           TEXT NOT NULL UNIQUE,
    password_hash   TEXT NOT NULL,
    nombre          TEXT NOT NULL,
    sucursal_id     BIGINT,
    es_super_admin  BOOLEAN NOT NULL DEFAULT FALSE,
    activo          BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ============================================================
-- REGISTRO DE DISPOSITIVOS POS CONECTADOS (interno, no sync)
-- ============================================================
CREATE TABLE IF NOT EXISTS pos_devices (
    id              BIGSERIAL PRIMARY KEY,
    device_uuid     TEXT NOT NULL UNIQUE,
    sucursal_id     BIGINT NOT NULL,
    nombre          TEXT,
    last_push_at    TIMESTAMPTZ,
    last_pull_at    TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ============================================================
-- SUCURSALES (sync)
-- ============================================================
CREATE TABLE IF NOT EXISTS sucursales (
    id          BIGSERIAL PRIMARY KEY,
    uuid        TEXT NOT NULL UNIQUE,
    nombre      TEXT NOT NULL,
    direccion   TEXT,
    telefono    TEXT,
    activa      INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at  TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at  TEXT
);

INSERT INTO sucursales (id, uuid, nombre, direccion, telefono)
VALUES (1, '00000000-0000-7000-8000-000000000001', 'Principal', '', '')
ON CONFLICT (uuid) DO NOTHING;

-- ============================================================
-- CATÁLOGO (sync row-level)
-- ============================================================
CREATE TABLE IF NOT EXISTS categorias (
    id          BIGSERIAL PRIMARY KEY,
    uuid        TEXT NOT NULL UNIQUE,
    nombre      TEXT NOT NULL,
    descripcion TEXT,
    updated_at  TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at  TEXT
);

CREATE TABLE IF NOT EXISTS proveedores (
    id          BIGSERIAL PRIMARY KEY,
    uuid        TEXT NOT NULL UNIQUE,
    nombre      TEXT NOT NULL,
    contacto    TEXT,
    telefono    TEXT,
    email       TEXT,
    notas       TEXT,
    created_at  TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at  TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at  TEXT
);

CREATE TABLE IF NOT EXISTS productos (
    id               BIGSERIAL PRIMARY KEY,
    uuid             TEXT NOT NULL UNIQUE,
    codigo           TEXT NOT NULL,
    codigo_tipo      TEXT NOT NULL DEFAULT 'INTERNO',
    nombre           TEXT NOT NULL,
    descripcion      TEXT,
    categoria_id     BIGINT,
    precio_costo     NUMERIC(12,2) NOT NULL DEFAULT 0,
    precio_venta     NUMERIC(12,2) NOT NULL DEFAULT 0,
    stock_actual     NUMERIC(12,2) NOT NULL DEFAULT 0,
    stock_minimo     NUMERIC(12,2) NOT NULL DEFAULT 0,
    proveedor_id     BIGINT,
    foto_url         TEXT,
    search_text      TEXT,
    activo           INTEGER NOT NULL DEFAULT 1,
    created_at       TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at       TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at       TEXT
);
CREATE INDEX IF NOT EXISTS idx_productos_codigo ON productos(codigo);

CREATE TABLE IF NOT EXISTS clientes (
    id                   BIGSERIAL PRIMARY KEY,
    uuid                 TEXT NOT NULL UNIQUE,
    nombre               TEXT NOT NULL,
    telefono             TEXT,
    email                TEXT,
    descuento_porcentaje NUMERIC(6,2) NOT NULL DEFAULT 0,
    activo               INTEGER NOT NULL DEFAULT 1,
    notas                TEXT,
    created_at           TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at           TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at           TEXT
);

CREATE TABLE IF NOT EXISTS usuarios (
    id              BIGSERIAL PRIMARY KEY,
    uuid            TEXT NOT NULL UNIQUE,
    nombre_completo TEXT NOT NULL,
    nombre_usuario  TEXT NOT NULL,
    pin             TEXT NOT NULL,
    password_hash   TEXT NOT NULL,
    rol_id          BIGINT NOT NULL,
    activo          INTEGER NOT NULL DEFAULT 1,
    ultimo_login    TEXT,
    created_at      TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at      TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at      TEXT
);

CREATE TABLE IF NOT EXISTS stock_sucursal (
    id           BIGSERIAL PRIMARY KEY,
    uuid         TEXT NOT NULL UNIQUE,
    producto_id  BIGINT NOT NULL,
    sucursal_id  BIGINT NOT NULL,
    stock_actual NUMERIC(12,2) NOT NULL DEFAULT 0,
    stock_minimo NUMERIC(12,2) NOT NULL DEFAULT 0,
    updated_at   TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    UNIQUE(producto_id, sucursal_id)
);

-- ============================================================
-- TRANSACCIONAL (sync aggregate)
-- ============================================================
CREATE TABLE IF NOT EXISTS ventas (
    id                BIGSERIAL PRIMARY KEY,
    uuid              TEXT NOT NULL UNIQUE,
    sucursal_id       BIGINT NOT NULL,
    folio             TEXT NOT NULL,
    usuario_id        BIGINT NOT NULL,
    cliente_id        BIGINT,
    subtotal          NUMERIC(12,2) NOT NULL DEFAULT 0,
    descuento         NUMERIC(12,2) NOT NULL DEFAULT 0,
    total             NUMERIC(12,2) NOT NULL DEFAULT 0,
    metodo_pago       TEXT NOT NULL DEFAULT 'efectivo',
    monto_recibido    NUMERIC(12,2) NOT NULL DEFAULT 0,
    cambio            NUMERIC(12,2) NOT NULL DEFAULT 0,
    anulada           INTEGER NOT NULL DEFAULT 0,
    anulada_por       BIGINT,
    motivo_anulacion  TEXT,
    fecha             TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at        TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at        TEXT
);
CREATE INDEX IF NOT EXISTS idx_ventas_sucursal_fecha ON ventas(sucursal_id, fecha);

CREATE TABLE IF NOT EXISTS venta_detalle (
    id                   BIGSERIAL PRIMARY KEY,
    uuid                 TEXT NOT NULL UNIQUE,
    venta_id             BIGINT NOT NULL,
    producto_id          BIGINT NOT NULL,
    cantidad             NUMERIC(12,2) NOT NULL DEFAULT 1,
    precio_original      NUMERIC(12,2) NOT NULL DEFAULT 0,
    descuento_porcentaje NUMERIC(6,2) NOT NULL DEFAULT 0,
    descuento_monto      NUMERIC(12,2) NOT NULL DEFAULT 0,
    precio_final         NUMERIC(12,2) NOT NULL DEFAULT 0,
    subtotal             NUMERIC(12,2) NOT NULL DEFAULT 0,
    autorizado_por       BIGINT,
    updated_at           TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at           TEXT
);

CREATE TABLE IF NOT EXISTS recepciones (
    id           BIGSERIAL PRIMARY KEY,
    uuid         TEXT NOT NULL UNIQUE,
    sucursal_id  BIGINT NOT NULL,
    orden_id     BIGINT,
    usuario_id   BIGINT NOT NULL,
    proveedor_id BIGINT,
    fecha        TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    notas        TEXT,
    updated_at   TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at   TEXT
);

CREATE TABLE IF NOT EXISTS recepcion_detalle (
    id           BIGSERIAL PRIMARY KEY,
    uuid         TEXT NOT NULL UNIQUE,
    recepcion_id BIGINT NOT NULL,
    producto_id  BIGINT NOT NULL,
    cantidad     NUMERIC(12,2) NOT NULL DEFAULT 1,
    precio_costo NUMERIC(12,2) NOT NULL DEFAULT 0,
    updated_at   TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at   TEXT
);

CREATE TABLE IF NOT EXISTS ordenes_pedido (
    id              BIGSERIAL PRIMARY KEY,
    uuid            TEXT NOT NULL UNIQUE,
    sucursal_id     BIGINT NOT NULL,
    folio           TEXT NOT NULL,
    usuario_id      BIGINT NOT NULL,
    origen          TEXT NOT NULL DEFAULT 'POS',
    proveedor_id    BIGINT,
    estado          TEXT NOT NULL DEFAULT 'borrador',
    notas           TEXT,
    fecha_pedido    TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    fecha_recepcion TEXT,
    updated_at      TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at      TEXT
);

CREATE TABLE IF NOT EXISTS orden_pedido_detalle (
    id                BIGSERIAL PRIMARY KEY,
    uuid              TEXT NOT NULL UNIQUE,
    orden_id          BIGINT NOT NULL,
    producto_id       BIGINT NOT NULL,
    cantidad_pedida   NUMERIC(12,2) NOT NULL DEFAULT 1,
    cantidad_recibida NUMERIC(12,2) NOT NULL DEFAULT 0,
    precio_costo      NUMERIC(12,2) NOT NULL DEFAULT 0,
    updated_at        TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at        TEXT
);

CREATE TABLE IF NOT EXISTS presupuestos (
    id            BIGSERIAL PRIMARY KEY,
    uuid          TEXT NOT NULL UNIQUE,
    sucursal_id   BIGINT NOT NULL,
    folio         TEXT NOT NULL,
    usuario_id    BIGINT NOT NULL,
    cliente_id    BIGINT,
    estado        TEXT NOT NULL DEFAULT 'pendiente',
    notas         TEXT,
    vigencia_dias INTEGER NOT NULL DEFAULT 7,
    total         NUMERIC(12,2) NOT NULL DEFAULT 0,
    fecha         TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    venta_id      BIGINT,
    updated_at    TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at    TEXT
);

CREATE TABLE IF NOT EXISTS presupuesto_detalle (
    id                   BIGSERIAL PRIMARY KEY,
    uuid                 TEXT NOT NULL UNIQUE,
    presupuesto_id       BIGINT NOT NULL,
    producto_id          BIGINT,
    descripcion          TEXT NOT NULL,
    cantidad             NUMERIC(12,2) NOT NULL DEFAULT 1,
    precio_unitario      NUMERIC(12,2) NOT NULL DEFAULT 0,
    descuento_porcentaje NUMERIC(6,2) NOT NULL DEFAULT 0,
    subtotal             NUMERIC(12,2) NOT NULL DEFAULT 0,
    updated_at           TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at           TEXT
);

CREATE TABLE IF NOT EXISTS cortes (
    id                          BIGSERIAL PRIMARY KEY,
    uuid                        TEXT NOT NULL UNIQUE,
    sucursal_id                 BIGINT NOT NULL,
    tipo                        TEXT NOT NULL,
    usuario_id                  BIGINT NOT NULL,
    fecha_inicio                TEXT NOT NULL,
    fecha_fin                   TEXT NOT NULL,
    fondo_inicial               NUMERIC(12,2) NOT NULL DEFAULT 0,
    total_ventas_efectivo       NUMERIC(12,2) NOT NULL DEFAULT 0,
    total_ventas_tarjeta        NUMERIC(12,2) NOT NULL DEFAULT 0,
    total_ventas_transferencia  NUMERIC(12,2) NOT NULL DEFAULT 0,
    total_ventas                NUMERIC(12,2) NOT NULL DEFAULT 0,
    num_transacciones           INTEGER NOT NULL DEFAULT 0,
    total_descuentos            NUMERIC(12,2) NOT NULL DEFAULT 0,
    total_anulaciones           NUMERIC(12,2) NOT NULL DEFAULT 0,
    total_entradas_efectivo     NUMERIC(12,2) NOT NULL DEFAULT 0,
    total_retiros_efectivo      NUMERIC(12,2) NOT NULL DEFAULT 0,
    efectivo_esperado           NUMERIC(12,2) NOT NULL DEFAULT 0,
    efectivo_contado            NUMERIC(12,2) NOT NULL DEFAULT 0,
    diferencia                  NUMERIC(12,2) NOT NULL DEFAULT 0,
    nota_diferencia             TEXT,
    fondo_siguiente             NUMERIC(12,2) NOT NULL DEFAULT 0,
    impreso                     INTEGER NOT NULL DEFAULT 0,
    created_at                  TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at                  TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at                  TEXT
);
CREATE INDEX IF NOT EXISTS idx_cortes_sucursal_fecha ON cortes(sucursal_id, created_at);

CREATE TABLE IF NOT EXISTS corte_denominaciones (
    id           BIGSERIAL PRIMARY KEY,
    uuid         TEXT NOT NULL UNIQUE,
    corte_id     BIGINT NOT NULL,
    denominacion NUMERIC(12,2) NOT NULL,
    tipo         TEXT NOT NULL,
    cantidad     INTEGER NOT NULL DEFAULT 0,
    subtotal     NUMERIC(12,2) NOT NULL DEFAULT 0,
    updated_at   TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at   TEXT
);

CREATE TABLE IF NOT EXISTS corte_vendedores (
    id            BIGSERIAL PRIMARY KEY,
    uuid          TEXT NOT NULL UNIQUE,
    corte_id      BIGINT NOT NULL,
    usuario_id    BIGINT NOT NULL,
    num_ventas    INTEGER NOT NULL DEFAULT 0,
    total_vendido NUMERIC(12,2) NOT NULL DEFAULT 0,
    hora_inicio   TEXT NOT NULL,
    hora_fin      TEXT NOT NULL,
    updated_at    TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at    TEXT
);

CREATE TABLE IF NOT EXISTS movimientos_caja (
    id              BIGSERIAL PRIMARY KEY,
    uuid            TEXT NOT NULL UNIQUE,
    sucursal_id     BIGINT NOT NULL,
    tipo            TEXT NOT NULL,
    usuario_id      BIGINT NOT NULL,
    monto           NUMERIC(12,2) NOT NULL,
    concepto        TEXT NOT NULL,
    autorizado_por  BIGINT,
    corte_id        BIGINT,
    fecha           TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at      TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at      TEXT
);

CREATE TABLE IF NOT EXISTS aperturas_caja (
    id              BIGSERIAL PRIMARY KEY,
    uuid            TEXT NOT NULL UNIQUE,
    sucursal_id     BIGINT NOT NULL,
    usuario_id      BIGINT NOT NULL,
    fondo_declarado NUMERIC(12,2) NOT NULL,
    nota            TEXT,
    fecha           TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at      TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at      TEXT
);

CREATE TABLE IF NOT EXISTS devoluciones (
    id                 BIGSERIAL PRIMARY KEY,
    uuid               TEXT NOT NULL UNIQUE,
    sucursal_id        BIGINT NOT NULL,
    folio              TEXT NOT NULL,
    venta_id           BIGINT NOT NULL,
    usuario_id         BIGINT NOT NULL,
    autorizado_por     BIGINT,
    motivo             TEXT NOT NULL,
    total_devuelto     NUMERIC(12,2) NOT NULL DEFAULT 0,
    movimiento_caja_id BIGINT,
    fecha              TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at         TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at         TEXT
);

CREATE TABLE IF NOT EXISTS devolucion_detalle (
    id               BIGSERIAL PRIMARY KEY,
    uuid             TEXT NOT NULL UNIQUE,
    devolucion_id    BIGINT NOT NULL,
    venta_detalle_id BIGINT NOT NULL,
    producto_id      BIGINT NOT NULL,
    cantidad         NUMERIC(12,2) NOT NULL,
    precio_unitario  NUMERIC(12,2) NOT NULL,
    subtotal         NUMERIC(12,2) NOT NULL,
    updated_at       TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at       TEXT
);

CREATE TABLE IF NOT EXISTS transferencias (
    id                    BIGSERIAL PRIMARY KEY,
    uuid                  TEXT NOT NULL UNIQUE,
    folio                 TEXT NOT NULL,
    sucursal_origen_id    BIGINT NOT NULL,
    sucursal_destino_id   BIGINT NOT NULL,
    usuario_id            BIGINT NOT NULL,
    estado                TEXT NOT NULL DEFAULT 'PENDIENTE',
    notas                 TEXT,
    fecha_envio           TEXT,
    fecha_recepcion       TEXT,
    created_at            TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at            TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at            TEXT
);

CREATE TABLE IF NOT EXISTS transferencia_detalle (
    id               BIGSERIAL PRIMARY KEY,
    uuid             TEXT NOT NULL UNIQUE,
    transferencia_id BIGINT NOT NULL,
    producto_id      BIGINT NOT NULL,
    cantidad         NUMERIC(12,2) NOT NULL,
    updated_at       TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    deleted_at       TEXT
);

-- ============================================================
-- sync_cursor: marcador global de cambios (auto-incremental).
-- Cada upsert en tablas sync incrementa y registra aquí para pull ordenado.
-- Es interno del servidor, nunca se sincroniza, por eso TIMESTAMPTZ.
-- ============================================================
CREATE TABLE IF NOT EXISTS sync_cursor (
    id          BIGSERIAL PRIMARY KEY,
    tabla       TEXT NOT NULL,
    uuid        TEXT NOT NULL,
    sucursal_id BIGINT,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    origen_device TEXT
);
CREATE INDEX IF NOT EXISTS idx_sync_cursor_id ON sync_cursor(id);
CREATE INDEX IF NOT EXISTS idx_sync_cursor_suc ON sync_cursor(sucursal_id, id);
