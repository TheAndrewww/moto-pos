// store/cortesStore.ts — Estado del módulo de Cortes de Caja

import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';

// ─── Tipos ────────────────────────────────────────────────

export interface MovimientoCaja {
  id: number;
  tipo: 'ENTRADA' | 'RETIRO';
  usuario_id: number;
  usuario_nombre: string;
  monto: number;
  concepto: string;
  autorizado_por: number | null;
  corte_id: number | null;
  fecha: string;
}

export interface VendedorResumen {
  usuario_id: number;
  usuario_nombre: string;
  num_ventas: number;
  total_vendido: number;
  hora_inicio: string;
  hora_fin: string;
}

export interface DatosCorte {
  fecha_inicio: string;
  fecha_fin: string;
  fondo_inicial: number;
  total_ventas_efectivo: number;
  total_ventas_tarjeta: number;
  total_ventas_transferencia: number;
  total_ventas: number;
  num_transacciones: number;
  total_descuentos: number;
  total_anulaciones: number;
  total_entradas_efectivo: number;
  total_retiros_efectivo: number;
  efectivo_esperado: number;
  movimientos: MovimientoCaja[];
  vendedores: VendedorResumen[];
}

export interface DenominacionInput {
  denominacion: number;
  tipo: 'BILLETE' | 'MONEDA';
  cantidad: number;
}

export interface NuevoMovimiento {
  tipo: 'ENTRADA' | 'RETIRO';
  usuario_id: number;
  monto: number;
  concepto: string;
  autorizado_por?: number | null;
}

export interface NuevoCorte {
  tipo: 'PARCIAL' | 'DIA';
  usuario_id: number;
  fecha_inicio: string;
  fecha_fin: string;
  datos: DatosCorte;
  efectivo_contado: number;
  nota_diferencia?: string | null;
  fondo_siguiente: number;
  denominaciones?: DenominacionInput[] | null;
}

export interface CorteCreado {
  id: number;
  tipo: string;
  diferencia: number;
  efectivo_esperado: number;
  efectivo_contado: number;
  fondo_siguiente: number;
  created_at: string;
}

export interface CorteResumen {
  id: number;
  tipo: string;
  usuario_nombre: string;
  created_at: string;
  efectivo_esperado: number;
  efectivo_contado: number;
  diferencia: number;
  fondo_siguiente: number;
}

export interface NuevaApertura {
  usuario_id: number;
  fondo_declarado: number;
  nota?: string | null;
}

export interface AperturaCaja {
  id: number;
  usuario_id: number;
  usuario_nombre: string;
  fondo_declarado: number;
  nota: string | null;
  fecha: string;
}

// ─── Store ────────────────────────────────────────────────

interface CortesState {
  movimientosPendientes: MovimientoCaja[];
  cortesPrevios: CorteResumen[];
  aperturaHoy: AperturaCaja | null;
  cargando: boolean;

  crearMovimiento: (datos: NuevoMovimiento) => Promise<MovimientoCaja>;
  cargarMovimientosPendientes: () => Promise<void>;
  calcularDatosCorte: (fechaInicio: string, fechaFin: string) => Promise<DatosCorte>;
  crearCorte: (datos: NuevoCorte) => Promise<CorteCreado>;
  cargarCortes: (limite?: number) => Promise<void>;
  verificarCorteDiaPendiente: () => Promise<string | null>;
  crearApertura: (datos: NuevaApertura) => Promise<AperturaCaja>;
  obtenerAperturaHoy: () => Promise<AperturaCaja | null>;
  obtenerFondoSugerido: () => Promise<number>;
}

export const useCortesStore = create<CortesState>((set) => ({
  movimientosPendientes: [],
  cortesPrevios: [],
  aperturaHoy: null,
  cargando: false,

  crearMovimiento: async (datos) => {
    const mov = await invoke<MovimientoCaja>('crear_movimiento_caja', { datos });
    set((s) => ({
      movimientosPendientes: [mov, ...s.movimientosPendientes],
    }));
    return mov;
  },

  cargarMovimientosPendientes: async () => {
    const items = await invoke<MovimientoCaja[]>('listar_movimientos_sin_corte');
    set({ movimientosPendientes: items });
  },

  calcularDatosCorte: async (fechaInicio, fechaFin) => {
    return invoke<DatosCorte>('calcular_datos_corte', {
      fechaInicio,
      fechaFin,
    });
  },

  crearCorte: async (datos) => {
    set({ cargando: true });
    try {
      const corte = await invoke<CorteCreado>('crear_corte', { datos });
      // Recargar movimientos (ahora tienen corte_id asignado)
      const pendientes = await invoke<MovimientoCaja[]>('listar_movimientos_sin_corte');
      set({ movimientosPendientes: pendientes, cargando: false });
      return corte;
    } catch (e) {
      set({ cargando: false });
      throw e;
    }
  },

  cargarCortes: async (limite = 50) => {
    const items = await invoke<CorteResumen[]>('listar_cortes', { limite });
    set({ cortesPrevios: items });
  },

  verificarCorteDiaPendiente: async () => {
    return invoke<string | null>('verificar_corte_dia_pendiente');
  },

  crearApertura: async (datos) => {
    const apertura = await invoke<AperturaCaja>('crear_apertura_caja', { datos });
    set({ aperturaHoy: apertura });
    return apertura;
  },

  obtenerAperturaHoy: async () => {
    const apertura = await invoke<AperturaCaja | null>('obtener_apertura_hoy');
    set({ aperturaHoy: apertura });
    return apertura;
  },

  obtenerFondoSugerido: async () => {
    return invoke<number>('obtener_fondo_sugerido');
  },
}));
