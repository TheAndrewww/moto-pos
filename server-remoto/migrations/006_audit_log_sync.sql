-- 006_audit_log_sync.sql — Bitácora bidireccional desktop ↔ web.
--
-- HASTA HOY las dos bitácoras viven separadas:
--   - desktop SQLite `audit_log` con `origen='POS'` (nunca se sincronizaba)
--   - postgres   `audit_log` con `origen='WEB'` (alimenta `listar_bitacora`)
--
-- ESTE PARCHE las une. El web `listar_bitacora` ya leerá entradas POS+WEB
-- automáticamente porque ambas viven en la misma tabla postgres después
-- de que el desktop empiece a sincronizar.
--
-- Para que el sync funcione, `audit_log` necesita las 3 columnas que el
-- pipeline ya espera en cualquier tabla sincronizable:
--   - uuid       TEXT UNIQUE   → identidad estable cross-device
--   - updated_at TEXT NOT NULL → LWW (aunque audit_log es append-only)
--   - deleted_at TEXT          → soft-delete (defensa en profundidad;
--                                la bitácora no se borra desde la UI)
--
-- También agregamos un trigger que registra sync_cursor automáticamente,
-- evitando tocar los 26 call sites de `INSERT INTO audit_log` en rpc.rs.

-- ─── 1. Columnas faltantes ──────────────────────────────────────────────
ALTER TABLE audit_log
    ADD COLUMN IF NOT EXISTS uuid       TEXT,
    ADD COLUMN IF NOT EXISTS updated_at TEXT,
    ADD COLUMN IF NOT EXISTS deleted_at TEXT;

-- Backfill uuids para las filas existentes.
UPDATE audit_log
   SET uuid = replace(gen_random_uuid()::text, '-', '')
 WHERE uuid IS NULL;

-- updated_at = fecha (igualar al timestamp de la entrada).
UPDATE audit_log
   SET updated_at = fecha
 WHERE updated_at IS NULL;

-- Default generativo para inserts futuros (no toca filas existentes).
ALTER TABLE audit_log
    ALTER COLUMN uuid SET DEFAULT replace(gen_random_uuid()::text, '-', ''),
    ALTER COLUMN uuid SET NOT NULL,
    ALTER COLUMN updated_at SET DEFAULT to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS'),
    ALTER COLUMN updated_at SET NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_audit_log_uuid ON audit_log(uuid);

-- ─── 2. Trigger sync_cursor automático ──────────────────────────────────
--
-- En CADA INSERT a audit_log, registramos en sync_cursor para que otros
-- dispositivos lo pull-een. El `origen_device` se toma de una session
-- variable que el código de sync_apply setea cuando aplica un push del
-- desktop. Si la variable no está seteada (caso normal del web), usamos
-- 'web-pos' como default.
--
-- Sin esta variable seteada, los inserts del desktop (vía /sync/push)
-- registrarían sync_cursor con origen='web-pos', y luego el desktop
-- pull-earía sus PROPIAS entradas → no es bug grave (el sync apply hace
-- LWW y descarta), pero genera tráfico innecesario.

CREATE OR REPLACE FUNCTION audit_log_sync_cursor_trigger()
RETURNS TRIGGER AS $$
DECLARE
    v_origin TEXT;
BEGIN
    -- current_setting con missing_ok=true devuelve '' si la var no existe.
    v_origin := COALESCE(NULLIF(current_setting('sync.origin', true), ''), 'web-pos');
    INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device)
    VALUES ('audit_log', NEW.uuid, 1, v_origin);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_audit_log_sync_cursor ON audit_log;
CREATE TRIGGER trg_audit_log_sync_cursor
    AFTER INSERT ON audit_log
    FOR EACH ROW
    EXECUTE FUNCTION audit_log_sync_cursor_trigger();

-- Backfill: registrar las entradas históricas en sync_cursor para que
-- el desktop las pull-ee también. Usamos origen='backfill' para que
-- ningún device se las salte por self-exclude. sync_cursor no tiene
-- UNIQUE, así que duplicar es seguro pero no útil — solo corremos el
-- backfill si la tabla audit_log tiene filas y ninguna está aún en
-- sync_cursor (idempotencia ante re-corrida de migración).
INSERT INTO sync_cursor (tabla, uuid, sucursal_id, origen_device)
SELECT 'audit_log', a.uuid, 1, 'backfill'
  FROM audit_log a
 WHERE a.uuid IS NOT NULL
   AND NOT EXISTS (
       SELECT 1 FROM sync_cursor c
        WHERE c.tabla = 'audit_log' AND c.uuid = a.uuid
   );
