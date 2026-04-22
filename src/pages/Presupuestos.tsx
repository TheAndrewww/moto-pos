// pages/Presupuestos.tsx — Gestión de cotizaciones

import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useProductStore, type Producto } from '../store/productStore';
import { useAuthStore } from '../store/authStore';
import { useVentaStore } from '../store/ventaStore';
import {
  ClipboardList, Plus, X, Search, Eye, Check, XCircle,
  RefreshCw, Trash2,
} from 'lucide-react';

interface Presupuesto {
  id: number;
  folio: string;
  usuario_nombre: string;
  cliente_nombre: string | null;
  estado: string;
  notas: string | null;
  vigencia_dias: number;
  total: number;
  fecha: string;
}

interface PresupuestoDetalle {
  id: number;
  producto_id: number | null;
  producto_nombre: string | null;
  descripcion: string;
  cantidad: number;
  precio_unitario: number;
  descuento_porcentaje: number;
  subtotal: number;
}

interface ItemForm {
  producto: Producto | null;
  descripcion: string;
  cantidad: number;
  precio_unitario: number;
  descuento_porcentaje: number;
}

const ESTADOS: Record<string, { label: string; color: string; bg: string }> = {
  pendiente: { label: 'Pendiente', color: '#e6a817', bg: 'rgba(230,168,23,0.1)' },
  aceptado: { label: 'Aceptado', color: '#22b378', bg: 'rgba(34,179,120,0.1)' },
  convertido: { label: 'Convertido', color: '#6c75f6', bg: 'rgba(108,117,246,0.1)' },
  cancelado: { label: 'Cancelado', color: '#dc3545', bg: 'rgba(220,53,69,0.1)' },
};

