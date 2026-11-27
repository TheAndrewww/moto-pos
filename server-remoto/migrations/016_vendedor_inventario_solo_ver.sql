-- 016_vendedor_inventario_solo_ver.sql — El vendedor consulta Inventario
-- pero no lo modifica (2026-11-26).
--
-- Espejo de las migraciones 012 y 016 del POS de escritorio. La 007 de aquí
-- le dejó al vendedor (rol 2) `inventario.crear` y `inventario.editar`, así
-- que en el POS web un empleado podía dar de alta, editar y ajustar
-- existencias. El dueño pidió que puedan VER el inventario pero no
-- modificarlo; un producto nuevo solo lo dan de alta desde Recepción
-- (permiso `recepcion.crear`, que ya tienen).
--
-- Las RPC de productos ahora también revisan estos permisos en el servidor.
-- Idempotente: UNIQUE(rol_id, modulo, accion) desde la 007.

INSERT INTO permisos (rol_id, modulo, accion, permitido) VALUES
    (2, 'inventario', 'ver',      1),
    (2, 'inventario', 'crear',    0),
    (2, 'inventario', 'editar',   0),
    (2, 'inventario', 'eliminar', 0)
ON CONFLICT (rol_id, modulo, accion) DO UPDATE SET permitido = EXCLUDED.permitido;
