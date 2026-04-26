// utils/ticket.ts — Impresión de ticket (térmica ESC/POS o HTML fallback)

import { printHTML, escapeHTML } from './print';
import { invoke } from '../lib/invokeCompat';

export interface ConfigNegocio {
  nombre: string;
  direccion: string;
  telefono: string;
  rfc: string;
  mensaje_pie: string;
  /** Nombre de la impresora térmica ESC/POS en el sistema.
   *  Si está vacía → fallback a impresión HTML (navegador). */
  impresora_termica?: string;
}

export interface TicketItem {
  nombre: string;
  codigo: string;
  cantidad: number;
  precio_final: number;
  subtotal: number;
  descuento_porcentaje?: number;
}

export interface TicketData {
  folio: string;
  fecha: string;
  usuario: string;
  cliente?: string | null;
  items: TicketItem[];
  subtotal: number;
  descuento: number;
  /** Monto agregado por redondeo al peso siguiente (>= 0). Opcional. */
  redondeo?: number;
  total: number;
  metodo_pago: string;
  monto_recibido?: number;
  cambio?: number;
  reimpresion?: boolean;
  es_presupuesto?: boolean;
}

const fmt = (n: number) => `$${n.toFixed(2)}`;

export function buildTicketHTML(negocio: ConfigNegocio, t: TicketData): string {
  const metodo = t.metodo_pago.charAt(0).toUpperCase() + t.metodo_pago.slice(1);
  const mostrarEfectivo = t.metodo_pago === 'efectivo' && t.monto_recibido !== undefined;

  const itemsHTML = t.items.map(i => `
    <div class="item">
      <div class="item-nom">${escapeHTML(i.nombre)}</div>
      <div class="item-row">
        <span class="item-qty">${i.cantidad} x ${fmt(i.precio_final)}${i.descuento_porcentaje ? ` (-${i.descuento_porcentaje}%)` : ''}</span>
        <span class="item-sub">${fmt(i.subtotal)}</span>
      </div>
    </div>
  `).join('');

  return `<!DOCTYPE html><html><head><meta charset="utf-8"><title>${t.es_presupuesto ? 'Presupuesto' : 'Ticket'} ${escapeHTML(t.folio)}</title>
<style>
  @page { size: 58mm auto; margin: 0; }
  html, body { margin: 0; padding: 0; font-family: 'Courier New', monospace; color: #000; }
  body { width: 50mm; padding: 2mm; font-size: 9pt; line-height: 1.25; }
  .center { text-align: center; }
  .right { text-align: right; }
  .bold { font-weight: 700; }
  .big { font-size: 12pt; }
  .xl { font-size: 14pt; }
  .muted { font-size: 8pt; color: #333; }
  .sep { border-top: 1px dashed #000; margin: 5px 0; }
  .sep-thick { border-top: 2px solid #000; margin: 6px 0; }
  .row { display: flex; justify-content: space-between; gap: 4px; }
  .item { margin-bottom: 3px; }
  .item-nom { font-size: 9pt; }
  .item-row { display: flex; justify-content: space-between; font-size: 9pt; }
  .item-sub { font-weight: 700; }
  .total-row { font-size: 13pt; font-weight: 900; color: #000; }
  .reprint { border: 1px solid #000; padding: 2px 6px; display: inline-block; font-size: 8pt; margin-bottom: 3px; }
  .logo { width: 40mm; max-width: 80%; height: auto; margin: 3px auto; display: block; }
  .brand-name { font-size: 11pt; font-weight: 900; letter-spacing: 0.5px; margin: 2px 0; color: #000; }
  .brand-sub { font-size: 8pt; color: #000; font-weight: 700; letter-spacing: 1px; margin-bottom: 2px; }
  .footer-msg { font-size: 9pt; font-weight: 600; margin: 4px 0 2px; }
  .footer-brand { font-size: 7pt; color: #000; font-weight: 700; letter-spacing: 0.5px; margin-top: 4px; }
</style></head><body>
  ${t.es_presupuesto ? '<div class="center"><span class="reprint">*** PRESUPUESTO ***</span><br><span style="font-size: 7pt; font-weight: bold; padding-top: 2px; display: inline-block;">Esta cotización puede variar sin previo aviso.</span></div>' : (t.reimpresion ? '<div class="center"><span class="reprint">*** REIMPRESIÓN ***</span></div>' : '')}
  <div class="center">
    <img class="logo" src="${window.location.origin}/logo-ticket.png" alt="Moto Refaccionaria LB" onerror="this.style.display='none'" />
    <div class="brand-name">${escapeHTML(negocio.nombre || 'MOTO REFACCIONARIA LB')}</div>
    <div class="brand-sub">ABASOLO, GUANAJUATO</div>
  </div>
  ${negocio.direccion ? `<div class="center muted">${escapeHTML(negocio.direccion)}</div>` : ''}
  ${negocio.telefono ? `<div class="center muted">Tel: ${escapeHTML(negocio.telefono)}</div>` : ''}
  ${negocio.rfc ? `<div class="center muted">RFC: ${escapeHTML(negocio.rfc)}</div>` : ''}
  <div class="sep-thick"></div>
  <div class="row"><span>Folio:</span><span class="bold">${escapeHTML(t.folio)}</span></div>
  <div class="row"><span>Fecha:</span><span>${escapeHTML(t.fecha)}</span></div>
  <div class="row"><span>Cajero:</span><span>${escapeHTML(t.usuario.split(' ')[0])}</span></div>
  ${t.cliente ? `<div class="row"><span>Cliente:</span><span>${escapeHTML(t.cliente)}</span></div>` : ''}
  <div class="sep"></div>
  ${itemsHTML}
  <div class="sep"></div>
  ${(t.descuento > 0 || (t.redondeo ?? 0) > 0) ? `
    <div class="row"><span>Subtotal:</span><span>${fmt(t.subtotal)}</span></div>
    ${t.descuento > 0 ? `<div class="row"><span>Descuento:</span><span>-${fmt(t.descuento)}</span></div>` : ''}
    ${(t.redondeo ?? 0) > 0 ? `<div class="row"><span>Redondeo:</span><span>+${fmt(t.redondeo!)}</span></div>` : ''}
  ` : ''}
  <div class="row total-row"><span>TOTAL:</span><span>${fmt(t.total)}</span></div>
  <div class="sep"></div>
  ${!t.es_presupuesto ? `
    <div class="row"><span>Pago:</span><span class="bold">${escapeHTML(metodo)}</span></div>
    ${mostrarEfectivo ? `
      <div class="row"><span>Recibido:</span><span>${fmt(t.monto_recibido!)}</span></div>
      <div class="row bold"><span>Cambio:</span><span>${fmt(t.cambio!)}</span></div>
    ` : ''}
    <div class="sep"></div>
  ` : ''}
  <div class="center footer-msg">${escapeHTML(negocio.mensaje_pie || '¡Gracias por su compra!')}</div>
  <div class="center muted">Conserve este ticket</div>
  <div class="center footer-brand">MOTO REFACCIONARIA LB</div>
</body></html>`;
}

