// pages/Etiquetas.tsx — Generador de etiquetas de precio
//
// Imprime una etiqueta por hoja en una etiquetadora térmica (rollo continuo).
// Tamaño por defecto: 39 × 30 mm. Configurable desde la UI.

import { useState, useEffect, useRef, useMemo } from 'react';
import { useProductStore, type Producto } from '../store/productStore';
import { Tag, Search, Printer, X, Trash2 } from 'lucide-react';
import { printHTMLDialogOverlay, escapeHTML } from '../utils/print';
import QRCode from 'qrcode';

const QR_OPTS: QRCode.QRCodeToDataURLOptions = {
  errorCorrectionLevel: 'M',
  margin: 0,
  width: 160,
  color: { dark: '#000000', light: '#ffffff' },
};

// Defaults para etiquetadora térmica de rollo continuo
const ANCHO_DEFAULT_MM = 39;
const ALTO_DEFAULT_MM = 30;

function useQrCache(codigos: string[]) {
  const [cache, setCache] = useState<Record<string, string>>({});
  const key = useMemo(() => codigos.join('|'), [codigos]);
  useEffect(() => {
    let cancelado = false;
    const faltantes = codigos.filter(c => !cache[c]);
    if (faltantes.length === 0) return;
    Promise.all(faltantes.map(async c => [c, await QRCode.toDataURL(c, QR_OPTS)] as const))
      .then(pares => {
        if (cancelado) return;
        setCache(prev => {
          const next = { ...prev };
          for (const [c, url] of pares) next[c] = url;
          return next;
        });
      });
    return () => { cancelado = true; };
  }, [key]);
  return cache;
}

