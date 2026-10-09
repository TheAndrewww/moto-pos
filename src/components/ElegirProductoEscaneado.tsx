// components/ElegirProductoEscaneado.tsx — Cuando lo escaneado contiene más
// de un código del inventario (p.ej. un código de proveedor con descripción
// larga), se pregunta cuál es. El primero de la lista es el que aparece antes
// en lo escaneado y queda preseleccionado: Enter lo agrega, ↑/↓ cambian, Esc
// cancela.

import { useEffect, useState } from 'react';
import { ScanLine } from 'lucide-react';

export interface ProductoElegible {
  id: number;
  codigo: string;
  nombre: string;
  stock_actual: number;
  precio_venta?: number;
}

const fmt = (n: number) => '$' + n.toLocaleString('es-MX', { minimumFractionDigits: 2, maximumFractionDigits: 2 });

export default function ElegirProductoEscaneado<P extends ProductoElegible>({
  escaneado, candidatos, onElegir, onCancelar,
}: {
  escaneado: string;
  candidatos: P[];
  onElegir: (p: P) => void;
  onCancelar: () => void;
}) {
  const [sel, setSel] = useState(0);

  useEffect(() => {
    // En captura y detenido: los atajos de la pantalla (Esc, F10, Enter) no
    // deben dispararse mientras se elige.
    const h = (e: KeyboardEvent) => {
      if (e.key === 'ArrowDown') setSel(s => Math.min(s + 1, candidatos.length - 1));
      else if (e.key === 'ArrowUp') setSel(s => Math.max(s - 1, 0));
      else if (e.key === 'Enter') onElegir(candidatos[sel]);
      else if (e.key === 'Escape') onCancelar();
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener('keydown', h, true);
    return () => window.removeEventListener('keydown', h, true);
  }, [candidatos, sel, onElegir, onCancelar]);

  return (
    <div
      className="pos-modal-overlay"
      onClick={onCancelar}
      style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)', zIndex: 1000,
        display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 16,
      }}
    >
      <div
        className="card pos-modal-content"
        onClick={e => e.stopPropagation()}
        style={{ width: '100%', maxWidth: 560, padding: 0, overflow: 'hidden' }}
      >
        <div style={{ padding: '14px 18px', borderBottom: '1px solid var(--color-border)' }}>
          <div style={{ fontWeight: 700, fontSize: 15, display: 'flex', alignItems: 'center', gap: 8 }}>
            <ScanLine size={16} /> ¿Qué producto es?
          </div>
          <div style={{ fontSize: 12, color: 'var(--color-text-dim)', marginTop: 4 }}>
            El código escaneado contiene varios códigos de tu inventario.
          </div>
          <div className="mono" style={{
            fontSize: 11, color: 'var(--color-text-dim)', marginTop: 6,
            whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis',
          }} title={escaneado}>
            {escaneado}
          </div>
        </div>
        <div style={{ maxHeight: '50vh', overflowY: 'auto' }}>
          {candidatos.map((p, i) => (
            <button
              key={p.id}
              onClick={() => onElegir(p)}
              onMouseEnter={() => setSel(i)}
              style={{
                display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12,
                width: '100%', padding: '10px 18px', border: 'none', textAlign: 'left', cursor: 'pointer',
                borderBottom: '1px solid var(--color-border)',
                background: i === sel ? 'var(--color-surface-2)' : 'transparent',
                color: 'var(--color-text)',
              }}
            >
              <div style={{ minWidth: 0 }}>
                <div className="mono" style={{ fontWeight: 700, fontSize: 14 }}>{p.codigo}</div>
                <div style={{ fontSize: 12, color: 'var(--color-text-dim)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {p.nombre} · Stock: {p.stock_actual}
                </div>
              </div>
              {p.precio_venta !== undefined && (
                <div className="mono" style={{ fontWeight: 700, color: 'var(--color-primary)', fontSize: 13, flexShrink: 0 }}>
                  {fmt(p.precio_venta)}
                </div>
              )}
            </button>
          ))}
        </div>
        <div style={{ padding: '10px 18px', fontSize: 11, color: 'var(--color-text-dim)', display: 'flex', justifyContent: 'space-between' }}>
          <span>Enter agrega · ↑ ↓ cambian · Esc cancela</span>
          <button className="btn btn-ghost btn-sm" onClick={onCancelar}>Cancelar</button>
        </div>
      </div>
    </div>
  );
}