export async function imprimirTicket(negocio: ConfigNegocio, data: TicketData): Promise<void> {
  // 1) Si hay impresora térmica configurada, intentar ESC/POS directo (sin ventana).
  const impresora = (negocio.impresora_termica || '').trim();
  if (impresora) {
    try {
      await invoke('imprimir_ticket_termico', {
        datos: {
          negocio_nombre: negocio.nombre,
          negocio_direccion: negocio.direccion,
          negocio_telefono: negocio.telefono,
          negocio_rfc: negocio.rfc,
          mensaje_pie: negocio.mensaje_pie,
          folio: data.folio,
          fecha: data.fecha,
          usuario: data.usuario.split(' ')[0],
          cliente: data.cliente ?? null,
          items: data.items.map(i => ({
            cantidad: i.cantidad,
            nombre: i.nombre,
            precio_unitario: i.precio_final,
            subtotal: i.subtotal,
          })),
          subtotal: data.subtotal,
          descuento: data.descuento,
          redondeo: data.redondeo ?? 0,
          total: data.total,
          metodo_pago: data.metodo_pago,
        },
        impresora,
      });
      return; // Éxito — no abrimos nada más.
    } catch (e) {
      console.warn('Impresión térmica falló, usando fallback HTML:', e);
      // Continúa al fallback de abajo.
    }
  }
  // 2) Fallback: HTML en ventana del navegador (comportamiento legacy).
  await printHTML(buildTicketHTML(negocio, data));
}
