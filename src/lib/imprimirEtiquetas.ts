// lib/imprimirEtiquetas.ts — Generador compartido de impresión de etiquetas.
//
// Lo usan pages/Etiquetas.tsx (botón Imprimir + vista previa) y
// pages/Catalogo.tsx (modal "¿imprimir etiquetas?" tras crear producto).
// El layout (nombre arriba, QR en medio, código abajo) y el QR salen de aquí
// para las dos cosas: un cambio aquí afecta a la impresión y a la vista previa.
//
// POR QUÉ EL QR ES <svg> EN LÍNEA Y NO <img src="data:...">
// printHTMLDialogOverlay inserta el HTML y llama window.print() en el mismo
// task. Un <img> recién insertado carga su imagen de forma asíncrona (aunque
// sea un data: URL), y la impresión por script de Chromium / WebView2 (la PC
// de la tienda, Windows) toma la "foto" de la página en ese momento sin
// esperar cargas → la etiqueta salía con nombre y código pero con el cuadro
// del QR en blanco. Antes de v0.1.36 no se notaba porque la impresión usaba
// el MISMO data: URL que la vista previa ya había cargado (caché de memoria);
// en v0.1.36 la impresión pasó a width 256 y la vista previa siguió en 160,
// así que el URL dejó de coincidir y el QR salió en blanco siempre (y el
// modal del Catálogo, que no tiene vista previa, nunca funcionó).
// En macOS no se reproduce: Tauri reemplaza window.print() por un invoke
// asíncrono que imprime después, cuando la imagen ya cargó.
// Un <svg> en línea se pinta en el mismo layout, no depende de cargas, y
// además es vectorial: la térmica lo rasteriza nítido a 203/300 dpi en vez de
// reescalar un PNG con bordes grises.
// Por lo mismo: NO poner `await` antes de printHTMLDialogOverlay y usar solo
// fuentes del sistema (Arial, Courier New); una fuente web aún no cargada
// tendría el mismo problema.

import QRCode from 'qrcode';
import { printHTMLDialogOverlay, escapeHTML } from '../utils/print';
import type { Producto } from '../store/productStore';

// Defaults del rollo que usa el negocio (35x24 mm). El usuario puede
// cambiarlos en la página de Etiquetas vía sus inputs, pero para flujos
// rápidos (modal "imprimir tras crear producto") usamos estos sin
// preguntar.
export const ETIQUETA_ANCHO_DEFAULT_MM = 35;
export const ETIQUETA_ALTO_DEFAULT_MM = 24;

const PT_A_MM = 25.4 / 72;
// Zona de silencio (módulos blancos alrededor del QR). La norma pide 4; con
// 2 los lectores 2D de mano leen sin problema y cabe en 35x24 con un QR de
// ~0.56 mm por módulo (≈4.5 puntos a 203 dpi). Antes era 0 y el QR quedaba a
// 0.36 mm del texto.
const QR_MARGEN_MODULOS = 2;

const clamp = (v: number, min: number, max: number) => Math.min(max, Math.max(min, v));
// Redondeo a centésimas: evita que el CSS lleve valores como 1.4000000000000001mm.
const r2 = (v: number) => Math.round(v * 100) / 100;

export interface LayoutEtiqueta {
  padXMm: number;
  padYMm: number;
  gapMm: number;
  nombrePt: number;
  codigoPt: number;
  qrMm: number;
}

/// Tamaños de la etiqueta vertical (nombre 2 líneas / QR / código).
/// El QR toma TODO el alto que dejan los textos (antes era un % fijo).
/// 35x24 → QR 14.0 mm, nombre 6.48 pt, código 6 pt.
export function calcularLayoutEtiqueta(anchoMm: number, altoMm: number): LayoutEtiqueta {
  const padYMm = r2(clamp(altoMm * 0.04, 0.8, 1.5));
  const padXMm = r2(clamp(anchoMm * 0.04, 1.0, 2.0));
  const gapMm = 0.4;
  const nombrePt = r2(clamp(altoMm * 0.27, 6, 10));
  const codigoPt = r2(clamp(altoMm * 0.25, 5.5, 9));
  const nombreAltoMm = 2 * nombrePt * 1.05 * PT_A_MM; // 2 líneas, line-height 1.05
  const codigoAltoMm = codigoPt * 1.15 * PT_A_MM;     // 1 línea, line-height 1.15
  const altoLibre = altoMm - 2 * padYMm - nombreAltoMm - codigoAltoMm - 2 * gapMm;
  const anchoLibre = anchoMm - 2 * padXMm;
  const qrMm = Math.floor(clamp(Math.min(altoLibre, anchoLibre), 8, 30) * 10) / 10;
  return { padXMm, padYMm, gapMm, nombrePt, codigoPt, qrMm };
}

