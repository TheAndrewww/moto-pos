// pages/CortesCaja.tsx — Módulo de Cortes de Caja

import { useState, useEffect, useCallback, useRef } from 'react';
import { useAuthStore } from '../store/authStore';
import { invoke, isTauri } from '../lib/invokeCompat';
import {
  useCortesStore,
  type MovimientoCaja,
  type DatosCorte,
  type DenominacionInput,
  type CorteResumen,
  type CorteDetalle,
  type EsperadoApertura,
  ERR_DATOS_CAMBIARON,
} from '../store/cortesStore';
import {
  DollarSign, ArrowDownLeft, ArrowUpRight, Clock,
  CheckCircle, AlertTriangle, X, ChevronDown, ChevronUp,
  Sunrise, FileText, LogOut, Calculator
} from 'lucide-react';

// ─── Utilidades ───────────────────────────────────────────

const fmt = (n: number) => `$${n.toFixed(2)}`;

/** Redondea a centavos (evita diferencias fantasma por sumas de flotantes). */
const round2 = (n: number) => Math.round(n * 100) / 100;

function fechaHoyInicio() {
  const d = new Date();
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day} 00:00:00`;
}

function ahora() {
  const d = new Date();
  const y = d.getFullYear();
  const mo = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  const h = String(d.getHours()).padStart(2, '0');
  const mi = String(d.getMinutes()).padStart(2, '0');
  const s = String(d.getSeconds()).padStart(2, '0');
  return `${y}-${mo}-${day} ${h}:${mi}:${s}`;
}

function fmtHora(fecha: string) {
  return fecha.length >= 16 ? fecha.substring(11, 16) : fecha;
}

function fmtFecha(fecha: string) {
  return fecha.length >= 10 ? fecha.substring(0, 10) : fecha;
}

// Denominaciones mexicanas
const DENOMINACIONES: { valor: number; tipo: 'BILLETE' | 'MONEDA'; label: string }[] = [
  { valor: 1000, tipo: 'BILLETE', label: '$1,000' },
  { valor: 500,  tipo: 'BILLETE', label: '$500' },
  { valor: 200,  tipo: 'BILLETE', label: '$200' },
  { valor: 100,  tipo: 'BILLETE', label: '$100' },
  { valor: 50,   tipo: 'BILLETE', label: '$50' },
  { valor: 20,   tipo: 'BILLETE', label: '$20' },
  { valor: 20,   tipo: 'MONEDA',  label: '$20 c' },
  { valor: 10,   tipo: 'MONEDA',  label: '$10 c' },
  { valor: 5,    tipo: 'MONEDA',  label: '$5 c' },
  { valor: 2,    tipo: 'MONEDA',  label: '$2 c' },
  { valor: 1,    tipo: 'MONEDA',  label: '$1 c' },
  { valor: 0.5,  tipo: 'MONEDA',  label: '$0.50' },
];

// ─── Componente Principal ─────────────────────────────────

interface Props {
  onAbrirMovimiento?: () => void;
  onAbrirParcial?: () => void;
  onAbrirDia?: () => void;
  triggerMovimiento?: number;
  triggerParcial?: number;
  triggerDia?: number;
  fechaObjetivoDia?: string | null; // YYYY-MM-DD — si está set, el corte DIA cubre ese día
  onCorteDiaHecho?: () => void;
  /** El Dashboard mantiene este módulo montado (oculto con display:none);
   *  avisa cuándo está a la vista para refrescar lo que cambia afuera. */
  visible?: boolean;
}

export default function CortesCaja({
  triggerMovimiento = 0,
  triggerParcial = 0,
  triggerDia = 0,
  fechaObjetivoDia = null,
  onCorteDiaHecho,
  visible = true,
}: Props) {
  const { usuario } = useAuthStore();
  const {
    movimientosPendientes,
    cortesPrevios,
    cargarMovimientosPendientes,
    cargarCortes,
    infoCaja,
    cargarInfoCaja,
  } = useCortesStore();

  const [tab, setTab] = useState<'movimientos' | 'historial' | 'auditoria'>('movimientos');
  const [showModalMov, setShowModalMov] = useState(false);
  const [showModalParcial, setShowModalParcial] = useState(false);
  const [showModalDia, setShowModalDia] = useState(false);
  // Contador para volver a pedir la vista previa de la caja de la tienda
  // (web espejo): al volver a esta pantalla y después de recargar().
  const [refrescoAviso, setRefrescoAviso] = useState(0);

  const esAdmin = usuario?.es_admin ?? false;

  // ¿Se hacen cortes en este equipo? Siempre en escritorio. En la web solo con
  // caja propia (modo individual): la caja de la tienda la corta el escritorio.
  const cortesAqui = isTauri() || (infoCaja?.cortes_en_este_equipo ?? false);

  useEffect(() => {
    cargarMovimientosPendientes();
    cargarCortes(50);
    if (!isTauri()) cargarInfoCaja();
  }, []);

  // Triggers desde el contenedor padre
  useEffect(() => { if (triggerMovimiento > 0) setShowModalMov(true); }, [triggerMovimiento]);
  useEffect(() => { if (triggerParcial > 0 && cortesAqui) setShowModalParcial(true); }, [triggerParcial]);
  useEffect(() => { if (triggerDia > 0 && esAdmin && cortesAqui) setShowModalDia(true); }, [triggerDia, esAdmin]);

  // Triggers locales (atajos de teclado dentro de la vista misma)
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (showModalMov || showModalParcial || showModalDia) return;
      if (e.key === 'F6') { e.preventDefault(); setShowModalMov(true); }
      if (e.key === 'F11' && cortesAqui) { e.preventDefault(); setShowModalParcial(true); }
      if (e.key === 'F12' && esAdmin && cortesAqui) { e.preventDefault(); setShowModalDia(true); }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [showModalMov, showModalParcial, showModalDia, esAdmin, cortesAqui]);

  const recargar = useCallback(async () => {
    setRefrescoAviso(n => n + 1);
    await cargarMovimientosPendientes();
    await cargarCortes(50);
  }, []);

  // El módulo vive montado aunque esté oculto: al volver a mostrarse, el
  // aviso de la caja de la tienda se recalcula (ventas web, entradas/retiros
  // o un corte del escritorio pudieron cambiarlo mientras tanto).
  const visibleAntes = useRef(visible);
  useEffect(() => {
    if (visible && !visibleAntes.current) setRefrescoAviso(n => n + 1);
    visibleAntes.current = visible;
  }, [visible]);

  return (
    <div className="animate-fade-in" style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>

      {/* ─── Header ─── */}
      <div style={{
        padding: '16px 20px',
        borderBottom: '1px solid var(--color-border)',
        display: 'flex', alignItems: 'center', gap: 12,
        background: 'var(--color-surface)',
      }}>
        <Calculator size={20} style={{ color: 'var(--color-primary)' }} />
        <h2 style={{ fontSize: 16, fontWeight: 800, flex: 1 }}>Cortes de Caja</h2>

        <button
          className="btn btn-ghost btn-sm"
          onClick={() => setShowModalMov(true)}
          title="F6"
        >
          <ArrowDownLeft size={14} /> Movimiento <span style={{ opacity: 0.5, fontSize: 10 }}>F6</span>
        </button>
        {cortesAqui && (
          <button
            className="btn btn-primary btn-sm"
            onClick={() => setShowModalParcial(true)}
            title="F11"
          >
            <FileText size={14} /> Corte de Turno <span style={{ opacity: 0.5, fontSize: 10 }}>F11</span>
          </button>
        )}
        {cortesAqui && esAdmin && (
          <button
            className="btn btn-primary btn-sm"
            style={{ background: 'var(--color-danger)', borderColor: 'var(--color-danger)' }}
            onClick={() => setShowModalDia(true)}
            title="F12"
          >
            <LogOut size={14} /> Cerrar Caja <span style={{ opacity: 0.5, fontSize: 10 }}>F12</span>
          </button>
        )}
      </div>

      {/* Web en modo espejo: esta pantalla muestra la caja de la tienda, que
          se corta y se abre en el POS de escritorio. */}
      {!isTauri() && infoCaja && !infoCaja.cortes_en_este_equipo && (
        <AvisoCajaDeLaTienda escritorioRecibeWeb={infoCaja.escritorio_recibe_web} refresco={refrescoAviso} />
      )}

      {/* ─── Tabs ─── */}
      <div style={{
        display: 'flex', gap: 0,
        borderBottom: '1px solid var(--color-border)',
        background: 'var(--color-surface)',
        padding: '0 20px',
      }}>
        {/* La pestaña de auditoría solo la ve el admin: expone descuadres
            históricos y detalle de ventas/movimientos, no es información
            para un vendedor. */}
        {(esAdmin
          ? (['movimientos', 'historial', 'auditoria'] as const)
          : (['movimientos', 'historial'] as const)
        ).map(t => (
          <button
            key={t}
            onClick={() => setTab(t)}
            style={{
              padding: '10px 16px',
              border: 'none', background: 'transparent',
              fontSize: 13, fontWeight: 600, cursor: 'pointer',
              color: tab === t ? 'var(--color-primary)' : 'var(--color-text-muted)',
              borderBottom: tab === t ? '2px solid var(--color-primary)' : '2px solid transparent',
              transition: 'all 0.1s',
            }}
          >
            {t === 'movimientos'
              ? `Movimientos (${movimientosPendientes.length})`
              : t === 'historial' ? 'Historial' : '🔍 Auditoría'}
          </button>
        ))}
      </div>

      {/* ─── Contenido ─── */}
      <div style={{ flex: 1, overflow: 'auto', padding: 20 }}>

        {tab === 'movimientos' && (
          <TabMovimientos
            movimientos={movimientosPendientes}
            onNuevo={() => setShowModalMov(true)}
          />
        )}

        {tab === 'historial' && (
          <TabHistorial cortes={cortesPrevios} />
        )}

        {tab === 'auditoria' && esAdmin && <TabAuditoria cajaWeb={infoCaja?.caja === 'web'} />}
      </div>

      {/* ─── Modales ─── */}
      {showModalMov && (
        <ModalMovimiento
          onClose={() => setShowModalMov(false)}
          onSuccess={recargar}
        />
      )}
      {showModalParcial && (
        <ModalCorteTurno
          onClose={() => setShowModalParcial(false)}
          onSuccess={recargar}
        />
      )}
      {showModalDia && (
        <ModalCorteDelDia
          onClose={() => setShowModalDia(false)}
          onSuccess={async () => { await recargar(); onCorteDiaHecho?.(); }}
          fechaObjetivo={fechaObjetivoDia}
        />
      )}
    </div>
  );
}

// ─── Aviso: caja de la tienda vista desde la web ──────────
//
// Un equipo web en modo espejo vende y registra entradas/retiros con el dinero
// del cajón de la tienda, pero los cortes y la apertura de esa caja los hace
// SOLO el POS de escritorio (un solo escritor para su cadena). Aquí se muestra
// la vista previa con lo que ya se sincronizó.

function AvisoCajaDeLaTienda({ escritorioRecibeWeb, refresco }: { escritorioRecibeWeb: boolean; refresco: number }) {
  const { calcularDatosCorte } = useCortesStore();
  const [datos, setDatos] = useState<DatosCorte | null>(null);

  // Se recalcula con cada `refresco` (ahora() y el inicio del período cambian).
  // `vivo` descarta respuestas que llegan fuera de orden: solo cuenta la de la
  // última petición.
  useEffect(() => {
    let vivo = true;
    calcularDatosCorte(fechaHoyInicio(), ahora())
      .then(d => { if (vivo) setDatos(d); })
      .catch(() => { if (vivo) setDatos(null); });
    return () => { vivo = false; };
  }, [refresco]);

  return (
    <div style={{
      padding: '10px 20px', borderBottom: '1px solid var(--color-border)',
      background: 'rgba(59,130,246,0.08)', fontSize: 12, lineHeight: 1.5,
      display: 'flex', flexDirection: 'column', gap: 4,
    }}>
      <div style={{ fontWeight: 700, color: 'var(--color-text)' }}>
        Los cortes de la caja de la tienda se hacen en el POS de escritorio.
      </div>
      <div style={{ color: 'var(--color-text-muted)' }}>
        Las ventas, entradas y retiros que registres en este equipo entran a esa caja y se cuentan
        en el próximo corte del escritorio.
        {datos && (
          <> Según lo sincronizado, desde el último corte ({fmtFecha(datos.fecha_inicio)} {fmtHora(datos.fecha_inicio)})
            debería haber <strong className="mono">{fmt(datos.efectivo_esperado)}</strong> en el cajón.</>
        )}
      </div>
      {!escritorioRecibeWeb && (
        <div style={{ color: 'var(--color-warning)', fontWeight: 600 }}>
          ⚠️ Ningún POS de escritorio actualizado se ha conectado en los últimos 2 días: lo que registres
          aquí no aparecerá en su corte hasta que lo actualicen.
        </div>
      )}
    </div>
  );
}

// ─── Tab: Movimientos ─────────────────────────────────────

function TabMovimientos({ movimientos, onNuevo }: { movimientos: MovimientoCaja[]; onNuevo: () => void }) {
  if (movimientos.length === 0) {
    return (
      <div style={{ textAlign: 'center', padding: 48, color: 'var(--color-text-dim)' }}>
        <DollarSign size={32} style={{ marginBottom: 12, opacity: 0.3 }} />
        <p>No hay movimientos pendientes de corte.</p>
        <button className="btn btn-ghost btn-sm" style={{ marginTop: 12 }} onClick={onNuevo}>
          Registrar movimiento
        </button>
      </div>
    );
  }

  const totalEntradas = movimientos.filter(m => m.tipo === 'ENTRADA').reduce((s, m) => s + m.monto, 0);
  const totalRetiros = movimientos.filter(m => m.tipo === 'RETIRO').reduce((s, m) => s + m.monto, 0);

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
      {/* Resumen rápido */}
      <div className="pos-2col-grid" style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12, marginBottom: 8 }}>
        <div className="card" style={{ padding: '12px 16px', display: 'flex', alignItems: 'center', gap: 10 }}>
          <ArrowDownLeft size={18} style={{ color: 'var(--color-success)' }} />
          <div>
            <p style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>Entradas</p>
            <p className="mono" style={{ fontSize: 18, fontWeight: 700, color: 'var(--color-success)' }}>{fmt(totalEntradas)}</p>
          </div>
        </div>
        <div className="card" style={{ padding: '12px 16px', display: 'flex', alignItems: 'center', gap: 10 }}>
          <ArrowUpRight size={18} style={{ color: 'var(--color-danger)' }} />
          <div>
            <p style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>Retiros</p>
            <p className="mono" style={{ fontSize: 18, fontWeight: 700, color: 'var(--color-danger)' }}>{fmt(totalRetiros)}</p>
          </div>
        </div>
      </div>

      {movimientos.map(m => (
        <FilaMovimiento key={m.id} m={m} />
      ))}
    </div>
  );
}

function FilaMovimiento({ m }: { m: MovimientoCaja }) {
  const esEntrada = m.tipo === 'ENTRADA';
  return (
    <div className="card" style={{
      padding: '12px 16px', display: 'flex', alignItems: 'center', gap: 14,
    }}>
      <div style={{
        width: 36, height: 36, borderRadius: 8, flexShrink: 0,
        background: esEntrada ? 'rgba(34,197,94,0.1)' : 'rgba(220,53,69,0.1)',
        display: 'flex', alignItems: 'center', justifyContent: 'center',
      }}>
        {esEntrada
          ? <ArrowDownLeft size={18} style={{ color: 'var(--color-success)' }} />
          : <ArrowUpRight size={18} style={{ color: 'var(--color-danger)' }} />}
      </div>

      <div style={{ flex: 1 }}>
        <p style={{ fontSize: 14, fontWeight: 600, color: 'var(--color-text)' }}>{m.concepto}</p>
        <p style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>
          {m.usuario_nombre} · {fmtHora(m.fecha)}
        </p>
      </div>

      <p className="mono" style={{
        fontSize: 16, fontWeight: 700,
        color: esEntrada ? 'var(--color-success)' : 'var(--color-danger)',
      }}>
        {esEntrada ? '+' : '-'}{fmt(m.monto)}
      </p>
    </div>
  );
}

// ─── Tab: Auditoría de caja ───────────────────────────────
//
// Diagnóstico de solo lectura. Detecta las fugas conocidas del módulo
// de cortes y cuantifica cuánto dinero explica cada una. No modifica
// nada — es para entender antes de corregir.

interface AuditoriaResumen {
  ventas_sin_corte_count: number;
  ventas_sin_corte_efectivo: number;
  movimientos_desalineados_count: number;
  movimientos_desalineados_monto: number;
  movimientos_huerfanos_count: number;
  movimientos_huerfanos_monto: number;
  eslabones_rotos_count: number;
  eslabones_rotos_monto: number;
  cortes_inconsistentes_count: number;
  dias_sin_cierre_count: number;
  impacto_total_estimado: number;
}
interface VentaSinCorte { id: number; folio: string; fecha: string; total: number; metodo_pago: string; }
interface MovDesalineado {
  id: number; tipo: string; monto: number; concepto: string; fecha: string;
  corte_id: number; corte_tipo: string; corte_inicio: string; corte_fin: string;
}
interface MovHuerfano { id: number; tipo: string; monto: number; concepto: string; fecha: string; }
interface EslabonRoto {
  corte_id: number; tipo: string; fecha: string;
  fondo_anterior: number; fondo_inicial: number; diferencia: number;
}
interface CorteInconsistente {
  id: number; tipo: string; created_at: string;
  esperado_guardado: number; esperado_recalculado: number; descuadre: number;
}
interface DiaSinCierre { dia: string; num_ventas: number; total_ventas: number; total_efectivo: number; }
interface Auditoria {
  resumen: AuditoriaResumen;
  ventas_sin_corte: VentaSinCorte[];
  movimientos_desalineados: MovDesalineado[];
  movimientos_huerfanos: MovHuerfano[];
  cadena_rota: EslabonRoto[];
  cortes_inconsistentes: CorteInconsistente[];
  dias_sin_cierre: DiaSinCierre[];
}

function TabAuditoria({ cajaWeb }: { cajaWeb: boolean }) {
  const [data, setData] = useState<Auditoria | null>(null);
  const [cargando, setCargando] = useState(false);
  const [error, setError] = useState('');
  const [dias, setDias] = useState(90);
  const [expandido, setExpandido] = useState<string | null>(null);

  const correr = async (d: number) => {
    setCargando(true);
    setError('');
    try {
      const r = await invoke<Auditoria>('auditar_caja', { limiteDias: d });
      setData(r);
    } catch (e: any) {
      setError(e?.message ?? String(e));
    } finally {
      setCargando(false);
    }
  };

  useEffect(() => { correr(dias); }, []);

  const toggle = (k: string) => setExpandido(prev => (prev === k ? null : k));

  if (cargando && !data) {
    return <div style={{ padding: 40, textAlign: 'center', color: 'var(--color-text-dim)' }}>
      Analizando movimientos de caja…
    </div>;
  }

  if (error) {
    return <div className="card" style={{ padding: 20, color: 'var(--color-danger)' }}>
      Error al auditar: <code style={{ fontSize: 12 }}>{error}</code>
    </div>;
  }

  if (!data) return null;
  const r = data.resumen;
  // Los días sin cierre ya no son una fuga (el siguiente corte los incluye),
  // así que no cuentan para el veredicto.
  const hayProblemas =
    r.ventas_sin_corte_count > 0 || r.movimientos_desalineados_count > 0 ||
    r.movimientos_huerfanos_count > 0 || r.eslabones_rotos_count > 0 ||
    r.cortes_inconsistentes_count > 0;

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
      {/* Controles */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
        <span style={{ fontSize: 12, color: 'var(--color-text-muted)', fontWeight: 600 }}>
          Analizar últimos
        </span>
        {[30, 90, 180, 365].map(d => (
          <button
            key={d}
            className={`btn btn-sm ${dias === d ? '' : 'btn-ghost'}`}
            onClick={() => { setDias(d); correr(d); }}
            disabled={cargando}
          >
            {d} días
          </button>
        ))}
        {cargando && <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>Analizando…</span>}
      </div>

      {/* En la web se audita la caja con la que trabaja este equipo. */}
      {!isTauri() && (
        <div style={{ fontSize: 12, color: 'var(--color-text-muted)' }}>
          {cajaWeb
            ? 'Caja auditada: la caja web (la de los equipos con caja propia).'
            : 'Caja auditada: la caja de la tienda, según lo que ya se sincronizó con el POS de escritorio.'}
        </div>
      )}

      {/* Veredicto */}
      <div className="card" style={{
        padding: 18,
        background: hayProblemas ? 'rgba(239,68,68,0.08)' : 'rgba(34,197,94,0.08)',
        border: `1px solid ${hayProblemas ? 'rgba(239,68,68,0.4)' : 'rgba(34,197,94,0.4)'}`,
      }}>
        {hayProblemas ? (
          <>
            <div style={{ fontSize: 15, fontWeight: 800, color: 'var(--color-danger)', marginBottom: 6 }}>
              Se detectaron descuadres históricos en el flujo de caja
            </div>
            <div style={{ fontSize: 12, color: 'var(--color-text-muted)', marginBottom: 8 }}>
              Los generó el cálculo anterior de cortes. Desde la versión 0.1.39 los cortes forman una
              cadena continua y estas fugas ya no se producen.
            </div>
            <div style={{ fontSize: 13, marginBottom: 10 }}>
              Impacto estimado acumulado:{' '}
              <strong className="mono" style={{ fontSize: 17 }}>
                {r.impacto_total_estimado >= 0 ? '+' : ''}{fmt(r.impacto_total_estimado)}
              </strong>
              <span style={{ color: 'var(--color-text-dim)' }}>
                {' '}({r.impacto_total_estimado >= 0 ? 'sobrante teórico' : 'faltante teórico'})
              </span>
            </div>
          </>
        ) : (
          <div style={{ fontSize: 15, fontWeight: 800, color: 'var(--color-success)' }}>
            No se detectaron descuadres en este período
          </div>
        )}
      </div>

      {/* Hallazgos */}
      <SeccionAuditoria
        clave="dias"
        titulo="Días con ventas sin cierre de caja (informativo)"
        conteo={r.dias_sin_cierre_count}
        monto={data.dias_sin_cierre.reduce((s, d) => s + d.total_efectivo, 0)}
        explicacion="Días en que no se hizo cierre. Con el cálculo nuevo no se pierde nada: el siguiente corte incluye esas ventas, y si al abrir faltaba dinero queda registrado como ajuste de apertura."
        expandido={expandido === 'dias'}
        onToggle={() => toggle('dias')}
      >
        <TablaAudit
          columnas={['Día', 'Ventas', 'Total', 'Efectivo']}
          filas={data.dias_sin_cierre.map(d => [
            d.dia, String(d.num_ventas), fmt(d.total_ventas), fmt(d.total_efectivo),
          ])}
        />
      </SeccionAuditoria>

      <SeccionAuditoria
        clave="ventas"
        titulo="Ventas que no entraron en ningún corte"
        conteo={r.ventas_sin_corte_count}
        monto={r.ventas_sin_corte_efectivo}
        explicacion="Estas ventas ocurrieron fuera del rango de fechas de todos los cortes registrados. El efectivo entró a la caja pero ningún corte lo esperaba."
        expandido={expandido === 'ventas'}
        onToggle={() => toggle('ventas')}
      >
        <TablaAudit
          columnas={['Folio', 'Fecha', 'Método', 'Total']}
          filas={data.ventas_sin_corte.slice(0, 200).map(v => [
            v.folio, v.fecha.substring(0, 16), v.metodo_pago, fmt(v.total),
          ])}
          nota={data.ventas_sin_corte.length > 200
            ? `Mostrando 200 de ${data.ventas_sin_corte.length}` : undefined}
        />
      </SeccionAuditoria>

      <SeccionAuditoria
        clave="movs"
        titulo="Movimientos aplicados al corte equivocado"
        conteo={r.movimientos_desalineados_count}
        monto={r.movimientos_desalineados_monto}
        explicacion="La fecha del movimiento cae fuera del período del corte al que quedó asignado. Pasa porque la asignación barre todos los movimientos huérfanos sin importar cuándo ocurrieron: un retiro viejo se resta del corte de hoy (falta dinero), una entrada vieja se suma (sobra)."
        expandido={expandido === 'movs'}
        onToggle={() => toggle('movs')}
      >
        <TablaAudit
          columnas={['Tipo', 'Monto', 'Concepto', 'Fecha mov.', 'Corte', 'Período del corte']}
          filas={data.movimientos_desalineados.slice(0, 200).map(m => [
            m.tipo, fmt(m.monto), m.concepto, m.fecha.substring(0, 16),
            `#${m.corte_id} ${m.corte_tipo}`,
            `${m.corte_inicio.substring(5, 16)} → ${m.corte_fin.substring(5, 16)}`,
          ])}
          nota={data.movimientos_desalineados.length > 200
            ? `Mostrando 200 de ${data.movimientos_desalineados.length}` : undefined}
        />
      </SeccionAuditoria>

      <SeccionAuditoria
        clave="huerfanos"
        titulo="Movimientos que ningún corte contó"
        conteo={r.movimientos_huerfanos_count}
        monto={r.movimientos_huerfanos_monto}
        explicacion="Retiros o entradas sin corte asignado y anteriores al último corte: el dinero se movió, pero ningún corte lo esperaba y ya ninguno lo contará."
        expandido={expandido === 'huerfanos'}
        onToggle={() => toggle('huerfanos')}
      >
        <TablaAudit
          columnas={['Tipo', 'Monto', 'Concepto', 'Fecha']}
          filas={data.movimientos_huerfanos.map(m => [
            m.tipo, fmt(m.monto), m.concepto, m.fecha.substring(0, 16),
          ])}
        />
      </SeccionAuditoria>

      <SeccionAuditoria
        clave="cadena"
        titulo="Cortes que no arrancaron con lo que dejó el anterior"
        conteo={r.eslabones_rotos_count}
        monto={r.eslabones_rotos_monto}
        explicacion="El fondo inicial de estos cortes no es el que dejó el corte anterior. Es el síntoma de 'al día siguiente no cuadra con lo que se dejó': antes la apertura reemplazaba el fondo sin compararlo."
        expandido={expandido === 'cadena'}
        onToggle={() => toggle('cadena')}
      >
        <TablaAudit
          columnas={['Corte', 'Fecha', 'Dejó el anterior', 'Arrancó con', 'Diferencia']}
          filas={data.cadena_rota.map(e => [
            `#${e.corte_id} ${e.tipo}`,
            e.fecha.substring(0, 16),
            fmt(e.fondo_anterior),
            fmt(e.fondo_inicial),
            `${e.diferencia >= 0 ? '+' : ''}${fmt(e.diferencia)}`,
          ])}
        />
      </SeccionAuditoria>

      <SeccionAuditoria
        clave="inconsistentes"
        titulo="Cortes con aritmética inconsistente"
        conteo={r.cortes_inconsistentes_count}
        monto={data.cortes_inconsistentes.reduce((s, c) => s + c.descuadre, 0)}
        explicacion="En estos cortes, (fondo inicial + ventas efectivo + entradas − retiros) no da el efectivo esperado que quedó guardado. Señal de que los totales se calcularon en momentos distintos: hubo ventas o movimientos entre abrir el corte y confirmarlo."
        expandido={expandido === 'inconsistentes'}
        onToggle={() => toggle('inconsistentes')}
      >
        <TablaAudit
          columnas={['Corte', 'Fecha', 'Guardado', 'Recalculado', 'Descuadre']}
          filas={data.cortes_inconsistentes.map(c => [
            `#${c.id} ${c.tipo}`,
            c.created_at.substring(0, 16),
            fmt(c.esperado_guardado),
            fmt(c.esperado_recalculado),
            `${c.descuadre >= 0 ? '+' : ''}${fmt(c.descuadre)}`,
          ])}
        />
      </SeccionAuditoria>
    </div>
  );
}