export default function Presupuestos() {
  const { productos, cargarTodo } = useProductStore();
  const { usuario } = useAuthStore();
  const { clientes, cargarClientes } = useVentaStore();

  const [presupuestos, setPresupuestos] = useState<Presupuesto[]>([]);
  const [cargando, setCargando] = useState(true);
  const [filtroEstado, setFiltroEstado] = useState('');
  const [showCrear, setShowCrear] = useState(false);
  const [detalle, setDetalle] = useState<{ presup: Presupuesto; items: PresupuestoDetalle[] } | null>(null);

  const cargarDatos = async () => {
    setCargando(true);
    try {
      const data = await invoke<Presupuesto[]>('listar_presupuestos', {
        estadoFiltro: filtroEstado || null,
      });
      setPresupuestos(data);
    } catch {}
    setCargando(false);
  };

  useEffect(() => { cargarTodo(); cargarClientes(); }, []);
  useEffect(() => { cargarDatos(); }, [filtroEstado]);

  const fmt = (n: number) => `$${n.toFixed(2)}`;

  const formatFecha = (fecha: string) => {
    try {
      const d = new Date(fecha + 'Z');
      return d.toLocaleString('es-MX', { day: '2-digit', month: '2-digit', year: 'numeric', hour: '2-digit', minute: '2-digit' });
    } catch { return fecha; }
  };

  const verDetalle = async (p: Presupuesto) => {
    try {
      const items = await invoke<PresupuestoDetalle[]>('obtener_detalle_presupuesto', {
        presupuestoId: p.id,
      });
      setDetalle({ presup: p, items });
    } catch {}
  };

  const cambiarEstado = async (id: number, estado: string) => {
    if (!usuario) return;
    try {
      await invoke('cambiar_estado_presupuesto', {
        presupuestoId: id,
        nuevoEstado: estado,
        usuarioId: usuario.id,
      });
      cargarDatos();
      if (detalle?.presup.id === id) {
        setDetalle(d => d ? { ...d, presup: { ...d.presup, estado } } : null);
      }
    } catch {}
  };

  // ─── Formulario de nuevo presupuesto ────

  const FormCrear = () => {
    const [items, setItems] = useState<ItemForm[]>([]);
    const [clienteId, setClienteId] = useState<number | ''>('');
    const [notas, setNotas] = useState('');
    const [vigencia, setVigencia] = useState(7);
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
      const existente = items.findIndex(i => i.producto?.id === prod.id);
      if (existente >= 0) {
        const newItems = [...items];
        newItems[existente].cantidad += 1;
        setItems(newItems);
      } else {
        setItems([...items, {
          producto: prod,
          descripcion: prod.nombre,
          cantidad: 1,
          precio_unitario: prod.precio_venta,
          descuento_porcentaje: 0,
        }]);
      }
      setBusqueda('');
    };

    const quitarItem = (idx: number) => {
      setItems(items.filter((_, i) => i !== idx));
    };

    const calcSubtotal = (item: ItemForm) => {
      const desc = item.precio_unitario * (item.descuento_porcentaje / 100);
      return (item.precio_unitario - desc) * item.cantidad;
    };

    const total = items.reduce((acc, i) => acc + calcSubtotal(i), 0);

    const handleSubmit = async () => {
      if (!usuario) return;
      if (items.length === 0) return setError('Agrega al menos un producto');
      setGuardando(true);
      setError('');
      try {
        await invoke('crear_presupuesto', {
          presupuesto: {
            usuario_id: usuario.id,
            cliente_id: clienteId || null,
            notas: notas || null,
            vigencia_dias: vigencia,
            total,
            items: items.map(i => ({
              producto_id: i.producto?.id || null,
              descripcion: i.descripcion,
              cantidad: i.cantidad,
              precio_unitario: i.precio_unitario,
              descuento_porcentaje: i.descuento_porcentaje,
              subtotal: calcSubtotal(i),
            })),
          },
        });
        setShowCrear(false);
        cargarDatos();
      } catch (err: any) {
        setError(err?.toString() || 'Error al crear presupuesto');
      }
      setGuardando(false);
    };

    return (
      <div style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)',
        display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 100,
      }} onClick={() => setShowCrear(false)}>
        <div className="card animate-fade-in" style={{ width: 700, maxHeight: '90vh', overflow: 'auto', padding: 24 }}
          onClick={e => e.stopPropagation()}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
            <h2 style={{ fontSize: 18, fontWeight: 700 }}>📋 Nuevo Presupuesto</h2>
            <button className="btn btn-ghost btn-sm" onClick={() => setShowCrear(false)}>
              <X size={18} />
            </button>
          </div>

          {/* Cliente + Vigencia */}
          <div style={{ display: 'grid', gridTemplateColumns: '1fr 120px', gap: 12, marginBottom: 16 }}>
            <div>
              <label style={labelStyle}>CLIENTE</label>
              <select className="input" value={clienteId} onChange={e => setClienteId(e.target.value ? Number(e.target.value) : '')}>
                <option value="">Público general</option>
                {clientes.map(c => <option key={c.id} value={c.id}>{c.nombre}</option>)}
              </select>
            </div>
            <div>
              <label style={labelStyle}>VIGENCIA (DÍAS)</label>
              <input className="input mono" type="number" min={1} value={vigencia}
                onChange={e => setVigencia(Number(e.target.value) || 7)} />
            </div>
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
                    <span className="mono" style={{ color: 'var(--color-primary)', fontWeight: 700 }}>{fmt(p.precio_venta)}</span>
                  </button>
                ))}
              </div>
            )}
          </div>

          {/* Items del presupuesto */}
          {items.length > 0 ? (
            <div style={{ marginBottom: 16 }}>
              <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
                <thead>
                  <tr style={{ background: 'var(--color-surface-2)', fontSize: 11, fontWeight: 600, color: 'var(--color-text-dim)', textTransform: 'uppercase' }}>
                    <th style={{ padding: '6px 10px', textAlign: 'left' }}>Producto</th>
                    <th style={{ padding: '6px 10px', width: 60 }}>Cant.</th>
                    <th style={{ padding: '6px 10px', width: 90 }}>Precio</th>
                    <th style={{ padding: '6px 10px', width: 60 }}>Desc%</th>
                    <th style={{ padding: '6px 10px', width: 90 }}>Subtotal</th>
                    <th style={{ padding: '6px 10px', width: 30 }}></th>
                  </tr>
                </thead>
                <tbody>
                  {items.map((item, idx) => (
                    <tr key={idx} style={{ borderBottom: '1px solid var(--color-border)' }}>
                      <td style={{ padding: '6px 10px' }}>{item.descripcion}</td>
                      <td style={{ padding: '6px 10px' }}>
                        <input className="input mono" type="number" min={1} value={item.cantidad}
                          style={{ width: 50, padding: '2px 6px', textAlign: 'center' }}
                          onChange={e => {
                            const n = [...items]; n[idx] = { ...n[idx], cantidad: Number(e.target.value) || 1 }; setItems(n);
                          }} />
                      </td>
                      <td style={{ padding: '6px 10px' }}>
                        <input className="input mono" type="number" step="0.01" value={item.precio_unitario}
                          style={{ width: 80, padding: '2px 6px', textAlign: 'right' }}
                          onChange={e => {
                            const n = [...items]; n[idx] = { ...n[idx], precio_unitario: Number(e.target.value) || 0 }; setItems(n);
                          }} />
                      </td>
                      <td style={{ padding: '6px 10px' }}>
                        <input className="input mono" type="number" step="1" min={0} max={100} value={item.descuento_porcentaje}
                          style={{ width: 50, padding: '2px 6px', textAlign: 'center' }}
                          onChange={e => {
                            const n = [...items]; n[idx] = { ...n[idx], descuento_porcentaje: Number(e.target.value) || 0 }; setItems(n);
                          }} />
                      </td>
                      <td className="mono" style={{ padding: '6px 10px', textAlign: 'right', fontWeight: 700, color: 'var(--color-primary)' }}>
                        {fmt(calcSubtotal(item))}
                      </td>
                      <td style={{ padding: '6px 10px' }}>
                        <button className="btn btn-ghost btn-sm" onClick={() => quitarItem(idx)}>
                          <Trash2 size={12} />
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
              <div style={{ display: 'flex', justifyContent: 'flex-end', padding: '10px', borderTop: '2px solid var(--color-border)' }}>
                <span style={{ fontSize: 18, fontWeight: 800, color: 'var(--color-primary)' }} className="mono">
                  Total: {fmt(total)}
                </span>
              </div>
            </div>
          ) : (
            <div style={{ padding: 24, textAlign: 'center', color: 'var(--color-text-dim)', borderRadius: 10, border: '1px dashed var(--color-border)', marginBottom: 16 }}>
              Busca y agrega productos al presupuesto
            </div>
          )}

          {/* Notas */}
          <div style={{ marginBottom: 16 }}>
            <label style={labelStyle}>NOTAS</label>
            <input className="input" placeholder="Notas opcionales para el presupuesto..."
              value={notas} onChange={e => setNotas(e.target.value)} />
          </div>

          {error && <p style={{ color: 'var(--color-danger)', fontSize: 13, marginBottom: 8 }}>{error}</p>}

          <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
            <button className="btn btn-ghost" onClick={() => setShowCrear(false)}>Cancelar</button>
            <button className="btn btn-primary" disabled={guardando || items.length === 0} onClick={handleSubmit}>
              {guardando ? 'Guardando...' : 'Crear Presupuesto'}
            </button>
          </div>
        </div>
      </div>
    );
  };

  // ─── Modal de detalle ────

  const ModalDetalle = () => {
    if (!detalle) return null;
    const { presup, items } = detalle;
    const est = ESTADOS[presup.estado] || ESTADOS.pendiente;

    return (
      <div style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)',
        display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 100,
      }} onClick={() => setDetalle(null)}>
        <div className="card animate-fade-in" style={{ width: 600, maxHeight: '85vh', overflow: 'auto', padding: 24 }}
          onClick={e => e.stopPropagation()}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
            <div>
              <h2 style={{ fontSize: 18, fontWeight: 700 }}>Presupuesto {presup.folio}</h2>
              <p style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>
                {formatFecha(presup.fecha)} · {presup.usuario_nombre}
                {presup.cliente_nombre && ` · Cliente: ${presup.cliente_nombre}`}
              </p>
            </div>
            <button className="btn btn-ghost btn-sm" onClick={() => setDetalle(null)}><X size={18} /></button>
          </div>

          {/* Estado */}
          <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 16 }}>
            <span style={{
              fontSize: 12, fontWeight: 700, padding: '4px 14px', borderRadius: 10,
              background: est.bg, color: est.color,
            }}>{est.label}</span>
            {presup.estado === 'pendiente' && (
              <>
                <button className="btn btn-ghost btn-sm" style={{ color: 'var(--color-success)' }}
                  onClick={() => cambiarEstado(presup.id, 'aceptado')}>
                  <Check size={14} /> Aceptar
                </button>
                <button className="btn btn-ghost btn-sm" style={{ color: 'var(--color-danger)' }}
                  onClick={() => cambiarEstado(presup.id, 'cancelado')}>
                  <XCircle size={14} /> Cancelar
                </button>
              </>
            )}
          </div>

          {/* Items */}
          <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
            <thead>
              <tr style={{ background: 'var(--color-surface-2)', fontSize: 11, fontWeight: 600, color: 'var(--color-text-dim)', textTransform: 'uppercase' }}>
                <th style={{ padding: '6px 10px', textAlign: 'left' }}>Producto</th>
                <th style={{ padding: '6px 10px' }}>Cant.</th>
                <th style={{ padding: '6px 10px' }}>Precio</th>
                <th style={{ padding: '6px 10px' }}>Desc%</th>
                <th style={{ padding: '6px 10px', textAlign: 'right' }}>Subtotal</th>
              </tr>
            </thead>
            <tbody>
              {items.map(i => (
                <tr key={i.id} style={{ borderBottom: '1px solid var(--color-border)' }}>
                  <td style={{ padding: '6px 10px' }}>{i.descripcion}</td>
                  <td className="mono" style={{ padding: '6px 10px', textAlign: 'center' }}>{i.cantidad}</td>
                  <td className="mono" style={{ padding: '6px 10px', textAlign: 'center' }}>{fmt(i.precio_unitario)}</td>
                  <td className="mono" style={{ padding: '6px 10px', textAlign: 'center' }}>{i.descuento_porcentaje > 0 ? `${i.descuento_porcentaje}%` : '—'}</td>
                  <td className="mono" style={{ padding: '6px 10px', textAlign: 'right', fontWeight: 700 }}>{fmt(i.subtotal)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <div style={{ display: 'flex', justifyContent: 'flex-end', padding: '12px 10px', borderTop: '2px solid var(--color-border)' }}>
            <span className="mono" style={{ fontSize: 20, fontWeight: 800, color: 'var(--color-primary)' }}>
              Total: {fmt(presup.total)}
            </span>
          </div>

          {presup.notas && (
            <p style={{ fontSize: 12, color: 'var(--color-text-muted)', marginTop: 8 }}>
              📝 {presup.notas}
            </p>
          )}
        </div>
      </div>
    );
  };

  // ─── Render principal ────

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      {/* Header */}
      <div style={{
        padding: '12px 20px',
        borderBottom: '1px solid var(--color-border)',
        background: 'var(--color-surface)',
        display: 'flex', flexDirection: 'column', gap: 10,
      }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
            <ClipboardList size={20} style={{ color: 'var(--color-primary)' }} />
            <h2 style={{ fontSize: 17, fontWeight: 800, color: 'var(--color-text)' }}>Presupuestos</h2>
            <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>·</span>
            <span style={{ fontSize: 12, color: 'var(--color-text-muted)' }}>{presupuestos.length} cotizaciones</span>
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-ghost" onClick={cargarDatos}><RefreshCw size={14} /></button>
            <button className="btn btn-primary" onClick={() => setShowCrear(true)}>
              <Plus size={16} /> Nuevo Presupuesto
            </button>
          </div>
        </div>

        <div style={{ display: 'flex', gap: 8 }}>
          <select className="input" value={filtroEstado} onChange={e => setFiltroEstado(e.target.value)}
            style={{ width: 180 }}>
            <option value="">Todos los estados</option>
            {Object.entries(ESTADOS).map(([k, v]) => <option key={k} value={k}>{v.label}</option>)}
          </select>
        </div>
      </div>

      {/* Lista */}
      <div style={{ flex: 1, overflow: 'auto' }}>
        {cargando ? (
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', color: 'var(--color-text-dim)' }}>
            <span className="animate-pulse-soft">Cargando presupuestos...</span>
          </div>
        ) : presupuestos.length === 0 ? (
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', flexDirection: 'column', gap: 12, color: 'var(--color-text-dim)' }}>
            <ClipboardList size={48} strokeWidth={1.2} />
            <p style={{ fontSize: 16, fontWeight: 600 }}>No hay presupuestos</p>
            <p style={{ fontSize: 13 }}>Crea tu primer presupuesto con el botón de arriba</p>
          </div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column' }}>
            {presupuestos.map(p => {
              const est = ESTADOS[p.estado] || ESTADOS.pendiente;
              return (
                <div
                  key={p.id}
                  style={{
                    display: 'flex', alignItems: 'center', gap: 14,
                    padding: '12px 20px', borderBottom: '1px solid var(--color-border)',
                    cursor: 'pointer', transition: 'background 0.1s',
                  }}
                  onMouseEnter={e => (e.currentTarget.style.background = 'var(--color-surface)')}
                  onMouseLeave={e => (e.currentTarget.style.background = 'transparent')}
                  onClick={() => verDetalle(p)}
                >
                  <span className="mono" style={{ fontSize: 13, fontWeight: 700, color: 'var(--color-text-muted)', minWidth: 90 }}>
                    {p.folio}
                  </span>
                  <span style={{
                    fontSize: 10, fontWeight: 700, padding: '2px 10px', borderRadius: 10,
                    background: est.bg, color: est.color, minWidth: 80, textAlign: 'center',
                  }}>
                    {est.label}
                  </span>
                  <span style={{ flex: 1, fontSize: 13, color: 'var(--color-text)' }}>
                    {p.cliente_nombre || 'Público general'}
                  </span>
                  <span className="mono" style={{ fontSize: 15, fontWeight: 800, color: 'var(--color-primary)' }}>
                    {fmt(p.total)}
                  </span>
                  <span style={{ fontSize: 11, color: 'var(--color-text-dim)', minWidth: 120, textAlign: 'right' }}>
                    {formatFecha(p.fecha)}
                  </span>
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