/// QR como <svg> en línea, síncrono y vectorial. Mismo contenido que antes
/// (QRCode con corrección M, sin ECI): una etiqueta vieja que sí salió con QR
/// escanea igual. El SVG no lleva ids ni texto del usuario, así que se puede
/// repetir en el DOM y meter con innerHTML sin escapar nada.
/// Si el texto no se puede codificar (vacío), devuelve un recuadro vacío para
/// no abortar el lote.
export function qrSvg(texto: string): string {
  try {
    const { modules } = QRCode.create(texto, { errorCorrectionLevel: 'M' });
    const n = modules.size;
    const m = QR_MARGEN_MODULOS;
    const total = n + 2 * m;
    // Un rectángulo por cada racha horizontal de módulos negros.
    let d = '';
    for (let y = 0; y < n; y++) {
      for (let x = 0; x < n; x++) {
        if (!modules.get(y, x)) continue;
        let w = 1;
        while (x + w < n && modules.get(y, x + w)) w++;
        d += `M${x + m} ${y + m}h${w}v1h-${w}z`;
        x += w - 1;
      }
    }
    return `<svg class="qr" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${total} ${total}" shape-rendering="crispEdges" aria-hidden="true"><rect width="${total}" height="${total}" fill="#fff"/><path fill="#000" d="${d}"/></svg>`;
  } catch {
    return `<svg class="qr" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1" aria-hidden="true"></svg>`;
  }
}

interface ImprimirEtiquetasInput {
  items: { producto: Producto; cantidad: number }[];
  anchoMm?: number;
  altoMm?: number;
}

/// Imprime N etiquetas (expandidas por `cantidad`), una por página del
/// tamaño del rollo. Todo es síncrono hasta window.print() — no hay `await`
/// antes — para que el diálogo se abra en el mismo gesto del usuario
/// (Safari/WebKit) y con el QR ya pintado. Se deja `async` para no cambiar
/// la firma que usan Etiquetas y Catálogo.
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

  // Un SVG por código único (50 etiquetas del mismo producto = 1 QR).
  const qrPorCodigo = new Map<string, string>();
  for (const p of etiquetas) {
    if (!qrPorCodigo.has(p.codigo)) qrPorCodigo.set(p.codigo, qrSvg(p.codigo));
  }

  const L = calcularLayoutEtiqueta(anchoMm, altoMm);

  // Reglas de la etiqueta acotadas a #print-dialog-overlay: el <style> vive
  // en el DOM mientras exista el overlay y no debe tocar el resto de la app.
  const html = `
    <style>
      @page { size: ${anchoMm}mm ${altoMm}mm; margin: 0; }
      @media print {
        html, body { margin: 0 !important; padding: 0 !important; background: #fff !important; }
      }
      #print-dialog-overlay .etq-contenedor { font-family: Arial, Helvetica, sans-serif; }
      #print-dialog-overlay .etq {
        width: ${anchoMm}mm;
        height: ${altoMm}mm;
        padding: ${L.padYMm}mm ${L.padXMm}mm;
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: space-between;
        gap: ${L.gapMm}mm;
        break-after: page;
        page-break-after: always;
        break-inside: avoid;
        page-break-inside: avoid;
        box-sizing: border-box;
        overflow: hidden;
        color: #000;
        background: #fff;
        text-align: center;
        -webkit-print-color-adjust: exact;
        print-color-adjust: exact;
      }
      #print-dialog-overlay .etq:last-child { break-after: auto; page-break-after: auto; }
      #print-dialog-overlay .etq-nombre {
        width: 100%;
        flex: 0 1 auto;
        min-height: 0;
        font-size: ${L.nombrePt}pt;
        font-weight: 700;
        line-height: 1.05;
        overflow: hidden;
        display: -webkit-box;
        -webkit-line-clamp: 2;
        -webkit-box-orient: vertical;
        word-break: break-word;
      }
      #print-dialog-overlay .qr {
        width: ${L.qrMm}mm;
        height: ${L.qrMm}mm;
        flex: 0 0 auto;
        display: block;
        max-width: none;
      }
      #print-dialog-overlay .etq-codigo {
        width: 100%;
        flex: 0 0 auto;
        font-size: ${L.codigoPt}pt;
        line-height: 1.15;
        font-family: 'Courier New', Consolas, monospace;
        font-weight: 700;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
      }
    </style>
    <div class="etq-contenedor">
    ${etiquetas.map(p => `
      <div class="etq">
        <div class="etq-nombre">${escapeHTML(p.nombre)}</div>
        ${qrPorCodigo.get(p.codigo)}
        <div class="etq-codigo">${escapeHTML(p.codigo)}</div>
      </div>
    `).join('')}
    </div>`;

  printHTMLDialogOverlay(html);
}