function SeccionAuditoria({
  titulo, conteo, monto, explicacion, expandido, onToggle, children,
}: {
  clave: string;
  titulo: string;
  conteo: number;
  monto: number;
  explicacion: string;
  expandido: boolean;
  onToggle: () => void;
  children: React.ReactNode;
}) {
  const limpio = conteo === 0;
  return (
    <div className="card" style={{ padding: 0, overflow: 'hidden', opacity: limpio ? 0.6 : 1 }}>
      <div
        onClick={limpio ? undefined : onToggle}
        style={{
          padding: '14px 16px', cursor: limpio ? 'default' : 'pointer',
          display: 'flex', alignItems: 'center', gap: 12,
          background: expandido ? 'var(--color-surface-2)' : 'transparent',
        }}
      >
        <span style={{ fontSize: 18 }}>{limpio ? '✅' : '⚠️'}</span>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ fontSize: 14, fontWeight: 700 }}>{titulo}</div>
          <div style={{ fontSize: 11, color: 'var(--color-text-dim)', marginTop: 2, lineHeight: 1.45 }}>
            {explicacion}
          </div>
        </div>
        <div style={{ textAlign: 'right', flexShrink: 0 }}>
          <div className="mono" style={{
            fontSize: 16, fontWeight: 800,
            color: limpio ? 'var(--color-success)' : 'var(--color-danger)',
          }}>
            {conteo}
          </div>
          {!limpio && Math.abs(monto) > 0.005 && (
            <div className="mono" style={{ fontSize: 11, color: 'var(--color-text-muted)' }}>
              {monto >= 0 ? '+' : ''}{fmt(monto)}
            </div>
          )}
        </div>
        {!limpio && (expandido ? <ChevronUp size={16} /> : <ChevronDown size={16} />)}
      </div>
      {expandido && !limpio && (
        <div style={{ padding: '0 16px 16px', maxHeight: 340, overflow: 'auto' }}>
          {children}
        </div>
      )}
    </div>
  );
}

