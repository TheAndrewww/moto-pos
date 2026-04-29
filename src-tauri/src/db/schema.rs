// db/schema.rs — Schema SQLite completo para Fase 1
// POS Moto Refaccionaria

pub const SCHEMA_V1: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
PRAGMA cache_size = -20000;  -- 20MB cache
PRAGMA synchronous = NORMAL;

-- ============================================================
-- ROLES Y PERMISOS
-- ============================================================
CREATE TABLE IF NOT EXISTS roles (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    nombre      TEXT NOT NULL UNIQUE,  -- dueño | vendedor | almacenista
    descripcion TEXT,
    es_admin    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS permisos (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    rol_id    INTEGER NOT NULL REFERENCES roles(id),
    modulo    TEXT NOT NULL,  -- ventas | inventario | precios | pedidos | reportes | usuarios | bitacora
    accion    TEXT NOT NULL,  -- ver | crear | editar | eliminar | anular
    permitido INTEGER NOT NULL DEFAULT 0
);

-- ============================================================
-- USUARIOS Y SESIONES
-- ============================================================
CREATE TABLE IF NOT EXISTS usuarios (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    nombre_completo TEXT NOT NULL,
    nombre_usuario  TEXT NOT NULL UNIQUE,
    pin             TEXT NOT NULL,           -- 4 dígitos (hasheado)
    password_hash   TEXT NOT NULL,           -- bcrypt
    rol_id          INTEGER NOT NULL REFERENCES roles(id),
    activo          INTEGER NOT NULL DEFAULT 1,
    ultimo_login    TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
    synced_at       TEXT
);

CREATE TABLE IF NOT EXISTS sesiones (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    usuario_id  INTEGER NOT NULL REFERENCES usuarios(id),
    inicio      TEXT NOT NULL DEFAULT (datetime('now')),
    fin         TEXT,
    origen      TEXT NOT NULL DEFAULT 'POS'  -- POS | WEB
);

-- ============================================================
-- CATEGORÍAS Y PROVEEDORES
-- ============================================================
CREATE TABLE IF NOT EXISTS categorias (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    nombre      TEXT NOT NULL UNIQUE,
    descripcion TEXT
);

CREATE TABLE IF NOT EXISTS proveedores (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    nombre   TEXT NOT NULL,
    contacto TEXT,
    telefono TEXT,
    email    TEXT,
    notas    TEXT,
    activo   INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    synced_at  TEXT
);

-- ============================================================
-- CLIENTES
-- ============================================================
CREATE TABLE IF NOT EXISTS clientes (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    nombre               TEXT NOT NULL,
    telefono             TEXT,
    email                TEXT,
    descuento_porcentaje REAL NOT NULL DEFAULT 0,  -- % descuento fijo del cliente (ej: 10 = 10%)
    activo               INTEGER NOT NULL DEFAULT 1,
    notas                TEXT,
    created_at           TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at           TEXT NOT NULL DEFAULT (datetime('now')),
    synced_at            TEXT
);

-- ============================================================
-- CONFIGURACIÓN DE DESCUENTOS
-- ============================================================
CREATE TABLE IF NOT EXISTS config_descuentos (
    id                              INTEGER PRIMARY KEY DEFAULT 1,
    descuento_max_vendedor_pct      REAL NOT NULL DEFAULT 15.0,
    descuento_max_total_pct         REAL NOT NULL DEFAULT 10.0,
    precio_minimo_global_margen     REAL NOT NULL DEFAULT 5.0,
    updated_at                      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- ============================================================
-- CONFIGURACIÓN DEL NEGOCIO (para tickets)
-- ============================================================
CREATE TABLE IF NOT EXISTS config_negocio (
    id          INTEGER PRIMARY KEY DEFAULT 1,
    nombre      TEXT NOT NULL DEFAULT 'Moto Refaccionaria',
    direccion   TEXT NOT NULL DEFAULT '',
    telefono    TEXT NOT NULL DEFAULT '',
    rfc         TEXT NOT NULL DEFAULT '',
    mensaje_pie TEXT NOT NULL DEFAULT '¡Gracias por su compra!',
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- ============================================================
-- PRODUCTOS / CATÁLOGO
-- ============================================================
CREATE TABLE IF NOT EXISTS productos (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    codigo          TEXT NOT NULL UNIQUE,
    codigo_tipo     TEXT NOT NULL DEFAULT 'INTERNO',  -- EAN13 | CODE128 | QR | INTERNO
    nombre          TEXT NOT NULL,
    descripcion     TEXT,
    categoria_id    INTEGER REFERENCES categorias(id),
    precio_costo    REAL NOT NULL DEFAULT 0,
    precio_venta    REAL NOT NULL DEFAULT 0,
    stock_actual    REAL NOT NULL DEFAULT 0,
    stock_minimo    REAL NOT NULL DEFAULT 0,
    proveedor_id    INTEGER REFERENCES proveedores(id),
    foto_url        TEXT,
    search_text     TEXT,                       -- pre-calculado: código+nombre+desc en minúsculas sin acentos
    activo          INTEGER NOT NULL DEFAULT 1,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
    synced_at       TEXT
);

-- Secuencia para códigos internos MR-XXXXX
CREATE TABLE IF NOT EXISTS codigo_secuencia (
    id           INTEGER PRIMARY KEY DEFAULT 1,
    ultimo_valor INTEGER NOT NULL DEFAULT 0
);

-- Secuencia para folios de venta V-XXXXXX
CREATE TABLE IF NOT EXISTS folio_secuencia (
    id           INTEGER PRIMARY KEY DEFAULT 1,
    ultimo_valor INTEGER NOT NULL DEFAULT 0
);

-- ============================================================
-- VENTAS
-- ============================================================
CREATE TABLE IF NOT EXISTS ventas (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    folio           TEXT NOT NULL UNIQUE,
    usuario_id      INTEGER NOT NULL REFERENCES usuarios(id),
    cliente_id      INTEGER REFERENCES clientes(id),
    subtotal        REAL NOT NULL DEFAULT 0,
    descuento       REAL NOT NULL DEFAULT 0,
    total           REAL NOT NULL DEFAULT 0,
    metodo_pago     TEXT NOT NULL DEFAULT 'efectivo',  -- efectivo | tarjeta | transferencia
    monto_recibido  REAL NOT NULL DEFAULT 0,
    cambio          REAL NOT NULL DEFAULT 0,
    anulada         INTEGER NOT NULL DEFAULT 0,
    anulada_por     INTEGER REFERENCES usuarios(id),
    motivo_anulacion TEXT,
    fecha           TEXT NOT NULL DEFAULT (datetime('now')),
    synced_at       TEXT
);

CREATE TABLE IF NOT EXISTS venta_detalle (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    venta_id             INTEGER NOT NULL REFERENCES ventas(id),
    producto_id          INTEGER NOT NULL REFERENCES productos(id),
    cantidad             REAL NOT NULL DEFAULT 1,
    precio_original      REAL NOT NULL DEFAULT 0,
    descuento_porcentaje REAL NOT NULL DEFAULT 0,
    descuento_monto      REAL NOT NULL DEFAULT 0,
    precio_final         REAL NOT NULL DEFAULT 0,
    subtotal             REAL NOT NULL DEFAULT 0,
    autorizado_por       INTEGER REFERENCES usuarios(id)
);

-- ============================================================
-- PRESUPUESTOS
-- ============================================================
CREATE TABLE IF NOT EXISTS presupuestos (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    folio       TEXT NOT NULL UNIQUE,
    usuario_id  INTEGER NOT NULL REFERENCES usuarios(id),
    cliente_id  INTEGER REFERENCES clientes(id),
    estado      TEXT NOT NULL DEFAULT 'pendiente',  -- pendiente | aceptado | convertido | cancelado
    notas       TEXT,
    vigencia_dias INTEGER NOT NULL DEFAULT 7,
    total       REAL NOT NULL DEFAULT 0,
    fecha       TEXT NOT NULL DEFAULT (datetime('now')),
    venta_id    INTEGER REFERENCES ventas(id),
    synced_at   TEXT
);

CREATE TABLE IF NOT EXISTS presupuesto_detalle (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    presupuesto_id       INTEGER NOT NULL REFERENCES presupuestos(id),
    producto_id          INTEGER REFERENCES productos(id),
    descripcion          TEXT NOT NULL,
    cantidad             REAL NOT NULL DEFAULT 1,
    precio_unitario      REAL NOT NULL DEFAULT 0,
    descuento_porcentaje REAL NOT NULL DEFAULT 0,
    subtotal             REAL NOT NULL DEFAULT 0
);

-- ============================================================
-- ÓRDENES DE PEDIDO A PROVEEDORES
-- ============================================================
CREATE TABLE IF NOT EXISTS ordenes_pedido (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    folio            TEXT NOT NULL UNIQUE,
    usuario_id       INTEGER NOT NULL REFERENCES usuarios(id),
    origen           TEXT NOT NULL DEFAULT 'POS',  -- POS | WEB
    proveedor_id     INTEGER REFERENCES proveedores(id),
    estado           TEXT NOT NULL DEFAULT 'borrador',  -- borrador | enviada | recibida_parcial | recibida_completa
    notas            TEXT,
    fecha_pedido     TEXT NOT NULL DEFAULT (datetime('now')),
    fecha_recepcion  TEXT,
    synced_at        TEXT
);

CREATE TABLE IF NOT EXISTS orden_pedido_detalle (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    orden_id         INTEGER NOT NULL REFERENCES ordenes_pedido(id),
    producto_id      INTEGER NOT NULL REFERENCES productos(id),
    cantidad_pedida  REAL NOT NULL DEFAULT 1,
    cantidad_recibida REAL NOT NULL DEFAULT 0,
    precio_costo     REAL NOT NULL DEFAULT 0
);

-- ============================================================
-- RECEPCIONES DE MERCANCÍA
-- ============================================================
CREATE TABLE IF NOT EXISTS recepciones (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    orden_id     INTEGER REFERENCES ordenes_pedido(id),
    usuario_id   INTEGER NOT NULL REFERENCES usuarios(id),
    proveedor_id INTEGER REFERENCES proveedores(id),
    fecha        TEXT NOT NULL DEFAULT (datetime('now')),
    notas        TEXT,
    synced_at    TEXT
);

CREATE TABLE IF NOT EXISTS recepcion_detalle (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    recepcion_id  INTEGER NOT NULL REFERENCES recepciones(id),
    producto_id   INTEGER NOT NULL REFERENCES productos(id),
    cantidad      REAL NOT NULL DEFAULT 1,
    precio_costo  REAL NOT NULL DEFAULT 0
);

-- ============================================================
-- BITÁCORA DE AUDITORÍA (SOLO INSERT, NUNCA UPDATE/DELETE)
-- ============================================================
CREATE TABLE IF NOT EXISTS audit_log (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    usuario_id         INTEGER REFERENCES usuarios(id),
    accion             TEXT NOT NULL,
    tabla_afectada     TEXT,
    registro_id        INTEGER,
    datos_anteriores   TEXT,  -- JSON
    datos_nuevos       TEXT,  -- JSON
    descripcion_legible TEXT NOT NULL,
    origen             TEXT NOT NULL DEFAULT 'POS',
    fecha              TEXT NOT NULL DEFAULT (datetime('now')),
    synced_at          TEXT
);

-- ============================================================
-- LOG DE SINCRONIZACIÓN
-- ============================================================
CREATE TABLE IF NOT EXISTS sync_log (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    tabla        TEXT NOT NULL,
    registro_id  INTEGER NOT NULL,
    accion       TEXT NOT NULL,  -- INSERT | UPDATE | DELETE
    datos_json   TEXT,
    fecha        TEXT NOT NULL DEFAULT (datetime('now')),
    sincronizado INTEGER NOT NULL DEFAULT 0
);

-- ============================================================
-- CORTES DE CAJA
-- ============================================================
CREATE TABLE IF NOT EXISTS cortes (
    id                          INTEGER PRIMARY KEY AUTOINCREMENT,
    tipo                        TEXT NOT NULL CHECK(tipo IN ('PARCIAL', 'DIA')),
    usuario_id                  INTEGER NOT NULL REFERENCES usuarios(id),
    fecha_inicio                TEXT NOT NULL,
    fecha_fin                   TEXT NOT NULL,
    fondo_inicial               REAL NOT NULL DEFAULT 0,
    total_ventas_efectivo       REAL NOT NULL DEFAULT 0,
    total_ventas_tarjeta        REAL NOT NULL DEFAULT 0,
    total_ventas_transferencia  REAL NOT NULL DEFAULT 0,
    total_ventas                REAL NOT NULL DEFAULT 0,
    num_transacciones           INTEGER NOT NULL DEFAULT 0,
    total_descuentos            REAL NOT NULL DEFAULT 0,
    total_anulaciones           REAL NOT NULL DEFAULT 0,
    total_entradas_efectivo     REAL NOT NULL DEFAULT 0,
    total_retiros_efectivo      REAL NOT NULL DEFAULT 0,
    efectivo_esperado           REAL NOT NULL DEFAULT 0,
    efectivo_contado            REAL NOT NULL DEFAULT 0,
    diferencia                  REAL NOT NULL DEFAULT 0,
    nota_diferencia             TEXT,
    fondo_siguiente             REAL NOT NULL DEFAULT 0,
    impreso                     INTEGER NOT NULL DEFAULT 0,
    created_at                  TEXT NOT NULL DEFAULT (datetime('now', 'localtime')),
    synced_at                   TEXT
);

CREATE TABLE IF NOT EXISTS corte_denominaciones (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    corte_id     INTEGER NOT NULL REFERENCES cortes(id),
    denominacion REAL NOT NULL,
    tipo         TEXT NOT NULL CHECK(tipo IN ('BILLETE', 'MONEDA')),
    cantidad     INTEGER NOT NULL DEFAULT 0,
    subtotal     REAL NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS movimientos_caja (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    tipo            TEXT NOT NULL CHECK(tipo IN ('ENTRADA', 'RETIRO')),
    usuario_id      INTEGER NOT NULL REFERENCES usuarios(id),
    monto           REAL NOT NULL,
    concepto        TEXT NOT NULL,
    autorizado_por  INTEGER REFERENCES usuarios(id),
    corte_id        INTEGER REFERENCES cortes(id),
    fecha           TEXT NOT NULL DEFAULT (datetime('now', 'localtime')),
    synced_at       TEXT
);

CREATE TABLE IF NOT EXISTS corte_vendedores (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    corte_id      INTEGER NOT NULL REFERENCES cortes(id),
    usuario_id    INTEGER NOT NULL REFERENCES usuarios(id),
    num_ventas    INTEGER NOT NULL DEFAULT 0,
    total_vendido REAL NOT NULL DEFAULT 0,
    hora_inicio   TEXT NOT NULL,
    hora_fin      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS aperturas_caja (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    usuario_id      INTEGER NOT NULL REFERENCES usuarios(id),
    fondo_declarado REAL NOT NULL,
    nota            TEXT,
    fecha           TEXT NOT NULL DEFAULT (datetime('now', 'localtime')),
    synced_at       TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_apertura_dia_unica ON aperturas_caja(date(fecha));

-- ============================================================
-- DEVOLUCIONES (parciales)
-- ============================================================
CREATE TABLE IF NOT EXISTS devoluciones (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    folio              TEXT NOT NULL UNIQUE,
    venta_id           INTEGER NOT NULL REFERENCES ventas(id),
    usuario_id         INTEGER NOT NULL REFERENCES usuarios(id),
    autorizado_por     INTEGER REFERENCES usuarios(id),
    motivo             TEXT NOT NULL,
    total_devuelto     REAL NOT NULL DEFAULT 0,
    movimiento_caja_id INTEGER REFERENCES movimientos_caja(id),
    fecha              TEXT NOT NULL DEFAULT (datetime('now', 'localtime')),
    synced_at          TEXT
);

CREATE TABLE IF NOT EXISTS devolucion_detalle (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    devolucion_id    INTEGER NOT NULL REFERENCES devoluciones(id),
    venta_detalle_id INTEGER NOT NULL REFERENCES venta_detalle(id),
    producto_id      INTEGER NOT NULL REFERENCES productos(id),
    cantidad         REAL NOT NULL,
    precio_unitario  REAL NOT NULL,
    subtotal         REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS devolucion_folio_secuencia (
    id           INTEGER PRIMARY KEY DEFAULT 1,
    ultimo_valor INTEGER NOT NULL DEFAULT 0
);

-- ============================================================
-- DISPOSITIVOS MÓVILES CONECTADOS (Fase 3.1)
-- ============================================================
CREATE TABLE IF NOT EXISTS dispositivos_conectados (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    nombre        TEXT NOT NULL,
    user_agent    TEXT,
    usuario_id    INTEGER NOT NULL REFERENCES usuarios(id),
    jwt_jti       TEXT NOT NULL UNIQUE,
    ultimo_ping   TEXT,
    ip_ultima     TEXT,
    created_at    TEXT NOT NULL DEFAULT (datetime('now','localtime')),
    revocado      INTEGER NOT NULL DEFAULT 0
);

-- ============================================================
-- ÍNDICES (velocidad crítica del POS)
-- ============================================================
CREATE INDEX IF NOT EXISTS idx_productos_codigo     ON productos(codigo);
CREATE INDEX IF NOT EXISTS idx_productos_nombre     ON productos(nombre);
CREATE INDEX IF NOT EXISTS idx_productos_categoria  ON productos(categoria_id);
CREATE INDEX IF NOT EXISTS idx_productos_stock      ON productos(stock_actual);
CREATE INDEX IF NOT EXISTS idx_productos_search     ON productos(search_text);
CREATE INDEX IF NOT EXISTS idx_ventas_fecha         ON ventas(fecha);
CREATE INDEX IF NOT EXISTS idx_ventas_usuario       ON ventas(usuario_id);
CREATE INDEX IF NOT EXISTS idx_ventas_folio         ON ventas(folio);
CREATE INDEX IF NOT EXISTS idx_vd_venta             ON venta_detalle(venta_id);
CREATE INDEX IF NOT EXISTS idx_vd_producto          ON venta_detalle(producto_id);
CREATE INDEX IF NOT EXISTS idx_audit_usuario_fecha  ON audit_log(usuario_id, fecha);
CREATE INDEX IF NOT EXISTS idx_audit_accion_fecha   ON audit_log(accion, fecha);
CREATE INDEX IF NOT EXISTS idx_clientes_telefono    ON clientes(telefono);
CREATE INDEX IF NOT EXISTS idx_cortes_fecha         ON cortes(created_at);
CREATE INDEX IF NOT EXISTS idx_movimientos_fecha    ON movimientos_caja(fecha);
CREATE INDEX IF NOT EXISTS idx_devoluciones_venta   ON devoluciones(venta_id);
CREATE INDEX IF NOT EXISTS idx_devoluciones_fecha   ON devoluciones(fecha);
CREATE INDEX IF NOT EXISTS idx_devdet_devolucion    ON devolucion_detalle(devolucion_id);
CREATE INDEX IF NOT EXISTS idx_devdet_vdetalle      ON devolucion_detalle(venta_detalle_id);
CREATE INDEX IF NOT EXISTS idx_disp_jti              ON dispositivos_conectados(jwt_jti);
CREATE INDEX IF NOT EXISTS idx_disp_usuario          ON dispositivos_conectados(usuario_id);
"#;

/// Datos iniciales del sistema (roles, permisos, config)
pub const SEED_DATA: &str = r#"
-- Insertar roles base si no existen
INSERT OR IGNORE INTO roles (id, nombre, descripcion, es_admin) VALUES
    (1, 'dueño',       'Acceso total al sistema',         1),
    (2, 'vendedor',    'Realizar ventas y cobrar',        0),
    (3, 'almacenista', 'Gestión de inventario y pedidos', 0);

-- Configuración de descuentos por defecto
INSERT OR IGNORE INTO config_descuentos (id, descuento_max_vendedor_pct, descuento_max_total_pct, precio_minimo_global_margen)
    VALUES (1, 15.0, 10.0, 5.0);

-- Configuración de negocio por defecto
INSERT OR IGNORE INTO config_negocio (id, nombre, direccion, telefono, rfc, mensaje_pie)
    VALUES (1, 'Moto Refaccionaria', '', '', '', '¡Gracias por su compra!');

-- Secuencia inicial de códigos internos
INSERT OR IGNORE INTO codigo_secuencia (id, ultimo_valor) VALUES (1, 0);

-- Secuencia inicial de folios de venta
INSERT OR IGNORE INTO folio_secuencia (id, ultimo_valor) VALUES (1, 0);

-- Secuencia inicial de folios de devolución
INSERT OR IGNORE INTO devolucion_folio_secuencia (id, ultimo_valor) VALUES (1, 0);

-- Permisos del DUEÑO (acceso total)
INSERT OR IGNORE INTO permisos (rol_id, modulo, accion, permitido) VALUES
    (1, 'ventas',     'ver',      1), (1, 'ventas',     'crear',    1),
    (1, 'ventas',     'editar',   1), (1, 'ventas',     'eliminar', 1),
    (1, 'ventas',     'anular',   1),
    (1, 'devoluciones','ver',     1), (1, 'devoluciones','crear',   1),
    (1, 'inventario', 'ver',      1), (1, 'inventario', 'crear',    1),
    (1, 'inventario', 'editar',   1), (1, 'inventario', 'eliminar', 1),
    (1, 'precios',    'ver',      1), (1, 'precios',    'editar',   1),
    (1, 'pedidos',    'ver',      1), (1, 'pedidos',    'crear',    1),
    (1, 'pedidos',    'editar',   1),
    (1, 'reportes',   'ver',      1),
    (1, 'usuarios',   'ver',      1), (1, 'usuarios',   'crear',    1),
    (1, 'usuarios',   'editar',   1), (1, 'usuarios',   'eliminar', 1),
    (1, 'bitacora',   'ver',      1);

-- Permisos del VENDEDOR
INSERT OR IGNORE INTO permisos (rol_id, modulo, accion, permitido) VALUES
    (2, 'ventas',     'ver',      1), (2, 'ventas',     'crear',    1),
    (2, 'ventas',     'anular',   0),
    (2, 'devoluciones','ver',     1), (2, 'devoluciones','crear',   1),
    (2, 'inventario', 'ver',      1), (2, 'inventario', 'crear',    1),
    (2, 'inventario', 'editar',   1),
    (2, 'precios',    'ver',      0), (2, 'precios',    'editar',   0),
    (2, 'pedidos',    'ver',      1), (2, 'pedidos',    'crear',    1),
    (2, 'reportes',   'ver',      0),
    (2, 'usuarios',   'ver',      0),
    (2, 'bitacora',   'ver',      0);

-- Permisos del ALMACENISTA
INSERT OR IGNORE INTO permisos (rol_id, modulo, accion, permitido) VALUES
    (3, 'ventas',     'ver',      0), (3, 'ventas',     'crear',    0),
    (3, 'inventario', 'ver',      1), (3, 'inventario', 'crear',    1),
    (3, 'inventario', 'editar',   1),
    (3, 'pedidos',    'ver',      1), (3, 'pedidos',    'crear',    1),
    (3, 'precios',    'ver',      0), (3, 'reportes',   'ver',      0),
    (3, 'usuarios',   'ver',      0), (3, 'bitacora',   'ver',      0);

-- Categorías base para moto refaccionaria
INSERT OR IGNORE INTO categorias (nombre, descripcion) VALUES
    ('Frenos',          'Balatas, pastillas, discos, cables de freno'),
    ('Cadenas y Piñones', 'Cadenas, piñones, coronas, sprockets'),
    ('Aceites y Lubricantes', 'Aceite de motor, lubricante de cadena, fluido de frenos'),
    ('Eléctrico',       'Bujías, focos, faros, interruptores, cableado'),
    ('Suspensión',      'Amortiguadores, horquillas, resortes'),
    ('Motor',           'Pistones, válvulas, juntas, filtros de aire'),
    ('Carrocería',      'Espejos, calaveras, guardafangos, plásticos'),
    ('Transmisión',     'Clutch, cables, palancas, embrague'),
    ('Neumáticos',      'Llantas, cámaras, válvulas'),
    ('Accesorios',      'Llaveros, fundas, accesorios generales');
"#;
