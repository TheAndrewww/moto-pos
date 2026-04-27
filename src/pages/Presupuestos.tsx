// pages/Presupuestos.tsx — Gestión de cotizaciones

import { useState, useEffect } from 'react';
import { invoke } from '../lib/invokeCompat';
import { useProductStore } from '../store/productStore';
import { useAuthStore } from '../store/authStore';
import { useVentaStore } from '../store/ventaStore';
import {
  ClipboardList, Plus, X, Eye, Check, XCircle,
  RefreshCw, ShoppingCart, Printer
} from 'lucide-react';
import { imprimirTicket, type ConfigNegocio, type TicketData } from '../utils/ticket';

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

const ESTADOS: Record<string, { label: string; color: string; bg: string }> = {
  pendiente: { label: 'Pendiente', color: '#e6a817', bg: 'rgba(230,168,23,0.1)' },
  aceptado: { label: 'Aceptado', color: '#22b378', bg: 'rgba(34,179,120,0.1)' },
  convertido: { label: 'Convertido', color: '#6c75f6', bg: 'rgba(108,117,246,0.1)' },
  cancelado: { label: 'Cancelado', color: '#dc3545', bg: 'rgba(220,53,69,0.1)' },
};

interface PresupuestosProps {
  onIrAVenta?: () => void;
}

export default function Presupuestos({ onIrAVenta }: PresupuestosProps = {}) {
  const { productos, cargarTodo } = useProductStore();
  const { usuario } = useAuthStore();
  const { clientes, cargarClientes, nuevaTab, setModo, cargarPresupuestoEnNuevaTab } = useVentaStore();

  const [presupuestos, setPresupuestos] = useState<Presupuesto[]>([]);
  const [cargando, setCargando] = useState(true);
  const [filtroEstado, setFiltroEstado] = useState('');
  const [detalle, setDetalle] = useState<{ presup: Presupuesto; items: PresupuestoDetalle[] } | null>(null);
  const [configNegocio, setConfigNegocio] = useState<ConfigNegocio | null>(null);

  const cargarDatos = async () => {
    setCargando(true);
    try {
      const c = await invoke<ConfigNegocio>('obtener_config_negocio').catch(() => null);
      if (c) setConfigNegocio(c);

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

  // ─── Acciones ────

  const handleNuevoPresupuesto = () => {
    const id = nuevaTab();
    setTimeout(() => {
      setModo('presupuesto');
      onIrAVenta?.();
    }, 0);
    return id;
  };

  const handleConvertirAVenta = async (p: Presupuesto, items: PresupuestoDetalle[]) => {
    const itemsBase = items
      .map(i => {
        const prod = productos.find(x => x.id === i.producto_id);
        if (!prod) return null;
        return {
          producto: prod,
          cantidad: i.cantidad,
          precio_unitario: i.precio_unitario,
          descuento_porcentaje: i.descuento_porcentaje,
        };
      })
      .filter((x): x is NonNullable<typeof x> => x !== null);

    if (itemsBase.length === 0) {
      alert('No se pudieron cargar los productos del presupuesto.');
      return;
    }
    if (itemsBase.length < items.length) {
      if (!confirm(`Algunos productos ya no existen en el catálogo. Se cargarán ${itemsBase.length} de ${items.length}. ¿Continuar?`)) return;
    }

    const cliente = p.cliente_nombre
      ? clientes.find(c => c.nombre === p.cliente_nombre) ?? null
      : null;

    cargarPresupuestoEnNuevaTab(p.id, p.folio, itemsBase, cliente);
    setDetalle(null);
    onIrAVenta?.();
  };

  const handleImprimir = (p: Presupuesto, items: PresupuestoDetalle[]) => {
    if (!configNegocio) {
      alert("No se cargó la configuración del ticket.");
      return;
    }
    const ticketData: TicketData = {
      folio: p.folio,
      fecha: formatFecha(p.fecha),
      usuario: p.usuario_nombre,
      cliente: p.cliente_nombre,
      items: items.map(i => ({
        nombre: i.descripcion,
        codigo: i.producto_id ? String(i.producto_id) : 'S/C',
        cantidad: i.cantidad,
        precio_final: i.precio_unitario,
        subtotal: i.subtotal,
        descuento_porcentaje: i.descuento_porcentaje,
      })),
      subtotal: items.reduce((s, i) => s + (i.precio_unitario * i.cantidad), 0),
      descuento: items.reduce((s, i) => s + ((i.precio_unitario * i.cantidad) - i.subtotal), 0),
      total: p.total,
      metodo_pago: 'N/A',
      es_presupuesto: true,
    };
    imprimirTicket(configNegocio, ticketData);
  };

  // ─── Modal de detalle ────

  const ModalDetalle = () => {
    if (!detalle) return null;
    const { presup, items } = detalle;
    const est = ESTADOS[presup.estado] || ESTADOS.pendiente;

    return (
      <div className="pos-modal-overlay" style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)',
        display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 100,
      }} onClick={() => setDetalle(null)}>
        <div className="card animate-fade-in pos-modal-content pos-modal-fluid" style={{ width: 600, maxWidth: 600, maxHeight: '85vh', overflow: 'auto', padding: 24 }}
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
          <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 16, flexWrap: 'wrap' }}>
            <span style={{
              fontSize: 12, fontWeight: 700, padding: '4px 14px', borderRadius: 10,
              background: est.bg, color: est.color,
            }}>{est.label}</span>
            {(presup.estado === 'pendiente' || presup.estado === 'aceptado') && (
              <>
                <button className="btn btn-primary btn-sm"
                  onClick={() => handleConvertirAVenta(presup, items)}>
                  <ShoppingCart size={14} /> Convertir a Venta
                </button>
                {presup.estado === 'pendiente' && (
                  <button className="btn btn-ghost btn-sm" style={{ color: 'var(--color-success)' }}
                    onClick={() => cambiarEstado(presup.id, 'aceptado')}>
                    <Check size={14} /> Aceptar
                  </button>
                )}
                <button className="btn btn-ghost btn-sm" style={{ color: 'var(--color-danger)' }}
                  onClick={() => cambiarEstado(presup.id, 'cancelado')}>
                  <XCircle size={14} /> Cancelar
                </button>
              </>
            )}
            <div style={{ flex: 1 }}></div>
            <button className="btn btn-ghost btn-sm"
              onClick={() => handleImprimir(presup, items)}>
              <Printer size={14} /> Imprimir
            </button>
          </div>

          {/* Items */}
          <table className="responsive-table" style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
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
                  <td data-label="Producto" style={{ padding: '6px 10px' }}>{i.descripcion}</td>
                  <td data-label="Cant." className="mono" style={{ padding: '6px 10px', textAlign: 'center' }}>{i.cantidad}</td>
                  <td data-label="Precio" className="mono" style={{ padding: '6px 10px', textAlign: 'center' }}>{fmt(i.precio_unitario)}</td>
                  <td data-label="Desc%" className="mono" style={{ padding: '6px 10px', textAlign: 'center' }}>{i.descuento_porcentaje > 0 ? `${i.descuento_porcentaje}%` : '—'}</td>
                  <td data-label="Subtotal" className="mono" style={{ padding: '6px 10px', textAlign: 'right', fontWeight: 700 }}>{fmt(i.subtotal)}</td>
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
            <button className="btn btn-primary" onClick={handleNuevoPresupuesto}>
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
                  className="pos-list-row"
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

      {detalle && <ModalDetalle />}
    </div>
  );
}
