// pages/Pedidos.tsx — Gestión de pedidos a proveedores

import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useProductStore, type Producto } from '../store/productStore';
import { useAuthStore } from '../store/authStore';
import {
  ScrollText, Plus, X, Search, Eye, RefreshCw, Trash2,
  Send, PackageCheck,
} from 'lucide-react';

interface OrdenPedido {
  id: number;
  proveedor_nombre: string | null;
  usuario_nombre: string;
  estado: string;
  notas: string | null;
  fecha: string;
  total_items: number;
}

interface OrdenDetalle {
  id: number;
  producto_id: number;
  producto_nombre: string;
  producto_codigo: string;
  cantidad_pedida: number;
  cantidad_recibida: number;
  precio_costo: number;
}

interface ItemForm {
  producto: Producto;
  cantidad: number;
  precio_costo: number;
}

const ESTADOS: Record<string, { label: string; color: string; bg: string }> = {
  borrador: { label: 'Borrador', color: '#999', bg: 'rgba(150,150,150,0.1)' },
  enviada: { label: 'Enviada', color: '#6c75f6', bg: 'rgba(108,117,246,0.1)' },
  recibida: { label: 'Recibida', color: '#22b378', bg: 'rgba(34,179,120,0.1)' },
  cancelada: { label: 'Cancelada', color: '#dc3545', bg: 'rgba(220,53,69,0.1)' },
};

