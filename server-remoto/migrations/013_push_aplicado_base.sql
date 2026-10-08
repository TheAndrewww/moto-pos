-- 013_push_aplicado_base.sql — Recordar QUÉ se aplicó de cada equipo sobre cada base.
--
-- Caso: el escritorio empuja el producto P (venta 1) sobre la base B, el
-- servidor lo aplica pero la respuesta se pierde; antes del reintento la cajera
-- vende P otra vez, así que el reintento trae otro contenido (A') con la misma
-- base B vieja. Fusionar A' contra B volvería a restar la venta 1. Con esto el
-- servidor sabe que sobre B ya aplicó A de este equipo y usa A como base real.
ALTER TABLE sync_push_aplicado ADD COLUMN IF NOT EXISTS id BIGSERIAL;
ALTER TABLE sync_push_aplicado ADD COLUMN IF NOT EXISTS base_md5 TEXT;
ALTER TABLE sync_push_aplicado ADD COLUMN IF NOT EXISTS datos JSONB;
CREATE INDEX IF NOT EXISTS idx_sync_push_aplicado_base
    ON sync_push_aplicado (device_uuid, tabla, uuid, base_md5, id);
