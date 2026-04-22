// utils/ticket.ts — Plantilla de ticket térmico 80mm

import { printHTML, escapeHTML } from './print';

export interface ConfigNegocio {
  nombre: string;
  direccion: string;
  telefono: string;
  rfc: string;
  mensaje_pie: string;
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
  total: number;
  metodo_pago: string;
  monto_recibido?: number;
  cambio?: number;
  reimpresion?: boolean;
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

  return `<!DOCTYPE html><html><head><meta charset="utf-8"><title>Ticket ${escapeHTML(t.folio)}</title>
<style>
  @page { size: 80mm auto; margin: 0; }
  html, body { margin: 0; padding: 0; font-family: 'Courier New', monospace; color: #000; }
  body { width: 76mm; padding: 2mm; font-size: 10pt; line-height: 1.25; }
  .center { text-align: center; }
  .right { text-align: right; }
  .bold { font-weight: 700; }
  .big { font-size: 12pt; }
  .xl { font-size: 14pt; }
  .muted { font-size: 8pt; }
  .sep { border-top: 1px dashed #000; margin: 4px 0; }
  .row { display: flex; justify-content: space-between; gap: 4px; }
  .item { margin-bottom: 3px; }
  .item-nom { font-size: 9pt; }
  .item-row { display: flex; justify-content: space-between; font-size: 9pt; }
  .item-qty { }
  .item-sub { font-weight: 700; }
  .total-row { font-size: 13pt; font-weight: 900; }
  .reprint { border: 1px solid #000; padding: 2px 6px; display: inline-block; font-size: 8pt; margin-bottom: 3px; }
</style></head><body>
  ${t.reimpresion ? '<div class="center"><span class="reprint">*** REIMPRESIÓN ***</span></div>' : ''}
  <div class="center bold xl">${escapeHTML(negocio.nombre)}</div>
  ${negocio.direccion ? `<div class="center muted">${escapeHTML(negocio.direccion)}</div>` : ''}
  ${negocio.telefono ? `<div class="center muted">Tel: ${escapeHTML(negocio.telefono)}</div>` : ''}
  ${negocio.rfc ? `<div class="center muted">RFC: ${escapeHTML(negocio.rfc)}</div>` : ''}
  <div class="sep"></div>
  <div class="row"><span>Folio:</span><span class="bold">${escapeHTML(t.folio)}</span></div>
  <div class="row"><span>Fecha:</span><span>${escapeHTML(t.fecha)}</span></div>
  <div class="row"><span>Cajero:</span><span>${escapeHTML(t.usuario)}</span></div>
  ${t.cliente ? `<div class="row"><span>Cliente:</span><span>${escapeHTML(t.cliente)}</span></div>` : ''}
  <div class="sep"></div>
  ${itemsHTML}
  <div class="sep"></div>
  ${t.descuento > 0 ? `
    <div class="row"><span>Subtotal:</span><span>${fmt(t.subtotal)}</span></div>
    <div class="row"><span>Descuento:</span><span>-${fmt(t.descuento)}</span></div>
  ` : ''}
  <div class="row total-row"><span>TOTAL:</span><span>${fmt(t.total)}</span></div>
  <div class="sep"></div>
  <div class="row"><span>Pago:</span><span class="bold">${escapeHTML(metodo)}</span></div>
  ${mostrarEfectivo ? `
    <div class="row"><span>Recibido:</span><span>${fmt(t.monto_recibido!)}</span></div>
    <div class="row big bold"><span>Cambio:</span><span>${fmt(t.cambio || 0)}</span></div>
  ` : ''}
  <div class="sep"></div>
  <div class="center">${escapeHTML(negocio.mensaje_pie)}</div>
  <div class="center muted">Conserve este ticket</div>
</body></html>`;
}

export async function imprimirTicket(negocio: ConfigNegocio, data: TicketData): Promise<void> {
  await printHTML(buildTicketHTML(negocio, data));
}
