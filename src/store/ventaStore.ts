// store/ventaStore.ts — Estado del carrito y proceso de venta
// Múltiples pestañas simultáneas (cada pestaña es una venta independiente)

import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import type { Producto } from './productStore';

export type MetodoPago = 'efectivo' | 'tarjeta' | 'transferencia';

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
}

function generarId(): string {
  if (typeof crypto !== 'undefined' && (crypto as any).randomUUID) {
    return (crypto as any).randomUUID();
  }
  return `t-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
}

function nuevaTabVacia(nombre: string): TabVenta {
  return {
    id: generarId(),
    nombre,
    items: [],
    clienteSeleccionado: null,
    metodoPago: 'efectivo',
    montoRecibido: 0,
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
  total: () => number;
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
    total: () => getActiva().items.reduce((acc, i) => acc + i.subtotal, 0),
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
        };

        const result = await invoke<VentaCreada>('crear_venta', { venta });
        set({ ventaExitosa: result, procesando: false });
        return result;
      } catch (e: any) {
        set({ procesando: false });
        throw new Error(e?.toString() || 'Error al procesar la venta');
      }
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
