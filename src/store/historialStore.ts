// store/historialStore.ts — Historial de ventas + anulaciones + devoluciones

import { create } from 'zustand';
import { invoke } from '../lib/invokeCompat';

// ─── Tipos ────────────────────────────────────────────────

export interface VentaResumen {
  id: number;
  folio: string;
  usuario_nombre: string;
  cliente_nombre: string | null;
  total: number;
  metodo_pago: string;
  anulada: boolean;
  fecha: string;
  num_productos: number;
}

export interface VentaDetalleItem {
  id: number;
  producto_id: number;
  codigo: string;
  nombre: string;
  cantidad: number;
  cantidad_devuelta: number;
  cantidad_disponible: number;
  precio_original: number;
  descuento_porcentaje: number;
  descuento_monto: number;
  precio_final: number;
  subtotal: number;
}

export interface VentaDetalleCompleto {
  id: number;
  folio: string;
  usuario_id: number;
  usuario_nombre: string;
  cliente_id: number | null;
  cliente_nombre: string | null;
  subtotal: number;
  descuento: number;
  total: number;
  metodo_pago: string;
  anulada: boolean;
  anulada_por_nombre: string | null;
  motivo_anulacion: string | null;
  fecha: string;
  items: VentaDetalleItem[];
  total_devuelto: number;
}

export interface FiltrosBusqueda {
  folio?: string;
  fecha_inicio?: string;
  fecha_fin?: string;
  cliente_texto?: string;
  articulo_texto?: string;
  limite?: number;
}

export interface ItemDevolucion {
  venta_detalle_id: number;
  cantidad: number;
}

export interface NuevaDevolucion {
  venta_id: number;
  usuario_id: number;
  autorizado_por?: number | null;
  motivo: string;
  items: ItemDevolucion[];
}

export interface DevolucionCreada {
  id: number;
  folio: string;
  total_devuelto: number;
  fecha: string;
}

export interface DevolucionResumen {
  id: number;
  folio: string;
  venta_id: number;
  venta_folio: string;
  usuario_nombre: string;
  autorizado_por_nombre: string | null;
  motivo: string;
  total_devuelto: number;
  num_items: number;
  fecha: string;
}

// ─── Store ────────────────────────────────────────────────

interface HistorialState {
  ventas: VentaResumen[];
  devoluciones: DevolucionResumen[];
  ventasDetalladas: Record<number, VentaDetalleCompleto>;
  cargando: boolean;

  buscarVentas: (filtros: FiltrosBusqueda) => Promise<VentaResumen[]>;
  obtenerDetalleVenta: (ventaId: number) => Promise<VentaDetalleCompleto>;
  obtenerDetalleCached: (ventaId: number) => Promise<VentaDetalleCompleto>;
  anularVenta: (ventaId: number, usuarioId: number, motivo: string) => Promise<boolean>;
  crearDevolucion: (datos: NuevaDevolucion) => Promise<DevolucionCreada>;
  listarDevoluciones: (limite?: number) => Promise<void>;
}

export const useHistorialStore = create<HistorialState>((set, get) => ({
  ventas: [],
  devoluciones: [],
  ventasDetalladas: {},
  cargando: false,

  buscarVentas: async (filtros) => {
    set({ cargando: true });
    try {
      const rows = await invoke<VentaResumen[]>('buscar_ventas', {
        folio: filtros.folio || null,
        fechaInicio: filtros.fecha_inicio || null,
        fechaFin: filtros.fecha_fin || null,
        clienteTexto: filtros.cliente_texto || null,
        articuloTexto: filtros.articulo_texto || null,
        limite: filtros.limite || 100,
      });
      set({ ventas: rows, cargando: false });
      return rows;
    } catch (e) {
      set({ cargando: false });
      throw e;
    }
  },

  obtenerDetalleVenta: async (ventaId) => {
    return invoke<VentaDetalleCompleto>('obtener_detalle_venta', { ventaId });
  },

  obtenerDetalleCached: async (ventaId: number) => {
    const cached = get().ventasDetalladas[ventaId];
    if (cached) return cached;
    const detalle = await invoke<VentaDetalleCompleto>('obtener_detalle_venta', { ventaId });
    set((s) => ({ ventasDetalladas: { ...s.ventasDetalladas, [ventaId]: detalle } }));
    return detalle;
  },

  anularVenta: async (ventaId, usuarioId, motivo) => {
    return invoke<boolean>('anular_venta', {
      ventaId,
      usuarioId,
      motivo,
    });
  },

  crearDevolucion: async (datos) => {
    return invoke<DevolucionCreada>('crear_devolucion', { datos });
  },

  listarDevoluciones: async (limite = 100) => {
    const rows = await invoke<DevolucionResumen[]>('listar_devoluciones', { limite });
    set({ devoluciones: rows });
  },
}));
