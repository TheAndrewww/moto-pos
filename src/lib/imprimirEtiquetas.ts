// lib/imprimirEtiquetas.ts — Generador compartido de impresión de etiquetas.
//
// Antes esta lógica vivía solo dentro de pages/Etiquetas.tsx. Cuando el
// usuario pidió "tras crear un producto, imprimir N etiquetas de una vez",
// fue obvio que necesitábamos reusar el mismo motor de render desde otras
// pantallas (Catálogo) sin duplicar los cálculos de tamaño/QR.
//
// El layout es idéntico a Etiquetas.tsx (nombre arriba, QR centrado,
// código abajo) — un cambio aquí afecta a las dos pantallas.

import QRCode from 'qrcode';
import { printHTMLDialogOverlay, escapeHTML } from '../utils/print';
import type { Producto } from '../store/productStore';

// Defaults del rollo que usa el negocio (35x24 mm). El usuario puede
// cambiarlos en la página de Etiquetas vía sus inputs, pero para flujos
// rápidos (modal "imprimir tras crear producto") usamos estos sin
// preguntar.
export const ETIQUETA_ANCHO_DEFAULT_MM = 35;
export const ETIQUETA_ALTO_DEFAULT_MM = 24;

const QR_OPTS: QRCode.QRCodeToDataURLOptions = {
  errorCorrectionLevel: 'M',
  margin: 0,
  width: 256,
};

interface ImprimirEtiquetasInput {
  items: { producto: Producto; cantidad: number }[];
  anchoMm?: number;
  altoMm?: number;
}

/// Imprime N etiquetas (expandidas por `cantidad`) llamando al diálogo
/// de impresión del navegador con el HTML formateado al tamaño del rollo.
/// Devuelve sin esperar a que el usuario imprima — el diálogo es modal
/// del navegador. Resuelve cuando se mostró el diálogo.
export async function imprimirEtiquetas({
  items,
  anchoMm = ETIQUETA_ANCHO_DEFAULT_MM,
  altoMm = ETIQUETA_ALTO_DEFAULT_MM,
}: ImprimirEtiquetasInput): Promise<void> {
  // Expandir cada item por su cantidad → array plano de productos a
  // renderizar.
  const etiquetas = items.flatMap(s =>
    Array.from({ length: Math.max(0, Math.floor(s.cantidad)) }, () => s.producto)
  );
  if (etiquetas.length === 0) return;

  // Pre-generar QR para cada código único (no por etiqueta — si imprimes
  // 50 etiquetas del mismo producto, generas el QR una vez).
  const codigosUnicos = Array.from(new Set(items.map(s => s.producto.codigo)));
  const qrMap: Record<string, string> = {};
  await Promise.all(
    codigosUnicos.map(async c => {
      qrMap[c] = await QRCode.toDataURL(c, QR_OPTS);
    })
  );

  // Tamaños proporcionales al label (mismos que Etiquetas.tsx).
  const padMm = Math.max(0.5, Math.min(1.2, Math.min(anchoMm, altoMm) * 0.03));
  const qrMm = Math.max(
    10,
    Math.min(
      Math.floor(altoMm * 0.55),
      Math.floor(anchoMm * 0.75),
      22,
    ),
  );
  const nombrePt = Math.max(6, Math.min(11, altoMm * 0.22));
  const codigoPt = Math.max(5, Math.min(9, altoMm * 0.20));

  const html = `
    <style>
      @page {
        size: ${anchoMm}mm ${altoMm}mm;
        margin: 0;
      }
      html, body { margin: 0 !important; padding: 0 !important; background: #fff; }
      .etiqueta-print-container { font-family: Arial, sans-serif; }
      .etiqueta {
        width: ${anchoMm}mm;
        height: ${altoMm}mm;
        padding: ${padMm}mm;
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: space-between;
        gap: ${padMm * 0.5}mm;
        page-break-after: always;
        page-break-inside: avoid;
        break-after: page;
        break-inside: avoid;
        box-sizing: border-box;
        overflow: hidden;
        color: #000;
        text-align: center;
      }
      .etiqueta:last-child { page-break-after: auto; break-after: auto; }
      .nombre {
        width: 100%;
        font-size: ${nombrePt}pt; font-weight: 800;
        line-height: 1.05;
        overflow: hidden;
        display: -webkit-box;
        -webkit-line-clamp: 2; -webkit-box-orient: vertical;
        word-break: break-word;
      }
      .qr {
        width: ${qrMm}mm; height: ${qrMm}mm;
        flex-shrink: 0; display: block;
      }
      .codigo {
        width: 100%;
        font-size: ${codigoPt}pt; color: #000; font-family: monospace;
        font-weight: 700;
        white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
      }
    </style>
    <div class="etiqueta-print-container">
    ${etiquetas.map(p => `
      <div class="etiqueta">
        <div class="nombre">${escapeHTML(p.nombre)}</div>
        <img class="qr" src="${qrMap[p.codigo]}" alt="${escapeHTML(p.codigo)}" />
        <div class="codigo">${escapeHTML(p.codigo)}</div>
      </div>
    `).join('')}
    </div>`;

  printHTMLDialogOverlay(html);
}