export default function Pedidos() {
  const { productos, cargarTodo, proveedores } = useProductStore();
  const { usuario } = useAuthStore();

  const [ordenes, setOrdenes] = useState<OrdenPedido[]>([]);
  const [cargando, setCargando] = useState(true);
  const [filtroEstado, setFiltroEstado] = useState('');
  const [showCrear, setShowCrear] = useState(false);
  const [detalle, setDetalle] = useState<{ orden: OrdenPedido; items: OrdenDetalle[] } | null>(null);

  const cargarDatos = async () => {
    setCargando(true);
    try {
      const data = await invoke<OrdenPedido[]>('listar_ordenes_pedido', {
        estadoFiltro: filtroEstado || null,
      });
      setOrdenes(data);
    } catch {}
    setCargando(false);
  };

  useEffect(() => { cargarTodo(); }, []);
  useEffect(() => { cargarDatos(); }, [filtroEstado]);

  const fmt = (n: number) => `$${n.toFixed(2)}`;
  const formatFecha = (fecha: string) => {
    try {
      const d = new Date(fecha + 'Z');
      return d.toLocaleString('es-MX', { day: '2-digit', month: '2-digit', year: 'numeric', hour: '2-digit', minute: '2-digit' });
    } catch { return fecha; }
  };

  const verDetalle = async (o: OrdenPedido) => {
    try {
      const items = await invoke<OrdenDetalle[]>('obtener_detalle_orden', { ordenId: o.id });
      setDetalle({ orden: o, items });
    } catch {}
  };

  const cambiarEstado = async (id: number, estado: string) => {
    if (!usuario) return;
    try {
      await invoke('cambiar_estado_orden', { ordenId: id, nuevoEstado: estado, usuarioId: usuario.id });
      cargarDatos();
      if (detalle?.orden.id === id) setDetalle(d => d ? { ...d, orden: { ...d.orden, estado } } : null);
    } catch {}
  };

  // ─── Form crear ────

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

    const handleSubmit = async () => {
      if (!usuario) return;
      if (items.length === 0) return setError('Agrega al menos un producto');
      setGuardando(true); setError('');
      try {
        await invoke('crear_orden_pedido', {
          orden: {
            usuario_id: usuario.id,
            proveedor_id: proveedorId || null,
            notas: notas || null,
            items: items.map(i => ({
              producto_id: i.producto.id,
              cantidad_pedida: i.cantidad,
              precio_costo: i.precio_costo,
            })),
          },
        });
        setShowCrear(false);
        cargarDatos();
      } catch (err: any) {
        setError(err?.toString() || 'Error');
      }
      setGuardando(false);
    };

    return (
      <div style={{ position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)', display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 100 }}
        onClick={() => setShowCrear(false)}>
        <div className="card animate-fade-in" style={{ width: 650, maxHeight: '90vh', overflow: 'auto', padding: 24 }}
          onClick={e => e.stopPropagation()}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
            <h2 style={{ fontSize: 18, fontWeight: 700 }}>📝 Nuevo Pedido a Proveedor</h2>
            <button className="btn btn-ghost btn-sm" onClick={() => setShowCrear(false)}><X size={18} /></button>
          </div>

          <div style={{ marginBottom: 16 }}>
            <label style={labelStyle}>PROVEEDOR</label>
            <select className="input" value={proveedorId} onChange={e => setProveedorId(e.target.value ? Number(e.target.value) : '')}>
              <option value="">Seleccionar proveedor</option>
              {proveedores.map(p => <option key={p.id} value={p.id}>{p.nombre}</option>)}
            </select>
          </div>

          <div style={{ position: 'relative', marginBottom: 12 }}>
            <Search size={16} style={{ position: 'absolute', left: 12, top: '50%', transform: 'translateY(-50%)', color: 'var(--color-text-dim)' }} />
            <input className="input" placeholder="Buscar producto para pedir..."
              value={busqueda} onChange={e => setBusqueda(e.target.value)}
              style={{ paddingLeft: 36, width: '100%' }} />
            {filtrados.length > 0 && (
              <div className="card" style={{ position: 'absolute', top: '100%', left: 0, right: 0, zIndex: 10, maxHeight: 200, overflow: 'auto', padding: 0, marginTop: 4 }}>
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

          {items.length > 0 ? (
            <div style={{ marginBottom: 16 }}>
              <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
                <thead>
                  <tr style={{ background: 'var(--color-surface-2)', fontSize: 11, fontWeight: 600, color: 'var(--color-text-dim)', textTransform: 'uppercase' }}>
                    <th style={{ padding: '6px 10px', textAlign: 'left' }}>Producto</th>
                    <th style={{ padding: '6px 10px', width: 80 }}>Cantidad</th>
                    <th style={{ padding: '6px 10px', width: 100 }}>Costo Est.</th>
                    <th style={{ padding: '6px 10px', width: 30 }}></th>
                  </tr>
                </thead>
                <tbody>
                  {items.map((item, idx) => (
                    <tr key={idx} style={{ borderBottom: '1px solid var(--color-border)' }}>
                      <td style={{ padding: '6px 10px', fontWeight: 600 }}>{item.producto.nombre}</td>
                      <td style={{ padding: '6px 10px' }}>
                        <input className="input mono" type="number" min={1} value={item.cantidad}
                          style={{ width: 65, padding: '2px 6px', textAlign: 'center' }}
                          onChange={e => { const n = [...items]; n[idx] = { ...n[idx], cantidad: Number(e.target.value) || 1 }; setItems(n); }} />
                      </td>
                      <td style={{ padding: '6px 10px' }}>
                        <input className="input mono" type="number" step="0.01" value={item.precio_costo}
                          style={{ width: 85, padding: '2px 6px', textAlign: 'right' }}
                          onChange={e => { const n = [...items]; n[idx] = { ...n[idx], precio_costo: Number(e.target.value) || 0 }; setItems(n); }} />
                      </td>
                      <td style={{ padding: '6px 10px' }}>
                        <button className="btn btn-ghost btn-sm" onClick={() => setItems(items.filter((_, i) => i !== idx))}><Trash2 size={12} /></button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <div style={{ padding: 24, textAlign: 'center', color: 'var(--color-text-dim)', borderRadius: 10, border: '1px dashed var(--color-border)', marginBottom: 16 }}>
              Agrega productos al pedido
            </div>
          )}

          <div style={{ marginBottom: 16 }}>
            <label style={labelStyle}>NOTAS</label>
            <input className="input" placeholder="Notas opcionales..." value={notas} onChange={e => setNotas(e.target.value)} />
          </div>

          {error && <p style={{ color: 'var(--color-danger)', fontSize: 13, marginBottom: 8 }}>{error}</p>}

          <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
            <button className="btn btn-ghost" onClick={() => setShowCrear(false)}>Cancelar</button>
            <button className="btn btn-primary" disabled={guardando || items.length === 0} onClick={handleSubmit}>
              {guardando ? 'Guardando...' : 'Crear Pedido'}
            </button>
          </div>
        </div>
      </div>
    );
  };

  // ─── Modal detalle ────

  const ModalDetalle = () => {
    if (!detalle) return null;
    const { orden, items } = detalle;
    const est = ESTADOS[orden.estado] || ESTADOS.borrador;

    return (
      <div style={{ position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)', display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 100 }}
        onClick={() => setDetalle(null)}>
        <div className="card animate-fade-in" style={{ width: 600, maxHeight: '85vh', overflow: 'auto', padding: 24 }}
          onClick={e => e.stopPropagation()}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
            <div>
              <h2 style={{ fontSize: 18, fontWeight: 700 }}>Pedido #{orden.id}</h2>
              <p style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>
                {formatFecha(orden.fecha)} · {orden.usuario_nombre}
                {orden.proveedor_nombre && ` · ${orden.proveedor_nombre}`}
              </p>
            </div>
            <button className="btn btn-ghost btn-sm" onClick={() => setDetalle(null)}><X size={18} /></button>
          </div>

          <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 16 }}>
            <span style={{ fontSize: 12, fontWeight: 700, padding: '4px 14px', borderRadius: 10, background: est.bg, color: est.color }}>{est.label}</span>
            {orden.estado === 'borrador' && (
              <>
                <button className="btn btn-ghost btn-sm" style={{ color: '#6c75f6' }} onClick={() => cambiarEstado(orden.id, 'enviada')}>
                  <Send size={14} /> Marcar Enviada
                </button>
                <button className="btn btn-ghost btn-sm" style={{ color: 'var(--color-danger)' }} onClick={() => cambiarEstado(orden.id, 'cancelada')}>
                  <X size={14} /> Cancelar
                </button>
              </>
            )}
            {orden.estado === 'enviada' && (
              <button className="btn btn-ghost btn-sm" style={{ color: '#22b378' }} onClick={() => cambiarEstado(orden.id, 'recibida')}>
                <PackageCheck size={14} /> Marcar Recibida
              </button>
            )}
          </div>

          <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
            <thead>
              <tr style={{ background: 'var(--color-surface-2)', fontSize: 11, fontWeight: 600, color: 'var(--color-text-dim)', textTransform: 'uppercase' }}>
                <th style={{ padding: '6px 10px', textAlign: 'left' }}>Producto</th>
                <th style={{ padding: '6px 10px' }}>Pedido</th>
                <th style={{ padding: '6px 10px' }}>Recibido</th>
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
                  <td className="mono" style={{ padding: '6px 10px', textAlign: 'center' }}>{i.cantidad_pedida}</td>
                  <td className="mono" style={{ padding: '6px 10px', textAlign: 'center', color: i.cantidad_recibida >= i.cantidad_pedida ? '#22b378' : 'var(--color-warning)' }}>
                    {i.cantidad_recibida}
                  </td>
                  <td className="mono" style={{ padding: '6px 10px', textAlign: 'right' }}>{fmt(i.precio_costo)}</td>
                </tr>
              ))}
            </tbody>
          </table>

          {orden.notas && <p style={{ fontSize: 12, color: 'var(--color-text-muted)', marginTop: 12 }}>📝 {orden.notas}</p>}
        </div>
      </div>
    );
  };

  // ─── Render ────

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      <div style={{
        padding: '12px 20px', borderBottom: '1px solid var(--color-border)',
        background: 'var(--color-surface)', display: 'flex', flexDirection: 'column', gap: 10,
      }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
            <ScrollText size={20} style={{ color: 'var(--color-primary)' }} />
            <h2 style={{ fontSize: 17, fontWeight: 800, color: 'var(--color-text)' }}>Pedidos a Proveedores</h2>
            <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>·</span>
            <span style={{ fontSize: 12, color: 'var(--color-text-muted)' }}>{ordenes.length} pedidos</span>
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-ghost" onClick={cargarDatos}><RefreshCw size={14} /></button>
            <button className="btn btn-primary" onClick={() => setShowCrear(true)}>
              <Plus size={16} /> Nuevo Pedido
            </button>
          </div>
        </div>
        <select className="input" value={filtroEstado} onChange={e => setFiltroEstado(e.target.value)} style={{ width: 180 }}>
          <option value="">Todos los estados</option>
          {Object.entries(ESTADOS).map(([k, v]) => <option key={k} value={k}>{v.label}</option>)}
        </select>
      </div>

      <div style={{ flex: 1, overflow: 'auto' }}>
        {cargando ? (
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', color: 'var(--color-text-dim)' }}>
            <span className="animate-pulse-soft">Cargando pedidos...</span>
          </div>
        ) : ordenes.length === 0 ? (
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', flexDirection: 'column', gap: 12, color: 'var(--color-text-dim)' }}>
            <ScrollText size={48} strokeWidth={1.2} />
            <p style={{ fontSize: 16, fontWeight: 600 }}>No hay pedidos</p>
          </div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column' }}>
            {ordenes.map(o => {
              const est = ESTADOS[o.estado] || ESTADOS.borrador;
              return (
                <div key={o.id}
                  style={{
                    display: 'flex', alignItems: 'center', gap: 14,
                    padding: '12px 20px', borderBottom: '1px solid var(--color-border)',
                    cursor: 'pointer', transition: 'background 0.1s',
                  }}
                  onMouseEnter={e => (e.currentTarget.style.background = 'var(--color-surface)')}
                  onMouseLeave={e => (e.currentTarget.style.background = 'transparent')}
                  onClick={() => verDetalle(o)}
                >
                  <span className="mono" style={{ fontSize: 13, fontWeight: 700, color: 'var(--color-text-muted)', minWidth: 50 }}>#{o.id}</span>
                  <span style={{ fontSize: 10, fontWeight: 700, padding: '2px 10px', borderRadius: 10, background: est.bg, color: est.color, minWidth: 80, textAlign: 'center' }}>
                    {est.label}
                  </span>
                  <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>{o.total_items} items</span>
                  <span style={{ flex: 1, fontSize: 13, color: 'var(--color-text)' }}>{o.proveedor_nombre || 'Sin proveedor'}</span>
                  <span style={{ fontSize: 11, color: 'var(--color-text-dim)', minWidth: 120, textAlign: 'right' }}>{formatFecha(o.fecha)}</span>
                  <Eye size={14} style={{ color: 'var(--color-text-dim)' }} />
                </div>
              );
            })}
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
