// store/cortesStore.ts — Estado del módulo de Cortes de Caja

import { create } from 'zustand';
import { invoke, isTauri } from '../lib/invokeCompat';

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
  cortes_parciales_hoy: number;
  total_retirado_parciales: number;
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
  pin_autorizacion?: string | null;
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
  /** Retiro al momento de un corte de turno. Escritorio y servidor lo
   *  registran dentro de la misma transacción del corte (no descuadra el corte
   *  que lo origina). */
  retiro?: { monto: number; concepto: string; pin_autorizacion?: string | null } | null;
}

/** Lo que debería haber en caja al abrir: fondo que dejó el último corte +
 *  efectivo que entró/salió desde entonces. */
export interface EsperadoApertura {
  esperado: number;
  fondo_ultimo_corte: number;
  flujo_desde_corte: number;
  hay_referencia: boolean;
  ultimo_corte_fecha: string | null;
}

/** Prefijo del error que devuelven escritorio y servidor cuando entraron
 *  ventas o movimientos mientras el modal de corte estaba abierto. */
export const ERR_DATOS_CAMBIARON = 'DATOS_CAMBIARON';

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
  fondo_inicial: number;
  total_ventas_efectivo: number;
  total_ventas_tarjeta: number;
  total_ventas_transferencia: number;
  total_ventas: number;
  num_transacciones: number;
  total_entradas_efectivo: number;
  total_retiros_efectivo: number;
  efectivo_esperado: number;
  efectivo_contado: number;
  diferencia: number;
  nota_diferencia: string | null;
  fondo_siguiente: number;
}

export interface DenominacionDetalle {
  denominacion: number;
  tipo: string;
  cantidad: number;
  subtotal: number;
}

export interface CorteDetalle {
  corte: CorteResumen;
  denominaciones: DenominacionDetalle[];
  movimientos: MovimientoCaja[];
  vendedores: VendedorResumen[];
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

/** Con qué caja trabaja este equipo (`obtener_modo_caja` en la web).
 *  - 'principal' = el cajón de la tienda. Sus cortes y su apertura se hacen
 *    SOLO en el POS de escritorio; un equipo web en modo espejo vende y
 *    registra entradas/retiros con ese dinero, pero no corta.
 *  - 'web' = caja propia de los equipos web en modo individual: tiene su
 *    apertura y sus cortes aquí mismo. */
export interface InfoCaja {
  modo: 'espejo' | 'individual';
  caja: 'principal' | 'web';
  /** true = en este equipo se hacen cortes y aperturas (siempre en escritorio). */
  cortes_en_este_equipo: boolean;
  /** false = ningún POS de escritorio actualizado se ha reportado en 2 días:
   *  las ventas web de la caja de la tienda todavía no llegan a su corte. */
  escritorio_recibe_web: boolean;
}

/** El escritorio ES la caja de la tienda: corta y abre aquí. */
const INFO_CAJA_ESCRITORIO: InfoCaja = {
  modo: 'espejo',
  caja: 'principal',
  cortes_en_este_equipo: true,
  escritorio_recibe_web: true,
};

/** Lee la info de caja de una respuesta del servidor (login,
 *  obtener_modo_caja o configurar_modo_caja). */
export function infoCajaDeRespuesta(r: {
  modo?: string; modo_caja?: string; caja?: string;
  cortes_en_este_equipo?: boolean; escritorio_recibe_web?: boolean;
} | null | undefined): InfoCaja | null {
  if (!r) return null;
  const modo = (r.modo ?? r.modo_caja) === 'individual' ? 'individual' : 'espejo';
  const caja = r.caja === 'web' || r.caja === 'principal'
    ? r.caja
    : (modo === 'individual' ? 'web' : 'principal');
  return {
    modo,
    caja,
    cortes_en_este_equipo: r.cortes_en_este_equipo ?? caja === 'web',
    escritorio_recibe_web: r.escritorio_recibe_web ?? true,
  };
}

// ─── Store ────────────────────────────────────────────────

interface CortesState {
  movimientosPendientes: MovimientoCaja[];
  cortesPrevios: CorteResumen[];
  aperturaHoy: AperturaCaja | null;
  cargando: boolean;
  /** null mientras la web no sabe aún su caja (en escritorio nunca es null). */
  infoCaja: InfoCaja | null;

  cargarInfoCaja: () => Promise<InfoCaja>;
  setInfoCaja: (info: InfoCaja) => void;

  crearMovimiento: (datos: NuevoMovimiento) => Promise<MovimientoCaja>;
  cargarMovimientosPendientes: () => Promise<void>;
  calcularDatosCorte: (fechaInicio: string, fechaFin: string) => Promise<DatosCorte>;
  crearCorte: (datos: NuevoCorte) => Promise<CorteCreado>;
  cargarCortes: (limite?: number) => Promise<void>;
  obtenerDetalleCorte: (id: number) => Promise<CorteDetalle>;
  verificarCorteDiaPendiente: () => Promise<string | null>;
  obtenerInicioProximoCierre: () => Promise<string>;
  crearApertura: (datos: NuevaApertura) => Promise<AperturaCaja>;
  obtenerAperturaHoy: () => Promise<AperturaCaja | null>;
  obtenerFondoSugerido: () => Promise<number>;
  obtenerEsperadoApertura: () => Promise<EsperadoApertura>;
}

export const useCortesStore = create<CortesState>((set, get) => ({
  movimientosPendientes: [],
  cortesPrevios: [],
  aperturaHoy: null,
  cargando: false,
  infoCaja: isTauri() ? INFO_CAJA_ESCRITORIO : null,

  cargarInfoCaja: async () => {
    if (isTauri()) return INFO_CAJA_ESCRITORIO;
    try {
      const r = await invoke<Parameters<typeof infoCajaDeRespuesta>[0]>('obtener_modo_caja');
      const info = infoCajaDeRespuesta(r);
      if (info) {
        set({ infoCaja: info });
        return info;
      }
    } catch {
      // Sin respuesta: se queda lo que había.
    }
    // Sin dato del servidor: lo seguro es NO ofrecer cortes aquí (la caja de
    // la tienda solo se corta en el escritorio).
    const actual = get().infoCaja;
    if (actual) return actual;
    const prudente: InfoCaja = {
      modo: 'espejo', caja: 'principal', cortes_en_este_equipo: false, escritorio_recibe_web: true,
    };
    set({ infoCaja: prudente });
    return prudente;
  },

  setInfoCaja: (info) => set({ infoCaja: info }),

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

  obtenerDetalleCorte: async (id) => {
    return invoke<CorteDetalle>('obtener_detalle_corte', { id });
  },

  verificarCorteDiaPendiente: async () => {
    return invoke<string | null>('verificar_corte_dia_pendiente');
  },

  obtenerInicioProximoCierre: async () => {
    return invoke<string>('obtener_inicio_proximo_cierre');
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

  obtenerEsperadoApertura: async () => {
    return invoke<EsperadoApertura>('obtener_esperado_apertura');
  },
}));
