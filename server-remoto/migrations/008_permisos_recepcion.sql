-- 008_permisos_recepcion.sql — Módulo `recepcion` para los 3 roles seed.
--
-- Espejo de la migración 013 del POS desktop. El sidebar antes mostraba
-- "Recepción" si el usuario tenía `inventario.crear`; eso excluía al
-- vendedor (rol 2). El negocio pidió que el vendedor pueda recibir
-- mercancía sin tener inventario general → permiso propio `recepcion`.
--
-- La tabla `permisos` tiene UNIQUE(rol_id, modulo, accion) desde la
-- migración 007, así que ON CONFLICT DO NOTHING hace la operación
-- idempotente.

INSERT INTO permisos (rol_id, modulo, accion, permitido) VALUES
    (1, 'recepcion', 'ver',   1), (1, 'recepcion', 'crear', 1),
    (2, 'recepcion', 'ver',   1), (2, 'recepcion', 'crear', 1),
    (3, 'recepcion', 'ver',   1), (3, 'recepcion', 'crear', 1)
ON CONFLICT (rol_id, modulo, accion) DO NOTHING;
