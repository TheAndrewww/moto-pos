// store/ventaStore.ts — Estado del carrito y proceso de venta
// Múltiples pestañas simultáneas (cada pestaña es una venta independiente)

import { create } from 'zustand';
import { invoke } from '../lib/invokeCompat';
import type { Producto } from './productStore';

export type MetodoPago = 'efectivo' | 'tarjeta' | 'transferencia';
export type ModoTab = 'venta' | 'presupuesto';

export interface PresupuestoOrigen {
  id: number;
  folio: string;
}

export interface ItemCarrito {
  producto: Producto;
  cantidad: number;
  precioOriginal: number;
  descuentoPorcentaje: number;
  descuentoMonto: number;
  precioFinal: number;
  subtotal: number;
  autorizadoPor: number | null;
}

export interface Cliente {
  id: number;
  nombre: string;
  telefono: string | null;
  email: string | null;
  descuento_porcentaje: number;
  notas: string | null;
  activo: boolean;
}

export interface VentaCreada {
  id: number;
  folio: string;
  total: number;
  cambio: number;
  fecha: string;
}

export interface EstadisticasDia {
  total_ventas: number;
  num_transacciones: number;
  efectivo: number;
  tarjeta: number;
  transferencia: number;
  producto_top_nombre: string | null;
  producto_top_cantidad: number;
}

export interface TabVenta {
  id: string;
  nombre: string;
  items: ItemCarrito[];
  clienteSeleccionado: Cliente | null;
  metodoPago: MetodoPago;
  montoRecibido: number;
  modo: ModoTab;
  presupuestoOrigen: PresupuestoOrigen | null;
  notasPresupuesto: string;
  vigenciaPresupuesto: number;
}

