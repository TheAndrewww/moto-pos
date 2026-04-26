// pages/Recepcion.tsx — Recepción de mercancía (página completa + escaneo)

import { useState, useEffect, useMemo, useRef, useCallback } from 'react';
import { invoke } from '../lib/invokeCompat';
import { useProductStore, type Producto } from '../store/productStore';
import { useAuthStore } from '../store/authStore';
import {
  TruckIcon, Plus, Search, Eye, RefreshCw, Trash2, PackagePlus,
  ArrowLeft, Minus, PlusIcon, Barcode, ClipboardList,
} from 'lucide-react';

interface RecepcionRow {
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
  precio_venta: number;       // nuevo precio de venta (0 = no actualizar)
  pedido_cantidad?: number;   // cantidad pendiente de la orden, si aplica
}

// Multiplicadores estándar para calcular precio de venta a partir del costo.
const MULTIPLICADORES = [1.4, 1.5, 1.7] as const;

type Vista = 'lista' | 'crear' | 'detalle';

export default function Recepcion() {
  const { productos, cargarTodo, proveedores } = useProductStore();
  const { usuario } = useAuthStore();

  const [vista, setVista] = useState<Vista>('lista');
  const [recepciones, setRecepciones] = useState<RecepcionRow[]>([]);
  const [cargando, setCargando] = useState(true);
  const [detalle, setDetalle] = useState<{ recep: RecepcionRow; items: RecepcionDetalle[] } | null>(null);

  const cargarDatos = async () => {
    setCargando(true);
    try {
      const data = await invoke<RecepcionRow[]>('listar_recepciones');
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

  const verDetalle = async (r: RecepcionRow) => {
    try {
      const items = await invoke<RecepcionDetalle[]>('obtener_detalle_recepcion', { recepcionId: r.id });
      setDetalle({ recep: r, items });
      setVista('detalle');
    } catch {}
  };

  // ─── Vista: CREAR ────────────────────────────────────────────

  const VistaCrear = () => {
    const [items, setItems] = useState<ItemForm[]>([]);
    const [proveedorId, setProveedorId] = useState<number | ''>('');
    const [ordenId, setOrdenId] = useState<number | ''>('');
    const [ordenes, setOrdenes] = useState<OrdenPedido[]>([]);
    const [notas, setNotas] = useState('');
    const [guardando, setGuardando] = useState(false);
    const [error, setError] = useState('');
    const [flash, setFlash] = useState<{ nombre: string; cantidad: number } | null>(null);
    const [buscarTexto, setBuscarTexto] = useState('');
    const scanRef = useRef<HTMLInputElement>(null);

    // Cargar órdenes enviadas / parciales
    useEffect(() => {
      (async () => {
        try {
          const enviadas = await invoke<OrdenPedido[]>('listar_ordenes_pedido', { estadoFiltro: 'enviada' });
          const parciales = await invoke<OrdenPedido[]>('listar_ordenes_pedido', { estadoFiltro: 'recibida_parcial' });
          setOrdenes([...enviadas, ...parciales]);
        } catch {}
      })();
    }, []);

    // Foco permanente en escaneo
    useEffect(() => {
      scanRef.current?.focus();
    }, [items.length]);

    // Al elegir orden: cargar detalle y pre-llenar items con cantidad pendiente
    const cargarOrden = async (oid: number) => {
      try {
        const detalleOrden = await invoke<OrdenDetalle[]>('obtener_detalle_orden', { ordenId: oid });
        const orden = ordenes.find(o => o.id === oid);
        const nuevosItems: ItemForm[] = [];
        for (const d of detalleOrden) {
          const pendiente = Math.max(0, d.cantidad_pedida - d.cantidad_recibida);
          if (pendiente <= 0) continue;
          const prod = productos.find(p => p.id === d.producto_id);
          if (!prod) continue;
          nuevosItems.push({
            producto: prod,
            cantidad: pendiente,
            precio_costo: d.precio_costo,
            precio_venta: prod.precio_venta,    // sugerencia: precio actual
            pedido_cantidad: pendiente,
          });
        }
        setItems(nuevosItems);
        // Auto-seleccionar proveedor de la orden
        if (orden?.proveedor_nombre) {
          const prov = proveedores.find(p => p.nombre === orden.proveedor_nombre);
          if (prov) setProveedorId(prov.id);
        }
      } catch {}
    };

    const buscarResultados = useMemo(() => {
      if (buscarTexto.length < 2) return [];
      const q = buscarTexto.toLowerCase();
      return productos.filter(p =>
        p.nombre.toLowerCase().includes(q) ||
        p.codigo.toLowerCase().includes(q)
      ).slice(0, 15);
    }, [buscarTexto, productos]);

    const agregarProducto = useCallback((prod: Producto, cantidad = 1) => {
      setItems(prev => {
        const idx = prev.findIndex(i => i.producto.id === prod.id);
        if (idx >= 0) {
          const n = [...prev];
          n[idx] = { ...n[idx], cantidad: n[idx].cantidad + cantidad };
          return n;
        }
        return [...prev, {
          producto: prod,
          cantidad,
          precio_costo: prod.precio_costo,
          precio_venta: prod.precio_venta,    // default = precio actual (no cambia hasta que el usuario lo modifique)
        }];
      });
      setFlash({ nombre: prod.nombre, cantidad });
      setTimeout(() => setFlash(null), 1200);
    }, []);

    const handleScan = (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key !== 'Enter') return;
      const code = (e.target as HTMLInputElement).value.trim();
      if (!code) return;
      (e.target as HTMLInputElement).value = '';
      const prod = productos.find(p => p.codigo === code);
      if (prod) {
        agregarProducto(prod);
      } else {
        setError(`Código no encontrado: ${code}`);
        setTimeout(() => setError(''), 2500);
      }
    };

    const totalCosto = items.reduce((a, i) => a + i.cantidad * i.precio_costo, 0);
    const totalUnidades = items.reduce((a, i) => a + i.cantidad, 0);

    const handleSubmit = async () => {
      if (!usuario) return;
      if (items.length === 0) return setError('Agrega al menos un producto');
      setGuardando(true); setError('');
      try {
        await invoke('crear_recepcion', {
          recepcion: {
            usuario_id: usuario.id,
            proveedor_id: proveedorId || null,
            orden_id: ordenId || null,
            notas: notas || null,
            items: items.map(i => ({
              producto_id: i.producto.id,
              cantidad: i.cantidad,
              precio_costo: i.precio_costo,
              // Solo mandamos precio_venta si:
              //   1. tiene costo (no se aplica a productos con costo 0 — regla del dueño)
              //   2. el usuario lo modificó respecto al precio actual del producto
              // Así no sobrescribimos accidentalmente productos legacy sin costo.
              precio_venta:
                i.precio_costo > 0 && i.precio_venta > 0 && i.precio_venta !== i.producto.precio_venta
                  ? i.precio_venta
                  : null,
            })),
          },
        });
        await cargarDatos();
        await cargarTodo();
        setVista('lista');
      } catch (err: any) {
        setError(err?.toString() || 'Error al guardar recepción');
      }
      setGuardando(false);
    };

    return (
      <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
        <div className="recepcion-subheader" style={{
          padding: '10px 20px', borderBottom: '1px solid var(--color-border)',
          background: 'var(--color-surface)', display: 'flex', alignItems: 'center', gap: 10,
        }}>
          <button className="btn btn-ghost btn-sm" onClick={() => setVista('lista')}>
            <ArrowLeft size={16} /> Volver
          </button>
          <TruckIcon size={18} style={{ color: 'var(--color-primary)' }} />
          <h2 style={{ fontSize: 16, fontWeight: 800 }}>Nueva Recepción</h2>
          {ordenId && (
            <span style={{
              fontSize: 11, fontWeight: 700, padding: '3px 10px', borderRadius: 10,
              background: 'rgba(108,117,246,0.12)', color: '#6c75f6',
              display: 'flex', alignItems: 'center', gap: 4,
            }}>
              <ClipboardList size={11} /> Contra pedido #{ordenId}
            </span>
          )}
        </div>

        <div className="recepcion-grid" style={{ flex: 1, display: 'grid', gridTemplateColumns: '1fr 380px', minHeight: 0 }}>
          {/* Izquierda — escaneo + lista */}
          <div className="recepcion-col-left" style={{ display: 'flex', flexDirection: 'column', minHeight: 0, borderRight: '1px solid var(--color-border)' }}>
            {/* Barra de escaneo */}
            <div style={{ padding: '14px 16px', borderBottom: '1px solid var(--color-border)', background: 'var(--color-surface)' }}>
              <label style={labelStyle}>
                <Barcode size={12} style={{ display: 'inline', marginRight: 4, verticalAlign: 'middle' }} />
                ESCANEAR CÓDIGO
              </label>
              <input
                ref={scanRef}
                className="input mono"
                placeholder="Escanea el código del producto…"
                onKeyDown={handleScan}
                style={{ fontSize: 18, padding: '10px 14px', textAlign: 'center' }}
                autoFocus
              />
              {flash && (
                <div style={{
                  marginTop: 8, padding: '6px 10px', borderRadius: 6,
                  background: 'rgba(34,179,120,0.15)', color: '#22b378',
                  fontSize: 13, fontWeight: 600, textAlign: 'center',
                  animation: 'fade-in 0.15s ease',
                }}>
                  + {flash.cantidad} · {flash.nombre}
                </div>
              )}
              {error && !flash && (
                <div style={{
                  marginTop: 8, padding: '6px 10px', borderRadius: 6,
                  background: 'rgba(220,53,69,0.12)', color: 'var(--color-danger)',
                  fontSize: 12, textAlign: 'center',
                }}>
                  {error}
                </div>
              )}
            </div>

            {/* Buscador manual (fallback) */}
            <div style={{ padding: '10px 16px', borderBottom: '1px solid var(--color-border)', position: 'relative' }}>
              <div style={{ position: 'relative' }}>
                <Search size={14} style={{ position: 'absolute', left: 10, top: '50%', transform: 'translateY(-50%)', color: 'var(--color-text-dim)' }} />
                <input className="input"
                  placeholder="¿No escanea? Buscar por nombre…"
                  value={buscarTexto}
                  onChange={e => setBuscarTexto(e.target.value)}
                  style={{ paddingLeft: 30, fontSize: 12 }} />
              </div>
              {buscarResultados.length > 0 && (
                <div className="card" style={{
                  position: 'absolute', top: '100%', left: 16, right: 16, zIndex: 10,
                  maxHeight: 260, overflow: 'auto', padding: 0, marginTop: 2,
                }}>
                  {buscarResultados.map(p => (
                    <button key={p.id}
                      onClick={() => { agregarProducto(p); setBuscarTexto(''); scanRef.current?.focus(); }}
                      style={{
                        display: 'flex', justifyContent: 'space-between', width: '100%',
                        padding: '8px 12px', border: 'none', background: 'transparent',
                        color: 'var(--color-text)', cursor: 'pointer',
                        borderBottom: '1px solid var(--color-border)',
                        textAlign: 'left', fontSize: 12,
                      }}
                      onMouseEnter={e => (e.currentTarget.style.background = 'var(--color-surface-2)')}
                      onMouseLeave={e => (e.currentTarget.style.background = 'transparent')}
                    >
                      <span><span className="mono" style={{ fontSize: 10, color: 'var(--color-text-dim)' }}>{p.codigo}</span> {p.nombre}</span>
                    </button>
                  ))}
                </div>
              )}
            </div>

            {/* Lista de items escaneados */}
            <div style={{ flex: 1, overflow: 'auto' }}>
              {items.length === 0 ? (
                <div style={{ padding: 40, textAlign: 'center', color: 'var(--color-text-dim)' }}>
                  <PackagePlus size={48} strokeWidth={1.2} style={{ marginBottom: 12 }} />
                  <p style={{ fontSize: 14, fontWeight: 600 }}>Escanea o busca productos para registrarlos</p>
                  <p style={{ fontSize: 12 }}>Los productos se sumarán al stock cuando confirmes</p>
                </div>
              ) : (
                <table className="recepcion-items-table" style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
                  <thead style={{ position: 'sticky', top: 0, background: 'var(--color-surface-2)', zIndex: 1 }}>
                    <tr style={{ fontSize: 10, fontWeight: 600, color: 'var(--color-text-dim)', textTransform: 'uppercase' }}>
                      <th style={{ padding: '8px 12px', textAlign: 'left' }}>Producto</th>
                      <th style={{ padding: '8px 6px', width: 120 }}>Cantidad</th>
                      {ordenId && <th style={{ padding: '8px 6px', width: 70 }}>Pedido</th>}
                      <th style={{ padding: '8px 8px', width: 200 }}>Costo</th>
                      <th style={{ padding: '8px 8px', width: 92 }}>Venta</th>
                      <th style={{ padding: '8px 12px', width: 80, textAlign: 'right' }}>Total</th>
                      <th style={{ width: 28 }}></th>
                    </tr>
                  </thead>
                  <tbody>
                    {items.map((item, idx) => {
                      const excede = item.pedido_cantidad !== undefined && item.cantidad > item.pedido_cantidad;
                      // El multiplicador se desactiva si no hay costo (regla del dueño:
                      // no tocar productos legacy sin costo registrado).
                      const sinCosto = item.precio_costo <= 0;
                      const aplicarMultiplicador = (factor: number) => {
                        const n = [...items];
                        n[idx] = { ...n[idx], precio_venta: Number((n[idx].precio_costo * factor).toFixed(2)) };
                        setItems(n);
                      };
                      return (
                        <tr key={idx} style={{ borderBottom: '1px solid var(--color-border)' }}>
                          <td style={{ padding: '8px 12px' }}>
                            <div style={{ fontWeight: 600, fontSize: 13 }}>{item.producto.nombre}</div>
                            <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 2 }}>
                              <span className="mono" style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>{item.producto.codigo}</span>
                              <span style={{
                                fontSize: 10, fontWeight: 700,
                                padding: '1px 6px', borderRadius: 8,
                                background: item.producto.stock_actual <= item.producto.stock_minimo
                                  ? 'rgba(239, 68, 68, 0.15)' : 'rgba(148, 163, 184, 0.15)',
                                color: item.producto.stock_actual <= item.producto.stock_minimo
                                  ? 'var(--color-danger)' : 'var(--color-text-muted)',
                              }}>
                                Stock: {item.producto.stock_actual}
                              </span>
                            </div>
                          </td>
                          <td style={{ padding: '4px 6px' }}>
                            <div style={{ display: 'flex', alignItems: 'center', gap: 2, justifyContent: 'center' }}>
                              <button className="btn btn-ghost btn-sm" style={{ padding: '2px 4px' }}
                                onClick={() => {
                                  const n = [...items]; const c = Math.max(1, n[idx].cantidad - 1);
                                  n[idx] = { ...n[idx], cantidad: c }; setItems(n);
                                }}>
                                <Minus size={12} />
                              </button>
                              <input className="input mono" type="number" min={1} value={item.cantidad}
                                style={{ width: 52, padding: '2px 4px', textAlign: 'center', fontSize: 13 }}
                                onChange={e => { const n = [...items]; n[idx] = { ...n[idx], cantidad: Number(e.target.value) || 1 }; setItems(n); }} />
                              <button className="btn btn-ghost btn-sm" style={{ padding: '2px 4px' }}
                                onClick={() => { const n = [...items]; n[idx] = { ...n[idx], cantidad: n[idx].cantidad + 1 }; setItems(n); }}>
                                <PlusIcon size={12} />
                              </button>
                            </div>
                          </td>
                          {ordenId && (
                            <td className="mono" style={{ padding: '4px 6px', textAlign: 'center', fontSize: 12,
                              color: excede ? 'var(--color-warning)' : 'var(--color-text-dim)' }}>
                              {item.pedido_cantidad ?? '—'}
                            </td>
                          )}
                          <td style={{ padding: '4px 8px' }}>
                            <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
                              <input className="input mono" type="number" step="0.01" value={item.precio_costo}
                                style={{ width: 78, padding: '2px 6px', textAlign: 'right', fontSize: 12 }}
                                onChange={e => { const n = [...items]; n[idx] = { ...n[idx], precio_costo: Number(e.target.value) || 0 }; setItems(n); }} />
                              {/* Multiplicadores 1.4 / 1.5 / 1.7 → calculan precio_venta */}
                              {MULTIPLICADORES.map(m => (
                                <button
                                  key={m}
                                  type="button"
                                  className="btn btn-ghost btn-sm"
                                  disabled={sinCosto}
                                  title={sinCosto ? 'Captura primero el costo' : `Calcular precio de venta = costo × ${m}`}
                                  onClick={() => aplicarMultiplicador(m)}
                                  style={{
                                    padding: '2px 6px', fontSize: 10, fontWeight: 700,
                                    minWidth: 32,
                                    opacity: sinCosto ? 0.4 : 1,
                                    cursor: sinCosto ? 'not-allowed' : 'pointer',
                                  }}>
                                  ×{m}
                                </button>
                              ))}
                            </div>
                          </td>
                          <td style={{ padding: '4px 8px' }}>
                            <input className="input mono" type="number" step="0.01" value={item.precio_venta}
                              style={{ width: 80, padding: '2px 6px', textAlign: 'right', fontSize: 12 }}
                              onChange={e => { const n = [...items]; n[idx] = { ...n[idx], precio_venta: Number(e.target.value) || 0 }; setItems(n); }} />
                          </td>
                          <td className="mono" style={{ padding: '8px 12px', textAlign: 'right', fontWeight: 700, color: 'var(--color-primary)' }}>
                            {fmt(item.cantidad * item.precio_costo)}
                          </td>
                          <td style={{ padding: '4px' }}>
                            <button className="btn btn-ghost btn-sm" style={{ padding: '2px 4px', color: 'var(--color-danger)' }}
                              onClick={() => setItems(items.filter((_, i) => i !== idx))}>
                              <Trash2 size={12} />
                            </button>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              )}
            </div>
          </div>

          {/* Derecha — resumen / metadatos */}
          <div className="recepcion-col-right" style={{ display: 'flex', flexDirection: 'column', minHeight: 0, background: 'var(--color-surface)' }}>
            <div style={{ padding: '14px 16px', display: 'flex', flexDirection: 'column', gap: 12, flex: 1, overflow: 'auto' }}>
              <div>
                <label style={labelStyle}>RECIBIR CONTRA PEDIDO (OPCIONAL)</label>
                <select className="input" value={ordenId}
                  onChange={e => {
                    const val = e.target.value ? Number(e.target.value) : '';
                    if (items.length > 0 && val !== ordenId) {
                      if (!confirm('Cambiar el pedido vinculado reemplazará los items actuales. ¿Continuar?')) return;
                      setItems([]);
                    }
                    setOrdenId(val);
                    if (val) cargarOrden(val);
                  }}>
                  <option value="">Recepción libre (sin pedido previo)</option>
                  {ordenes.map(o => (
                    <option key={o.id} value={o.id}>
                      #{o.id} · {o.proveedor_nombre || 'Sin proveedor'} · {o.total_items} items
                    </option>
                  ))}
                </select>
                {ordenId && (
                  <p style={{ fontSize: 11, color: 'var(--color-text-dim)', marginTop: 4 }}>
                    Las cantidades pendientes se pre-llenaron. Al confirmar, se marcará el pedido como recibido (total o parcial).
                  </p>
                )}
              </div>

              <div>
                <label style={labelStyle}>PROVEEDOR</label>
                <select className="input" value={proveedorId}
                  onChange={e => setProveedorId(e.target.value ? Number(e.target.value) : '')}
                  disabled={!!ordenId}>
                  <option value="">Sin proveedor</option>
                  {proveedores.map(p => <option key={p.id} value={p.id}>{p.nombre}</option>)}
                </select>
              </div>

              <div>
                <label style={labelStyle}>NOTAS (FACTURA, REMISIÓN…)</label>
                <input className="input" placeholder="Ej. Factura #123 - 15 cajas" value={notas} onChange={e => setNotas(e.target.value)} />
              </div>

              {/* Totales */}
              <div style={{ marginTop: 'auto', display: 'flex', flexDirection: 'column', gap: 6, padding: 12, background: 'var(--color-surface-2)', borderRadius: 8 }}>
                <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: 12 }}>
                  <span style={{ color: 'var(--color-text-muted)' }}>Productos</span>
                  <span className="mono" style={{ fontWeight: 700 }}>{items.length}</span>
                </div>
                <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: 12 }}>
                  <span style={{ color: 'var(--color-text-muted)' }}>Unidades totales</span>
                  <span className="mono" style={{ fontWeight: 700 }}>{totalUnidades}</span>
                </div>
                <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: 14, fontWeight: 700, marginTop: 4, paddingTop: 6, borderTop: '1px solid var(--color-border)' }}>
                  <span style={{ color: 'var(--color-text-muted)' }}>Costo total</span>
                  <span className="mono" style={{ color: 'var(--color-primary)', fontSize: 17 }}>{fmt(totalCosto)}</span>
                </div>
              </div>
            </div>

            {error && !flash && (
              <p style={{ color: 'var(--color-danger)', fontSize: 12, padding: '0 16px 8px' }}>{error}</p>
            )}

            <div style={{ padding: '12px 16px', borderTop: '1px solid var(--color-border)' }}>
              <button className="btn btn-success btn-lg" style={{ width: '100%', justifyContent: 'center' }}
                disabled={guardando || items.length === 0} onClick={handleSubmit}>
                <PackagePlus size={16} />
                {guardando ? 'Guardando…' : `Confirmar Recepción (${totalUnidades} uds)`}
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
    const { recep, items } = detalle;
    const totalCosto = items.reduce((a, i) => a + i.cantidad * i.precio_costo, 0);
    const totalUnidades = items.reduce((a, i) => a + i.cantidad, 0);

    return (
      <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
        <div style={{
          padding: '10px 20px', borderBottom: '1px solid var(--color-border)',
          background: 'var(--color-surface)', display: 'flex', alignItems: 'center', gap: 10,
        }}>
          <button className="btn btn-ghost btn-sm" onClick={() => { setDetalle(null); setVista('lista'); }}>
            <ArrowLeft size={16} /> Volver
          </button>
          <TruckIcon size={18} style={{ color: 'var(--color-primary)' }} />
          <h2 style={{ fontSize: 16, fontWeight: 800 }}>Recepción #{recep.id}</h2>
        </div>

        <div style={{ flex: 1, overflow: 'auto', padding: 20 }}>
          <div className="recepcion-detalle-tiles" style={{ display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: 12, marginBottom: 20 }}>
            <Tile label="Fecha" value={formatFecha(recep.fecha)} />
            <Tile label="Proveedor" value={recep.proveedor_nombre || '—'} />
            <Tile label="Recibió" value={recep.usuario_nombre} />
            <Tile label="Costo total" value={fmt(totalCosto)} highlight />
          </div>
          <div className="recepcion-detalle-tiles-2" style={{ display: 'grid', gridTemplateColumns: 'repeat(2, 1fr)', gap: 12, marginBottom: 16 }}>
            <Tile label="Productos" value={String(items.length)} />
            <Tile label="Unidades" value={String(totalUnidades)} />
          </div>

          <div className="card" style={{ padding: 0, overflow: 'hidden' }}>
            <table className="recepcion-detalle-table" style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
              <thead>
                <tr style={{ background: 'var(--color-surface-2)', fontSize: 11, fontWeight: 600, color: 'var(--color-text-dim)', textTransform: 'uppercase' }}>
                  <th style={{ padding: '8px 12px', textAlign: 'left' }}>Producto</th>
                  <th style={{ padding: '8px 12px' }}>Cantidad</th>
                  <th style={{ padding: '8px 12px' }}>Costo</th>
                  <th style={{ padding: '8px 12px', textAlign: 'right' }}>Total</th>
                </tr>
              </thead>
              <tbody>
                {items.map(i => (
                  <tr key={i.id} style={{ borderBottom: '1px solid var(--color-border)' }}>
                    <td style={{ padding: '8px 12px' }}>
                      <div style={{ fontWeight: 600 }}>{i.producto_nombre}</div>
                      <div className="mono" style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>{i.producto_codigo}</div>
                    </td>
                    <td className="mono" style={{ padding: '8px 12px', textAlign: 'center', fontWeight: 700, color: '#22b378' }}>
                      +{i.cantidad}
                    </td>
                    <td className="mono" style={{ padding: '8px 12px', textAlign: 'center' }}>{fmt(i.precio_costo)}</td>
                    <td className="mono" style={{ padding: '8px 12px', textAlign: 'right', fontWeight: 700 }}>
                      {fmt(i.cantidad * i.precio_costo)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          {recep.notas && (
            <div className="card" style={{ padding: 12, marginTop: 14 }}>
              <div style={labelStyle}>NOTAS</div>
              <p style={{ fontSize: 13, color: 'var(--color-text-muted)' }}>{recep.notas}</p>
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
          <button className="btn btn-primary" onClick={() => setVista('crear')}>
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
            <button className="btn btn-primary" onClick={() => setVista('crear')}>
              <Plus size={16} /> Registrar primera
            </button>
          </div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column' }}>
            {recepciones.map(r => (
              <div key={r.id}
                className="recepcion-list-row"
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
