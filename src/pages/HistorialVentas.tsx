// pages/HistorialVentas.tsx — Historial de ventas + anulación + devoluciones parciales

import { useState, useEffect } from 'react';
import { invoke } from '../lib/invokeCompat';
import { useAuthStore } from '../store/authStore';
import {
  useHistorialStore,
  VentaResumen,
  VentaDetalleCompleto,
  VentaDetalleItem,
  DevolucionResumen,
} from '../store/historialStore';
import {
  ScrollText, Search, RefreshCw, X, Ban, Undo2, FileText, CheckCircle2, Printer,
} from 'lucide-react';
import { imprimirTicket, type ConfigNegocio } from '../utils/ticket';

type Tab = 'ventas' | 'devoluciones';

function fmt(n: number): string {
  return `$${n.toFixed(2)}`;
}

function formatFecha(fecha: string): string {
  try {
    const d = new Date(fecha.includes('T') ? fecha : fecha.replace(' ', 'T'));
    return d.toLocaleString('es-MX', {
      day: '2-digit', month: '2-digit', year: 'numeric',
      hour: '2-digit', minute: '2-digit', hour12: false,
    });
  } catch { return fecha; }
}

function hoyISO(): string {
  const d = new Date();
  return d.toISOString().slice(0, 10);
}

