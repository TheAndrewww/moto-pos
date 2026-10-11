-- 015_corte_web_333.sql — Corrige el corte web #333 (2026-10-11).
--
-- Fue el primer corte de la caja de la tienda hecho desde la web (v0.1.45).
-- Después se eliminó la venta de prueba V-010154 que ese corte había contado:
-- sus totales se vuelven a sacar de las ventas que quedan en su intervalo, lo
-- mismo que hace el escritorio al eliminar una venta de un corte cerrado
-- (nunca toca lo contado ni el fondo). Y se borra su movimiento "Faltante del
-- corte web #333": desde v0.1.46 el corte web baja al escritorio y la
-- diferencia vive en el corte, ya no como movimiento de caja (el escritorio lo
-- borra en su migración 018).
-- En cualquier otra base (copias, pruebas) no encuentra nada y no hace nada.

WITH c AS (
    SELECT id,
           replace(substr(fecha_inicio, 1, 19), 'T', ' ') AS ini,
           LEAST(replace(substr(fecha_fin, 1, 19), 'T', ' '),
                 replace(substr(created_at, 1, 19), 'T', ' ')) AS fin
      FROM cortes
     WHERE uuid = '01a12bc4-72ff-77f3-af1a-3062b1b11983'
       AND origen = 'web' AND caja = 'principal'
       AND total_ventas_efectivo = 2322.00
), v AS (
    SELECT COALESCE(SUM(total) FILTER (WHERE anulada = 0 AND metodo_pago = 'efectivo'), 0)      AS ef,
           COALESCE(SUM(total) FILTER (WHERE anulada = 0 AND metodo_pago = 'tarjeta'), 0)       AS tj,
           COALESCE(SUM(total) FILTER (WHERE anulada = 0 AND metodo_pago = 'transferencia'), 0) AS tr,
           COALESCE(SUM(total) FILTER (WHERE anulada = 0), 0)                                   AS tv,
           COUNT(*) FILTER (WHERE anulada = 0)                                                  AS n,
           COALESCE(SUM(descuento) FILTER (WHERE anulada = 0), 0)                               AS d,
           COALESCE(SUM(total) FILTER (WHERE anulada = 1), 0)                                   AS an
      FROM ventas, c
     WHERE ventas.caja = 'principal' AND ventas.deleted_at IS NULL
       AND replace(substr(COALESCE(ventas.registrado_at, ventas.fecha), 1, 19), 'T', ' ') > c.ini
       AND replace(substr(COALESCE(ventas.registrado_at, ventas.fecha), 1, 19), 'T', ' ') <= c.fin
)
UPDATE cortes k
   SET total_ventas_efectivo      = v.ef,
       total_ventas_tarjeta       = v.tj,
       total_ventas_transferencia = v.tr,
       total_ventas               = v.tv,
       num_transacciones          = v.n,
       total_descuentos           = v.d,
       total_anulaciones          = v.an,
       efectivo_esperado          = k.fondo_inicial + v.ef + k.total_entradas_efectivo - k.total_retiros_efectivo,
       diferencia                 = k.efectivo_contado
                                    - (k.fondo_inicial + v.ef + k.total_entradas_efectivo - k.total_retiros_efectivo),
       updated_at                 = to_char(now() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD HH24:MI:SS')
  FROM c, v
 WHERE k.id = c.id;

-- El movimiento de faltante, con su rastro de sync (como /sync/purgar).
DELETE FROM movimientos_caja
 WHERE uuid = '01a12bc4-7315-77d1-8127-a504dee2d05b' AND origen = 'web'
   AND concepto = 'Faltante del corte web #333';
DELETE FROM sync_cursor        WHERE tabla = 'movimientos_caja' AND uuid      = '01a12bc4-7315-77d1-8127-a504dee2d05b';
DELETE FROM sync_fk_estado     WHERE tabla = 'movimientos_caja' AND uuid      = '01a12bc4-7315-77d1-8127-a504dee2d05b';
DELETE FROM sync_fk_pendiente  WHERE tabla = 'movimientos_caja' AND fila_uuid = '01a12bc4-7315-77d1-8127-a504dee2d05b';
DELETE FROM sync_push_aplicado WHERE tabla = 'movimientos_caja' AND uuid      = '01a12bc4-7315-77d1-8127-a504dee2d05b';
DELETE FROM sync_fk_celda_web  WHERE tabla = 'movimientos_caja' AND uuid      = '01a12bc4-7315-77d1-8127-a504dee2d05b';
DELETE FROM sync_id_map        WHERE tabla = 'movimientos_caja' AND uuid      = '01a12bc4-7315-77d1-8127-a504dee2d05b';
