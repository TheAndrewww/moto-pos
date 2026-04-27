// pages/Pedidos.tsx — Gestión de pedidos a proveedores (vistas de página completa)

import { useState, useEffect, useMemo } from 'react';
import { invoke } from '../lib/invokeCompat';
import { useProductStore, type Producto } from '../store/productStore';
import { useAuthStore } from '../store/authStore';
import {
  ScrollText, Plus, X, Search, Eye, RefreshCw, Trash2,
  Send, PackageCheck, ArrowLeft, ShoppingBag, FileText,
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

type Vista = 'lista' | 'crear' | 'detalle';

const ESTADOS: Record<string, { label: string; color: string; bg: string }> = {
  borrador: { label: 'Borrador', color: '#999', bg: 'rgba(150,150,150,0.1)' },
  enviada: { label: 'Enviada', color: '#6c75f6', bg: 'rgba(108,117,246,0.1)' },
  recibida_parcial: { label: 'Recibida parcial', color: '#e6a817', bg: 'rgba(230,168,23,0.12)' },
  recibida_completa: { label: 'Recibida', color: '#22b378', bg: 'rgba(34,179,120,0.1)' },
  recibida: { label: 'Recibida', color: '#22b378', bg: 'rgba(34,179,120,0.1)' },
  cancelada: { label: 'Cancelada', color: '#dc3545', bg: 'rgba(220,53,69,0.1)' },
};

export default function Pedidos() {
  const { productos, cargarTodo, proveedores } = useProductStore();
  const { usuario } = useAuthStore();

  const [vista, setVista] = useState<Vista>('lista');
  const [ordenes, setOrdenes] = useState<OrdenPedido[]>([]);
  const [cargando, setCargando] = useState(true);
  const [filtroEstado, setFiltroEstado] = useState('');
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
      setVista('detalle');
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

  // ─── Vista: CREAR PEDIDO ─────────────────────────────────────

  const VistaCrear = () => {
    const [items, setItems] = useState<ItemForm[]>([]);
    const [proveedorId, setProveedorId] = useState<number | ''>('');
    const [notas, setNotas] = useState('');
    const [busqueda, setBusqueda] = useState('');
    const [guardando, setGuardando] = useState(false);
    const [error, setError] = useState('');

    const proveedorNombre = proveedorId
      ? proveedores.find(p => p.id === proveedorId)?.nombre
      : null;

    const productosFiltrados = useMemo(() => {
      const base = proveedorId
        ? productos.filter(p => p.proveedor_id === proveedorId)
        : productos;
      if (busqueda.length < 2) return base.slice(0, 50);
      const q = busqueda.toLowerCase();
      return base.filter(p =>
        p.nombre.toLowerCase().includes(q) ||
        p.codigo.toLowerCase().includes(q)
      ).slice(0, 100);
    }, [productos, proveedorId, busqueda]);

    const agregarItem = (prod: Producto) => {
      const existente = items.findIndex(i => i.producto.id === prod.id);
      if (existente >= 0) {
        const n = [...items]; n[existente].cantidad += 1; setItems(n);
      } else {
        setItems([...items, { producto: prod, cantidad: 1, precio_costo: prod.precio_costo }]);
      }
    };

    const totalItems = items.reduce((a, i) => a + i.cantidad, 0);
    const totalCosto = items.reduce((a, i) => a + i.cantidad * i.precio_costo, 0);

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
        await cargarDatos();
        setVista('lista');
      } catch (err: any) {
        setError(err?.toString() || 'Error');
      }
      setGuardando(false);
    };

    return (
      <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
        {/* Header */}
        <div style={{
          padding: '10px 20px', borderBottom: '1px solid var(--color-border)',
          background: 'var(--color-surface)', display: 'flex', alignItems: 'center', gap: 10,
        }}>
          <button className="btn btn-ghost btn-sm" onClick={() => setVista('lista')}>
            <ArrowLeft size={16} /> Volver
          </button>
          <ScrollText size={18} style={{ color: 'var(--color-primary)' }} />
          <h2 style={{ fontSize: 16, fontWeight: 800 }}>Nuevo Pedido a Proveedor</h2>
        </div>

        {/* Body 2 columnas */}
        <div className="pos-pedidos-grid" style={{ flex: 1, display: 'grid', gridTemplateColumns: '1fr 420px', minHeight: 0 }}>
          {/* Izquierda — proveedor + catálogo */}
          <div style={{ display: 'flex', flexDirection: 'column', minHeight: 0, borderRight: '1px solid var(--color-border)' }}>
            <div className="pos-2col-grid" style={{ padding: '12px 16px', borderBottom: '1px solid var(--color-border)', display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 10 }}>
              <div>
                <label style={labelStyle}>PROVEEDOR</label>
                <select className="input" value={proveedorId}
                  onChange={e => {
                    const nuevo = e.target.value ? Number(e.target.value) : '';
                    if (items.length > 0 && nuevo !== proveedorId) {
                      if (!confirm('Cambiar de proveedor vaciará los productos ya agregados. ¿Continuar?')) return;
                      setItems([]);
                    }
                    setProveedorId(nuevo);
                  }}>
                  <option value="">Seleccionar proveedor</option>
                  {proveedores.map(p => <option key={p.id} value={p.id}>{p.nombre}</option>)}
                </select>
              </div>
              <div>
                <label style={labelStyle}>BUSCAR PRODUCTO</label>
                <div style={{ position: 'relative' }}>
                  <Search size={14} style={{ position: 'absolute', left: 10, top: '50%', transform: 'translateY(-50%)', color: 'var(--color-text-dim)' }} />
                  <input className="input"
                    placeholder={proveedorNombre ? `Buscar en ${proveedorNombre}...` : 'Buscar...'}
                    value={busqueda}
                    onChange={e => setBusqueda(e.target.value)}
                    style={{ paddingLeft: 32 }} />
                </div>
              </div>
            </div>

            <div style={{ flex: 1, overflow: 'auto' }}>
              {!proveedorId ? (
                <div style={{ padding: 32, textAlign: 'center', color: 'var(--color-text-dim)' }}>
                  <ShoppingBag size={40} strokeWidth={1.2} style={{ marginBottom: 10 }} />
                  <p style={{ fontSize: 14, fontWeight: 600 }}>Selecciona un proveedor</p>
                  <p style={{ fontSize: 12 }}>Solo verás los productos de ese proveedor</p>
                </div>
              ) : productosFiltrados.length === 0 ? (
                <div style={{ padding: 32, textAlign: 'center', color: 'var(--color-text-dim)', fontSize: 13 }}>
                  {busqueda ? `Sin coincidencias para "${busqueda}"` : 'Este proveedor no tiene productos asignados'}
                </div>
              ) : (
                <div>
                  {productosFiltrados.map(p => {
                    const enCarrito = items.find(i => i.producto.id === p.id);
                    return (
                      <button key={p.id}
                        onClick={() => agregarItem(p)}
                        style={{
                          display: 'grid', gridTemplateColumns: '1fr 90px 70px',
                          alignItems: 'center', gap: 10,
                          width: '100%', padding: '10px 16px', border: 'none',
                          borderBottom: '1px solid var(--color-border)',
                          background: enCarrito ? 'rgba(108,117,246,0.06)' : 'transparent',
                          color: 'var(--color-text)', cursor: 'pointer', textAlign: 'left', fontSize: 13,
                        }}
                        onMouseEnter={e => { if (!enCarrito) e.currentTarget.style.background = 'var(--color-surface-2)'; }}
                        onMouseLeave={e => { if (!enCarrito) e.currentTarget.style.background = 'transparent'; }}
                      >
                        <div>
                          <div style={{ fontWeight: 600 }}>{p.nombre}</div>
                          <div className="mono" style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>
                            {p.codigo} · Stock: {p.stock_actual}
                          </div>
                        </div>
                        <span className="mono" style={{ fontSize: 12, color: 'var(--color-text-muted)', textAlign: 'right' }}>
                          {fmt(p.precio_costo)}
                        </span>
                        <span style={{
                          fontSize: 11, fontWeight: 700,
                          color: enCarrito ? 'var(--color-primary)' : 'var(--color-text-dim)',
                          textAlign: 'right',
                        }}>
                          {enCarrito ? `× ${enCarrito.cantidad}` : '+ Agregar'}
                        </span>
                      </button>
                    );
                  })}
                </div>
              )}
            </div>
          </div>

          {/* Derecha — carrito del pedido */}
          <div style={{ display: 'flex', flexDirection: 'column', minHeight: 0, background: 'var(--color-surface)' }}>
            <div style={{ padding: '12px 16px', borderBottom: '1px solid var(--color-border)', display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
              <h3 style={{ fontSize: 13, fontWeight: 700, color: 'var(--color-text-muted)', textTransform: 'uppercase' }}>
                Items del pedido
              </h3>
              <span style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>
                {items.length} productos · {totalItems} unidades
              </span>
            </div>

            <div style={{ flex: 1, overflow: 'auto', padding: items.length ? 0 : 20 }}>
              {items.length === 0 ? (
                <div style={{ textAlign: 'center', color: 'var(--color-text-dim)', fontSize: 13, padding: '40px 12px' }}>
                  Agrega productos desde la lista de la izquierda
                </div>
              ) : (
                <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 12 }}>
                  <thead style={{ position: 'sticky', top: 0, background: 'var(--color-surface-2)' }}>
                    <tr style={{ fontSize: 10, fontWeight: 600, color: 'var(--color-text-dim)', textTransform: 'uppercase' }}>
                      <th style={{ padding: '6px 10px', textAlign: 'left' }}>Producto</th>
                      <th style={{ padding: '6px 8px', width: 54 }}>Cant.</th>
                      <th style={{ padding: '6px 8px', width: 72 }}>Costo</th>
                      <th style={{ padding: '6px 10px', width: 70, textAlign: 'right' }}>Total</th>
                      <th style={{ width: 24 }}></th>
                    </tr>
                  </thead>
                  <tbody>
                    {items.map((item, idx) => (
                      <tr key={idx} style={{ borderBottom: '1px solid var(--color-border)' }}>
                        <td style={{ padding: '6px 10px' }}>
                          <div style={{ fontWeight: 600, fontSize: 12 }}>{item.producto.nombre}</div>
                          <div className="mono" style={{ fontSize: 10, color: 'var(--color-text-dim)' }}>{item.producto.codigo}</div>
                        </td>
                        <td style={{ padding: '4px 6px' }}>
                          <input className="input mono" type="number" min={1} value={item.cantidad}
                            style={{ width: 48, padding: '2px 4px', textAlign: 'center', fontSize: 12 }}
                            onChange={e => { const n = [...items]; n[idx] = { ...n[idx], cantidad: Number(e.target.value) || 1 }; setItems(n); }} />
                        </td>
                        <td style={{ padding: '4px 6px' }}>
                          <input className="input mono" type="number" step="0.01" value={item.precio_costo}
                            style={{ width: 68, padding: '2px 4px', textAlign: 'right', fontSize: 12 }}
                            onChange={e => { const n = [...items]; n[idx] = { ...n[idx], precio_costo: Number(e.target.value) || 0 }; setItems(n); }} />
                        </td>
                        <td className="mono" style={{ padding: '4px 10px', textAlign: 'right', fontWeight: 700, color: 'var(--color-primary)', fontSize: 12 }}>
                          {fmt(item.cantidad * item.precio_costo)}
                        </td>
                        <td style={{ padding: '4px' }}>
                          <button className="btn btn-ghost btn-sm" style={{ padding: '2px 4px', color: 'var(--color-danger)' }}
                            onClick={() => setItems(items.filter((_, i) => i !== idx))}>
                            <Trash2 size={12} />
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>

            <div style={{ padding: '12px 16px', borderTop: '1px solid var(--color-border)', display: 'flex', flexDirection: 'column', gap: 10 }}>
              <div>
                <label style={labelStyle}>NOTAS</label>
                <input className="input" placeholder="Notas opcionales..." value={notas} onChange={e => setNotas(e.target.value)} />
              </div>
              <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: 14, fontWeight: 700 }}>
                <span style={{ color: 'var(--color-text-muted)' }}>Costo estimado</span>
                <span className="mono" style={{ color: 'var(--color-primary)', fontSize: 18 }}>{fmt(totalCosto)}</span>
              </div>
              {error && <p style={{ color: 'var(--color-danger)', fontSize: 12 }}>{error}</p>}
              <button className="btn btn-primary btn-lg" style={{ width: '100%', justifyContent: 'center' }}
                disabled={guardando || items.length === 0} onClick={handleSubmit}>
                {guardando ? 'Guardando...' : 'Crear Pedido'}
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  };

  // ─── Vista: DETALLE ──────────────────────────────────────────

  const VistaDetalle = () => {
    if (!detalle) return null;
    const { orden, items } = detalle;
    const est = ESTADOS[orden.estado] || ESTADOS.borrador;
    const totalCostoEstimado = items.reduce((a, i) => a + i.cantidad_pedida * i.precio_costo, 0);
    const totalPedido = items.reduce((a, i) => a + i.cantidad_pedida, 0);
    const totalRecibido = items.reduce((a, i) => a + i.cantidad_recibida, 0);

    return (
      <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
        <div style={{
          padding: '10px 20px', borderBottom: '1px solid var(--color-border)',
          background: 'var(--color-surface)', display: 'flex', alignItems: 'center', gap: 10,
        }}>
          <button className="btn btn-ghost btn-sm" onClick={() => { setDetalle(null); setVista('lista'); }}>
            <ArrowLeft size={16} /> Volver
          </button>
          <FileText size={18} style={{ color: 'var(--color-primary)' }} />
          <h2 style={{ fontSize: 16, fontWeight: 800 }}>Pedido #{orden.id}</h2>
          <span style={{
            fontSize: 11, fontWeight: 700, padding: '3px 10px', borderRadius: 10,
            background: est.bg, color: est.color,
          }}>{est.label}</span>
          <span style={{ flex: 1 }} />
          {orden.estado === 'borrador' && (
            <>
              <button className="btn btn-primary btn-sm" onClick={() => cambiarEstado(orden.id, 'enviada')}>
                <Send size={14} /> Marcar Enviada
              </button>
              <button className="btn btn-ghost btn-sm" style={{ color: 'var(--color-danger)' }}
                onClick={() => cambiarEstado(orden.id, 'cancelada')}>
                <X size={14} /> Cancelar
              </button>
            </>
          )}
          {orden.estado === 'enviada' && (
            <button className="btn btn-success btn-sm" onClick={() => cambiarEstado(orden.id, 'recibida_completa')}>
              <PackageCheck size={14} /> Marcar Recibida (sin detalle)
            </button>
          )}
        </div>

        <div style={{ flex: 1, overflow: 'auto', padding: 20 }}>
          <div className="pos-stats-4" style={{ display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: 12, marginBottom: 20 }}>
            <Tile label="Fecha" value={formatFecha(orden.fecha)} />
            <Tile label="Proveedor" value={orden.proveedor_nombre || '—'} />
            <Tile label="Creó" value={orden.usuario_nombre} />
            <Tile label="Costo estimado" value={fmt(totalCostoEstimado)} highlight />
          </div>

          <div className="pos-stats-3" style={{ display: 'grid', gridTemplateColumns: 'repeat(3, 1fr)', gap: 12, marginBottom: 16 }}>
            <Tile label="Productos" value={String(items.length)} />
            <Tile label="Unidades pedidas" value={String(totalPedido)} />
            <Tile label="Unidades recibidas" value={String(totalRecibido)}
              highlight={totalRecibido > 0 && totalRecibido < totalPedido ? 'warn' : totalRecibido >= totalPedido && totalPedido > 0 ? 'ok' : false} />
          </div>

          <div className="card" style={{ padding: 0, overflow: 'hidden' }}>
            <table className="responsive-table" style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
              <thead>
                <tr style={{ background: 'var(--color-surface-2)', fontSize: 11, fontWeight: 600, color: 'var(--color-text-dim)', textTransform: 'uppercase' }}>
                  <th style={{ padding: '8px 12px', textAlign: 'left' }}>Producto</th>
                  <th style={{ padding: '8px 12px' }}>Pedido</th>
                  <th style={{ padding: '8px 12px' }}>Recibido</th>
                  <th style={{ padding: '8px 12px' }}>Pendiente</th>
                  <th style={{ padding: '8px 12px', textAlign: 'right' }}>Costo</th>
                </tr>
              </thead>
              <tbody>
                {items.map(i => {
                  const pendiente = Math.max(0, i.cantidad_pedida - i.cantidad_recibida);
                  const completo = i.cantidad_recibida >= i.cantidad_pedida;
                  return (
                    <tr key={i.id} style={{ borderBottom: '1px solid var(--color-border)' }}>
                      <td data-label="Producto" style={{ padding: '8px 12px' }}>
                        <div style={{ fontWeight: 600 }}>{i.producto_nombre}</div>
                        <div className="mono" style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>{i.producto_codigo}</div>
                      </td>
                      <td data-label="Pedido" className="mono" style={{ padding: '8px 12px', textAlign: 'center' }}>{i.cantidad_pedida}</td>
                      <td data-label="Recibido" className="mono" style={{ padding: '8px 12px', textAlign: 'center', fontWeight: 700, color: completo ? '#22b378' : 'var(--color-warning)' }}>
                        {i.cantidad_recibida}
                      </td>
                      <td data-label="Pendiente" className="mono" style={{ padding: '8px 12px', textAlign: 'center', color: pendiente > 0 ? 'var(--color-warning)' : 'var(--color-text-dim)' }}>
                        {pendiente > 0 ? pendiente : '—'}
                      </td>
                      <td data-label="Costo" className="mono" style={{ padding: '8px 12px', textAlign: 'right' }}>{fmt(i.precio_costo)}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>

          {orden.notas && (
            <div className="card" style={{ padding: 12, marginTop: 14 }}>
              <div style={labelStyle}>NOTAS</div>
              <p style={{ fontSize: 13, color: 'var(--color-text-muted)' }}>{orden.notas}</p>
            </div>
          )}
        </div>
      </div>
    );
  };

  // ─── Vista: LISTA ────────────────────────────────────────────

  if (vista === 'crear') return <VistaCrear />;
  if (vista === 'detalle') return <VistaDetalle />;

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
            <button className="btn btn-primary" onClick={() => setVista('crear')}>
              <Plus size={16} /> Nuevo Pedido
            </button>
          </div>
        </div>
        <select className="input" value={filtroEstado} onChange={e => setFiltroEstado(e.target.value)} style={{ width: 180 }}>
          <option value="">Todos los estados</option>
          {Object.entries(ESTADOS)
            .filter(([k]) => k !== 'recibida') // legacy
            .map(([k, v]) => <option key={k} value={k}>{v.label}</option>)}
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
            <button className="btn btn-primary" onClick={() => setVista('crear')}>
              <Plus size={16} /> Crear primero
            </button>
          </div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column' }}>
            {ordenes.map(o => {
              const est = ESTADOS[o.estado] || ESTADOS.borrador;
              return (
                <div key={o.id}
                  className="pos-list-row"
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
                  <span style={{ fontSize: 10, fontWeight: 700, padding: '2px 10px', borderRadius: 10, background: est.bg, color: est.color, minWidth: 110, textAlign: 'center' }}>
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
    </div>
  );
}

// ─── helpers ────────────────────────────────────────────────

const labelStyle: React.CSSProperties = {
  fontSize: 11, fontWeight: 600, color: 'var(--color-text-muted)',
  display: 'block', marginBottom: 4, textTransform: 'uppercase', letterSpacing: '0.3px',
};

function Tile({ label, value, highlight }: { label: string; value: string; highlight?: boolean | 'warn' | 'ok' }) {
  const color = highlight === true ? 'var(--color-primary)'
    : highlight === 'warn' ? 'var(--color-warning)'
    : highlight === 'ok' ? '#22b378'
    : 'var(--color-text)';
  return (
    <div className="card" style={{ padding: 12 }}>
      <div style={labelStyle}>{label}</div>
      <div className="mono" style={{ fontSize: 16, fontWeight: 700, color }}>{value}</div>
    </div>
  );
}