export default function HistorialVentas() {
  const { usuario } = useAuthStore();
  const { buscarVentas, listarDevoluciones, ventas, devoluciones, cargando } = useHistorialStore();
  const [tab, setTab] = useState<Tab>('ventas');

  // Filtros
  const [folio, setFolio] = useState('');
  const [fechaInicio, setFechaInicio] = useState(hoyISO());
  const [fechaFin, setFechaFin] = useState(hoyISO());
  const [clienteTexto, setClienteTexto] = useState('');

  // Modal de detalle de venta
  const [ventaSeleccionada, setVentaSeleccionada] = useState<VentaDetalleCompleto | null>(null);
  const [modalAnular, setModalAnular] = useState(false);
  const [modalDevolver, setModalDevolver] = useState(false);

  const cargar = async () => {
    try {
      await buscarVentas({
        folio,
        fecha_inicio: fechaInicio,
        fecha_fin: fechaFin,
        cliente_texto: clienteTexto,
      });
    } catch (e) {
      alert('Error al buscar: ' + e);
    }
  };

  const cargarDevoluciones = async () => {
    try {
      await listarDevoluciones(100);
    } catch (e) {
      alert('Error al cargar devoluciones: ' + e);
    }
  };

  useEffect(() => {
    if (tab === 'ventas') cargar();
    else cargarDevoluciones();
  }, [tab]);

  const abrirDetalle = async (venta: VentaResumen) => {
    try {
      const detalle = await invoke<VentaDetalleCompleto>('obtener_detalle_venta', {
        ventaId: venta.id,
      });
      setVentaSeleccionada(detalle);
    } catch (e) {
      alert('Error al abrir detalle: ' + e);
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      {/* Header */}
      <div style={{
        padding: '12px 20px', borderBottom: '1px solid var(--color-border)',
        background: 'var(--color-surface)', display: 'flex', alignItems: 'center', gap: 14,
      }}>
        <ScrollText size={18} style={{ color: 'var(--color-primary)' }} />
        <h2 style={{ fontSize: 16, fontWeight: 800, margin: 0, flex: 1 }}>
          Historial de ventas
        </h2>
        <div style={{ display: 'flex', gap: 4 }}>
          <button
            className={`btn btn-sm ${tab === 'ventas' ? '' : 'btn-ghost'}`}
            onClick={() => setTab('ventas')}
          >
            Ventas
          </button>
          <button
            className={`btn btn-sm ${tab === 'devoluciones' ? '' : 'btn-ghost'}`}
            onClick={() => setTab('devoluciones')}
          >
            Devoluciones
          </button>
        </div>
      </div>

      {tab === 'ventas' && (
        <>
          {/* Filtros */}
          <div style={{
            padding: '12px 20px', borderBottom: '1px solid var(--color-border)',
            background: 'var(--color-surface-2)', display: 'flex', gap: 10, flexWrap: 'wrap', alignItems: 'end',
          }}>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              <label style={{ fontSize: 11, color: 'var(--color-text-muted)', fontWeight: 600 }}>Folio</label>
              <input
                value={folio}
                onChange={(e) => setFolio(e.target.value)}
                placeholder="V-000123"
                className="input input-sm"
                style={{ width: 130 }}
              />
            </div>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              <label style={{ fontSize: 11, color: 'var(--color-text-muted)', fontWeight: 600 }}>Desde</label>
              <input
                type="date"
                value={fechaInicio}
                onChange={(e) => setFechaInicio(e.target.value)}
                className="input input-sm"
              />
            </div>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              <label style={{ fontSize: 11, color: 'var(--color-text-muted)', fontWeight: 600 }}>Hasta</label>
              <input
                type="date"
                value={fechaFin}
                onChange={(e) => setFechaFin(e.target.value)}
                className="input input-sm"
              />
            </div>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4, flex: 1, minWidth: 180 }}>
              <label style={{ fontSize: 11, color: 'var(--color-text-muted)', fontWeight: 600 }}>Cliente / teléfono</label>
              <input
                value={clienteTexto}
                onChange={(e) => setClienteTexto(e.target.value)}
                placeholder="Nombre o teléfono"
                className="input input-sm"
              />
            </div>
            <button className="btn btn-sm" onClick={cargar} disabled={cargando}>
              <Search size={14} /> Buscar
            </button>
            <button
              className="btn btn-ghost btn-sm"
              onClick={() => { setFolio(''); setFechaInicio(hoyISO()); setFechaFin(hoyISO()); setClienteTexto(''); }}
            >
              <RefreshCw size={14} /> Limpiar
            </button>
          </div>

          {/* Tabla ventas */}
          <div style={{ flex: 1, overflow: 'auto' }}>
            <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
              <thead style={{ position: 'sticky', top: 0, background: 'var(--color-surface)', zIndex: 1 }}>
                <tr style={{ borderBottom: '1px solid var(--color-border)' }}>
                  <th style={thStyle}>Folio</th>
                  <th style={thStyle}>Fecha</th>
                  <th style={thStyle}>Cajero</th>
                  <th style={thStyle}>Cliente</th>
                  <th style={thStyle}>Pago</th>
                  <th style={{ ...thStyle, textAlign: 'right' }}>Total</th>
                  <th style={{ ...thStyle, textAlign: 'center' }}>Items</th>
                  <th style={thStyle}>Estado</th>
                  <th style={thStyle}></th>
                </tr>
              </thead>
              <tbody>
                {ventas.length === 0 && !cargando && (
                  <tr>
                    <td colSpan={9} style={{ padding: 40, textAlign: 'center', color: 'var(--color-text-dim)' }}>
                      No se encontraron ventas con esos filtros.
                    </td>
                  </tr>
                )}
                {ventas.map((v) => (
                  <tr
                    key={v.id}
                    style={{
                      borderBottom: '1px solid var(--color-border)',
                      opacity: v.anulada ? 0.5 : 1,
                      cursor: 'pointer',
                    }}
                    onClick={() => abrirDetalle(v)}
                  >
                    <td style={tdStyle}><code style={{ fontSize: 12 }}>{v.folio}</code></td>
                    <td style={tdStyle}>{formatFecha(v.fecha)}</td>
                    <td style={tdStyle}>{v.usuario_nombre}</td>
                    <td style={tdStyle}>{v.cliente_nombre || '—'}</td>
                    <td style={tdStyle}>{v.metodo_pago}</td>
                    <td style={{ ...tdStyle, textAlign: 'right', fontWeight: 600 }}>{fmt(v.total)}</td>
                    <td style={{ ...tdStyle, textAlign: 'center' }}>{v.num_productos}</td>
                    <td style={tdStyle}>
                      {v.anulada ? (
                        <span style={{
                          fontSize: 11, padding: '2px 8px', borderRadius: 10,
                          background: 'rgba(216,56,77,0.12)', color: 'var(--color-primary)', fontWeight: 600,
                        }}>ANULADA</span>
                      ) : (
                        <span style={{
                          fontSize: 11, padding: '2px 8px', borderRadius: 10,
                          background: 'rgba(34,179,120,0.12)', color: '#22b378', fontWeight: 600,
                        }}>OK</span>
                      )}
                    </td>
                    <td style={tdStyle}>
                      <button className="btn btn-ghost btn-sm" onClick={(e) => { e.stopPropagation(); abrirDetalle(v); }}>
                        <FileText size={13} />
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}

      {tab === 'devoluciones' && (
        <div style={{ flex: 1, overflow: 'auto' }}>
          <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
            <thead style={{ position: 'sticky', top: 0, background: 'var(--color-surface)', zIndex: 1 }}>
              <tr style={{ borderBottom: '1px solid var(--color-border)' }}>
                <th style={thStyle}>Folio</th>
                <th style={thStyle}>Venta</th>
                <th style={thStyle}>Fecha</th>
                <th style={thStyle}>Usuario</th>
                <th style={thStyle}>Autorizó</th>
                <th style={thStyle}>Motivo</th>
                <th style={{ ...thStyle, textAlign: 'right' }}>Monto</th>
                <th style={{ ...thStyle, textAlign: 'center' }}>Items</th>
              </tr>
            </thead>
            <tbody>
              {devoluciones.length === 0 && (
                <tr>
                  <td colSpan={8} style={{ padding: 40, textAlign: 'center', color: 'var(--color-text-dim)' }}>
                    No hay devoluciones registradas.
                  </td>
                </tr>
              )}
              {devoluciones.map((d: DevolucionResumen) => (
                <tr key={d.id} style={{ borderBottom: '1px solid var(--color-border)' }}>
                  <td style={tdStyle}><code style={{ fontSize: 12 }}>{d.folio}</code></td>
                  <td style={tdStyle}><code style={{ fontSize: 12 }}>{d.venta_folio}</code></td>
                  <td style={tdStyle}>{formatFecha(d.fecha)}</td>
                  <td style={tdStyle}>{d.usuario_nombre}</td>
                  <td style={tdStyle}>{d.autorizado_por_nombre || '—'}</td>
                  <td style={tdStyle}>{d.motivo}</td>
                  <td style={{ ...tdStyle, textAlign: 'right', fontWeight: 600, color: 'var(--color-primary)' }}>
                    -{fmt(d.total_devuelto)}
                  </td>
                  <td style={{ ...tdStyle, textAlign: 'center' }}>{d.num_items}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {/* Modal detalle de venta */}
      {ventaSeleccionada && !modalAnular && !modalDevolver && (
        <ModalDetalleVenta
          venta={ventaSeleccionada}
          onClose={() => setVentaSeleccionada(null)}
          onAnular={() => setModalAnular(true)}
          onDevolver={() => setModalDevolver(true)}
        />
      )}

      {/* Modal anular */}
      {ventaSeleccionada && modalAnular && (
        <ModalAnularVenta
          venta={ventaSeleccionada}
          usuarioId={usuario!.id}
          esAdmin={usuario?.es_admin ?? false}
          onClose={() => setModalAnular(false)}
          onSuccess={() => {
            setModalAnular(false);
            setVentaSeleccionada(null);
            cargar();
          }}
        />
      )}

      {/* Modal devolver */}
      {ventaSeleccionada && modalDevolver && (
        <ModalDevolverVenta
          venta={ventaSeleccionada}
          usuarioId={usuario!.id}
          esAdmin={usuario?.es_admin ?? false}
          onClose={() => setModalDevolver(false)}
          onSuccess={() => {
            setModalDevolver(false);
            setVentaSeleccionada(null);
            cargar();
          }}
        />
      )}
    </div>
  );
}

// ─── Modal: Detalle de venta ──────────────────────────────

function ModalDetalleVenta({
  venta, onClose, onAnular, onDevolver,
}: {
  venta: VentaDetalleCompleto;
  onClose: () => void;
  onAnular: () => void;
  onDevolver: () => void;
}) {
  const hoy = new Date().toISOString().slice(0, 10);
  const fechaVenta = venta.fecha.slice(0, 10);
  const esHoy = fechaVenta === hoy;
  const totalDisponibleADevolver = venta.items.reduce((s, i) => s + i.cantidad_disponible, 0);
  const puedeDevolver = !venta.anulada && totalDisponibleADevolver > 0;
  const puedeAnular = !venta.anulada && esHoy && venta.total_devuelto === 0;

  const handleReimprimir = async () => {
    try {
      const negocio = await invoke<ConfigNegocio>('obtener_config_negocio');
      imprimirTicket(negocio, {
        folio: venta.folio,
        fecha: venta.fecha,
        usuario: venta.usuario_nombre,
        cliente: venta.cliente_nombre,
        items: venta.items.map(i => ({
          nombre: i.nombre,
          codigo: i.codigo,
          cantidad: i.cantidad,
          precio_final: i.precio_final,
          subtotal: i.subtotal,
          descuento_porcentaje: i.descuento_porcentaje,
        })),
        subtotal: venta.subtotal,
        descuento: venta.descuento,
        // El redondeo no se guarda en BD; se infiere por diferencia.
        // Si total > (subtotal - descuento) entonces hubo redondeo hacia arriba.
        redondeo: Math.max(0, +(venta.total - (venta.subtotal - venta.descuento)).toFixed(2)),
        total: venta.total,
        metodo_pago: venta.metodo_pago,
        reimpresion: true,
      });
    } catch (e) {
      alert('No se pudo imprimir el ticket');
    }
  };

  return (
    <ModalWrapper onClose={onClose} maxWidth={720}>
      <div style={modalHeaderStyle}>
        <div>
          <h3 style={{ margin: 0, fontSize: 16, fontWeight: 700 }}>
            Venta <code>{venta.folio}</code>
          </h3>
          <div style={{ fontSize: 12, color: 'var(--color-text-muted)', marginTop: 3 }}>
            {formatFecha(venta.fecha)} · Cajero: {venta.usuario_nombre}
            {venta.cliente_nombre && ` · Cliente: ${venta.cliente_nombre}`}
          </div>
        </div>
        <button className="btn btn-ghost btn-sm" onClick={onClose}><X size={16} /></button>
      </div>

      {venta.anulada && (
        <div style={{
          margin: '12px 20px', padding: 10, borderRadius: 6,
          background: 'rgba(216,56,77,0.1)', border: '1px solid rgba(216,56,77,0.3)',
          fontSize: 12, color: 'var(--color-primary)',
        }}>
          <strong>Venta anulada</strong>
          {venta.anulada_por_nombre && ` por ${venta.anulada_por_nombre}`}
          {venta.motivo_anulacion && ` — Motivo: ${venta.motivo_anulacion}`}
        </div>
      )}

      <div style={{ padding: '0 20px', maxHeight: 380, overflow: 'auto' }}>
        <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 12 }}>
          <thead>
            <tr style={{ borderBottom: '1px solid var(--color-border)', textAlign: 'left' }}>
              <th style={{ padding: '6px 4px' }}>Código</th>
              <th style={{ padding: '6px 4px' }}>Producto</th>
              <th style={{ padding: '6px 4px', textAlign: 'right' }}>Cant.</th>
              <th style={{ padding: '6px 4px', textAlign: 'right' }}>Devuelto</th>
              <th style={{ padding: '6px 4px', textAlign: 'right' }}>P. unit.</th>
              <th style={{ padding: '6px 4px', textAlign: 'right' }}>Subtotal</th>
            </tr>
          </thead>
          <tbody>
            {venta.items.map(it => (
              <tr key={it.id} style={{ borderBottom: '1px solid var(--color-border)' }}>
                <td style={{ padding: '6px 4px', fontFamily: 'monospace' }}>{it.codigo}</td>
                <td style={{ padding: '6px 4px' }}>{it.nombre}</td>
                <td style={{ padding: '6px 4px', textAlign: 'right' }}>{it.cantidad}</td>
                <td style={{ padding: '6px 4px', textAlign: 'right', color: it.cantidad_devuelta > 0 ? 'var(--color-primary)' : 'inherit' }}>
                  {it.cantidad_devuelta > 0 ? it.cantidad_devuelta : '—'}
                </td>
                <td style={{ padding: '6px 4px', textAlign: 'right' }}>{fmt(it.precio_final)}</td>
                <td style={{ padding: '6px 4px', textAlign: 'right', fontWeight: 600 }}>{fmt(it.subtotal)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <div style={{
        margin: '12px 20px', padding: 10, background: 'var(--color-surface-2)',
        borderRadius: 6, fontSize: 13,
      }}>
        <div style={{ display: 'flex', justifyContent: 'space-between' }}>
          <span>Subtotal:</span>
          <span>{fmt(venta.subtotal)}</span>
        </div>
        {venta.descuento > 0 && (
          <div style={{ display: 'flex', justifyContent: 'space-between', color: 'var(--color-text-muted)' }}>
            <span>Descuento:</span>
            <span>-{fmt(venta.descuento)}</span>
          </div>
        )}
        <div style={{ display: 'flex', justifyContent: 'space-between', fontWeight: 700, fontSize: 15, marginTop: 4 }}>
          <span>Total:</span>
          <span>{fmt(venta.total)}</span>
        </div>
        {venta.total_devuelto > 0 && (
          <div style={{ display: 'flex', justifyContent: 'space-between', color: 'var(--color-primary)', fontWeight: 600, marginTop: 4 }}>
            <span>Devuelto:</span>
            <span>-{fmt(venta.total_devuelto)}</span>
          </div>
        )}
      </div>

      <div style={{
        padding: '12px 20px', borderTop: '1px solid var(--color-border)',
        display: 'flex', gap: 8, justifyContent: 'flex-end',
      }}>
        <button className="btn btn-ghost" onClick={onClose}>Cerrar</button>
        <button className="btn btn-ghost" onClick={handleReimprimir} title="Reimprimir ticket">
          <Printer size={14} /> Reimprimir
        </button>
        {puedeDevolver && (
          <button className="btn" style={{ background: 'var(--color-warning)', color: '#fff' }} onClick={onDevolver}>
            <Undo2 size={14} /> Devolución parcial
          </button>
        )}
        {puedeAnular && (
          <button className="btn btn-danger" onClick={onAnular}>
            <Ban size={14} /> Anular venta
          </button>
        )}
      </div>
    </ModalWrapper>
  );
}

// ─── Modal: Anular venta ──────────────────────────────────

function ModalAnularVenta({
  venta, usuarioId, esAdmin, onClose, onSuccess,
}: {
  venta: VentaDetalleCompleto;
  usuarioId: number;
  esAdmin: boolean;
  onClose: () => void;
  onSuccess: () => void;
}) {
  const [motivo, setMotivo] = useState('');
  const [pin, setPin] = useState('');
  const [procesando, setProcesando] = useState(false);

  const confirmar = async () => {
    if (!motivo.trim()) {
      alert('El motivo es obligatorio');
      return;
    }
    setProcesando(true);
    try {
      // Si no es admin, validar PIN
      if (!esAdmin) {
        const ok = await invoke<boolean>('verificar_pin_dueno', { pin });
        if (!ok) {
          alert('PIN del dueño incorrecto');
          setProcesando(false);
          return;
        }
      }
      await invoke('anular_venta', { ventaId: venta.id, usuarioId, motivo });
      onSuccess();
    } catch (e: any) {
      alert('Error al anular: ' + e);
      setProcesando(false);
    }
  };

  return (
    <ModalWrapper onClose={onClose} maxWidth={440}>
      <div style={modalHeaderStyle}>
        <h3 style={{ margin: 0, fontSize: 16, fontWeight: 700, color: 'var(--color-primary)' }}>
          <Ban size={16} style={{ verticalAlign: 'middle', marginRight: 6 }} />
          Anular venta {venta.folio}
        </h3>
        <button className="btn btn-ghost btn-sm" onClick={onClose}><X size={16} /></button>
      </div>

      <div style={{ padding: 20, display: 'flex', flexDirection: 'column', gap: 14 }}>
        <div style={{
          padding: 10, borderRadius: 6, background: 'rgba(216,56,77,0.08)',
          border: '1px solid rgba(216,56,77,0.25)', fontSize: 12,
        }}>
          Se restaurará el stock de {venta.items.length} producto(s) y se marcará la venta como anulada.
          El total <strong>{fmt(venta.total)}</strong> se descontará del ingreso del día en el corte.
        </div>

        <div>
          <label style={labelStyle}>Motivo de la anulación *</label>
          <textarea
            value={motivo}
            onChange={(e) => setMotivo(e.target.value)}
            placeholder="Ej: Cliente se arrepintió, error al cobrar, producto equivocado..."
            className="input"
            rows={3}
            style={{ width: '100%', resize: 'vertical' }}
          />
        </div>

        {!esAdmin && (
          <div>
            <label style={labelStyle}>PIN del dueño *</label>
            <input
              type="password"
              inputMode="numeric"
              maxLength={4}
              value={pin}
              onChange={(e) => setPin(e.target.value.replace(/\D/g, ''))}
              placeholder="••••"
              className="input"
              style={{ width: 120, letterSpacing: 4, textAlign: 'center', fontSize: 18 }}
              autoFocus
            />
          </div>
        )}
      </div>

      <div style={{
        padding: '12px 20px', borderTop: '1px solid var(--color-border)',
        display: 'flex', gap: 8, justifyContent: 'flex-end',
      }}>
        <button className="btn btn-ghost" onClick={onClose} disabled={procesando}>Cancelar</button>
        <button
          className="btn btn-danger"
          onClick={confirmar}
          disabled={procesando || !motivo.trim() || (!esAdmin && pin.length !== 4)}
        >
          {procesando ? 'Anulando...' : 'Confirmar anulación'}
        </button>
      </div>
    </ModalWrapper>
  );
}

// ─── Modal: Devolución parcial ────────────────────────────

function ModalDevolverVenta({
  venta, usuarioId, esAdmin, onClose, onSuccess,
}: {
  venta: VentaDetalleCompleto;
  usuarioId: number;
  esAdmin: boolean;
  onClose: () => void;
  onSuccess: () => void;
}) {
  const [cantidades, setCantidades] = useState<Record<number, number>>({});
  const [motivo, setMotivo] = useState('');
  const [pin, setPin] = useState('');
  const [procesando, setProcesando] = useState(false);

  const itemsDisponibles = venta.items.filter(i => i.cantidad_disponible > 0);

  const total = itemsDisponibles.reduce((s, it) => {
    const c = cantidades[it.id] || 0;
    return s + c * it.precio_final;
  }, 0);

  const hayAlgo = Object.values(cantidades).some(v => v > 0);

  const setCantidad = (item: VentaDetalleItem, valor: number) => {
    const clamp = Math.max(0, Math.min(item.cantidad_disponible, valor));
    setCantidades(prev => ({ ...prev, [item.id]: clamp }));
  };

  const confirmar = async () => {
    if (!motivo.trim()) {
      alert('El motivo es obligatorio');
      return;
    }
    if (!hayAlgo) {
      alert('Debes indicar al menos un producto a devolver');
      return;
    }
    setProcesando(true);
    try {
      let autorizadoPor: number | null = null;
      if (!esAdmin) {
        const id = await invoke<number | null>('resolver_dueno_por_pin', { pin });
        if (!id) {
          alert('PIN del dueño incorrecto');
          setProcesando(false);
          return;
        }
        autorizadoPor = id;
      }
      const items = Object.entries(cantidades)
        .filter(([_, c]) => c > 0)
        .map(([id, cantidad]) => ({
          venta_detalle_id: Number(id),
          cantidad: Number(cantidad),
        }));

      await invoke('crear_devolucion', {
        datos: {
          venta_id: venta.id,
          usuario_id: usuarioId,
          autorizado_por: autorizadoPor,
          motivo,
          items,
        },
      });
      alert('Devolución registrada. Se generó un RETIRO de caja por ' + fmt(total));
      onSuccess();
    } catch (e: any) {
      alert('Error al devolver: ' + e);
      setProcesando(false);
    }
  };

  return (
    <ModalWrapper onClose={onClose} maxWidth={680}>
      <div style={modalHeaderStyle}>
        <h3 style={{ margin: 0, fontSize: 16, fontWeight: 700, color: 'var(--color-warning)' }}>
          <Undo2 size={16} style={{ verticalAlign: 'middle', marginRight: 6 }} />
          Devolución parcial — {venta.folio}
        </h3>
        <button className="btn btn-ghost btn-sm" onClick={onClose}><X size={16} /></button>
      </div>

      <div style={{ padding: '12px 20px 0' }}>
        <div style={{
          padding: 10, borderRadius: 6, background: 'rgba(245,158,11,0.08)',
          border: '1px solid rgba(245,158,11,0.25)', fontSize: 12, marginBottom: 12,
        }}>
          Indica la cantidad a devolver por cada producto. Se restaurará el stock y se registrará un RETIRO automático de caja por el monto devuelto.
        </div>

        <div style={{ maxHeight: 280, overflow: 'auto' }}>
          <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 12 }}>
            <thead>
              <tr style={{ borderBottom: '1px solid var(--color-border)' }}>
                <th style={{ padding: '6px 4px', textAlign: 'left' }}>Producto</th>
                <th style={{ padding: '6px 4px', textAlign: 'right' }}>Vendido</th>
                <th style={{ padding: '6px 4px', textAlign: 'right' }}>Disponible</th>
                <th style={{ padding: '6px 4px', textAlign: 'center' }}>A devolver</th>
                <th style={{ padding: '6px 4px', textAlign: 'right' }}>P. unit.</th>
                <th style={{ padding: '6px 4px', textAlign: 'right' }}>Subtotal</th>
              </tr>
            </thead>
            <tbody>
              {venta.items.map(it => {
                const c = cantidades[it.id] || 0;
                const disabled = it.cantidad_disponible <= 0;
                return (
                  <tr key={it.id} style={{ borderBottom: '1px solid var(--color-border)', opacity: disabled ? 0.4 : 1 }}>
                    <td style={{ padding: '6px 4px' }}>
                      <div style={{ fontWeight: 600 }}>{it.nombre}</div>
                      <div style={{ fontSize: 10, color: 'var(--color-text-muted)', fontFamily: 'monospace' }}>{it.codigo}</div>
                    </td>
                    <td style={{ padding: '6px 4px', textAlign: 'right' }}>{it.cantidad}</td>
                    <td style={{ padding: '6px 4px', textAlign: 'right' }}>{it.cantidad_disponible}</td>
                    <td style={{ padding: '6px 4px', textAlign: 'center' }}>
                      <input
                        type="number"
                        min={0}
                        max={it.cantidad_disponible}
                        step={1}
                        value={c}
                        disabled={disabled}
                        onChange={(e) => setCantidad(it, Number(e.target.value))}
                        className="input input-sm"
                        style={{ width: 70, textAlign: 'center' }}
                      />
                    </td>
                    <td style={{ padding: '6px 4px', textAlign: 'right' }}>{fmt(it.precio_final)}</td>
                    <td style={{ padding: '6px 4px', textAlign: 'right', fontWeight: 600 }}>
                      {c > 0 ? fmt(c * it.precio_final) : '—'}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </div>

      <div style={{ padding: '12px 20px', display: 'flex', flexDirection: 'column', gap: 10 }}>
        <div>
          <label style={labelStyle}>Motivo de la devolución *</label>
          <textarea
            value={motivo}
            onChange={(e) => setMotivo(e.target.value)}
            placeholder="Ej: Producto defectuoso, cliente cambió de opinión, talla incorrecta..."
            className="input"
            rows={2}
            style={{ width: '100%', resize: 'vertical' }}
          />
        </div>

        {!esAdmin && (
          <div>
            <label style={labelStyle}>PIN del dueño *</label>
            <input
              type="password"
              inputMode="numeric"
              maxLength={4}
              value={pin}
              onChange={(e) => setPin(e.target.value.replace(/\D/g, ''))}
              placeholder="••••"
              className="input"
              style={{ width: 120, letterSpacing: 4, textAlign: 'center', fontSize: 18 }}
            />
          </div>
        )}

        <div style={{
          padding: 10, borderRadius: 6, background: 'var(--color-surface-2)',
          display: 'flex', justifyContent: 'space-between', alignItems: 'center',
        }}>
          <span style={{ fontSize: 13, color: 'var(--color-text-muted)' }}>Total a reembolsar:</span>
          <span style={{ fontSize: 18, fontWeight: 800, color: 'var(--color-primary)' }}>
            {fmt(total)}
          </span>
        </div>
      </div>

      <div style={{
        padding: '12px 20px', borderTop: '1px solid var(--color-border)',
        display: 'flex', gap: 8, justifyContent: 'flex-end',
      }}>
        <button className="btn btn-ghost" onClick={onClose} disabled={procesando}>Cancelar</button>
        <button
          className="btn"
          style={{ background: 'var(--color-warning)', color: '#fff' }}
          onClick={confirmar}
          disabled={procesando || !hayAlgo || !motivo.trim() || (!esAdmin && pin.length !== 4)}
        >
          <CheckCircle2 size={14} />
          {procesando ? 'Procesando...' : 'Confirmar devolución'}
        </button>
      </div>
    </ModalWrapper>
  );
}

// ─── Utilidades UI ────────────────────────────────────────

function ModalWrapper({
  children, onClose, maxWidth = 600,
}: {
  children: React.ReactNode;
  onClose: () => void;
  maxWidth?: number;
}) {
  return (
    <div
      onClick={onClose}
      style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)',
        display: 'flex', alignItems: 'center', justifyContent: 'center',
        zIndex: 100,
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          background: 'var(--color-surface)', borderRadius: 10,
          boxShadow: '0 10px 40px rgba(0,0,0,0.3)',
          width: '100%', maxWidth, maxHeight: '90vh',
          display: 'flex', flexDirection: 'column', overflow: 'hidden',
        }}
      >
        {children}
      </div>
    </div>
  );
}

const thStyle: React.CSSProperties = {
  padding: '8px 10px', textAlign: 'left', fontSize: 11,
  fontWeight: 700, color: 'var(--color-text-muted)', textTransform: 'uppercase',
};
const tdStyle: React.CSSProperties = { padding: '8px 10px' };
const modalHeaderStyle: React.CSSProperties = {
  padding: '14px 20px', borderBottom: '1px solid var(--color-border)',
  display: 'flex', alignItems: 'center', justifyContent: 'space-between',
};
const labelStyle: React.CSSProperties = {
  display: 'block', fontSize: 11, fontWeight: 600,
  color: 'var(--color-text-muted)', marginBottom: 4,
};