function TablaAudit({ columnas, filas, nota }: {
  columnas: string[];
  filas: string[][];
  nota?: string;
}) {
  return (
    <>
      <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 12 }}>
        <thead style={{ position: 'sticky', top: 0, background: 'var(--color-surface)' }}>
          <tr>
            {columnas.map(c => (
              <th key={c} style={{
                textAlign: 'left', padding: '6px 8px', fontSize: 10,
                textTransform: 'uppercase', color: 'var(--color-text-muted)',
                borderBottom: '1px solid var(--color-border)', whiteSpace: 'nowrap',
              }}>{c}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {filas.map((f, i) => (
            <tr key={i} style={{ borderBottom: '1px solid var(--color-border)' }}>
              {f.map((celda, j) => (
                <td key={j} className={j > 0 ? 'mono' : undefined}
                  style={{ padding: '6px 8px', whiteSpace: 'nowrap' }}>
                  {celda}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {nota && (
        <p style={{ fontSize: 11, color: 'var(--color-text-dim)', marginTop: 8 }}>{nota}</p>
      )}
    </>
  );
}

// ─── Tab: Historial ───────────────────────────────────────

function TabHistorial({ cortes }: { cortes: CorteResumen[] }) {
  if (cortes.length === 0) {
    return (
      <div style={{ textAlign: 'center', padding: 48, color: 'var(--color-text-dim)' }}>
        <Clock size={32} style={{ marginBottom: 12, opacity: 0.3 }} />
        <p>No hay cortes registrados aún.</p>
      </div>
    );
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
      {cortes.map(c => <TarjetaCorte key={c.id} corte={c} />)}
    </div>
  );
}

function TarjetaCorte({ corte }: { corte: CorteResumen }) {
  const { obtenerDetalleCorte } = useCortesStore();
  const [expanded, setExpanded] = useState(false);
  const [detalle, setDetalle] = useState<CorteDetalle | null>(null);
  const [cargando, setCargando] = useState(false);

  const dif = corte.diferencia;
  const esDia = corte.tipo === 'DIA';

  const colorDif = dif === 0 ? 'var(--color-success)'
    : dif > 0 ? 'var(--color-warning)'
    : 'var(--color-danger)';

  const labelDif = dif === 0 ? 'Cuadra' : dif > 0 ? `Sobrante +${fmt(dif)}` : `Faltante ${fmt(dif)}`;

  const toggleExpand = async () => {
    if (!expanded && !detalle) {
      setCargando(true);
      try {
        const d = await obtenerDetalleCorte(corte.id);
        setDetalle(d);
      } catch (e) {
        console.error(e);
      }
      setCargando(false);
    }
    setExpanded(!expanded);
  };

  return (
    <div className="card" style={{ padding: 0, overflow: 'hidden' }}>
      {/* Header clickable */}
      <div
        onClick={toggleExpand}
        style={{
          padding: '14px 16px', cursor: 'pointer',
          display: 'flex', flexDirection: 'column', gap: 10,
          background: expanded ? 'var(--color-surface-2)' : 'transparent',
          transition: 'background 0.15s',
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          <span style={{
            fontSize: 10, fontWeight: 700, padding: '2px 8px', borderRadius: 6,
            background: esDia ? 'rgba(99,102,241,0.12)' : 'rgba(158,122,126,0.12)',
            color: esDia ? 'var(--color-primary)' : 'var(--color-text-muted)',
            textTransform: 'uppercase',
          }}>
            {esDia ? 'Cierre del día' : 'Corte de turno'}
          </span>

          <span style={{ fontSize: 13, color: 'var(--color-text-muted)', flex: 1 }}>
            {fmtFecha(corte.created_at)} · {fmtHora(corte.created_at)} · {corte.usuario_nombre}
          </span>

          <span style={{ fontSize: 13, fontWeight: 700, color: colorDif }}>
            {labelDif}
          </span>

          {expanded ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
        </div>

        {/* Resumen de ventas por método */}
        <div style={{ display: 'flex', gap: 16, flexWrap: 'wrap' }}>
          <MiniStat label="Ventas Efectivo" value={fmt(corte.total_ventas_efectivo)} color="var(--color-success)" />
          <MiniStat label="Tarjeta" value={fmt(corte.total_ventas_tarjeta)} color="#6366f1" />
          <MiniStat label="Transferencia" value={fmt(corte.total_ventas_transferencia)} color="#06b6d4" />
          <MiniStat label="Total Ventas" value={fmt(corte.total_ventas)} color="var(--color-text)" bold />
          <div style={{ marginLeft: 'auto', display: 'flex', gap: 16 }}>
            <MiniStat label="Esperado" value={fmt(corte.efectivo_esperado)} color="var(--color-primary)" />
            <MiniStat label="Contado" value={fmt(corte.efectivo_contado)} color="var(--color-text)" />
            <MiniStat label="Fondo sig." value={fmt(corte.fondo_siguiente)} color="var(--color-text-muted)" />
          </div>
        </div>
      </div>

      {/* Nota de diferencia visible sin expandir */}
      {corte.nota_diferencia && (
        <div style={{
          padding: '8px 16px', fontSize: 12,
          background: dif < 0 ? 'rgba(239,68,68,0.06)' : 'rgba(245,158,11,0.06)',
          borderTop: '1px solid var(--color-border)',
          color: dif < 0 ? 'var(--color-danger)' : 'var(--color-warning)',
          fontWeight: 600,
        }}>
          📝 {corte.nota_diferencia}
        </div>
      )}

      {/* Detalle expandido */}
      {expanded && (
        <div style={{ borderTop: '1px solid var(--color-border)', padding: 16 }}>
          {cargando ? (
            <div style={{ textAlign: 'center', padding: 20, color: 'var(--color-text-dim)' }}>Cargando detalle...</div>
          ) : detalle ? (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>

              {/* Desglose de caja */}
              <div>
                <h4 style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-muted)', marginBottom: 8, textTransform: 'uppercase', letterSpacing: 0.5 }}>
                  Desglose de Caja
                </h4>
                <div className="card" style={{ padding: '10px 14px', fontSize: 13 }}>
                  <DesgloseFila label="Fondo inicial" valor={corte.fondo_inicial} />
                  <DesgloseFila label="+ Ventas en efectivo" valor={corte.total_ventas_efectivo} color="var(--color-success)" />
                  {corte.total_entradas_efectivo > 0 && <DesgloseFila label="+ Entradas de caja" valor={corte.total_entradas_efectivo} color="var(--color-success)" />}
                  {corte.total_retiros_efectivo > 0 && <DesgloseFila label="− Retiros de caja" valor={corte.total_retiros_efectivo} color="var(--color-danger)" negativo />}
                  <div style={{ borderTop: '1px solid var(--color-border)', marginTop: 6, paddingTop: 6, display: 'flex', justifyContent: 'space-between', fontWeight: 800 }}>
                    <span>= Efectivo esperado</span>
                    <span className="mono">{fmt(corte.efectivo_esperado)}</span>
                  </div>
                  <div style={{ display: 'flex', justifyContent: 'space-between', marginTop: 4 }}>
                    <span style={{ fontWeight: 600 }}>Efectivo contado</span>
                    <span className="mono" style={{ fontWeight: 700 }}>{fmt(corte.efectivo_contado)}</span>
                  </div>
                  <div style={{ display: 'flex', justifyContent: 'space-between', marginTop: 4, color: colorDif, fontWeight: 800 }}>
                    <span>Diferencia</span>
                    <span className="mono">{dif >= 0 ? '+' : ''}{fmt(dif)}</span>
                  </div>
                </div>
              </div>

              {/* Ventas por método */}
              <div>
                <h4 style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-muted)', marginBottom: 8, textTransform: 'uppercase', letterSpacing: 0.5 }}>
                  Ventas por Método ({corte.num_transacciones} transacciones)
                </h4>
                <div style={{ display: 'flex', gap: 10 }}>
                  <MetodoPagoCard label="💵 Efectivo" valor={corte.total_ventas_efectivo} total={corte.total_ventas} color="var(--color-success)" />
                  <MetodoPagoCard label="💳 Tarjeta" valor={corte.total_ventas_tarjeta} total={corte.total_ventas} color="#6366f1" />
                  <MetodoPagoCard label="📱 Transferencia" valor={corte.total_ventas_transferencia} total={corte.total_ventas} color="#06b6d4" />
                </div>
              </div>

              {/* Denominaciones */}
              {detalle.denominaciones.length > 0 && (
                <div>
                  <h4 style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-muted)', marginBottom: 8, textTransform: 'uppercase', letterSpacing: 0.5 }}>
                    Denominaciones Contadas
                  </h4>
                  <div className="card" style={{ padding: '10px 14px' }}>
                    <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '0 24px' }}>
                      {['BILLETE', 'MONEDA'].map(tipo => {
                        const items = detalle.denominaciones.filter(d => d.tipo === tipo);
                        if (items.length === 0) return null;
                        return (
                          <div key={tipo}>
                            <p style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-dim)', marginBottom: 6 }}>
                              {tipo === 'BILLETE' ? 'BILLETES' : 'MONEDAS'}
                            </p>
                            {items.map(d => (
                              <div key={`${d.denominacion}_${d.tipo}`} style={{ display: 'flex', justifyContent: 'space-between', fontSize: 12, marginBottom: 3 }}>
                                <span>${d.denominacion} × {d.cantidad}</span>
                                <span className="mono" style={{ fontWeight: 600 }}>{fmt(d.subtotal)}</span>
                              </div>
                            ))}
                          </div>
                        );
                      })}
                    </div>
                  </div>
                </div>
              )}

              {/* Movimientos */}
              {detalle.movimientos.length > 0 && (
                <div>
                  <h4 style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-muted)', marginBottom: 8, textTransform: 'uppercase', letterSpacing: 0.5 }}>
                    Movimientos de Caja
                  </h4>
                  <div className="card" style={{ padding: 0 }}>
                    {detalle.movimientos.map(m => (
                      <div key={m.id} style={{
                        padding: '8px 14px', borderBottom: '1px solid var(--color-border)',
                        display: 'flex', alignItems: 'center', gap: 10, fontSize: 12,
                      }}>
                        <span style={{
                          fontSize: 10, fontWeight: 700, padding: '1px 6px', borderRadius: 4,
                          background: m.tipo === 'ENTRADA' ? 'rgba(34,197,94,0.12)' : 'rgba(239,68,68,0.12)',
                          color: m.tipo === 'ENTRADA' ? 'var(--color-success)' : 'var(--color-danger)',
                        }}>
                          {m.tipo === 'ENTRADA' ? '+' : '−'}
                        </span>
                        <span style={{ flex: 1 }}>{m.concepto}</span>
                        <span style={{ color: 'var(--color-text-dim)' }}>{m.usuario_nombre} · {fmtHora(m.fecha)}</span>
                        <span className="mono" style={{ fontWeight: 700 }}>{fmt(m.monto)}</span>
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {/* Vendedores */}
              {detalle.vendedores.length > 0 && (
                <div>
                  <h4 style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-muted)', marginBottom: 8, textTransform: 'uppercase', letterSpacing: 0.5 }}>
                    Resumen por Vendedor
                  </h4>
                  <div className="card" style={{ padding: 0 }}>
                    {detalle.vendedores.map(v => (
                      <div key={v.usuario_id} style={{
                        padding: '8px 14px', borderBottom: '1px solid var(--color-border)',
                        display: 'flex', alignItems: 'center', gap: 10, fontSize: 12,
                      }}>
                        <span style={{ fontWeight: 600, flex: 1 }}>{v.usuario_nombre}</span>
                        <span style={{ color: 'var(--color-text-dim)' }}>{v.num_ventas} ventas</span>
                        <span className="mono" style={{ fontWeight: 700 }}>{fmt(v.total_vendido)}</span>
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </div>
          ) : null}
        </div>
      )}
    </div>
  );
}

// Helpers para TarjetaCorte
function MiniStat({ label, value, color, bold }: { label: string; value: string; color?: string; bold?: boolean }) {
  return (
    <div>
      <p style={{ fontSize: 10, color: 'var(--color-text-dim)', lineHeight: 1 }}>{label}</p>
      <p className="mono" style={{ fontSize: 13, fontWeight: bold ? 800 : 600, color: color || 'var(--color-text)' }}>{value}</p>
    </div>
  );
}

function DesgloseFila({ label, valor, color, negativo }: { label: string; valor: number; color?: string; negativo?: boolean }) {
  return (
    <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 3 }}>
      <span style={{ color: color || 'var(--color-text)' }}>{label}</span>
      <span className="mono" style={{ fontWeight: 600, color: color || 'var(--color-text)' }}>
        {negativo ? '-' : ''}{fmt(valor)}
      </span>
    </div>
  );
}

function MetodoPagoCard({ label, valor, total, color }: { label: string; valor: number; total: number; color: string }) {
  const pct = total > 0 ? ((valor / total) * 100).toFixed(0) : '0';
  return (
    <div className="card" style={{ flex: 1, padding: '10px 12px', textAlign: 'center' }}>
      <p style={{ fontSize: 12, fontWeight: 600, marginBottom: 4 }}>{label}</p>
      <p className="mono" style={{ fontSize: 18, fontWeight: 800, color }}>{fmt(valor)}</p>
      <p style={{ fontSize: 10, color: 'var(--color-text-dim)' }}>{pct}% del total</p>
    </div>
  );
}

// ─── Modal 1: Movimiento de Caja ──────────────────────────

function ModalMovimiento({ onClose, onSuccess }: { onClose: () => void; onSuccess: () => void }) {
  const { usuario } = useAuthStore();
  const { crearMovimiento } = useCortesStore();

  const [tipo, setTipo] = useState<'ENTRADA' | 'RETIRO'>('RETIRO');
  const [monto, setMonto] = useState('');
  const [concepto, setConcepto] = useState('');
  const [pinDueno, setPinDueno] = useState('');
  const [error, setError] = useState('');
  const [guardando, setGuardando] = useState(false);

  const esAdmin = usuario?.es_admin ?? false;
  const montoNum = parseFloat(monto) || 0;
  const requierePin = tipo === 'RETIRO' && montoNum > 500 && !esAdmin;

  const handleConfirmar = async () => {
    if (montoNum <= 0) { setError('El monto debe ser mayor a $0'); return; }
    if (!concepto.trim()) { setError('El concepto es obligatorio'); return; }
    if (requierePin && pinDueno.length < 4) { setError('Ingresa el PIN del dueño'); return; }

    setGuardando(true);
    setError('');
    try {
      await crearMovimiento({
        tipo,
        usuario_id: usuario!.id,
        monto: montoNum,
        concepto: concepto.trim(),
        pin_autorizacion: requierePin ? pinDueno : null,
      });
      await onSuccess();
      onClose();
    } catch (e: any) {
      setError(String(e));
      setGuardando(false);
    }
  };

  return (
    <div className="pos-modal-overlay" style={{
      position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.65)',
      display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 200,
    }} onClick={onClose}>
      <div className="card animate-fade-in pos-modal-content" style={{ width: 400, maxWidth: 400, padding: 24 }}
        onClick={e => e.stopPropagation()}>

        <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 20 }}>
          <DollarSign size={18} style={{ color: 'var(--color-success)' }} />
          <h3 style={{ fontSize: 16, fontWeight: 700, flex: 1 }}>Movimiento de Caja</h3>
          <button className="btn btn-ghost btn-sm" onClick={onClose}><X size={16} /></button>
        </div>

        {/* Tipo */}
        <div style={{ display: 'flex', gap: 8, marginBottom: 18 }}>
          {(['ENTRADA', 'RETIRO'] as const).map(t => (
            <button
              key={t}
              className={`btn ${tipo === t ? 'btn-primary' : 'btn-ghost'}`}
              style={{ flex: 1 }}
              onClick={() => setTipo(t)}
            >
              {t === 'ENTRADA' ? <ArrowDownLeft size={14} /> : <ArrowUpRight size={14} />}
              {t === 'ENTRADA' ? 'Entrada (+)' : 'Retiro (-)'}
            </button>
          ))}
        </div>

        {/* Monto */}
        <div style={{ marginBottom: 14 }}>
          <label style={{ fontSize: 11, fontWeight: 600, color: 'var(--color-text-muted)', display: 'block', marginBottom: 4 }}>
            MONTO
          </label>
          <input
            className="input mono"
            type="number"
            step="0.01"
            min="0"
            placeholder="$0.00"
            value={monto}
            onChange={e => setMonto(e.target.value)}
            autoFocus
            style={{ width: '100%', fontSize: 22, textAlign: 'center' }}
          />
        </div>

        {/* Concepto */}
        <div style={{ marginBottom: 14 }}>
          <label style={{ fontSize: 11, fontWeight: 600, color: 'var(--color-text-muted)', display: 'block', marginBottom: 4 }}>
            CONCEPTO
          </label>
          <input
            className="input"
            placeholder="Ej: Pago a proveedor, cambio extra..."
            value={concepto}
            onChange={e => setConcepto(e.target.value)}
            onKeyDown={e => { if (e.key === 'Enter') handleConfirmar(); }}
            style={{ width: '100%' }}
          />
        </div>

        {/* PIN del dueño (solo si retiro > $500 y no admin) */}
        {requierePin && (
          <div style={{ marginBottom: 14, padding: 12, background: 'rgba(245,158,11,0.1)', borderRadius: 8, border: '1px solid rgba(245,158,11,0.3)' }}>
            <p style={{ fontSize: 12, color: 'var(--color-warning)', fontWeight: 600, marginBottom: 8 }}>
              Retiro mayor a $500 — Se requiere PIN del dueño
            </p>
            <input
              className="input mono"
              type="password"
              maxLength={4}
              placeholder="PIN (4 dígitos)"
              value={pinDueno}
              onChange={e => setPinDueno(e.target.value)}
              style={{ width: '100%', textAlign: 'center', letterSpacing: 8, fontSize: 20 }}
            />
          </div>
        )}

        {error && (
          <p style={{ fontSize: 12, color: 'var(--color-danger)', marginBottom: 12 }}>{error}</p>
        )}

        <div style={{ display: 'flex', gap: 8 }}>
          <button className="btn btn-ghost" style={{ flex: 1 }} onClick={onClose}>Cancelar</button>
          <button
            className="btn btn-primary"
            style={{ flex: 2 }}
            onClick={handleConfirmar}
            disabled={guardando}
          >
            {guardando ? 'Guardando...' : 'Confirmar'}
          </button>
        </div>
      </div>
    </div>
  );
}

// ─── Modal 2: Corte de Turno (Corte Parcial) ─────────
//
// Usado al cambiar de turno. Permite contar todo el efectivo (esperado vs contado),
// calcular la diferencia, y opcionalmente realizar un retiro de ganancias/depósito.

function ModalCorteTurno({ onClose, onSuccess }: { onClose: () => void; onSuccess: () => void }) {
  const { usuario } = useAuthStore();
  const { calcularDatosCorte, crearCorte } = useCortesStore();

  const [datos, setDatos] = useState<DatosCorte | null>(null);
  const [cargandoDatos, setCargandoDatos] = useState(true);
  const [usarDenominaciones, setUsarDenominaciones] = useState(true);
  const [cantidades, setCantidades] = useState<Record<string, number>>({});
  const [efectivoContadoDirecto, setEfectivoContadoDirecto] = useState('');
  
  const [montoRetiro, setMontoRetiro] = useState('');
  const [conceptoRetiro, setConceptoRetiro] = useState('Retiro por cambio de turno');
  const [notaDiferencia, setNotaDiferencia] = useState('');
  const [pinDueno, setPinDueno] = useState('');
  const [error, setError] = useState('');
  const [aviso, setAviso] = useState('');
  const [guardando, setGuardando] = useState(false);

  const esAdmin = usuario?.es_admin ?? false;

  // El período lo define el servidor (desde el último corte hasta ahora), en
  // escritorio y en la web; los parámetros se mandan por compatibilidad y se
  // ignoran.
  const recalcular = () => calcularDatosCorte(fechaHoyInicio(), ahora());

  useEffect(() => {
    recalcular()
      .then(setDatos)
      .catch(e => setError(String(e)))
      .finally(() => setCargandoDatos(false));
  }, []);

  const totalDenominaciones = DENOMINACIONES.reduce((sum, d) => {
    const key = `${d.valor}_${d.tipo}`;
    return sum + (cantidades[key] || 0) * d.valor;
  }, 0);

  const efectivoEsperado = datos?.efectivo_esperado ?? 0;
  const efectivoContado = usarDenominaciones ? totalDenominaciones : (parseFloat(efectivoContadoDirecto) || 0);
  // Redondeado a centavos: sin esto una suma de flotantes daba 0.0000001 de
  // diferencia y exigía nota sin razón.
  const diferencia = datos ? round2(efectivoContado - datos.efectivo_esperado) : 0;
  
  const huboConteo = usarDenominaciones ? totalDenominaciones > 0 : efectivoContadoDirecto !== '';
  const requiereNota = huboConteo && diferencia !== 0;

  const numRetiro = round2(parseFloat(montoRetiro) || 0);
  const requierePin = numRetiro > 500 && !esAdmin;
  const excedeRetiro = numRetiro > efectivoContado;
  
  const fondoSiguiente = round2(efectivoContado - numRetiro);

  const handleConfirmar = async () => {
    if (!datos) return;
    if (efectivoContado < 0) { setError('El efectivo contado no puede ser negativo'); return; }
    if (requiereNota && !notaDiferencia.trim()) { setError('La nota es obligatoria cuando hay diferencia de dinero'); return; }
    if (excedeRetiro) { setError('No puedes retirar más efectivo del que hay contado en caja'); return; }
    if (numRetiro > 0 && !conceptoRetiro.trim()) { setError('El concepto del retiro es obligatorio'); return; }
    if (requierePin && pinDueno.length < 4) { setError('Ingresa el PIN del dueño para el retiro'); return; }

    const denominaciones: DenominacionInput[] | undefined = usarDenominaciones
      ? DENOMINACIONES
          .filter(d => (cantidades[`${d.valor}_${d.tipo}`] || 0) > 0)
          .map(d => ({
            denominacion: d.valor,
            tipo: d.tipo,
            cantidad: cantidades[`${d.valor}_${d.tipo}`] || 0,
          }))
      : undefined;

    setGuardando(true);
    setError('');
    setAviso('');
    const retiro = numRetiro > 0
      ? { monto: numRetiro, concepto: conceptoRetiro.trim(), pin_autorizacion: requierePin ? pinDueno : null }
      : null;
    try {
      // El retiro va DENTRO del corte (escritorio y web): se calcula el
      // esperado, se guarda el corte y se registra el retiro en una sola
      // transacción, así el retiro no descuadra el corte que lo origina ni
      // se duplica si hay que reintentar por DATOS_CAMBIARON.
      await crearCorte({
        tipo: 'PARCIAL',
        usuario_id: usuario!.id,
        fecha_inicio: datos.fecha_inicio,
        fecha_fin: datos.fecha_fin,
        datos,
        efectivo_contado: efectivoContado,
        nota_diferencia: notaDiferencia.trim() || null,
        fondo_siguiente: fondoSiguiente,
        denominaciones,
        retiro,
      });

      await onSuccess();
      onClose();
    } catch (e: any) {
      const msg = String(e);
      if (msg.includes(ERR_DATOS_CAMBIARON)) {
        // Entraron ventas o movimientos mientras se contaba. Recargamos el
        // esperado y conservamos el conteo para que solo haya que revisar.
        try {
          setDatos(await recalcular());
          setAviso('Se registraron ventas o movimientos mientras contabas. El efectivo esperado se actualizó: revisa la diferencia y confirma de nuevo.');
        } catch (e2) {
          setError(String(e2));
        }
      } else {
        setError(msg);
      }
      setGuardando(false);
    }
  };

  return (
    <div className="pos-modal-overlay" style={{
      position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.65)',
      display: 'flex', alignItems: 'flex-start', justifyContent: 'center',
      zIndex: 200, overflowY: 'auto', padding: '20px 0',
    }} onClick={onClose}>
      <div className="card animate-fade-in pos-modal-content pos-modal-fluid" style={{ width: 560, maxWidth: 560, padding: 0, overflow: 'hidden', margin: 'auto' }}
        onClick={e => e.stopPropagation()}>

        {/* Header */}
        <div style={{
          padding: '16px 20px', borderBottom: '1px solid var(--color-border)',
          display: 'flex', alignItems: 'center', gap: 10,
          background: 'rgba(99,102,241,0.08)',
        }}>
          <ArrowUpRight size={18} style={{ color: 'var(--color-primary)' }} />
          <div style={{ flex: 1 }}>
            <h3 style={{ fontSize: 15, fontWeight: 700 }}>Corte de Turno (Cambio de cajero)</h3>
            <p style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>
              {ahora().substring(0, 16).replace('T', ' ')} · {usuario?.nombre_completo}
            </p>
          </div>
          <button className="btn btn-ghost btn-sm" onClick={onClose}><X size={16} /></button>
        </div>

        <div style={{ padding: 20, display: 'flex', flexDirection: 'column', gap: 16 }}>
          {cargandoDatos && (
            <div style={{ textAlign: 'center', padding: 32, color: 'var(--color-text-dim)' }}>
              Calculando cuadre actual...
            </div>
          )}

          {datos && (
            <>
              {/* Resumen de ventas del turno */}
              <div style={{ display: 'flex', gap: 8 }}>
                <div className="card" style={{ flex: 1, padding: '10px 12px', textAlign: 'center' }}>
                  <p style={{ fontSize: 10, color: 'var(--color-text-dim)' }}>💵 Efectivo</p>
                  <p className="mono" style={{ fontSize: 16, fontWeight: 800, color: 'var(--color-success)' }}>{fmt(datos.total_ventas_efectivo)}</p>
                </div>
                <div className="card" style={{ flex: 1, padding: '10px 12px', textAlign: 'center' }}>
                  <p style={{ fontSize: 10, color: 'var(--color-text-dim)' }}>💳 Tarjeta</p>
                  <p className="mono" style={{ fontSize: 16, fontWeight: 800, color: '#6366f1' }}>{fmt(datos.total_ventas_tarjeta)}</p>
                </div>
                <div className="card" style={{ flex: 1, padding: '10px 12px', textAlign: 'center' }}>
                  <p style={{ fontSize: 10, color: 'var(--color-text-dim)' }}>📱 Transferencia</p>
                  <p className="mono" style={{ fontSize: 16, fontWeight: 800, color: '#06b6d4' }}>{fmt(datos.total_ventas_transferencia)}</p>
                </div>
                <div className="card" style={{ flex: 1, padding: '10px 12px', textAlign: 'center', background: 'var(--color-surface-2)' }}>
                  <p style={{ fontSize: 10, color: 'var(--color-text-dim)' }}>Total ({datos.num_transacciones})</p>
                  <p className="mono" style={{ fontSize: 16, fontWeight: 800 }}>{fmt(datos.total_ventas)}</p>
                </div>
              </div>

              {/* Desglose del efectivo esperado */}
              <div style={{
                padding: '12px 16px', borderRadius: 10,
                background: 'var(--color-surface-2)',
                border: '1px solid var(--color-border)',
                fontSize: 13,
              }}>
                <DesgloseFila label="Fondo inicial" valor={datos.fondo_inicial} />
                <DesgloseFila label="+ Ventas en efectivo" valor={datos.total_ventas_efectivo} color="var(--color-success)" />
                {datos.total_entradas_efectivo > 0 && <DesgloseFila label="+ Entradas de caja" valor={datos.total_entradas_efectivo} color="var(--color-success)" />}
                {datos.total_retiros_efectivo > 0 && <DesgloseFila label="− Retiros de caja" valor={datos.total_retiros_efectivo} color="var(--color-danger)" negativo />}
                <div style={{
                  borderTop: '1px solid var(--color-border)', marginTop: 6, paddingTop: 6,
                  display: 'flex', justifyContent: 'space-between', alignItems: 'center',
                }}>
                  <span style={{ fontWeight: 800 }}>= Efectivo esperado en caja</span>
                  <span className="mono" style={{ fontSize: 22, fontWeight: 800, color: 'var(--color-primary)' }}>
                    {fmt(efectivoEsperado)}
                  </span>
                </div>
              </div>

              {/* Conteo de caja */}
              <div>
                <button
                  className="btn btn-ghost btn-sm"
                  style={{ width: '100%', justifyContent: 'space-between', marginBottom: 8 }}
                  onClick={() => setUsarDenominaciones(!usarDenominaciones)}
                >
                  <span style={{ fontWeight: 700, color: 'var(--color-text)' }}>
                    1. CUENTA TODA LA CAJA {usarDenominaciones ? '(Fórmula)' : '(Poner Total)'}
                  </span>
                  {usarDenominaciones ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
                </button>

                {usarDenominaciones ? (
                  <div className="card" style={{ padding: '10px 14px' }}>
                    <div className="pos-2col-grid" style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '0 24px' }}>
                      {['BILLETE', 'MONEDA'].map(tipoD => (
                        <div key={tipoD}>
                          <p style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-dim)', marginBottom: 8 }}>
                            {tipoD === 'BILLETE' ? 'BILLETES' : 'MONEDAS'}
                          </p>
                          {DENOMINACIONES.filter(d => d.tipo === tipoD).map(d => {
                            const key = `${d.valor}_${d.tipo}`;
                            return (
                              <div key={key} style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 6 }}>
                                <span className="mono" style={{ width: 52, fontSize: 13, fontWeight: 600 }}>{d.label}</span>
                                <span style={{ color: 'var(--color-text-dim)', fontSize: 13 }}>×</span>
                                <input
                                  className="input mono"
                                  type="number"
                                  min="0"
                                  step="1"
                                  placeholder="0"
                                  value={cantidades[key] || ''}
                                  onChange={e => setCantidades(prev => ({ ...prev, [key]: parseInt(e.target.value) || 0 }))}
                                  style={{ width: 64, textAlign: 'center', padding: '4px 8px', fontSize: 13 }}
                                />
                                <span className="mono" style={{ fontSize: 12, color: 'var(--color-text-dim)', width: 72, textAlign: 'right' }}>
                                  {fmt((cantidades[key] || 0) * d.valor)}
                                </span>
                              </div>
                            );
                          })}
                        </div>
                      ))}
                    </div>
                    <div style={{
                      marginTop: 10, paddingTop: 10, borderTop: '1px solid var(--color-border)',
                      display: 'flex', justifyContent: 'space-between', alignItems: 'center',
                    }}>
                      <span style={{ fontSize: 13, fontWeight: 700 }}>TOTAL CONTADO EN CAJA</span>
                      <span className="mono" style={{ fontSize: 20, fontWeight: 800, color: 'var(--color-text)' }}>
                        {fmt(efectivoContado)}
                      </span>
                    </div>
                  </div>
                ) : (
                  <input
                    className="input mono"
                    type="number"
                    step="0.01"
                    min="0"
                    placeholder="$0.00"
                    value={efectivoContadoDirecto}
                    onChange={e => setEfectivoContadoDirecto(e.target.value)}
                    autoFocus
                    style={{ width: '100%', fontSize: 26, textAlign: 'center' }}
                  />
                )}
              </div>

              {/* Diferencia en tiempo real */}
              {huboConteo && (
                <div style={{ marginBottom: 10 }}>
                  <FilaDiferencia diferencia={diferencia} />
                </div>
              )}

              {/* Nota (obligatoria si hay diferencia) */}
              {requiereNota && (
                <div>
                  <label style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-warning)', display: 'block', marginBottom: 6 }}>
                    NOTA EXPLICATIVA SOBRE LA DIFERENCIA (Obligatoria)
                  </label>
                  <textarea
                    className="input"
                    placeholder="¿Por qué hay sobrante o faltante?"
                    value={notaDiferencia}
                    onChange={e => setNotaDiferencia(e.target.value)}
                    style={{ width: '100%', minHeight: 60, resize: 'vertical' }}
                  />
                </div>
              )}

              {/* Monto a retirar */}
              <div>
                <label style={{ fontSize: 13, fontWeight: 800, color: 'var(--color-text)', display: 'block', marginBottom: 8 }}>
                  2. ¿CUÁNTO DINERO RETIRAS? (Opcional)
                </label>
                <div style={{ display: 'flex', gap: 10, alignItems: 'flex-start' }}>
                  <div style={{ flex: 1 }}>
                    <input
                      className="input mono"
                      type="number"
                      step="0.01"
                      min="0"
                      placeholder="Ej. $0.00"
                      value={montoRetiro}
                      onChange={e => setMontoRetiro(e.target.value)}
                      style={{ width: '100%', fontSize: 20, padding: 12 }}
                    />
                  </div>
                  {numRetiro > 0 && (
                    <div style={{ flex: 2 }}>
                      <input
                        className="input"
                        placeholder="Concepto del retiro..."
                        value={conceptoRetiro}
                        onChange={e => setConceptoRetiro(e.target.value)}
                        style={{ width: '100%', fontSize: 14, padding: 14 }}
                      />
                    </div>
                  )}
                </div>
              </div>

              {/* Quedará en caja — la info clave del flujo */}
              <div style={{
                padding: '14px 16px', borderRadius: 10,
                background: excedeRetiro
                  ? 'rgba(239, 68, 68, 0.1)'
                  : 'rgba(34, 197, 94, 0.1)',
                border: `1px solid ${excedeRetiro ? 'var(--color-danger)' : 'var(--color-success)'}`,
                display: 'flex', justifyContent: 'space-between', alignItems: 'center',
                marginTop: 4
              }}>
                <span style={{ fontSize: 13, fontWeight: 800 }}>
                  {excedeRetiro ? '⚠️ No hay suficiente para retirar' : '3. FONDO PARA EL SIG. TURNO (Se quedará)'}
                </span>
                <span className="mono" style={{
                  fontSize: 24, fontWeight: 900,
                  color: excedeRetiro ? 'var(--color-danger)' : 'var(--color-success)',
                }}>
                  {fmt(fondoSiguiente)}
                </span>
              </div>

              {/* PIN del dueño si retiro > $500 y no admin */}
              {requierePin && (
                <div style={{
                  padding: 12, background: 'rgba(245,158,11,0.1)',
                  borderRadius: 8, border: '1px solid rgba(245,158,11,0.3)',
                }}>
                  <p style={{ fontSize: 12, color: 'var(--color-warning)', fontWeight: 600, marginBottom: 8 }}>
                    Retiro mayor a $500 — Se requiere PIN del dueño
                  </p>
                  <input
                    className="input mono"
                    type="password"
                    maxLength={4}
                    placeholder="PIN (4 dígitos)"
                    value={pinDueno}
                    onChange={e => setPinDueno(e.target.value)}
                    style={{ width: '100%', textAlign: 'center', letterSpacing: 8, fontSize: 20 }}
                  />
                </div>
              )}
            </>
          )}

          {aviso && <AvisoRecalculo texto={aviso} />}
          {error && <p style={{ fontSize: 13, fontWeight: 600, color: 'var(--color-danger)', textAlign: 'center' }}>{error}</p>}

          <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
            <button className="btn btn-ghost" style={{ flex: 1 }} onClick={onClose}>Cancelar</button>
            <button
              className="btn btn-primary"
              style={{ flex: 2 }}
              onClick={handleConfirmar}
              disabled={cargandoDatos || guardando || !datos}
            >
              {guardando ? 'Guardando...' : 'Confirmar Corte de Turno'}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

// ─── Modal 3: cierre de caja ───────────────────────────────

function ModalCorteDelDia({ onClose, onSuccess, fechaObjetivo }: {
  onClose: () => void;
  onSuccess: () => void;
  fechaObjetivo?: string | null;
}) {
  const { usuario } = useAuthStore();
  const { calcularDatosCorte, crearCorte, cargando, obtenerInicioProximoCierre } = useCortesStore();

  const [datos, setDatos] = useState<DatosCorte | null>(null);
  const [rangoFechas, setRangoFechas] = useState<{ inicio: string; fin: string } | null>(null);
  const [cargandoDatos, setCargandoDatos] = useState(true);
  const [usarDenominaciones, setUsarDenominaciones] = useState(true);
  const [cantidades, setCantidades] = useState<Record<string, number>>({});
  const [efectivoContadoDirecto, setEfectivoContadoDirecto] = useState('');
  const [nota, setNota] = useState('');
  const [fondoSiguiente, setFondoSiguiente] = useState('2000');
  const [error, setError] = useState('');
  const [aviso, setAviso] = useState('');

  // Hay ventas de días anteriores que todavía no entran en ningún corte.
  // Ya NO se cierra "el día de ayer" por separado (no se puede contar el
  // pasado): este cierre las incluye todas, desde el último corte.
  const esExtemporaneo = !!fechaObjetivo;
  // Un cierre cuenta para el día en que se confirma (uno por día). Si lo
  // pendiente es de un día anterior, este cierre ocupa el cierre de HOY.
  const cuentaComoCierreDeHoy = !!fechaObjetivo && fechaObjetivo.slice(0, 10) < fechaHoyInicio().slice(0, 10);

  // El cierre cubre desde el último corte hasta AHORA. Antes terminaba a
  // las 23:59:59 del día, y las ventas hechas después de cerrar quedaban
  // dentro de un rango ya cerrado sin contarse nunca.
  const recalcular = async () => {
    const fInicio = await obtenerInicioProximoCierre().catch(() => fechaHoyInicio());
    const fFin = ahora();
    setRangoFechas({ inicio: fInicio, fin: fFin });
    return calcularDatosCorte(fInicio, fFin);
  };

  useEffect(() => {
    recalcular()
      .then(setDatos)
      .catch(e => setError(String(e)))
      .finally(() => setCargandoDatos(false));
  }, []);

  const totalDenominaciones = DENOMINACIONES.reduce((sum, d) => {
    const key = `${d.valor}_${d.tipo}`;
    return sum + (cantidades[key] || 0) * d.valor;
  }, 0);

  const efectivoContado = usarDenominaciones
    ? totalDenominaciones
    : parseFloat(efectivoContadoDirecto) || 0;

  const diferencia = datos ? round2(efectivoContado - datos.efectivo_esperado) : 0;
  const requiereNota = efectivoContado > 0 && diferencia !== 0;

  const handleConfirmar = async () => {
    if (!datos || !rangoFechas) return;
    if (efectivoContado < 0) { setError('El efectivo contado no puede ser negativo'); return; }
    if (diferencia !== 0 && !nota.trim()) { setError('La nota es obligatoria cuando hay diferencia'); return; }

    const fondoNum = round2(parseFloat(fondoSiguiente) || 0);
    if (fondoNum > efectivoContado) {
      setError(`No puedes dejar ${fmt(fondoNum)} de fondo si solo contaste ${fmt(efectivoContado)}`);
      return;
    }

    const denominaciones: DenominacionInput[] | undefined = usarDenominaciones
      ? DENOMINACIONES
          .filter(d => (cantidades[`${d.valor}_${d.tipo}`] || 0) > 0)
          .map(d => ({
            denominacion: d.valor,
            tipo: d.tipo,
            cantidad: cantidades[`${d.valor}_${d.tipo}`] || 0,
          }))
      : undefined;

    setError('');
    setAviso('');
    try {
      await crearCorte({
        tipo: 'DIA',
        usuario_id: usuario!.id,
        fecha_inicio: rangoFechas.inicio,
        fecha_fin: rangoFechas.fin,
        datos,
        efectivo_contado: efectivoContado,
        nota_diferencia: nota.trim() || null,
        fondo_siguiente: fondoNum,
        denominaciones,
      });
      await onSuccess();
      onClose();
    } catch (e: any) {
      const msg = String(e);
      if (msg.includes(ERR_DATOS_CAMBIARON)) {
        try {
          setDatos(await recalcular());
          setAviso('Se registraron ventas o movimientos mientras contabas. El efectivo esperado se actualizó: revisa la diferencia y confirma de nuevo.');
        } catch (e2) {
          setError(String(e2));
        }
      } else {
        setError(msg);
      }
    }
  };

  return (
    <div className="pos-modal-overlay" style={{
      position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.75)',
      display: 'flex', alignItems: 'flex-start', justifyContent: 'center',
      zIndex: 200, overflowY: 'auto', padding: '20px 0',
    }} onClick={onClose}>
      <div className="card animate-fade-in pos-modal-content pos-modal-fluid" style={{ width: 600, maxWidth: 600, padding: 0, overflow: 'hidden', margin: 'auto' }}
        onClick={e => e.stopPropagation()}>

        {/* Header */}
        <div style={{
          padding: '16px 20px', borderBottom: '1px solid var(--color-border)',
          display: 'flex', alignItems: 'center', gap: 10,
          background: 'rgba(34,197,94,0.08)',
        }}>
          <CheckCircle size={18} style={{ color: 'var(--color-success)' }} />
          <div style={{ flex: 1 }}>
            <h3 style={{ fontSize: 15, fontWeight: 700 }}>Cierre de caja</h3>
            <p style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>
              {`${new Date().toLocaleDateString('es-MX', { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric' })} · ${usuario?.nombre_completo}`}
            </p>
            {esExtemporaneo && (
              <p style={{ fontSize: 11, color: 'var(--color-warning)', marginTop: 2, fontWeight: 600 }}>
                Incluye las ventas pendientes desde el {fechaObjetivo} (días sin cerrar).
              </p>
            )}
            {cuentaComoCierreDeHoy && (
              <p style={{ fontSize: 11, color: 'var(--color-danger)', marginTop: 2, fontWeight: 600 }}>
                ⚠️ Este cierre contará como el cierre de HOY; ya no podrás hacer otro cierre hoy. Si solo quieres contar el dinero ahora, usa Corte de turno.
              </p>
            )}
          </div>
          <button className="btn btn-ghost btn-sm" onClick={onClose}><X size={16} /></button>
        </div>

        <div style={{ padding: 20, display: 'flex', flexDirection: 'column', gap: 16, maxHeight: '80vh', overflowY: 'auto' }}>
          {cargandoDatos && (
            <div style={{ textAlign: 'center', padding: 32, color: 'var(--color-text-dim)' }}>
              Calculando resumen del día...
            </div>
          )}

          {datos && (
            <>
              {/* VENTAS */}
              <SeccionResumen titulo="RESUMEN DE VENTAS">
                <FilaResumen label="Efectivo" valor={fmt(datos.total_ventas_efectivo)} color="var(--color-success)" />
                <FilaResumen label="Tarjeta" valor={fmt(datos.total_ventas_tarjeta)} />
                <FilaResumen label="Transferencia" valor={fmt(datos.total_ventas_transferencia)} />
                <FilaResumen label={`Total (${datos.num_transacciones} transacciones)`} valor={fmt(datos.total_ventas)} bold />
                {datos.total_descuentos > 0 && (
                  <FilaResumen label="Descuentos" valor={`-${fmt(datos.total_descuentos)}`} color="var(--color-text-dim)" />
                )}
                {datos.total_anulaciones > 0 && (
                  <FilaResumen label="Anulaciones" valor={`-${fmt(datos.total_anulaciones)}`} color="var(--color-danger)" />
                )}
              </SeccionResumen>

              {/* Aviso de cortes parciales */}
              {datos.cortes_parciales_hoy > 0 && (
                <div style={{
                  padding: '10px 14px', borderRadius: 8,
                  background: 'rgba(99,102,241,0.08)', border: '1px solid rgba(99,102,241,0.2)',
                  fontSize: 12, color: 'var(--color-primary)',
                }}>
                  <strong>ℹ️ Se realizaron {datos.cortes_parciales_hoy} corte{datos.cortes_parciales_hoy > 1 ? 's' : ''} de turno hoy.</strong>
                  {datos.total_retirado_parciales > 0 && (
                    <span> Se retiraron <strong>{fmt(datos.total_retirado_parciales)}</strong> en cambios de turno.</span>
                  )}
                  <span> El efectivo esperado ya refleja solo lo que debe estar en caja ahora.</span>
                </div>
              )}

              {/* CAJA */}
              <SeccionResumen titulo="CÁLCULO DE EFECTIVO EN CAJA">
                <FilaResumen label="Fondo (desde último corte de turno o apertura)" valor={fmt(datos.fondo_inicial)} />
                <FilaResumen label="(+) Ventas efectivo (desde último turno)" valor={fmt(datos.total_ventas_efectivo)} color="var(--color-success)" />
                {datos.total_entradas_efectivo > 0 && (
                  <FilaResumen label="(+) Entradas" valor={fmt(datos.total_entradas_efectivo)} color="var(--color-success)" />
                )}
                {datos.total_retiros_efectivo > 0 && (
                  <FilaResumen label="(-) Retiros pendientes" valor={`-${fmt(datos.total_retiros_efectivo)}`} color="var(--color-danger)" />
                )}
                <div style={{ margin: '6px 0', borderTop: '1px solid var(--color-border)' }} />
              </SeccionResumen>

              {/* EFECTIVO ESPERADO — MUY PROMINENTE */}
              <div style={{
                padding: '20px 24px', borderRadius: 12,
                background: 'linear-gradient(135deg, rgba(99,102,241,0.12), rgba(99,102,241,0.04))',
                border: '2px solid var(--color-primary)',
                textAlign: 'center',
              }}>
                <p style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-muted)', textTransform: 'uppercase', letterSpacing: 1, marginBottom: 4 }}>
                  💰 Efectivo que debe haber en caja ahora
                </p>
                <p className="mono" style={{ fontSize: 42, fontWeight: 900, color: 'var(--color-primary)', lineHeight: 1 }}>
                  {fmt(datos.efectivo_esperado)}
                </p>
                {datos.cortes_parciales_hoy > 0 && datos.total_retirado_parciales > 0 && (
                  <p style={{ fontSize: 11, color: 'var(--color-text-dim)', marginTop: 6 }}>
                    (Ya se retiraron {fmt(datos.total_retirado_parciales)} en cortes de turno)
                  </p>
                )}
              </div>

              {/* DETALLE DE MOVIMIENTOS */}
              {datos.movimientos.length > 0 && (
                <SeccionResumen titulo="DETALLE DE MOVIMIENTOS">
                  <ListaMovimientosDetalle movimientos={datos.movimientos} />
                </SeccionResumen>
              )}

              {/* VENDEDORES */}
              {datos.vendedores.length > 0 && (
                <SeccionResumen titulo="VENDEDORES DEL DÍA">
                  {datos.vendedores.map(v => (
                    <div key={v.usuario_id} style={{ display: 'flex', justifyContent: 'space-between', padding: '4px 0', fontSize: 13 }}>
                      <span style={{ color: 'var(--color-text)' }}>{v.usuario_nombre}</span>
                      <span style={{ display: 'flex', gap: 12 }}>
                        <span style={{ color: 'var(--color-text-dim)' }}>{v.num_ventas} ventas</span>
                        <span className="mono" style={{ fontWeight: 700 }}>{fmt(v.total_vendido)}</span>
                      </span>
                    </div>
                  ))}
                </SeccionResumen>
              )}

              {/* DENOMINACIONES */}
              <div>
                <button
                  className="btn btn-ghost btn-sm"
                  style={{ width: '100%', justifyContent: 'space-between', marginBottom: 8 }}
                  onClick={() => setUsarDenominaciones(!usarDenominaciones)}
                >
                  <span style={{ fontWeight: 700, color: 'var(--color-text)' }}>
                    1. CUENTA TODA LA CAJA {usarDenominaciones ? '(Fórmula)' : '(Poner Total)'}
                  </span>
                  {usarDenominaciones ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
                </button>

                {usarDenominaciones && (
                  <div className="card" style={{ marginTop: 10, padding: '10px 14px' }}>
                    <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '0 24px' }}>
                      {['BILLETE', 'MONEDA'].map(tipoD => (
                        <div key={tipoD}>
                          <p style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-dim)', marginBottom: 8 }}>
                            {tipoD === 'BILLETE' ? 'BILLETES' : 'MONEDAS'}
                          </p>
                          {DENOMINACIONES.filter(d => d.tipo === tipoD).map(d => {
                            const key = `${d.valor}_${d.tipo}`;
                            return (
                              <div key={key} style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 6 }}>
                                <span className="mono" style={{ width: 52, fontSize: 13, fontWeight: 600 }}>{d.label}</span>
                                <span style={{ color: 'var(--color-text-dim)', fontSize: 13 }}>×</span>
                                <input
                                  className="input mono"
                                  type="number"
                                  min="0"
                                  step="1"
                                  placeholder="0"
                                  value={cantidades[key] || ''}
                                  onChange={e => setCantidades(prev => ({ ...prev, [key]: parseInt(e.target.value) || 0 }))}
                                  style={{ width: 64, textAlign: 'center', padding: '4px 8px', fontSize: 13 }}
                                />
                                <span className="mono" style={{ fontSize: 12, color: 'var(--color-text-dim)', width: 72, textAlign: 'right' }}>
                                  {fmt((cantidades[key] || 0) * d.valor)}
                                </span>
                              </div>
                            );
                          })}
                        </div>
                      ))}
                    </div>
                    <div style={{
                      marginTop: 10, paddingTop: 10, borderTop: '1px solid var(--color-border)',
                      display: 'flex', justifyContent: 'space-between', alignItems: 'center',
                    }}>
                      <span style={{ fontSize: 13, fontWeight: 700 }}>TOTAL CONTADO</span>
                      <span className="mono" style={{ fontSize: 20, fontWeight: 800, color: 'var(--color-success)' }}>
                        {fmt(totalDenominaciones)}
                      </span>
                    </div>
                  </div>
                )}
              </div>

              {/* Input directo (si no usa denominaciones) */}
              {!usarDenominaciones && (
                <div>
                  <label style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-muted)', display: 'block', marginBottom: 6 }}>
                    EFECTIVO CONTADO (lo que hay físicamente)
                  </label>
                  <input
                    className="input mono"
                    type="number"
                    step="0.01"
                    min="0"
                    placeholder="$0.00"
                    value={efectivoContadoDirecto}
                    onChange={e => setEfectivoContadoDirecto(e.target.value)}
                    style={{ width: '100%', fontSize: 24, textAlign: 'center' }}
                  />
                </div>
              )}

              {/* Diferencia */}
              {efectivoContado > 0 && (
                <FilaDiferencia diferencia={diferencia} grande />
              )}

              {/* Nota */}
              {requiereNota && (
                <div>
                  <label style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-warning)', display: 'block', marginBottom: 6 }}>
                    NOTA EXPLICATIVA (obligatoria)
                  </label>
                  <textarea
                    className="input"
                    placeholder="¿Por qué hay diferencia?"
                    value={nota}
                    onChange={e => setNota(e.target.value)}
                    style={{ width: '100%', minHeight: 72, resize: 'vertical' }}
                  />
                </div>
              )}

              {/* Fondo siguiente */}
              <div style={{
                padding: 14, borderRadius: 10,
                background: 'var(--color-surface-2)', border: '1px solid var(--color-border)',
              }}>
                <label style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-muted)', display: 'block', marginBottom: 8 }}>
                  FONDO DE CAJA PARA MAÑANA
                </label>
                <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
                  <input
                    className="input mono"
                    type="number"
                    step="100"
                    min="0"
                    value={fondoSiguiente}
                    onChange={e => setFondoSiguiente(e.target.value)}
                    style={{ width: 140, textAlign: 'center', fontSize: 18 }}
                  />
                  <span style={{ fontSize: 13, color: 'var(--color-text-dim)' }}>
                    Efectivo a retirar: <strong className="mono">{fmt(Math.max(0, efectivoContado - (parseFloat(fondoSiguiente) || 0)))}</strong>
                  </span>
                </div>
              </div>
            </>
          )}

          {aviso && <AvisoRecalculo texto={aviso} />}
          {error && <p style={{ fontSize: 12, color: 'var(--color-danger)' }}>{error}</p>}

          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-ghost" style={{ flex: 1 }} onClick={onClose}>Cancelar</button>
            <button
              className="btn btn-primary"
              style={{ flex: 2 }}
              onClick={handleConfirmar}
              disabled={cargandoDatos || cargando || !datos}
            >
              {cargando ? 'Cerrando día...' : 'Confirmar cierre del día'}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

