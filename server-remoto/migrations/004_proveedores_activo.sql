-- 004_proveedores_activo.sql — soft-toggle de proveedores en web.
--
-- El esquema postgres heredó `deleted_at` para tombstones de sync, pero la UI
-- web de proveedores trabaja con un flag `activo` (mismo patrón que clientes:
-- ocultar/restaurar sin perder los productos que los referencian).
--
-- Default 1 (activo) para que las filas existentes (incluyendo las que ya
-- vinieron del desktop antes de esta migración) sigan apareciendo.

ALTER TABLE proveedores
    ADD COLUMN IF NOT EXISTS activo INTEGER NOT NULL DEFAULT 1;
