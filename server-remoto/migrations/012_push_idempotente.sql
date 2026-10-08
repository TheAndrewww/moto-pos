-- 012_push_idempotente.sql — Un mismo envío del escritorio no se aplica dos veces.
--
-- Si la respuesta de un push se pierde (se cae el internet, se vence el tiempo
-- de espera, se cierra el POS), el escritorio vuelve a mandar exactamente el
-- mismo cambio. En productos eso no es inofensivo: la fusión de 3 vías aplica
-- la existencia como DIFERENCIA (stock_guardado + (entrante − base)), así que
-- aplicarla otra vez descontaría la misma venta dos veces.
--
-- huella = md5 del cambio entrante + su base. Si ya se aplicó, el reenvío se
-- acepta sin escribir nada.
CREATE TABLE IF NOT EXISTS sync_push_aplicado (
    device_uuid TEXT        NOT NULL,
    tabla       TEXT        NOT NULL,
    uuid        TEXT        NOT NULL,
    huella      TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (device_uuid, tabla, uuid, huella)
);
CREATE INDEX IF NOT EXISTS idx_sync_push_aplicado_fecha ON sync_push_aplicado (created_at);