// ─── Componentes reutilizables ────────────────────────────

function SeccionResumen({ titulo, children }: { titulo: string; children: React.ReactNode }) {
  return (
    <div className="card" style={{ padding: '12px 16px' }}>
      <p style={{ fontSize: 10, fontWeight: 700, color: 'var(--color-text-dim)', letterSpacing: 1, marginBottom: 10 }}>
        {titulo}
      </p>
      {children}
    </div>
  );
}

function ListaMovimientosDetalle({ movimientos }: { movimientos: MovimientoCaja[] }) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
      {movimientos.map(m => {
        const esEntrada = m.tipo === 'ENTRADA';
        const esDevolucion = m.concepto.toLowerCase().startsWith('devolución');
        const color = esEntrada ? 'var(--color-success)' : 'var(--color-danger)';
        const badgeLabel = esDevolucion ? 'DEVOLUCIÓN' : m.tipo;
        return (
          <div key={m.id} style={{
            display: 'flex', alignItems: 'center', gap: 10,
            padding: '6px 8px', borderRadius: 6,
            background: 'var(--color-surface-2)',
          }}>
            <span style={{
              fontSize: 9, fontWeight: 800, padding: '2px 6px', borderRadius: 4,
              background: `${color}22`, color, letterSpacing: 0.5, flexShrink: 0,
            }}>
              {badgeLabel}
            </span>
            <div style={{ flex: 1, minWidth: 0 }}>
              <p style={{
                fontSize: 12, fontWeight: 600, color: 'var(--color-text)',
                overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
              }} title={m.concepto}>
                {m.concepto}
              </p>
              <p style={{ fontSize: 10, color: 'var(--color-text-dim)' }}>
                {m.usuario_nombre} · {fmtHora(m.fecha)}
              </p>
            </div>
            <span className="mono" style={{ fontSize: 13, fontWeight: 700, color, flexShrink: 0 }}>
              {esEntrada ? '+' : '-'}{fmt(m.monto)}
            </span>
          </div>
        );
      })}
    </div>
  );
}

