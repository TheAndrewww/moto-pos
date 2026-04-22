// pages/Etiquetas.tsx — Generador de etiquetas de precio

import { useState, useEffect, useRef } from 'react';
import { useProductStore, type Producto } from '../store/productStore';
import { Tag, Search, Printer, X, Trash2 } from 'lucide-react';
import { printHTML, escapeHTML } from '../utils/print';

export default function Etiquetas() {
  const { productos, cargarTodo } = useProductStore();
  const [seleccionados, setSeleccionados] = useState<{ producto: Producto; cantidad: number }[]>([]);
  const [busqueda, setBusqueda] = useState('');
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

  const handlePrint = async () => {
    if (etiquetas.length === 0) return;
    const html = `<!DOCTYPE html><html><head><meta charset="utf-8"><title>Etiquetas</title>
      <style>
        @page { margin: 2mm; }
        html, body { margin: 0; padding: 0; font-family: Arial, sans-serif; }
        .etiqueta {
          width: 50mm; height: 25mm; border: 0.3mm dashed #ccc;
          padding: 2mm; display: inline-flex; flex-direction: column;
          justify-content: center; align-items: center; text-align: center;
          page-break-inside: avoid; box-sizing: border-box;
        }
        .nombre { font-size: 8pt; font-weight: 700; margin-bottom: 1mm; line-height: 1.2; }
        .precio { font-size: 14pt; font-weight: 900; }
        .codigo { font-size: 7pt; color: #666; font-family: monospace; }
      </style></head><body>
      ${etiquetas.map(p => `
        <div class="etiqueta">
          <div class="nombre">${escapeHTML(p.nombre)}</div>
          <div class="precio">${fmt(p.precio_venta)}</div>
          <div class="codigo">${escapeHTML(p.codigo)}</div>
        </div>
      `).join('')}
      </body></html>`;
    await printHTML(html);
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      {/* Header */}
      <div style={{
        padding: '12px 20px', borderBottom: '1px solid var(--color-border)',
        background: 'var(--color-surface)', display: 'flex', flexDirection: 'column', gap: 10,
      }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
            <Tag size={20} style={{ color: 'var(--color-primary)' }} />
            <h2 style={{ fontSize: 17, fontWeight: 800, color: 'var(--color-text)' }}>Etiquetas de Precio</h2>
            <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>·</span>
            <span style={{ fontSize: 12, color: 'var(--color-text-muted)' }}>{etiquetas.length} etiquetas</span>
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
      <div style={{ flex: 1, overflow: 'auto', display: 'grid', gridTemplateColumns: '300px 1fr', height: '100%' }}>
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
                  <div style={{ flex: 1 }}>
                    <div style={{ fontSize: 13, fontWeight: 600, lineHeight: 1.2 }}>{s.producto.nombre}</div>
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

        {/* Preview */}
        <div style={{ overflow: 'auto', padding: 20, background: 'var(--color-bg)' }} ref={previewRef}>
          {etiquetas.length === 0 ? (
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', flexDirection: 'column', gap: 12, color: 'var(--color-text-dim)' }}>
              <Tag size={48} strokeWidth={1.2} />
              <p style={{ fontSize: 16, fontWeight: 600 }}>Vista previa de etiquetas</p>
              <p style={{ fontSize: 13 }}>Selecciona productos en el buscador</p>
            </div>
          ) : (
            <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
              {etiquetas.map((p, idx) => (
                <div key={idx} style={{
                  width: 190, height: 95,
                  border: '1px dashed var(--color-border)',
                  borderRadius: 6, padding: '8px 10px',
                  display: 'flex', flexDirection: 'column',
                  justifyContent: 'center', alignItems: 'center',
                  textAlign: 'center', background: '#fff',
                }}>
                  <div style={{
                    fontSize: 11, fontWeight: 700, color: '#333',
                    lineHeight: 1.2, marginBottom: 4,
                    overflow: 'hidden', display: '-webkit-box',
                    WebkitLineClamp: 2, WebkitBoxOrient: 'vertical',
                  }}>{p.nombre}</div>
                  <div className="mono" style={{ fontSize: 22, fontWeight: 900, color: '#000' }}>
                    {fmt(p.precio_venta)}
                  </div>
                  <div className="mono" style={{ fontSize: 9, color: '#888', marginTop: 2 }}>
                    {p.codigo}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
