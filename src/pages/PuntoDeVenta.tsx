// pages/PuntoDeVenta.tsx — Pantalla principal de ventas del POS
// Carrito + escaneo + búsqueda + cobro

import { useState, useEffect, useRef, useCallback } from 'react';
import { useProductStore } from '../store/productStore';
import { useVentaStore, useVentaActiva, type MetodoPago } from '../store/ventaStore';
import { useAuthStore } from '../store/authStore';
import { Search, X, Minus, Plus, Trash2, CreditCard, Banknote, ArrowRightLeft, CheckCircle2, User, Percent, Lock, Plus as PlusIcon, Printer, FileText, Save, ShoppingCart } from 'lucide-react';
import { invoke } from '../lib/invokeCompat';
import { imprimirTicket, type ConfigNegocio, type TicketData } from '../utils/ticket';

export default function PuntoDeVenta() {
  const { productos, cargarTodo, busqueda, setBusqueda, productosFiltrados } = useProductStore();
  const {
    agregarProducto, quitarProducto, cambiarCantidad,
    total, totalSinRedondeo, redondeo, numItems,
    setMetodoPago, setMontoRecibido,
    procesarVenta, ventaExitosa, cerrarVentaExitosa, procesando,
    clientes, cargarClientes, seleccionarCliente,
    tabs, tabActivaId, nuevaTab, cerrarTab, activarTab,
    setModo, setNotasPresupuesto, setVigenciaPresupuesto, guardarComoPresupuesto,
  } = useVentaStore();
  const activa = useVentaActiva();
  const { items, clienteSeleccionado, metodoPago, montoRecibido, modo, presupuestoOrigen, notasPresupuesto, vigenciaPresupuesto } = activa;
  const { usuario } = useAuthStore();

  const [showCobro, setShowCobro] = useState(false);
  const [showMobileTabs, setShowMobileTabs] = useState(false);
  const [showBusqueda, setShowBusqueda] = useState(false);
  const [showDescuento, setShowDescuento] = useState<number | null>(null); // index del item
  const [descPorcentaje, setDescPorcentaje] = useState('');
  const [showPinAuth, setShowPinAuth] = useState(false);
  const [pinAuth, setPinAuth] = useState('');
  const [pinError, setPinError] = useState(false);
  const [confirmCerrarTab, setConfirmCerrarTab] = useState<string | null>(null);
  const [presupGuardado, setPresupGuardado] = useState<{ folio: string } | null>(null);
  const [mobileCartOpen, setMobileCartOpen] = useState(false);
  const [maxDescVendedor, setMaxDescVendedor] = useState(15);
  const [configNegocio, setConfigNegocio] = useState<ConfigNegocio | null>(null);
  const [ultimoTicket, setUltimoTicket] = useState<TicketData | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const scanRef = useRef<HTMLInputElement>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [localSearch, setLocalSearch] = useState('');

  // Cargar datos al montar
  useEffect(() => {
    cargarTodo();
    cargarClientes();
    // Cargar config de descuentos
    invoke<{ descuento_max_vendedor_pct: number }>('obtener_config_descuentos')
      .then(c => setMaxDescVendedor(c.descuento_max_vendedor_pct))
      .catch(() => {});
    invoke<ConfigNegocio>('obtener_config_negocio')
      .then(c => setConfigNegocio(c))
      .catch(() => {});
  }, []);

  // Focus automático en el campo de escaneo
  useEffect(() => {
    if (!showCobro && !showBusqueda && !ventaExitosa && scanRef.current) {
      // Limpiar el campo y el estado de búsqueda para evitar que se reabra el dropdown
      scanRef.current.value = '';
      setLocalSearch('');
      setBusqueda('');
      scanRef.current.focus();
    }
  }, [showCobro, showBusqueda, ventaExitosa, items]);

  // Manejar escaneo (código de barras vía HID)
  const handleScan = useCallback((e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      const code = (e.target as HTMLInputElement).value.trim();
      if (!code) return;
      const prod = productos.find(p => p.codigo === code);
      if (prod) {
        // Se permite vender sin stock (queda en negativo)
        agregarProducto(prod);
      } else {
        // Producto no encontrado — mostrar búsqueda
        setBusqueda(code);
        setShowBusqueda(true);
      }
      (e.target as HTMLInputElement).value = '';
    }
  }, [productos, agregarProducto, setBusqueda]);

  // Cerrar tab con confirmación si tiene items
  const handleCerrarTab = (id: string) => {
    const t = tabs.find(x => x.id === id);
    if (t && t.items.length > 0) {
      setConfirmCerrarTab(id);
      return;
    }
    cerrarTab(id);
  };

  // Atajos de teclado
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (ventaExitosa) {
        if (e.key === 'Enter' || e.key === 'Escape') cerrarVentaExitosa();
        return;
      }
      // Ctrl+B = buscar
      if (e.ctrlKey && e.key === 'b') { e.preventDefault(); setShowBusqueda(true); }
      // Ctrl+T = nueva pestaña
      if (e.ctrlKey && (e.key === 't' || e.key === 'T')) { e.preventDefault(); nuevaTab(); }
      // Ctrl+W = cerrar pestaña activa
      if (e.ctrlKey && (e.key === 'w' || e.key === 'W')) { e.preventDefault(); handleCerrarTab(tabActivaId); }
      // Ctrl+1..9 = activar pestaña
      if (e.ctrlKey && /^[1-9]$/.test(e.key)) {
        const idx = Number(e.key) - 1;
        if (tabs[idx]) { e.preventDefault(); activarTab(tabs[idx].id); }
      }
      // F10 = cobrar (solo en modo venta)
      if (e.key === 'F10' && items.length > 0 && modo === 'venta') { e.preventDefault(); setShowCobro(true); setMontoRecibido(0); }
      // Escape = cerrar overlay
      if (e.key === 'Escape') { setShowCobro(false); setShowBusqueda(false); }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [items, ventaExitosa, tabs, tabActivaId, modo]);

  const handleCobrar = async () => {
    if (!usuario) return;
    const montoFinal = metodoPago === 'efectivo' ? montoRecibido : total();
    if (metodoPago === 'efectivo' && montoFinal < total()) return;
    if (metodoPago !== 'efectivo') setMontoRecibido(total());

    // Snapshot para ticket antes de que se limpie el carrito
    const itemsSnapshot = items.map(i => ({
      nombre: i.producto.nombre,
      codigo: i.producto.codigo,
      cantidad: i.cantidad,
      precio_final: i.precioFinal,
      subtotal: i.subtotal,
      descuento_porcentaje: i.descuentoPorcentaje,
    }));
    const { subtotal, descuentoTotal } = useVentaStore.getState();
    const subtotalSnap = subtotal();
    const descuentoSnap = descuentoTotal();
    const redondeoSnap = redondeo();
    const totalSnap = total();
    const clienteSnap = clienteSeleccionado?.nombre || null;
    const metodoSnap = metodoPago;
    const recibidoSnap = metodoPago === 'efectivo' ? montoRecibido : totalSnap;

    try {
      const venta = await procesarVenta(usuario.id);
      setShowCobro(false);
      setMobileCartOpen(false);

      const ticket: TicketData = {
        folio: venta.folio,
        fecha: venta.fecha,
        usuario: usuario.nombre_completo,
        cliente: clienteSnap,
        items: itemsSnapshot,
        subtotal: subtotalSnap,
        descuento: descuentoSnap,
        redondeo: redondeoSnap,
        total: venta.total,
        metodo_pago: metodoSnap,
        monto_recibido: recibidoSnap,
        cambio: venta.cambio,
      };
      setUltimoTicket(ticket);
      // Auto-impresión deshabilitada por petición del usuario
    } catch (err: any) {
      alert(err.message);
    }
  };

  const handleGuardarPresupuesto = async () => {
    if (!usuario) return;
    if (items.length === 0) return;
    try {
      const result = await guardarComoPresupuesto(usuario.id);
      setPresupGuardado({ folio: result.folio });
      // Cerrar la pestaña actual (ya se guardó)
      cerrarTab(tabActivaId);
    } catch (err: any) {
      alert(err?.message || 'Error al guardar presupuesto');
    }
  };

  const reimprimirUltimo = () => {
    if (ultimoTicket && configNegocio) {
      imprimirTicket(configNegocio, { ...ultimoTicket, reimpresion: true });
    }
  };

  const fmt = (n: number) => `$${n.toFixed(2)}`;
  const esAdmin = usuario?.es_admin ?? false;

  // Descuento: aplicar
  const handleAplicarDescuento = async () => {
    if (showDescuento === null) return;
    const pct = parseFloat(descPorcentaje) || 0;
    if (pct <= 0 || pct > 100) return;

    // Si excede el límite del vendedor y no es admin, pedir PIN
    if (!esAdmin && pct > maxDescVendedor && !showPinAuth) {
      setShowPinAuth(true);
      setPinAuth('');
      setPinError(false);
      return;
    }

    const { aplicarDescuento } = useVentaStore.getState();
    aplicarDescuento(showDescuento, pct, showPinAuth ? usuario?.id ?? null : null);
    setShowDescuento(null);
    setDescPorcentaje('');
    setShowPinAuth(false);
    setPinAuth('');
  };

  const handlePinAuth = async () => {
    if (pinAuth.length !== 4) return;
    try {
      const ok = await invoke<boolean>('verificar_pin_dueno', { pin: pinAuth });
      if (ok) {
        // PIN válido — aplicar descuento
        const pct = parseFloat(descPorcentaje) || 0;
        if (showDescuento !== null) {
          const { aplicarDescuento } = useVentaStore.getState();
          aplicarDescuento(showDescuento, pct, usuario?.id ?? null);
        }
        setShowDescuento(null);
        setDescPorcentaje('');
        setShowPinAuth(false);
        setPinAuth('');
      } else {
        setPinError(true);
        setPinAuth('');
        setTimeout(() => setPinError(false), 600);
      }
    } catch {
      setPinError(true);
      setPinAuth('');
      setTimeout(() => setPinError(false), 600);
    }
  };

  // ──── Render ────

  // Overlay de venta exitosa
  if (ventaExitosa) {
    return (
      <div className="animate-fade-in" style={{
        display: 'flex', alignItems: 'center', justifyContent: 'center',
        height: '100%', flexDirection: 'column', gap: 20, padding: 20
      }}>
        <div style={{ textAlign: 'center', display: 'flex', flexDirection: 'column', alignItems: 'center' }}>
          <CheckCircle2 size={64} style={{ color: 'var(--color-success)', marginBottom: 12 }} />
          <h2 style={{ fontSize: 28, fontWeight: 800, color: 'var(--color-text)' }}>¡Venta Completada!</h2>
          <p className="mono" style={{ fontSize: 18, color: 'var(--color-text-muted)', marginTop: 4 }}>
            Folio: {ventaExitosa.folio}
          </p>
        </div>
        <div className="card pos-modal-content pos-modal-fluid" style={{ padding: 24, width: '100%', maxWidth: 400, textAlign: 'center' }}>
          {ventaExitosa.cambio > 0 ? (
            <>
              <p style={{ fontSize: 18, color: 'var(--color-text-muted)', fontWeight: 600, textTransform: 'uppercase', letterSpacing: 1 }}>
                Cambio a entregar
              </p>
              <div className="price-display" style={{ color: 'var(--color-danger)', fontSize: 56, marginTop: 8, lineHeight: 1 }}>
                {fmt(ventaExitosa.cambio)}
              </div>
            </>
          ) : (
            <div className="price-display" style={{ color: 'var(--color-success)', fontSize: 40, lineHeight: 1.2 }}>
              Pago Exacto
            </div>
          )}
        </div>
        <div className="pos-page-header" style={{ display: 'flex', gap: 10, justifyContent: 'center' }}>
          {ultimoTicket && (
            <button className="btn btn-ghost btn-lg" onClick={reimprimirUltimo} title="Imprimir ticket de venta">
              <Printer size={18} /> Imprimir Ticket
            </button>
          )}
          <button className="btn btn-primary btn-lg" onClick={cerrarVentaExitosa}>
            Nueva Venta (Enter)
          </button>
        </div>
      </div>
    );
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}>
      {/* ─── Barra de pestañas ─── */}
      <div className="pos-tabs-scroll" style={{
        display: 'flex', alignItems: 'center', gap: 2,
        padding: '6px 10px 0', background: 'var(--color-surface)',
        borderBottom: '1px solid var(--color-border)', overflowX: 'auto',
        flexShrink: 0,
      }}>
        {/* Envolver pestañas clásicas para que no se vean en celular */}
        <div className="pos-hide-mobile" style={{ display: 'flex', gap: 2, flexShrink: 0 }}>
          {tabs.map(t => {
            const isActiva = t.id === tabActivaId;
            const count = t.items.reduce((a, i) => a + i.cantidad, 0);
            const isPresup = t.modo === 'presupuesto';
            return (
              <div key={t.id}
                onClick={() => activarTab(t.id)}
                style={{
                  display: 'flex', alignItems: 'center', gap: 6,
                  padding: '6px 10px', borderRadius: '6px 6px 0 0',
                  background: isActiva ? 'var(--color-bg)' : 'transparent',
                  border: '1px solid ' + (isActiva ? 'var(--color-border)' : 'transparent'),
                  borderBottom: isActiva ? '1px solid var(--color-bg)' : '1px solid transparent',
                  marginBottom: -1, cursor: 'pointer', fontSize: 13,
                  fontWeight: isActiva ? 700 : 500,
                  color: isActiva ? 'var(--color-text)' : 'var(--color-text-muted)',
                  whiteSpace: 'nowrap',
                }}
              >
                {isPresup && <FileText size={12} style={{ color: '#e6a817' }} />}
                <span>{t.nombre}</span>
                {count > 0 && (
                  <span style={{
                    fontSize: 10, fontWeight: 700,
                    background: 'var(--color-primary)', color: '#fff',
                    padding: '1px 6px', borderRadius: 10, minWidth: 18, textAlign: 'center',
                  }}>{count}</span>
                )}
                <button
                  onClick={(e) => { e.stopPropagation(); handleCerrarTab(t.id); }}
                  style={{
                    border: 'none', background: 'transparent', cursor: 'pointer',
                    color: 'var(--color-text-dim)', padding: 0, display: 'flex',
                  }}
                  title="Cerrar pestaña (Ctrl+W)"
                >
                  <X size={12} />
                </button>
              </div>
            );
          })}
          <button
            className="btn btn-ghost btn-sm"
            onClick={() => nuevaTab()}
            title="Nueva venta (Ctrl+T)"
            style={{ padding: '4px 8px', marginLeft: 4, marginBottom: 2 }}
          >
            <PlusIcon size={14} />
          </button>
        </div>

        {/* SELECTOR MÓVIL (oculto en desktop) */}
        {(() => {
          const tActiva = tabs.find(t => t.id === tabActivaId);
          const c = tActiva ? tActiva.items.reduce((s,i)=>s+i.cantidad, 0) : 0;
          return (
            <button
              className="pos-show-mobile-flex btn btn-ghost btn-sm"
              onClick={() => setShowMobileTabs(true)}
              style={{
                padding: '6px 12px', fontWeight: 700, borderRadius: 6, gap: 8,
                background: 'var(--color-surface)', border: '1px solid var(--color-border)',
                marginBottom: 4, whiteSpace: 'nowrap'
              }}
            >
              <ShoppingCart size={14} style={{ color: 'var(--color-primary)' }} />
              {tActiva?.nombre || 'Venta'} {c > 0 && `(${c})`}
              <span style={{ fontSize: 10, opacity: 0.5, marginLeft: 2 }}>▼</span>
            </button>
          );
        })()}

        {/* Toggle Venta/Presupuesto para la pestaña activa */}
        <div style={{ marginLeft: 'auto', marginRight: 6, marginBottom: 4, display: 'flex', gap: 4, alignItems: 'center' }}>
          {presupuestoOrigen && (
            <span style={{
              fontSize: 11, fontWeight: 700, padding: '3px 10px', borderRadius: 10,
              background: 'rgba(108,117,246,0.12)', color: '#6c75f6',
              display: 'flex', alignItems: 'center', gap: 4,
            }} title="Esta venta convertirá el presupuesto original">
              <FileText size={11} /> Convirtiendo {presupuestoOrigen.folio}
            </span>
          )}
          <div style={{
            display: 'flex', background: 'var(--color-surface-2)',
            borderRadius: 6, padding: 2, fontSize: 12,
          }}>
            <button
              onClick={() => setModo('venta')}
              disabled={!!presupuestoOrigen}
              style={{
                padding: '4px 10px', border: 'none', borderRadius: 4, cursor: presupuestoOrigen ? 'not-allowed' : 'pointer',
                background: modo === 'venta' ? 'var(--color-primary)' : 'transparent',
                color: modo === 'venta' ? '#fff' : 'var(--color-text-muted)',
                fontWeight: 600, display: 'flex', alignItems: 'center', gap: 4,
                opacity: presupuestoOrigen ? 0.6 : 1,
              }}
              title="Venta (F10 para cobrar)"
            >
              <Banknote size={12} /> Venta
            </button>
            <button
              onClick={() => setModo('presupuesto')}
              disabled={!!presupuestoOrigen}
              style={{
                padding: '4px 10px', border: 'none', borderRadius: 4, cursor: presupuestoOrigen ? 'not-allowed' : 'pointer',
                background: modo === 'presupuesto' ? '#e6a817' : 'transparent',
                color: modo === 'presupuesto' ? '#fff' : 'var(--color-text-muted)',
                fontWeight: 600, display: 'flex', alignItems: 'center', gap: 4,
                opacity: presupuestoOrigen ? 0.6 : 1,
              }}
              title="Presupuesto (guardar cotización sin cobrar)"
            >
              <FileText size={12} /> Presupuesto
            </button>
          </div>
        </div>
      </div>

    <div className="pos-pdv-grid" style={{ display: 'grid', gridTemplateColumns: '1fr 360px', gridTemplateRows: 'minmax(0, 1fr)', flex: 1, minHeight: 0, gap: 0 }}>
      {/* ─── Panel Izquierdo: Productos ─── */}
      <div style={{ display: 'flex', flexDirection: 'column', borderRight: '1px solid var(--color-border)', minHeight: 0, minWidth: 0 }}>
        {/* Barra de escaneo */}
        <div style={{ padding: '10px 16px', borderBottom: '1px solid var(--color-border)', display: 'flex', gap: 8, position: 'relative', zIndex: 10 }}>
          <div style={{ flex: 1, position: 'relative' }}>
            <Search size={16} style={{
              position: 'absolute', left: 12, top: '50%', transform: 'translateY(-50%)',
              color: 'var(--color-text-dim)',
            }} />
            <input
              ref={scanRef}
              className="input pos-input-icon"
              placeholder="Buscar producto o escanear..."
              style={{ paddingLeft: 36, width: '100%' }}
              value={localSearch || ''}
              onKeyDown={handleScan}
              onChange={(e) => {
                const v = e.target.value;
                setLocalSearch(v);
                if (debounceRef.current) clearTimeout(debounceRef.current);
                if (v.length >= 2) {
                  debounceRef.current = setTimeout(() => {
                    setBusqueda(v);
                    setShowBusqueda(true);
                  }, 150);
                } else {
                  setShowBusqueda(false);
                }
              }}
              onFocus={() => {
                if (localSearch && localSearch.length >= 2) setShowBusqueda(true);
              }}
              onBlur={() => {
                // Pequeño delay para permitir clic en resultados
                setTimeout(() => setShowBusqueda(false), 200);
              }}
            />
            {/* Buscador flocado (Dropdown) */}
            {showBusqueda && busqueda && (
              <div className="card animate-fade-in" style={{
                position: 'absolute', top: '100%', left: 0, right: 0, marginTop: 4,
                maxHeight: '60vh', overflowY: 'auto', padding: 0, zIndex: 50,
                boxShadow: '0 10px 25px rgba(0,0,0,0.15)',
                display: 'flex', flexDirection: 'column'
              }}>
                {productosFiltrados().slice(0, 50).map((p) => {
                  const sinStock = p.stock_actual <= 0;
                  return (
                    <button
                      key={p.id}
                      style={{
                        display: 'flex', alignItems: 'center', justifyContent: 'space-between',
                        width: '100%', padding: '10px 16px', border: 'none',
                        background: 'transparent', color: 'var(--color-text)',
                        cursor: 'pointer', textAlign: 'left',
                        borderBottom: '1px solid var(--color-border)',
                      }}
                      onMouseEnter={(e) => { e.currentTarget.style.background = 'var(--color-surface-2)'; }}
                      onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}
                      onMouseDown={(e) => {
                        // Prevenir blur del input al hacer mousedown
                        e.preventDefault();
                      }}
                      onClick={() => {
                        agregarProducto(p);
                        setShowBusqueda(false);
                        setLocalSearch('');
                        setBusqueda('');
                        setTimeout(() => scanRef.current?.focus(), 50);
                      }}
                    >
                      <div>
                        <div style={{ fontWeight: 600, fontSize: 13, display: 'flex', alignItems: 'center', gap: 8 }}>
                          {p.nombre}
                          {sinStock && (
                            <span style={{
                              fontSize: 10, padding: '1px 8px', borderRadius: 10,
                              background: 'rgba(245,158,11,0.15)', color: 'var(--color-warning)',
                              fontWeight: 700,
                            }} title="Se venderá en negativo">Sin stock</span>
                          )}
                        </div>
                        <div style={{ fontSize: 11, color: 'var(--color-text-dim)', MathmarginTop: 2 }}>
                          <span className="mono">{p.codigo}</span>
                          <span style={{ color: p.stock_actual < 0 ? 'var(--color-danger)' : undefined, marginLeft: 6 }}>
                            · Stock: {p.stock_actual}
                          </span>
                        </div>
                      </div>
                      <div className="mono" style={{ fontWeight: 700, color: 'var(--color-primary)', fontSize: 13, textAlign: 'right' }}>
                        {fmt(p.precio_venta)}
                      </div>
                    </button>
                  );
                })}
                {productosFiltrados().length === 0 && busqueda && (
                  <div style={{ padding: 20, textAlign: 'center', color: 'var(--color-text-dim)', fontSize: 13 }}>
                    No se encontraron productos para "{busqueda}"
                  </div>
                )}
              </div>
            )}
          </div>
        </div>

        {/* Lista del carrito */}
        <div style={{ flex: 1, overflow: 'auto', padding: '8px 16px' }}>
          {items.length === 0 ? (
            <div style={{
              display: 'flex', alignItems: 'center', justifyContent: 'center',
              height: '100%', flexDirection: 'column', gap: 8,
              color: 'var(--color-text-dim)',
            }}>
              <Banknote size={40} strokeWidth={1.5} />
              <p style={{ fontSize: 14 }}>Escanea o busca un producto para empezar</p>
              <p style={{ fontSize: 12 }}>F10 para cobrar · Ctrl+B para buscar</p>
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              {/* Header */}
              <div className="pos-hide-mobile" style={{
                display: 'grid', gridTemplateColumns: '1fr 80px 110px 100px 40px',
                alignItems: 'center', gap: 8, padding: '4px 12px',
                fontSize: 11, fontWeight: 600, color: 'var(--color-text-dim)',
                textTransform: 'uppercase', letterSpacing: '0.5px',
              }}>
                <span>Producto</span>
                <span style={{ textAlign: 'center' }}>Cant.</span>
                <span style={{ textAlign: 'center' }}>Precio</span>
                <span style={{ textAlign: 'right' }}>Subtotal</span>
                <span></span>
              </div>
              {items.map((item, i) => (
                <div key={`${item.producto.id}-${i}`} className="cart-item" style={{
                  gridTemplateColumns: '1fr 80px 110px 100px 40px',
                }}>
                  {/* Producto */}
                  <div>
                    <div style={{ fontSize: 14, fontWeight: 600 }}>{item.producto.nombre}</div>
                    <div style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>
                      <span className="mono">{item.producto.codigo}</span>
                    </div>
                  </div>
                  {/* Cantidad */}
                  <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 4 }}>
                    <button className="btn btn-ghost btn-sm" style={{ padding: '2px 4px' }}
                      onClick={() => cambiarCantidad(i, item.cantidad - 1)}>
                      <Minus size={14} />
                    </button>
                    <span className="mono" style={{ fontWeight: 700, minWidth: 20, textAlign: 'center' }}>
                      {item.cantidad}
                    </span>
                    <button className="btn btn-ghost btn-sm" style={{ padding: '2px 4px' }}
                      onClick={() => cambiarCantidad(i, item.cantidad + 1)}>
                      <Plus size={14} />
                    </button>
                  </div>
                  {/* Precio + Descuento */}
                  <div style={{ textAlign: 'center' }}>
                    <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 2 }}>
                      <span className="mono" style={{ fontSize: 14, fontWeight: 600 }}>{fmt(item.precioFinal)}</span>
                      <button
                        className="btn btn-ghost btn-sm"
                        style={{ padding: '1px 3px', opacity: 0.5 }}
                        onClick={() => { setShowDescuento(i); setDescPorcentaje(item.descuentoPorcentaje > 0 ? String(item.descuentoPorcentaje) : ''); setShowPinAuth(false); }}
                        title="Aplicar descuento"
                      >
                        <Percent size={10} />
                      </button>
                    </div>
                    {item.descuentoPorcentaje > 0 && (
                      <div style={{ fontSize: 10, color: 'var(--color-warning)' }}>
                        -{item.descuentoPorcentaje}%
                      </div>
                    )}
                  </div>
                  {/* Subtotal */}
                  <div className="mono" style={{ textAlign: 'right', fontWeight: 700, fontSize: 14 }}>
                    {fmt(item.subtotal)}
                  </div>
                  {/* Quitar */}
                  <button className="btn btn-ghost btn-sm" style={{ padding: '2px 4px', color: 'var(--color-danger)' }}
                    onClick={() => quitarProducto(i)}>
                    <Trash2 size={14} />
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* ─── Panel Derecho: Resumen + Cobro ─── */}
      <div className={`pos-pdv-cart${mobileCartOpen ? ' open' : ''}`} style={{ display: 'flex', flexDirection: 'column', background: 'var(--color-surface)', minHeight: 0, minWidth: 0 }}>
        {/* Header móvil con botón cerrar (sólo se ve en mobile via CSS) */}
        <button
          onClick={() => setMobileCartOpen(false)}
          aria-label="Cerrar carrito"
          className="pos-cart-close"
          style={{
            display: 'none', alignItems: 'center', gap: 8, padding: '10px 14px',
            background: 'var(--color-surface-2)', border: 'none', borderBottom: '1px solid var(--color-border)',
            cursor: 'pointer', fontSize: 14, fontWeight: 700, color: 'var(--color-text)',
            justifyContent: 'space-between', width: '100%',
          }}
        >
          <span>Resumen del carrito</span>
          <X size={18} />
        </button>
        {/* Cliente */}
        <div style={{
          padding: '10px 16px',
          borderBottom: '1px solid var(--color-border)',
          display: 'flex', alignItems: 'center', gap: 8,
          flexShrink: 0,
        }}>
          <User size={16} style={{ color: 'var(--color-text-dim)' }} />
          <select
            value={clienteSeleccionado?.id || ''}
            onChange={(e) => {
              const id = e.target.value;
              if (!id) return seleccionarCliente(null);
              const cl = clientes.find(c => c.id === Number(id));
              if (cl) seleccionarCliente(cl);
            }}
            className="input"
            style={{ flex: 1, padding: '6px 10px', fontSize: 13 }}
          >
            <option value="">Público general (sin descuento)</option>
            {clientes.map(c => (
              <option key={c.id} value={c.id}>{c.nombre} {c.descuento_porcentaje > 0 ? `(-${c.descuento_porcentaje}%)` : ''}</option>
            ))}
          </select>
        </div>

        {/* Total grande */}
        <div style={{
          flex: 1, display: 'flex', flexDirection: 'column',
          justifyContent: 'center', alignItems: 'center', gap: 8,
          padding: 20,
        }}>
          <span style={{ fontSize: 12, fontWeight: 600, color: 'var(--color-text-muted)', textTransform: 'uppercase', letterSpacing: 1 }}>
            {modo === 'presupuesto' ? 'TOTAL COTIZACIÓN' : 'TOTAL'}
          </span>
          <div className="price-display" style={{ fontSize: 48, color: modo === 'presupuesto' ? '#e6a817' : undefined }}>
            {fmt(total())}
          </div>
          {/* Aviso de redondeo: solo en modo venta y cuando el raw tiene centavos */}
          {modo === 'venta' && redondeo() > 0 && (
            <div style={{
              fontSize: 11, color: 'var(--color-text-muted)',
              display: 'flex', alignItems: 'center', gap: 6,
              background: 'var(--color-surface-2)', padding: '3px 10px', borderRadius: 12,
              fontFamily: "'JetBrains Mono', monospace",
            }}
              title="El total se redondea hacia arriba al peso siguiente para evitar entregar centavos en el cambio."
            >
              {fmt(totalSinRedondeo())} <span>+</span> {fmt(redondeo())} <span style={{ color: 'var(--color-text-dim)' }}>redondeo</span>
            </div>
          )}
          <span style={{ fontSize: 13, color: 'var(--color-text-dim)' }}>
            {numItems()} artículo{numItems() !== 1 ? 's' : ''} en el carrito
          </span>

          {/* Método de pago — solo en modo venta */}
          {modo === 'venta' && (
            <div style={{ display: 'flex', gap: 6, marginTop: 12 }}>
              {(['efectivo', 'tarjeta', 'transferencia'] as MetodoPago[]).map((m) => (
                <button
                  key={m}
                  className={`btn ${metodoPago === m ? 'btn-primary' : 'btn-ghost'} btn-sm`}
                  onClick={() => setMetodoPago(m)}
                >
                  {m === 'efectivo' && <Banknote size={14} />}
                  {m === 'tarjeta' && <CreditCard size={14} />}
                  {m === 'transferencia' && <ArrowRightLeft size={14} />}
                  {m.charAt(0).toUpperCase() + m.slice(1)}
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Campo monto recibido (efectivo, solo venta) */}
        {modo === 'venta' && metodoPago === 'efectivo' && items.length > 0 && (
          <div style={{ padding: '0 16px 12px', display: 'flex', flexDirection: 'column', gap: 6, flexShrink: 0 }}>
            <label style={{ fontSize: 11, fontWeight: 600, color: 'var(--color-text-muted)' }}>
              MONTO RECIBIDO
            </label>
            <input
              className="input input-lg mono"
              type="number"
              step="0.01"
              placeholder="$0.00"
              value={montoRecibido || ''}
              onChange={(e) => setMontoRecibido(parseFloat(e.target.value) || 0)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && montoRecibido >= total()) handleCobrar();
              }}
              style={{ textAlign: 'center', fontSize: 24 }}
            />
            {montoRecibido > 0 && montoRecibido >= total() && (
              <div style={{ textAlign: 'center', color: 'var(--color-success)', fontWeight: 700, fontSize: 18 }}>
                Cambio: {fmt(montoRecibido - total())}
              </div>
            )}
          </div>
        )}

        {/* Campos extra en modo presupuesto */}
        {modo === 'presupuesto' && (
          <div style={{ padding: '0 16px 12px', display: 'flex', flexDirection: 'column', gap: 10, flexShrink: 0 }}>
            <div>
              <label style={{ fontSize: 11, fontWeight: 600, color: 'var(--color-text-muted)' }}>
                VIGENCIA (DÍAS)
              </label>
              <input
                className="input mono"
                type="number"
                min={1}
                value={vigenciaPresupuesto}
                onChange={(e) => setVigenciaPresupuesto(parseInt(e.target.value) || 7)}
                style={{ textAlign: 'center' }}
              />
            </div>
            <div>
              <label style={{ fontSize: 11, fontWeight: 600, color: 'var(--color-text-muted)' }}>
                NOTAS
              </label>
              <input
                className="input"
                placeholder="Notas opcionales..."
                value={notasPresupuesto}
                onChange={(e) => setNotasPresupuesto(e.target.value)}
              />
            </div>
          </div>
        )}

        {/* Botón principal */}
        <div style={{ padding: 16, flexShrink: 0 }}>
          {modo === 'venta' ? (
            <button
              className="btn btn-success btn-xl"
              style={{ width: '100%', justifyContent: 'center' }}
              disabled={
                items.length === 0 || procesando ||
                (metodoPago === 'efectivo' && montoRecibido < total())
              }
              onClick={handleCobrar}
            >
              {procesando ? 'Procesando...' : `Cobrar ${fmt(total())} (F10)`}
            </button>
          ) : (
            <button
              className="btn btn-xl"
              style={{ width: '100%', justifyContent: 'center', background: '#e6a817', color: '#fff' }}
              disabled={items.length === 0 || procesando}
              onClick={handleGuardarPresupuesto}
            >
              <Save size={18} />
              {procesando ? 'Guardando...' : 'Guardar Presupuesto'}
            </button>
          )}
        </div>
      </div>



      {/* ─── Modal de Descuento ─── */}
      {showDescuento !== null && (
        <div className="pos-modal-overlay" style={{
          position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.6)',
          display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 100,
        }} onClick={() => { setShowDescuento(null); setShowPinAuth(false); }}>
          <div className="card pos-modal-content pos-modal-fluid animate-fade-in" style={{ width: 340, maxWidth: '100%', padding: 24 }}
            onClick={e => e.stopPropagation()}>
            {!showPinAuth ? (
              <>
                <h3 style={{ fontSize: 16, fontWeight: 700, marginBottom: 4 }}>
                  <Percent size={16} style={{ marginRight: 6 }} />
                  Aplicar Descuento
                </h3>
                <p style={{ fontSize: 12, color: 'var(--color-text-dim)', marginBottom: 16 }}>
                  {items[showDescuento]?.producto.nombre}
                  {!esAdmin && <span> · Máx sin autorización: {maxDescVendedor}%</span>}
                </p>
                <div className="pos-filter-row" style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                  <input
                    className="input mono"
                    type="number"
                    step="1"
                    min="0"
                    max="100"
                    placeholder="0"
                    value={descPorcentaje}
                    onChange={e => setDescPorcentaje(e.target.value)}
                    onKeyDown={e => { if (e.key === 'Enter') handleAplicarDescuento(); }}
                    autoFocus
                    style={{ flex: 1, textAlign: 'center', fontSize: 24 }}
                  />
                  <span style={{ fontSize: 20, fontWeight: 700, color: 'var(--color-text-muted)' }}>%</span>
                </div>
                {/* Quick buttons */}
                <div style={{ display: 'flex', gap: 6, marginTop: 12 }}>
                  {[5, 10, 15, 20, 25].map(p => (
                    <button key={p} className="btn btn-ghost btn-sm" style={{ flex: 1 }}
                      onClick={() => setDescPorcentaje(String(p))}>
                      {p}%
                    </button>
                  ))}
                </div>
                <div style={{ display: 'flex', gap: 8, marginTop: 16, justifyContent: 'flex-end' }}>
                  <button className="btn btn-ghost" onClick={() => {
                    // Quitar descuento
                    const { aplicarDescuento } = useVentaStore.getState();
                    aplicarDescuento(showDescuento, 0, null);
                    setShowDescuento(null);
                  }}>Quitar</button>
                  <button className="btn btn-primary" onClick={handleAplicarDescuento}
                    disabled={!descPorcentaje || Number(descPorcentaje) <= 0}>
                    Aplicar
                  </button>
                </div>
              </>
            ) : (
              /* PIN del dueño para autorizar descuento alto */
              <>
                <h3 style={{ fontSize: 16, fontWeight: 700, marginBottom: 4, display: 'flex', alignItems: 'center', gap: 6 }}>
                  <Lock size={16} style={{ color: 'var(--color-warning)' }} />
                  Autorización Requerida
                </h3>
                <p style={{ fontSize: 12, color: 'var(--color-text-dim)', marginBottom: 16 }}>
                  Descuento de {descPorcentaje}% excede el límite ({maxDescVendedor}%).
                  Ingresa el PIN del dueño.
                </p>
                <input
                  className={`input mono ${pinError ? 'animate-shake' : ''}`}
                  type="password"
                  maxLength={4}
                  inputMode="numeric"
                  value={pinAuth}
                  onChange={e => {
                    const v = e.target.value.replace(/\D/g, '');
                    setPinAuth(v);
                    if (v.length === 4) {
                      setTimeout(() => handlePinAuth(), 50);
                    }
                  }}
                  autoFocus
                  placeholder="••••"
                  style={{ textAlign: 'center', fontSize: 28, letterSpacing: 8, width: '100%' }}
                />
                {pinError && (
                  <p style={{ color: 'var(--color-danger)', fontSize: 12, textAlign: 'center', marginTop: 6 }}>
                    PIN incorrecto
                  </p>
                )}
                <button className="btn btn-ghost btn-sm" style={{ marginTop: 12, width: '100%' }}
                  onClick={() => { setShowPinAuth(false); setPinAuth(''); }}>
                  Cancelar
                </button>
              </>
            )}
          </div>
        </div>
      )}

      {/* ─── Toast Presupuesto Guardado ─── */}
      {presupGuardado && (
        <div className="pos-modal-overlay" style={{
          position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.6)',
          display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 200,
        }} onClick={() => setPresupGuardado(null)}>
          <div className="card pos-modal-content pos-modal-fluid animate-fade-in" style={{ width: 360, maxWidth: '100%', padding: 28, textAlign: 'center' }}
            onClick={e => e.stopPropagation()}>
            <CheckCircle2 size={48} style={{ color: '#e6a817', marginBottom: 10, alignSelf: 'center' }} />
            <h3 style={{ fontSize: 18, fontWeight: 700, marginBottom: 4 }}>Presupuesto Guardado</h3>
            <p className="mono" style={{ fontSize: 15, color: 'var(--color-text-muted)', marginBottom: 16 }}>
              {presupGuardado.folio}
            </p>
            <button className="btn btn-primary" onClick={() => setPresupGuardado(null)} autoFocus>
              Continuar
            </button>
          </div>
        </div>
      )}

      {/* ─── Modal Confirmar Cerrar Pestaña ─── */}
      {confirmCerrarTab && (() => {
        const t = tabs.find(x => x.id === confirmCerrarTab);
        if (!t) return null;
        const count = t.items.reduce((a, i) => a + i.cantidad, 0);
        return (
          <div style={{
            position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.6)',
            display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 200,
          }} onClick={() => setConfirmCerrarTab(null)}>
            <div className="card animate-fade-in" style={{ width: 360, padding: 24 }} onClick={e => e.stopPropagation()}>
              <h3 style={{ fontSize: 16, fontWeight: 700, marginBottom: 8 }}>
                ¿Cerrar "{t.nombre}"?
              </h3>
              <p style={{ fontSize: 13, color: 'var(--color-text-muted)', marginBottom: 20 }}>
                Se perderán <strong>{count}</strong> artículo{count !== 1 ? 's' : ''} del carrito.
              </p>
              <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
                <button className="btn btn-ghost" onClick={() => setConfirmCerrarTab(null)} autoFocus>
                  Cancelar
                </button>
                <button className="btn btn-danger" onClick={() => {
                  cerrarTab(confirmCerrarTab);
                  setConfirmCerrarTab(null);
                }}>
                  Cerrar pestaña
                </button>
              </div>
            </div>
          </div>
        );
      })()}
      {/* ─── MODAL: Selector de Pestañas (Mobile) ─── */}
      {showMobileTabs && (
        <div style={{
          position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)', backdropFilter: 'blur(4px)',
          zIndex: 300, display: 'flex', flexDirection: 'column', justifyContent: 'flex-end',
        }} onClick={() => setShowMobileTabs(false)}>
          <div className="animate-fade-in" style={{
            background: 'var(--color-surface-2)', width: '100%', maxHeight: '80vh',
            padding: 16, borderTopLeftRadius: 16, borderTopRightRadius: 16, display: 'flex', flexDirection: 'column'
          }} onClick={e => e.stopPropagation()}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
              <h3 style={{ fontSize: 16, fontWeight: 800 }}>Ventas Abiertas ({tabs.length})</h3>
              <button className="btn btn-ghost btn-sm" onClick={() => setShowMobileTabs(false)} style={{ padding: 6, borderRadius: '50%' }}>
                <X size={20} />
              </button>
            </div>
            
            <div style={{ overflowY: 'auto', display: 'flex', flexDirection: 'column', gap: 8, paddingBottom: 16 }}>
              {tabs.map(t => {
                const count = t.items.reduce((s,i)=>s+i.cantidad, 0);
                const total = t.items.reduce((s,i)=>s+(i.precio_final * i.cantidad), 0);
                const isActiva = t.id === tabActivaId;
                return (
                  <div key={t.id} style={{ display: 'flex', gap: 8 }}>
                    <button
                      className="btn"
                      onClick={() => { activarTab(t.id); setShowMobileTabs(false); }}
                      style={{
                        flex: 1, justifyContent: 'flex-start', padding: '12px 16px', fontSize: 15,
                        background: isActiva ? 'var(--color-primary)' : 'var(--color-surface)',
                        color: isActiva ? '#fff' : 'var(--color-text)',
                        border: isActiva ? 'none' : '1px solid var(--color-border)',
                      }}
                    >
                      <ShoppingCart size={16} />
                      <div style={{ flex: 1, textAlign: 'left', marginLeft: 8 }}>
                        <div style={{ fontWeight: 700 }}>{t.nombre}</div>
                        {count > 0 && <div style={{ fontSize: 11, opacity: 0.8 }}>{count} artículos — ${total.toFixed(2)}</div>}
                      </div>
                    </button>
                    {tabs.length > 1 && (
                      <button
                        className="btn btn-ghost"
                        onClick={(e) => { e.stopPropagation(); setShowMobileTabs(false); handleCerrarTab(t.id); }}
                        style={{ padding: '0 16px', background: 'var(--color-surface)' }}
                      >
                         <X size={16} color="var(--color-danger)" />
                      </button>
                    )}
                  </div>
                );
              })}
              
              <button className="btn btn-success" style={{ padding: 16, fontSize: 16, marginTop: 8 }} onClick={() => { nuevaTab(); setShowMobileTabs(false); }}>
                 <PlusIcon size={18} /> Nueva Venta
              </button>
            </div>
          </div>
        </div>
      )}

      {/* ─── FAB para abrir el carrito en móvil ─── */}
      <button
        className="pos-fab"
        onClick={() => setMobileCartOpen(true)}
        aria-label="Abrir carrito"
        title="Abrir carrito"
      >
        <ShoppingCart size={24} />
        {numItems() > 0 && (
          <span className="pos-fab-badge">{numItems()}</span>
        )}
      </button>
    </div>
    </div>
  );
}