function FilaResumen({ label, valor, bold, color }: {
  label: string; valor: string; bold?: boolean; color?: string;
}) {
  return (
    <div style={{ display: 'flex', justifyContent: 'space-between', padding: '3px 0', fontSize: 13 }}>
      <span style={{ color: 'var(--color-text-muted)' }}>{label}</span>
      <span className="mono" style={{ fontWeight: bold ? 800 : 600, color: color || 'var(--color-text)' }}>
        {valor}
      </span>
    </div>
  );
}

// ─── Modal: Apertura de Caja (bloqueante al iniciar sesión) ──

interface ModalAperturaProps {
  onSuccess?: () => void;
  onClose?: () => void;
  bloqueante?: boolean;
}

export function ModalAperturaCaja({ onSuccess, onClose, bloqueante = true }: ModalAperturaProps) {
  const { usuario } = useAuthStore();
  const { crearApertura, obtenerEsperadoApertura } = useCortesStore();

  const [fondo, setFondo] = useState('');
  const [nota, setNota] = useState('');
  const [error, setError] = useState('');
  const [guardando, setGuardando] = useState(false);
  const [cargando, setCargando] = useState(true);
  // Lo que DEBERÍA haber (fondo del último corte + lo que pasó desde
  // entonces), en escritorio y en la caja web. El campo arranca vacío a
  // propósito: antes venía lleno con la sugerencia y se confirmaba sin
  // contar, así que un faltante de la noche nunca se detectaba al abrir.
  const [esperado, setEsperado] = useState<EsperadoApertura | null>(null);

  useEffect(() => {
    obtenerEsperadoApertura()
      .then(setEsperado)
      .catch(() => setEsperado(null))
      .finally(() => setCargando(false));
  }, []);

  const fondoNum = round2(parseFloat(fondo) || 0);
  const comparar = !!esperado?.hay_referencia && fondo !== '';
  const diferencia = comparar ? round2(fondoNum - esperado!.esperado) : 0;

  const handleConfirmar = async () => {
    if (fondo === '') { setError('Cuenta el efectivo de la caja y escribe el total'); return; }
    if (fondoNum < 0) { setError('El fondo no puede ser negativo'); return; }
    if (diferencia !== 0 && !nota.trim()) {
      setError('Lo contado no coincide con lo esperado: escribe una nota explicando la diferencia');
      return;
    }
    setGuardando(true);
    setError('');
    try {
      await crearApertura({
        usuario_id: usuario!.id,
        fondo_declarado: fondoNum,
        nota: nota.trim() || null,
      });
      onSuccess?.();
      onClose?.();
    } catch (e: any) {
      setError(String(e));
      setGuardando(false);
    }
  };

  const handleBackdrop = () => {
    if (!bloqueante) onClose?.();
  };

  return (
    <div className="pos-modal-overlay" style={{
      position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.85)',
      display: 'flex', alignItems: 'center', justifyContent: 'center',
      zIndex: 9999,
    }} onClick={handleBackdrop}>
      <div className="card animate-fade-in pos-modal-content" style={{ width: 440, maxWidth: 440, padding: 0, overflow: 'hidden' }}
        onClick={e => e.stopPropagation()}>

        <div style={{
          padding: '18px 22px', borderBottom: '1px solid var(--color-border)',
          background: 'linear-gradient(135deg, rgba(245,158,11,0.15), rgba(99,102,241,0.08))',
          display: 'flex', alignItems: 'center', gap: 12,
        }}>
          <Sunrise size={22} style={{ color: 'var(--color-warning)' }} />
          <div style={{ flex: 1 }}>
            <h3 style={{ fontSize: 16, fontWeight: 800 }}>Apertura de Caja</h3>
            <p style={{ fontSize: 12, color: 'var(--color-text-dim)', marginTop: 2 }}>
              {new Date().toLocaleDateString('es-MX', { weekday: 'long', day: 'numeric', month: 'long' })} · {usuario?.nombre_completo}
            </p>
          </div>
          {!bloqueante && (
            <button className="btn btn-ghost btn-sm" onClick={onClose}><X size={16} /></button>
          )}
        </div>

        <div style={{ padding: 22, display: 'flex', flexDirection: 'column', gap: 16 }}>
          <p style={{ fontSize: 13, color: 'var(--color-text-muted)', lineHeight: 1.5 }}>
            {esperado
              ? <>Antes de empezar, <strong>cuenta el efectivo que hay en la caja</strong>. Se compara contra lo que dejó el último corte para detectar faltantes o sobrantes desde entonces.</>
              : <>Antes de empezar las operaciones del día, declara el efectivo con el que abres la caja. Este será el <strong>fondo inicial</strong> contra el que se cuadrará el corte de hoy.</>}
          </p>

          {cargando ? (
            <p style={{ textAlign: 'center', color: 'var(--color-text-dim)', padding: 20 }}>Cargando sugerencia...</p>
          ) : (
            <>
              {esperado?.hay_referencia && (
                <div style={{
                  padding: 12, borderRadius: 8, background: 'var(--color-surface-2)',
                  display: 'flex', alignItems: 'center', gap: 12,
                }}>
                  <div style={{ flex: 1 }}>
                    <div style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-muted)' }}>DEBERÍA HABER</div>
                    <div className="mono" style={{ fontSize: 22, fontWeight: 800 }}>{fmt(esperado.esperado)}</div>
                    <div style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>
                      Fondo del último corte {fmt(esperado.fondo_ultimo_corte)}
                      {esperado.flujo_desde_corte !== 0 && (
                        <> {esperado.flujo_desde_corte > 0 ? '+' : '−'} {fmt(Math.abs(esperado.flujo_desde_corte))} de movimientos desde entonces</>
                      )}
                    </div>
                  </div>
                  <button
                    className="btn btn-sm"
                    onClick={() => { setFondo(String(esperado.esperado)); setError(''); }}
                    title="Usar si contaste y coincide exactamente"
                  >
                    Coincide
                  </button>
                </div>
              )}

              <div>
                <label style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-muted)', display: 'block', marginBottom: 6 }}>
                  {esperado ? 'EFECTIVO CONTADO EN CAJA' : 'FONDO DECLARADO'}
                </label>
                <input
                  className="input mono"
                  type="number"
                  step="0.01"
                  min="0"
                  placeholder="$0.00"
                  value={fondo}
                  onChange={e => setFondo(e.target.value)}
                  autoFocus
                  style={{ width: '100%', fontSize: 28, textAlign: 'center', fontWeight: 700 }}
                />
                {!esperado && (
                  <p style={{ fontSize: 11, color: 'var(--color-text-dim)', marginTop: 4, textAlign: 'center' }}>
                    Sugerencia basada en el último cierre del día
                  </p>
                )}
                {comparar && diferencia !== 0 && (
                  <p style={{
                    fontSize: 13, fontWeight: 700, marginTop: 6, textAlign: 'center',
                    color: diferencia < 0 ? 'var(--color-danger)' : 'var(--color-warning)',
                  }}>
                    {diferencia < 0 ? 'Faltan' : 'Sobran'} {fmt(Math.abs(diferencia))} respecto a lo esperado.
                    Se registrará como ajuste a tu nombre.
                  </p>
                )}
              </div>

              <div>
                <label style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-muted)', display: 'block', marginBottom: 6 }}>
                  {diferencia !== 0 ? 'NOTA (obligatoria: explica la diferencia)' : 'NOTA (opcional)'}
                </label>
                <input
                  className="input"
                  placeholder="Ej: Faltaba cambio chico, ajuste manual..."
                  value={nota}
                  onChange={e => setNota(e.target.value)}
                  onKeyDown={e => { if (e.key === 'Enter') handleConfirmar(); }}
                  style={{ width: '100%' }}
                />
              </div>
            </>
          )}

          {error && <p style={{ fontSize: 12, color: 'var(--color-danger)' }}>{error}</p>}

          <button
            className="btn btn-primary"
            onClick={handleConfirmar}
            disabled={guardando || cargando}
            style={{ marginTop: 4, padding: '12px', fontSize: 14, fontWeight: 700 }}
          >
            {guardando ? 'Abriendo caja...' : 'Abrir caja y comenzar'}
          </button>

          {bloqueante && (
            <p style={{ fontSize: 11, color: 'var(--color-text-dim)', textAlign: 'center', marginTop: -4 }}>
              Debes abrir la caja para usar el sistema
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

// Aviso cuando el servidor rechazó el corte porque el esperado cambió
// mientras el cajero contaba (entraron ventas o movimientos).
function AvisoRecalculo({ texto }: { texto: string }) {
  return (
    <div style={{
      padding: 10, borderRadius: 8, fontSize: 12, fontWeight: 600,
      background: 'rgba(245,158,11,0.12)', border: '1px solid rgba(245,158,11,0.45)',
      color: 'var(--color-warning)',
    }}>
      {texto}
    </div>
  );
}

function FilaDiferencia({ diferencia, grande }: { diferencia: number; grande?: boolean }) {
  const color = diferencia === 0 ? 'var(--color-success)'
    : diferencia > 0 ? 'var(--color-warning)'
    : 'var(--color-danger)';

  const label = diferencia === 0 ? '✓ Cuadra perfectamente'
    : diferencia > 0 ? `Sobrante: +${fmt(diferencia)}`
    : `Faltante: ${fmt(diferencia)}`;

  const icon = diferencia === 0 ? <CheckCircle size={grande ? 20 : 16} />
    : <AlertTriangle size={grande ? 20 : 16} />;

  return (
    <div style={{
      padding: grande ? '14px 18px' : '10px 14px',
      borderRadius: 10, border: `1px solid ${color}`,
      background: `${color}15`,
      display: 'flex', alignItems: 'center', gap: 10,
    }}>
      <span style={{ color }}>{icon}</span>
      <span className="mono" style={{
        fontSize: grande ? 20 : 15,
        fontWeight: 800, color,
      }}>
        {label}
      </span>
    </div>
  );
}
