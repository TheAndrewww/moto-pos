// pages/Recepcion.tsx — Recepción de mercancía (entrada de stock)

import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useProductStore, type Producto } from '../store/productStore';
import { useAuthStore } from '../store/authStore';
import {
  TruckIcon, Plus, X, Search, Eye, RefreshCw, Trash2, PackagePlus,
} from 'lucide-react';

interface Recepcion {
  id: number;
  usuario_nombre: string;
  proveedor_nombre: string | null;
  fecha: string;
  notas: string | null;
  total_items: number;
}

interface RecepcionDetalle {
  id: number;
  producto_id: number;
  producto_nombre: string;
  producto_codigo: string;
  cantidad: number;
  precio_costo: number;
}

interface ItemForm {
  producto: Producto;
  cantidad: number;
  precio_costo: number;
}

export default function Recepcion() {
  const { productos, cargarTodo, proveedores } = useProductStore();
  const { usuario } = useAuthStore();

  const [recepciones, setRecepciones] = useState<Recepcion[]>([]);
  const [cargando, setCargando] = useState(true);
  const [showCrear, setShowCrear] = useState(false);
  const [detalle, setDetalle] = useState<{ recep: Recepcion; items: RecepcionDetalle[] } | null>(null);

  const cargarDatos = async () => {
    setCargando(true);
    try {
      const data = await invoke<Recepcion[]>('listar_recepciones');
      setRecepciones(data);
    } catch {}
    setCargando(false);
  };

  useEffect(() => { cargarTodo(); }, []);
  useEffect(() => { cargarDatos(); }, []);

  const fmt = (n: number) => `$${n.toFixed(2)}`;

  const formatFecha = (fecha: string) => {
    try {
      const d = new Date(fecha + 'Z');
      return d.toLocaleString('es-MX', { day: '2-digit', month: '2-digit', year: 'numeric', hour: '2-digit', minute: '2-digit' });
    } catch { return fecha; }
  };

  const verDetalle = async (r: Recepcion) => {
    try {
      const items = await invoke<RecepcionDetalle[]>('obtener_detalle_recepcion', { recepcionId: r.id });
      setDetalle({ recep: r, items });
    } catch {}
  };

  // ─── Form crear recepción ────

  const FormCrear = () => {
    const [items, setItems] = useState<ItemForm[]>([]);
    const [proveedorId, setProveedorId] = useState<number | ''>('');
    const [notas, setNotas] = useState('');
    const [busqueda, setBusqueda] = useState('');
    const [guardando, setGuardando] = useState(false);
    const [error, setError] = useState('');

    const filtrados = busqueda.length >= 2
      ? productos.filter(p =>
          p.nombre.toLowerCase().includes(busqueda.toLowerCase()) ||
          p.codigo.toLowerCase().includes(busqueda.toLowerCase())
        ).slice(0, 20)
      : [];

    const agregarItem = (prod: Producto) => {
      const existente = items.findIndex(i => i.producto.id === prod.id);
      if (existente >= 0) {
        const n = [...items]; n[existente].cantidad += 1; setItems(n);
      } else {
        setItems([...items, { producto: prod, cantidad: 1, precio_costo: prod.precio_costo }]);
      }
      setBusqueda('');
    };

    const quitarItem = (idx: number) => setItems(items.filter((_, i) => i !== idx));

    const handleSubmit = async () => {
      if (!usuario) return;
      if (items.length === 0) return setError('Agrega al menos un producto');
      setGuardando(true);
      setError('');
      try {
        await invoke('crear_recepcion', {
          recepcion: {
            usuario_id: usuario.id,
            proveedor_id: proveedorId || null,
            notas: notas || null,
            items: items.map(i => ({
              producto_id: i.producto.id,
              cantidad: i.cantidad,
              precio_costo: i.precio_costo,
            })),
          },
        });
        setShowCrear(false);
        cargarDatos();
        cargarTodo(); // Refrescar stock
      } catch (err: any) {
        setError(err?.toString() || 'Error al crear recepción');
      }
      setGuardando(false);
    };

    return (
      <div style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)',
        display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 100,
      }} onClick={() => setShowCrear(false)}>
        <div className="card animate-fade-in" style={{ width: 650, maxHeight: '90vh', overflow: 'auto', padding: 24 }}
          onClick={e => e.stopPropagation()}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
            <h2 style={{ fontSize: 18, fontWeight: 700 }}>📦 Nueva Recepción de Mercancía</h2>
            <button className="btn btn-ghost btn-sm" onClick={() => setShowCrear(false)}><X size={18} /></button>
          </div>

          {/* Proveedor */}
          <div style={{ marginBottom: 16 }}>
            <label style={labelStyle}>PROVEEDOR</label>
            <select className="input" value={proveedorId} onChange={e => setProveedorId(e.target.value ? Number(e.target.value) : '')}>
              <option value="">Sin proveedor</option>
              {proveedores.map(p => <option key={p.id} value={p.id}>{p.nombre}</option>)}
            </select>
          </div>

          {/* Buscar productos */}
          <div style={{ position: 'relative', marginBottom: 12 }}>
            <Search size={16} style={{ position: 'absolute', left: 12, top: '50%', transform: 'translateY(-50%)', color: 'var(--color-text-dim)' }} />
            <input className="input" placeholder="Buscar producto para agregar..."
              value={busqueda} onChange={e => setBusqueda(e.target.value)}
              style={{ paddingLeft: 36, width: '100%' }} />
            {filtrados.length > 0 && (
              <div className="card" style={{
                position: 'absolute', top: '100%', left: 0, right: 0, zIndex: 10,
                maxHeight: 200, overflow: 'auto', padding: 0, marginTop: 4,
              }}>
                {filtrados.map(p => (
                  <button key={p.id} style={{
                    display: 'flex', justifyContent: 'space-between', width: '100%',
                    padding: '8px 14px', border: 'none', background: 'transparent',
                    color: 'var(--color-text)', cursor: 'pointer', borderBottom: '1px solid var(--color-border)',
                    textAlign: 'left', fontSize: 13,
                  }}
                    onMouseEnter={e => (e.currentTarget.style.background = 'var(--color-surface-2)')}
                    onMouseLeave={e => (e.currentTarget.style.background = 'transparent')}
                    onClick={() => agregarItem(p)}
                  >
                    <span><span className="mono" style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>{p.codigo}</span> {p.nombre}</span>
                    <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>Stock: {p.stock_actual}</span>
                  </button>
                ))}
              </div>
            )}
          </div>

          {/* Items */}
          {items.length > 0 ? (
            <div style={{ marginBottom: 16 }}>
              <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
                <thead>
                  <tr style={{ background: 'var(--color-surface-2)', fontSize: 11, fontWeight: 600, color: 'var(--color-text-dim)', textTransform: 'uppercase' }}>
                    <th style={{ padding: '6px 10px', textAlign: 'left' }}>Producto</th>
                    <th style={{ padding: '6px 10px', width: 80 }}>Cantidad</th>
                    <th style={{ padding: '6px 10px', width: 100 }}>Costo Unit.</th>
                    <th style={{ padding: '6px 10px', width: 90, textAlign: 'right' }}>Total</th>
                    <th style={{ padding: '6px 10px', width: 30 }}></th>
                  </tr>
                </thead>
                <tbody>
                  {items.map((item, idx) => (
                    <tr key={idx} style={{ borderBottom: '1px solid var(--color-border)' }}>
                      <td style={{ padding: '6px 10px' }}>
                        <div style={{ fontWeight: 600 }}>{item.producto.nombre}</div>
                        <div className="mono" style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>{item.producto.codigo}</div>
                      </td>
                      <td style={{ padding: '6px 10px' }}>
                        <input className="input mono" type="number" min={1} value={item.cantidad}
                          style={{ width: 65, padding: '2px 6px', textAlign: 'center' }}
                          onChange={e => {
                            const n = [...items]; n[idx] = { ...n[idx], cantidad: Number(e.target.value) || 1 }; setItems(n);
                          }} />
                      </td>
                      <td style={{ padding: '6px 10px' }}>
                        <input className="input mono" type="number" step="0.01" value={item.precio_costo}
                          style={{ width: 85, padding: '2px 6px', textAlign: 'right' }}
                          onChange={e => {
                            const n = [...items]; n[idx] = { ...n[idx], precio_costo: Number(e.target.value) || 0 }; setItems(n);
                          }} />
                      </td>
                      <td className="mono" style={{ padding: '6px 10px', textAlign: 'right', fontWeight: 700 }}>
                        {fmt(item.cantidad * item.precio_costo)}
                      </td>
                      <td style={{ padding: '6px 10px' }}>
                        <button className="btn btn-ghost btn-sm" onClick={() => quitarItem(idx)}><Trash2 size={12} /></button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
              <div style={{ display: 'flex', justifyContent: 'space-between', padding: '10px', borderTop: '2px solid var(--color-border)' }}>
                <span style={{ fontSize: 13, color: 'var(--color-text-muted)' }}>
                  {items.reduce((a, i) => a + i.cantidad, 0)} unidades en {items.length} productos
                </span>
                <span className="mono" style={{ fontSize: 16, fontWeight: 800, color: 'var(--color-primary)' }}>
                  Costo total: {fmt(items.reduce((a, i) => a + i.cantidad * i.precio_costo, 0))}
                </span>
              </div>
            </div>
          ) : (
            <div style={{ padding: 24, textAlign: 'center', color: 'var(--color-text-dim)', borderRadius: 10, border: '1px dashed var(--color-border)', marginBottom: 16 }}>
              Busca y agrega los productos recibidos
            </div>
          )}

          {/* Notas */}
          <div style={{ marginBottom: 16 }}>
            <label style={labelStyle}>NOTAS</label>
            <input className="input" placeholder="Notas opcionales..." value={notas} onChange={e => setNotas(e.target.value)} />
          </div>

          {error && <p style={{ color: 'var(--color-danger)', fontSize: 13, marginBottom: 8 }}>{error}</p>}

          <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
            <button className="btn btn-ghost" onClick={() => setShowCrear(false)}>Cancelar</button>
            <button className="btn btn-primary" disabled={guardando || items.length === 0} onClick={handleSubmit}>
              <PackagePlus size={16} /> {guardando ? 'Guardando...' : 'Confirmar Recepción'}
            </button>
          </div>
        </div>
      </div>
    );
  };

  // ─── Modal detalle ────

  const ModalDetalle = () => {
    if (!detalle) return null;
    const { recep, items } = detalle;
    return (
      <div style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)',
        display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 100,
      }} onClick={() => setDetalle(null)}>
        <div className="card animate-fade-in" style={{ width: 550, maxHeight: '85vh', overflow: 'auto', padding: 24 }}
          onClick={e => e.stopPropagation()}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
            <div>
              <h2 style={{ fontSize: 18, fontWeight: 700 }}>Recepción #{recep.id}</h2>
              <p style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>
                {formatFecha(recep.fecha)} · {recep.usuario_nombre}
                {recep.proveedor_nombre && ` · ${recep.proveedor_nombre}`}
              </p>
            </div>
            <button className="btn btn-ghost btn-sm" onClick={() => setDetalle(null)}><X size={18} /></button>
          </div>

          <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
            <thead>
              <tr style={{ background: 'var(--color-surface-2)', fontSize: 11, fontWeight: 600, color: 'var(--color-text-dim)', textTransform: 'uppercase' }}>
                <th style={{ padding: '6px 10px', textAlign: 'left' }}>Producto</th>
                <th style={{ padding: '6px 10px' }}>Cantidad</th>
                <th style={{ padding: '6px 10px', textAlign: 'right' }}>Costo</th>
              </tr>
            </thead>
            <tbody>
              {items.map(i => (
                <tr key={i.id} style={{ borderBottom: '1px solid var(--color-border)' }}>
                  <td style={{ padding: '6px 10px' }}>
                    <div style={{ fontWeight: 600 }}>{i.producto_nombre}</div>
                    <div className="mono" style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>{i.producto_codigo}</div>
                  </td>
                  <td className="mono" style={{ padding: '6px 10px', textAlign: 'center', fontWeight: 700, color: 'var(--color-success)' }}>+{i.cantidad}</td>
                  <td className="mono" style={{ padding: '6px 10px', textAlign: 'right' }}>{fmt(i.precio_costo)}</td>
                </tr>
              ))}
            </tbody>
          </table>

          {recep.notas && <p style={{ fontSize: 12, color: 'var(--color-text-muted)', marginTop: 12 }}>📝 {recep.notas}</p>}
        </div>
      </div>
    );
  };

  // ─── Render ────

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      <div style={{
        padding: '12px 20px', borderBottom: '1px solid var(--color-border)',
        background: 'var(--color-surface)', display: 'flex', alignItems: 'center', justifyContent: 'space-between',
      }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          <TruckIcon size={20} style={{ color: 'var(--color-primary)' }} />
          <h2 style={{ fontSize: 17, fontWeight: 800, color: 'var(--color-text)' }}>Recepción de Mercancía</h2>
          <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>·</span>
          <span style={{ fontSize: 12, color: 'var(--color-text-muted)' }}>{recepciones.length} recepciones</span>
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <button className="btn btn-ghost" onClick={cargarDatos}><RefreshCw size={14} /></button>
          <button className="btn btn-primary" onClick={() => setShowCrear(true)}>
            <Plus size={16} /> Nueva Recepción
          </button>
        </div>
      </div>

      <div style={{ flex: 1, overflow: 'auto' }}>
        {cargando ? (
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', color: 'var(--color-text-dim)' }}>
            <span className="animate-pulse-soft">Cargando recepciones...</span>
          </div>
        ) : recepciones.length === 0 ? (
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', flexDirection: 'column', gap: 12, color: 'var(--color-text-dim)' }}>
            <TruckIcon size={48} strokeWidth={1.2} />
            <p style={{ fontSize: 16, fontWeight: 600 }}>No hay recepciones</p>
            <p style={{ fontSize: 13 }}>Registra la primera entrada de mercancía</p>
          </div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column' }}>
            {recepciones.map(r => (
              <div key={r.id}
                style={{
                  display: 'flex', alignItems: 'center', gap: 14,
                  padding: '12px 20px', borderBottom: '1px solid var(--color-border)',
                  cursor: 'pointer', transition: 'background 0.1s',
                }}
                onMouseEnter={e => (e.currentTarget.style.background = 'var(--color-surface)')}
                onMouseLeave={e => (e.currentTarget.style.background = 'transparent')}
                onClick={() => verDetalle(r)}
              >
                <span className="mono" style={{ fontSize: 13, fontWeight: 700, color: 'var(--color-text-muted)', minWidth: 50 }}>
                  #{r.id}
                </span>
                <span style={{
                  fontSize: 11, fontWeight: 700, padding: '2px 10px', borderRadius: 10,
                  background: 'rgba(34,179,120,0.1)', color: '#22b378',
                }}>
                  {r.total_items} items
                </span>
                <span style={{ flex: 1, fontSize: 13, color: 'var(--color-text)' }}>
                  {r.proveedor_nombre || 'Sin proveedor'}
                </span>
                <span style={{ fontSize: 12, color: 'var(--color-text-muted)' }}>{r.usuario_nombre}</span>
                <span style={{ fontSize: 11, color: 'var(--color-text-dim)', minWidth: 120, textAlign: 'right' }}>
                  {formatFecha(r.fecha)}
                </span>
                <Eye size={14} style={{ color: 'var(--color-text-dim)' }} />
              </div>
            ))}
          </div>
        )}
      </div>

      {showCrear && <FormCrear />}
      {detalle && <ModalDetalle />}
    </div>
  );
}

const labelStyle: React.CSSProperties = {
  fontSize: 11, fontWeight: 600, color: 'var(--color-text-muted)',
  display: 'block', marginBottom: 4, textTransform: 'uppercase', letterSpacing: '0.3px',
};