export default function Etiquetas() {
  const { productos, cargarTodo } = useProductStore();
  const [seleccionados, setSeleccionados] = useState<{ producto: Producto; cantidad: number }[]>([]);
  const [busqueda, setBusqueda] = useState('');
  const [anchoMm, setAnchoMm] = useState<number>(ANCHO_DEFAULT_MM);
  const [altoMm, setAltoMm]   = useState<number>(ALTO_DEFAULT_MM);
  const previewRef = useRef<HTMLDivElement>(null);

  useEffect(() => { cargarTodo(); }, []);

  const filtrados = busqueda.length >= 2
    ? productos.filter(p =>
        p.nombre.toLowerCase().includes(busqueda.toLowerCase()) ||
        p.codigo.toLowerCase().includes(busqueda.toLowerCase())
      ).slice(0, 20)
    : [];

  const agregarProducto = (prod: Producto) => {
    const existente = seleccionados.findIndex(s => s.producto.id === prod.id);
    if (existente >= 0) {
      const n = [...seleccionados]; n[existente].cantidad += 1; setSeleccionados(n);
    } else {
      setSeleccionados([...seleccionados, { producto: prod, cantidad: 1 }]);
    }
    setBusqueda('');
  };

  const fmt = (n: number) => `$${n.toFixed(2)}`;

  // Generar todas las etiquetas expandidas por cantidad
  const etiquetas = seleccionados.flatMap(s =>
    Array.from({ length: s.cantidad }, () => s.producto)
  );

  const codigosUnicos = useMemo(
    () => Array.from(new Set(seleccionados.map(s => s.producto.codigo))),
    [seleccionados],
  );
  const qrCache = useQrCache(codigosUnicos);

  // ─── Tamaños proporcionales al label (layout vertical) ───
  // Layout: nombre arriba, QR en medio, código abajo.
  // QR limitado por la dimensión más pequeña; debe dejar espacio para texto arriba/abajo.
  const padMm = Math.max(0.5, Math.min(1.2, Math.min(anchoMm, altoMm) * 0.03));
  const qrMm = Math.max(
    10,
    Math.min(
      Math.floor(altoMm * 0.55),  // máx 55% del alto
      Math.floor(anchoMm * 0.75), // máx 75% del ancho
      22,                          // tope absoluto
    ),
  );
  // Tamaños de fuente proporcionales al alto. Una sola línea cada uno.
  const nombrePt = Math.max(6, Math.min(11, altoMm * 0.22));
  const codigoPt = Math.max(5, Math.min(9, altoMm * 0.20));

  const handlePrint = async () => {
    if (etiquetas.length === 0) return;
    const qrMap: Record<string, string> = {};
    await Promise.all(
      codigosUnicos.map(async c => { qrMap[c] = await QRCode.toDataURL(c, QR_OPTS); })
    );

    const html = `
      <style>
        /* Cada etiqueta es una página independiente del tamaño exacto del rollo */
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
  };

  // Ajustar entrada numérica con clamp seguro (evita 0 o NaN al borrar)
  const sanitizeMm = (v: string, fallback: number) => {
    const n = parseFloat(v);
    if (!Number.isFinite(n) || n <= 0) return fallback;
    return Math.min(200, Math.max(10, n));
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      {/* Header */}
      <div style={{
        padding: '12px 20px', borderBottom: '1px solid var(--color-border)',
        background: 'var(--color-surface)', display: 'flex', flexDirection: 'column', gap: 10,
      }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', flexWrap: 'wrap', gap: 10 }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
            <Tag size={20} style={{ color: 'var(--color-primary)' }} />
            <h2 style={{ fontSize: 17, fontWeight: 800, color: 'var(--color-text)' }}>Etiquetas de Precio</h2>
            <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>·</span>
            <span style={{ fontSize: 12, color: 'var(--color-text-muted)' }}>{etiquetas.length} etiquetas</span>
          </div>

          <div style={{ display: 'flex', alignItems: 'center', gap: 12, flexWrap: 'wrap' }}>
            {/* Tamaño de etiqueta */}
            <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
              <span style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-muted)', textTransform: 'uppercase' }}>
                Tamaño
              </span>
              <input
                type="number" min={10} max={200} step={1}
                value={anchoMm}
                onChange={e => setAnchoMm(sanitizeMm(e.target.value, ANCHO_DEFAULT_MM))}
                className="input mono"
                style={{ width: 64, padding: '4px 6px', textAlign: 'center', fontSize: 13 }}
                title="Ancho en mm"
              />
              <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>×</span>
              <input
                type="number" min={10} max={200} step={1}
                value={altoMm}
                onChange={e => setAltoMm(sanitizeMm(e.target.value, ALTO_DEFAULT_MM))}
                className="input mono"
                style={{ width: 64, padding: '4px 6px', textAlign: 'center', fontSize: 13 }}
                title="Alto en mm"
              />
              <span style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>mm</span>
            </div>

            <div style={{ display: 'flex', gap: 8 }}>
              {seleccionados.length > 0 && (
                <button className="btn btn-ghost" onClick={() => setSeleccionados([])}>
                  <Trash2 size={14} /> Limpiar
                </button>
              )}
              <button className="btn btn-primary" disabled={etiquetas.length === 0} onClick={handlePrint}>
                <Printer size={16} /> Imprimir
              </button>
            </div>
          </div>
        </div>

        <div style={{ position: 'relative' }}>
          <Search size={16} style={{ position: 'absolute', left: 12, top: '50%', transform: 'translateY(-50%)', color: 'var(--color-text-dim)' }} />
          <input className="input" placeholder="Buscar producto para generar etiqueta..."
            value={busqueda} onChange={e => setBusqueda(e.target.value)}
            style={{ paddingLeft: 36, width: '100%' }} />
          {filtrados.length > 0 && (
            <div className="card" style={{ position: 'absolute', top: '100%', left: 0, right: 0, zIndex: 10, maxHeight: 200, overflow: 'auto', padding: 0, marginTop: 4 }}>
              {filtrados.map(p => (
                <button key={p.id} style={{
                  display: 'flex', justifyContent: 'space-between', width: '100%',
                  padding: '8px 14px', border: 'none', background: 'transparent',
                  color: 'var(--color-text)', cursor: 'pointer', borderBottom: '1px solid var(--color-border)', textAlign: 'left', fontSize: 13,
                }}
                  onMouseEnter={e => (e.currentTarget.style.background = 'var(--color-surface-2)')}
                  onMouseLeave={e => (e.currentTarget.style.background = 'transparent')}
                  onClick={() => agregarProducto(p)}
                >
                  <span><span className="mono" style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>{p.codigo}</span> {p.nombre}</span>
                  <span className="mono" style={{ color: 'var(--color-primary)', fontWeight: 700 }}>{fmt(p.precio_venta)}</span>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Content: lista de productos + preview */}
      <div style={{ flex: 1, minHeight: 0, display: 'grid', gridTemplateColumns: '300px 1fr' }}>
        {/* Lista seleccionados */}
        <div style={{ borderRight: '1px solid var(--color-border)', overflow: 'auto', padding: 12 }}>
          <p style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-muted)', marginBottom: 10, textTransform: 'uppercase' }}>
            Productos seleccionados
          </p>
          {seleccionados.length === 0 ? (
            <div style={{ padding: 20, textAlign: 'center', color: 'var(--color-text-dim)', fontSize: 13 }}>
              Busca y agrega productos
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
              {seleccionados.map((s, idx) => (
                <div key={s.producto.id} style={{
                  display: 'flex', alignItems: 'center', gap: 8,
                  padding: '8px 10px', borderRadius: 8,
                  background: 'var(--color-surface)', border: '1px solid var(--color-border)',
                }}>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontSize: 13, fontWeight: 600, lineHeight: 1.2, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{s.producto.nombre}</div>
                    <div className="mono" style={{ fontSize: 12, color: 'var(--color-primary)', fontWeight: 700 }}>{fmt(s.producto.precio_venta)}</div>
                  </div>
                  <input className="input mono" type="number" min={1} value={s.cantidad}
                    style={{ width: 50, padding: '2px 6px', textAlign: 'center' }}
                    onChange={e => { const n = [...seleccionados]; n[idx] = { ...n[idx], cantidad: Number(e.target.value) || 1 }; setSeleccionados(n); }} />
                  <button className="btn btn-ghost btn-sm" onClick={() => setSeleccionados(seleccionados.filter((_, i) => i !== idx))}>
                    <X size={12} />
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* Preview — a escala real (1mm = 1mm) */}
        <div style={{ overflow: 'auto', padding: 20, background: 'var(--color-bg)' }} ref={previewRef}>
          {etiquetas.length === 0 ? (
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', flexDirection: 'column', gap: 12, color: 'var(--color-text-dim)' }}>
              <Tag size={48} strokeWidth={1.2} />
              <p style={{ fontSize: 16, fontWeight: 600 }}>Vista previa de etiquetas</p>
              <p style={{ fontSize: 13 }}>Selecciona productos en el buscador</p>
              <p style={{ fontSize: 11 }}>Tamaño actual: {anchoMm} × {altoMm} mm</p>
            </div>
          ) : (
            <>
              <p style={{ fontSize: 11, color: 'var(--color-text-dim)', marginBottom: 8 }}>
                Vista previa a escala real ({anchoMm} × {altoMm} mm). Cada etiqueta sale en su propia página.
              </p>
              <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
                {etiquetas.map((p, idx) => (
                  <div key={idx} style={{
                    width: `${anchoMm}mm`,
                    height: `${altoMm}mm`,
                    padding: `${padMm}mm`,
                    border: '1px dashed var(--color-border)',
                    borderRadius: 2,
                    display: 'flex', flexDirection: 'column',
                    alignItems: 'center', justifyContent: 'space-between',
                    gap: `${padMm * 0.5}mm`,
                    background: '#fff', color: '#000',
                    boxSizing: 'border-box', overflow: 'hidden',
                    fontFamily: 'Arial, sans-serif', textAlign: 'center',
                  }}>
                    <div style={{
                      width: '100%',
                      fontSize: `${nombrePt}pt`, fontWeight: 800,
                      lineHeight: 1.05,
                      overflow: 'hidden', display: '-webkit-box',
                      WebkitLineClamp: 2, WebkitBoxOrient: 'vertical',
                      wordBreak: 'break-word',
                    }}>{p.nombre}</div>
                    {qrCache[p.codigo] ? (
                      <img src={qrCache[p.codigo]} alt={p.codigo}
                        style={{ width: `${qrMm}mm`, height: `${qrMm}mm`, flexShrink: 0, display: 'block' }} />
                    ) : (
                      <div style={{ width: `${qrMm}mm`, height: `${qrMm}mm`, flexShrink: 0, background: '#f0f0f0' }} />
                    )}
                    <div style={{
                      width: '100%',
                      fontSize: `${codigoPt}pt`, color: '#000',
                      fontFamily: 'monospace', fontWeight: 700,
                      whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis',
                    }}>
                      {p.codigo}
                    </div>
                  </div>
                ))}
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