function generarId(): string {
  if (typeof crypto !== 'undefined' && (crypto as any).randomUUID) {
    return (crypto as any).randomUUID();
  }
  return `t-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
}

function nuevaTabVacia(nombre: string, modo: ModoTab = 'venta'): TabVenta {
  return {
    id: generarId(),
    nombre,
    items: [],
    clienteSeleccionado: null,
    metodoPago: 'efectivo',
    montoRecibido: 0,
    modo,
    presupuestoOrigen: null,
    notasPresupuesto: '',
    vigenciaPresupuesto: 7,
  };
}

interface VentaState {
  // Pestañas
  tabs: TabVenta[];
  tabActivaId: string;

  // Estado compartido
  ventaExitosa: VentaCreada | null;
  procesando: boolean;
  clientes: Cliente[];

  // Gestión de pestañas
  nuevaTab: () => string;
  cerrarTab: (id: string) => void;
  activarTab: (id: string) => void;

  // Computed (sobre pestaña activa)
  subtotal: () => number;
  descuentoTotal: () => number;
  totalSinRedondeo: () => number; // suma cruda antes de redondear (con centavos)
  redondeo: () => number;         // monto agregado por redondeo (>= 0)
  total: () => number;            // total a cobrar — redondeado al peso siguiente
  cambio: () => number;
  numItems: () => number;

  // Carrito (pestaña activa)
  agregarProducto: (producto: Producto) => void;
  quitarProducto: (index: number) => void;
  cambiarCantidad: (index: number, cantidad: number) => void;
  aplicarDescuento: (index: number, porcentaje: number, autorizadoPor?: number | null) => void;
  limpiarCarrito: () => void;

  // Cliente (pestaña activa)
  seleccionarCliente: (cliente: Cliente | null) => void;
  cargarClientes: () => Promise<void>;

  // Cobro (pestaña activa)
  setMetodoPago: (metodo: MetodoPago) => void;
  setMontoRecibido: (monto: number) => void;
  procesarVenta: (usuarioId: number) => Promise<VentaCreada>;
  cerrarVentaExitosa: () => void;

  // Modo Presupuesto (pestaña activa)
  setModo: (modo: ModoTab) => void;
  setNotasPresupuesto: (notas: string) => void;
  setVigenciaPresupuesto: (dias: number) => void;
  guardarComoPresupuesto: (usuarioId: number) => Promise<{ id: number; folio: string }>;
  cargarPresupuestoEnNuevaTab: (
    presupuestoId: number,
    folio: string,
    items: {
      producto: Producto;
      cantidad: number;
      precio_unitario: number;
      descuento_porcentaje: number;
    }[],
    cliente: Cliente | null,
  ) => string;
}

const tabInicial = nuevaTabVacia('Venta 1');

export const useVentaStore = create<VentaState>((set, get) => {
  const getActiva = (): TabVenta => {
    const s = get();
    return s.tabs.find(t => t.id === s.tabActivaId) ?? s.tabs[0];
  };

  const updateActiva = (fn: (t: TabVenta) => TabVenta) => {
    const s = get();
    set({ tabs: s.tabs.map(t => t.id === s.tabActivaId ? fn(t) : t) });
  };

  return {
    tabs: [tabInicial],
    tabActivaId: tabInicial.id,
    ventaExitosa: null,
    procesando: false,
    clientes: [],

    nuevaTab: () => {
      const s = get();
      // Nombre sugerido: siguiente número libre
      let n = s.tabs.length + 1;
      const nombres = new Set(s.tabs.map(t => t.nombre));
      while (nombres.has(`Venta ${n}`)) n++;
      const nueva = nuevaTabVacia(`Venta ${n}`);
      set({ tabs: [...s.tabs, nueva], tabActivaId: nueva.id });
      return nueva.id;
    },

    cerrarTab: (id) => {
      const s = get();
      if (s.tabs.length === 1) {
        // No cerrar la última — resetearla en su lugar
        const reset: TabVenta = { ...nuevaTabVacia('Venta 1'), id: s.tabs[0].id };
        set({ tabs: [reset], ventaExitosa: null });
        return;
      }
      const idx = s.tabs.findIndex(t => t.id === id);
      if (idx < 0) return;
      const newTabs = s.tabs.filter(t => t.id !== id);
      let newActiva = s.tabActivaId;
      if (s.tabActivaId === id) {
        newActiva = newTabs[Math.max(0, idx - 1)].id;
      }
      set({ tabs: newTabs, tabActivaId: newActiva });
    },

    activarTab: (id) => {
      if (get().tabs.some(t => t.id === id)) {
        set({ tabActivaId: id, ventaExitosa: null });
      }
    },

    subtotal: () => getActiva().items.reduce((acc, i) => acc + i.subtotal, 0),
    descuentoTotal: () => getActiva().items.reduce((acc, i) => acc + (i.descuentoMonto * i.cantidad), 0),
    // Total crudo (con centavos) — solo informativo / para tickets
    totalSinRedondeo: () => getActiva().items.reduce((acc, i) => acc + i.subtotal, 0),
    // Monto que se agrega al redondear hacia arriba al peso. 0 si ya es entero.
    // Math.round * 100 protege contra errores de punto flotante (e.g. 67.79999...)
    redondeo: () => {
      const raw = getActiva().items.reduce((acc, i) => acc + i.subtotal, 0);
      if (raw <= 0) return 0;
      const cents = Math.round(raw * 100);
      const remainder = cents % 100;
      if (remainder === 0) return 0;
      return (100 - remainder) / 100;
    },
    // Total a cobrar — siempre número entero de pesos (sin centavos).
    // Si raw es 67.80 → 68. Si raw es 70.00 → 70 (sin cambio).
    total: () => {
      const raw = getActiva().items.reduce((acc, i) => acc + i.subtotal, 0);
      if (raw <= 0) return 0;
      const cents = Math.round(raw * 100);
      const remainder = cents % 100;
      if (remainder === 0) return cents / 100;
      return Math.ceil(cents / 100);
    },
    cambio: () => {
      const total = get().total();
      return Math.max(0, getActiva().montoRecibido - total);
    },
    numItems: () => getActiva().items.reduce((acc, i) => acc + i.cantidad, 0),

    agregarProducto: (producto) => {
      updateActiva(t => {
        const precio = producto.precio_venta;
        const descCliente = t.clienteSeleccionado?.descuento_porcentaje || 0;

        const existingIdx = t.items.findIndex(i => i.producto.id === producto.id);
        if (existingIdx >= 0) {
          const newItems = [...t.items];
          const item = { ...newItems[existingIdx] };
          item.cantidad += 1;
          item.subtotal = item.precioFinal * item.cantidad;
          newItems[existingIdx] = item;
          return { ...t, items: newItems };
        }

        const descMonto = precio * (descCliente / 100);
        const precioFinal = precio - descMonto;
        const newItem: ItemCarrito = {
          producto,
          cantidad: 1,
          precioOriginal: precio,
          descuentoPorcentaje: descCliente,
          descuentoMonto: descMonto,
          precioFinal,
          subtotal: precioFinal,
          autorizadoPor: null,
        };
        return { ...t, items: [...t.items, newItem] };
      });
    },

    quitarProducto: (index) => updateActiva(t => ({
      ...t,
      items: t.items.filter((_, i) => i !== index),
    })),

    cambiarCantidad: (index, cantidad) => updateActiva(t => {
      if (cantidad <= 0) {
        return { ...t, items: t.items.filter((_, i) => i !== index) };
      }
      const newItems = [...t.items];
      const item = { ...newItems[index] };
      item.cantidad = cantidad;
      item.subtotal = item.precioFinal * cantidad;
      newItems[index] = item;
      return { ...t, items: newItems };
    }),

    aplicarDescuento: (index, porcentaje, autorizadoPor = null) => updateActiva(t => {
      const newItems = [...t.items];
      const item = { ...newItems[index] };
      item.descuentoPorcentaje = porcentaje;
      item.descuentoMonto = item.precioOriginal * (porcentaje / 100);
      item.precioFinal = item.precioOriginal - item.descuentoMonto;
      item.subtotal = item.precioFinal * item.cantidad;
      item.autorizadoPor = autorizadoPor;
      newItems[index] = item;
      return { ...t, items: newItems };
    }),

    limpiarCarrito: () => updateActiva(t => ({
      ...t,
      items: [],
      clienteSeleccionado: null,
      metodoPago: 'efectivo',
      montoRecibido: 0,
    })),

    seleccionarCliente: (cliente) => updateActiva(t => {
      const descPct = cliente?.descuento_porcentaje || 0;
      const items = t.items.map(item => {
        const precio = item.producto.precio_venta;
        const descMonto = precio * (descPct / 100);
        const precioFinal = precio - descMonto;
        return {
          ...item,
          precioOriginal: precio,
          descuentoPorcentaje: descPct,
          descuentoMonto: descMonto,
          precioFinal,
          subtotal: precioFinal * item.cantidad,
        };
      });
      return { ...t, clienteSeleccionado: cliente, items };
    }),

    cargarClientes: async () => {
      try {
        const clientes = await invoke<Cliente[]>('listar_clientes');
        set({ clientes });
      } catch {}
    },

    setMetodoPago: (metodo) => updateActiva(t => ({ ...t, metodoPago: metodo })),
    setMontoRecibido: (monto) => updateActiva(t => ({ ...t, montoRecibido: monto })),

    procesarVenta: async (usuarioId) => {
      const s = get();
      const activa = getActiva();
      set({ procesando: true });

      try {
        const venta = {
          usuario_id: usuarioId,
          cliente_id: activa.clienteSeleccionado?.id || null,
          subtotal: s.subtotal(),
          descuento: s.descuentoTotal(),
          // total ya viene redondeado al peso (Math.ceil). La diferencia
          // entre subtotal-descuento y total queda como redondeo implícito.
          total: s.total(),
          metodo_pago: activa.metodoPago,
          monto_recibido: activa.montoRecibido,
          cambio: s.cambio(),
          items: activa.items.map(i => ({
            producto_id: i.producto.id,
            cantidad: i.cantidad,
            precio_original: i.precioOriginal,
            descuento_porcentaje: i.descuentoPorcentaje,
            descuento_monto: i.descuentoMonto,
            precio_final: i.precioFinal,
            subtotal: i.subtotal,
            autorizado_por: i.autorizadoPor,
          })),
          presupuesto_origen_id: activa.presupuestoOrigen?.id || null,
        };

        const result = await invoke<VentaCreada>('crear_venta', { venta });
        set({ ventaExitosa: result, procesando: false });
        return result;
      } catch (e: any) {
        set({ procesando: false });
        throw new Error(e?.toString() || 'Error al procesar la venta');
      }
    },

    setModo: (modo) => updateActiva(t => ({ ...t, modo })),
    setNotasPresupuesto: (notas) => updateActiva(t => ({ ...t, notasPresupuesto: notas })),
    setVigenciaPresupuesto: (dias) => updateActiva(t => ({ ...t, vigenciaPresupuesto: dias })),

    guardarComoPresupuesto: async (usuarioId) => {
      const activa = getActiva();
      if (activa.items.length === 0) throw new Error('Agrega al menos un producto');
      set({ procesando: true });
      try {
        const total = activa.items.reduce((acc, i) => acc + i.subtotal, 0);
        const result = await invoke<{ id: number; folio: string }>('crear_presupuesto', {
          presupuesto: {
            usuario_id: usuarioId,
            cliente_id: activa.clienteSeleccionado?.id || null,
            notas: activa.notasPresupuesto || null,
            vigencia_dias: activa.vigenciaPresupuesto,
            total,
            items: activa.items.map(i => ({
              producto_id: i.producto.id,
              descripcion: i.producto.nombre,
              cantidad: i.cantidad,
              precio_unitario: i.precioOriginal,
              descuento_porcentaje: i.descuentoPorcentaje,
              subtotal: i.subtotal,
            })),
          },
        });
        set({ procesando: false });
        return result;
      } catch (e: any) {
        set({ procesando: false });
        throw new Error(e?.toString() || 'Error al crear presupuesto');
      }
    },

    cargarPresupuestoEnNuevaTab: (presupuestoId, folio, itemsBase, cliente) => {
      const s = get();
      const itemsCarrito: ItemCarrito[] = itemsBase.map(i => {
        const descMonto = i.precio_unitario * (i.descuento_porcentaje / 100);
        const precioFinal = i.precio_unitario - descMonto;
        return {
          producto: i.producto,
          cantidad: i.cantidad,
          precioOriginal: i.precio_unitario,
          descuentoPorcentaje: i.descuento_porcentaje,
          descuentoMonto: descMonto,
          precioFinal,
          subtotal: precioFinal * i.cantidad,
          autorizadoPor: null,
        };
      });
      const nueva: TabVenta = {
        ...nuevaTabVacia(folio, 'venta'),
        items: itemsCarrito,
        clienteSeleccionado: cliente,
        presupuestoOrigen: { id: presupuestoId, folio },
      };
      set({ tabs: [...s.tabs, nueva], tabActivaId: nueva.id, ventaExitosa: null });
      return nueva.id;
    },

    cerrarVentaExitosa: () => {
      const s = get();
      const activa = getActiva();
      if (s.tabs.length > 1) {
        // Cerrar la pestaña completada
        const idx = s.tabs.findIndex(t => t.id === activa.id);
        const newTabs = s.tabs.filter(t => t.id !== activa.id);
        const newActiva = newTabs[Math.max(0, idx - 1)].id;
        set({ tabs: newTabs, tabActivaId: newActiva, ventaExitosa: null });
      } else {
        // Única pestaña — resetear
        const reset: TabVenta = { ...nuevaTabVacia('Venta 1'), id: activa.id };
        set({ tabs: [reset], ventaExitosa: null });
      }
    },
  };
});

// Hook selectivo para consumir datos de la pestaña activa
export function useVentaActiva(): TabVenta {
  return useVentaStore(s => s.tabs.find(t => t.id === s.tabActivaId) ?? s.tabs[0]);
}
