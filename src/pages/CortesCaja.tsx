// pages/CortesCaja.tsx — Módulo de Cortes de Caja

import { useState, useEffect, useCallback } from 'react';
import { useAuthStore } from '../store/authStore';
import {
  useCortesStore,
  type MovimientoCaja,
  type DatosCorte,
  type DenominacionInput,
  type CorteResumen,
} from '../store/cortesStore';
import {
  DollarSign, ArrowDownLeft, ArrowUpRight, Clock,
  CheckCircle, AlertTriangle, X, ChevronDown, ChevronUp,
  Sunrise,
} from 'lucide-react';

// ─── Utilidades ───────────────────────────────────────────

const fmt = (n: number) => `$${n.toFixed(2)}`;

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

function fechaHoyFin() {
  const d = new Date();
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day} 23:59:59`;
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
}

export default function CortesCaja({
  triggerMovimiento = 0,
  triggerParcial = 0,
  triggerDia = 0,
  fechaObjetivoDia = null,
  onCorteDiaHecho,
}: Props) {
  const { usuario } = useAuthStore();
  const {
    movimientosPendientes,
    cortesPrevios,
    cargarMovimientosPendientes,
    cargarCortes,
  } = useCortesStore();

  const [tab, setTab] = useState<'movimientos' | 'historial'>('movimientos');
  const [showModalMov, setShowModalMov] = useState(false);
  const [showModalParcial, setShowModalParcial] = useState(false);
  const [showModalDia, setShowModalDia] = useState(false);

  const esAdmin = usuario?.es_admin ?? false;

  useEffect(() => {
    cargarMovimientosPendientes();
    cargarCortes(50);
  }, []);

  // Triggers desde Dashboard (atajos de teclado F6, F11, Shift+F11)
  useEffect(() => { if (triggerMovimiento > 0) setShowModalMov(true); }, [triggerMovimiento]);
  useEffect(() => { if (triggerParcial > 0) setShowModalParcial(true); }, [triggerParcial]);
  useEffect(() => { if (triggerDia > 0 && esAdmin) setShowModalDia(true); }, [triggerDia, esAdmin]);

  const recargar = useCallback(async () => {
    await cargarMovimientosPendientes();
    await cargarCortes(50);
  }, []);

  return (
    <div className="animate-fade-in" style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>

      {/* ─── Header ─── */}
      <div style={{
        padding: '16px 20px',
        borderBottom: '1px solid var(--color-border)',
        display: 'flex', alignItems: 'center', gap: 12,
        background: 'var(--color-surface)',
      }}>
        <DollarSign size={20} style={{ color: 'var(--color-success)' }} />
        <h2 style={{ fontSize: 16, fontWeight: 800, flex: 1 }}>Cortes de Caja</h2>

        <button
          className="btn btn-ghost btn-sm"
          onClick={() => setShowModalMov(true)}
          title="F6"
        >
          <ArrowDownLeft size={14} /> Movimiento <span style={{ opacity: 0.5, fontSize: 10 }}>F6</span>
        </button>
        <button
          className="btn btn-ghost btn-sm"
          onClick={() => setShowModalParcial(true)}
          title="F11"
        >
          <ArrowUpRight size={14} /> Retiro de Caja <span style={{ opacity: 0.5, fontSize: 10 }}>F11</span>
        </button>
        {esAdmin && (
          <button
            className="btn btn-primary btn-sm"
            onClick={() => setShowModalDia(true)}
            title="Shift+F11"
          >
            <CheckCircle size={14} /> Corte del Día <span style={{ opacity: 0.5, fontSize: 10 }}>⇧F11</span>
          </button>
        )}
      </div>

      {/* ─── Tabs ─── */}
      <div style={{
        display: 'flex', gap: 0,
        borderBottom: '1px solid var(--color-border)',
        background: 'var(--color-surface)',
        padding: '0 20px',
      }}>
        {(['movimientos', 'historial'] as const).map(t => (
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
            {t === 'movimientos' ? `Movimientos (${movimientosPendientes.length})` : 'Historial'}
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
      </div>

      {/* ─── Modales ─── */}
      {showModalMov && (
        <ModalMovimiento
          onClose={() => setShowModalMov(false)}
          onSuccess={recargar}
        />
      )}
      {showModalParcial && (
        <ModalRetiroCaja
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
      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12, marginBottom: 8 }}>
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
  const dif = corte.diferencia;
  const esDia = corte.tipo === 'DIA';

  const colorDif = dif === 0 ? 'var(--color-success)'
    : dif > 0 ? 'var(--color-warning)'
    : 'var(--color-danger)';

  const labelDif = dif === 0 ? 'Cuadra' : dif > 0 ? `Sobrante +${fmt(dif)}` : `Faltante ${fmt(dif)}`;

  return (
    <div className="card" style={{ padding: '14px 16px' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
        <span style={{
          fontSize: 10, fontWeight: 700, padding: '2px 8px', borderRadius: 6,
          background: esDia ? 'rgba(99,102,241,0.12)' : 'rgba(158,122,126,0.12)',
          color: esDia ? 'var(--color-primary)' : 'var(--color-text-muted)',
          textTransform: 'uppercase',
        }}>
          {esDia ? 'Cierre del día' : 'Parcial'}
        </span>

        <span style={{ fontSize: 13, color: 'var(--color-text-muted)', flex: 1 }}>
          {fmtFecha(corte.created_at)} · {fmtHora(corte.created_at)} · {corte.usuario_nombre}
        </span>

        <span style={{ fontSize: 13, fontWeight: 700, color: colorDif }}>
          {labelDif}
        </span>
      </div>

      <div style={{ display: 'flex', gap: 24, marginTop: 10 }}>
        <div>
          <p style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>Esperado</p>
          <p className="mono" style={{ fontSize: 15, fontWeight: 700 }}>{fmt(corte.efectivo_esperado)}</p>
        </div>
        <div>
          <p style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>Contado</p>
          <p className="mono" style={{ fontSize: 15, fontWeight: 700 }}>{fmt(corte.efectivo_contado)}</p>
        </div>
        {esDia && (
          <div>
            <p style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>Fondo siguiente</p>
            <p className="mono" style={{ fontSize: 15, fontWeight: 700 }}>{fmt(corte.fondo_siguiente)}</p>
          </div>
        )}
      </div>
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
    <div style={{
      position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.65)',
      display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 200,
    }} onClick={onClose}>
      <div className="card animate-fade-in" style={{ width: 400, padding: 24 }}
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

// ─── Modal 2: Retiro de Caja (antes "Corte Parcial") ─────────
//
// A diferencia del corte del día, esto NO toma snapshot ni cierra
// movimientos. Es un retiro de efectivo a mitad del día: el usuario
// ve cuánto hay esperado en caja, ingresa cuánto se lleva, y confirma.
// Bajo el capó crea un `movimiento_caja` tipo RETIRO (igual que F6
// pero con el contexto del balance actual).

function ModalRetiroCaja({ onClose, onSuccess }: { onClose: () => void; onSuccess: () => void }) {
  const { usuario } = useAuthStore();
  const { calcularDatosCorte, crearMovimiento } = useCortesStore();

  const [datos, setDatos] = useState<DatosCorte | null>(null);
  const [cargandoDatos, setCargandoDatos] = useState(true);
  const [monto, setMonto] = useState('');
  const [concepto, setConcepto] = useState('');
  const [pinDueno, setPinDueno] = useState('');
  const [error, setError] = useState('');
  const [guardando, setGuardando] = useState(false);
  const [fechaInicio] = useState(fechaHoyInicio);
  const [fechaFin] = useState(ahora);

  const esAdmin = usuario?.es_admin ?? false;

  useEffect(() => {
    calcularDatosCorte(fechaInicio, fechaFin)
      .then(setDatos)
      .catch(e => setError(String(e)))
      .finally(() => setCargandoDatos(false));
  }, []);

  const efectivoEsperado = datos?.efectivo_esperado ?? 0;
  const montoNum = parseFloat(monto) || 0;
  const restante = efectivoEsperado - montoNum;
  const requierePin = montoNum > 500 && !esAdmin;
  const excede = montoNum > efectivoEsperado;

  const handleConfirmar = async () => {
    if (!datos) return;
    if (montoNum <= 0) { setError('El monto a retirar debe ser mayor a $0'); return; }
    if (!concepto.trim()) { setError('El concepto es obligatorio'); return; }
    if (requierePin && pinDueno.length < 4) { setError('Ingresa el PIN del dueño'); return; }

    setGuardando(true);
    setError('');
    try {
      await crearMovimiento({
        tipo: 'RETIRO',
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
    <div style={{
      position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.65)',
      display: 'flex', alignItems: 'flex-start', justifyContent: 'center',
      zIndex: 200, overflowY: 'auto', padding: '20px 0',
    }} onClick={onClose}>
      <div className="card animate-fade-in" style={{ width: 480, padding: 0, overflow: 'hidden', margin: 'auto' }}
        onClick={e => e.stopPropagation()}>

        {/* Header */}
        <div style={{
          padding: '16px 20px', borderBottom: '1px solid var(--color-border)',
          display: 'flex', alignItems: 'center', gap: 10,
          background: 'rgba(245,158,11,0.08)',
        }}>
          <ArrowUpRight size={18} style={{ color: 'var(--color-warning)' }} />
          <div style={{ flex: 1 }}>
            <h3 style={{ fontSize: 15, fontWeight: 700 }}>Retiro de Caja</h3>
            <p style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>
              {ahora().substring(0, 16).replace('T', ' ')} · {usuario?.nombre_completo}
            </p>
          </div>
          <button className="btn btn-ghost btn-sm" onClick={onClose}><X size={16} /></button>
        </div>

        <div style={{ padding: 20, display: 'flex', flexDirection: 'column', gap: 16 }}>
          {cargandoDatos && (
            <div style={{ textAlign: 'center', padding: 32, color: 'var(--color-text-dim)' }}>
              Calculando...
            </div>
          )}

          {datos && (
            <>
              {/* Efectivo actualmente en caja */}
              <div style={{
                padding: '14px 16px', borderRadius: 10,
                background: 'var(--color-surface-2)',
                border: '1px solid var(--color-border)',
                display: 'flex', justifyContent: 'space-between', alignItems: 'center',
              }}>
                <span style={{ fontSize: 13, fontWeight: 600 }}>Efectivo en caja</span>
                <span className="mono" style={{ fontSize: 22, fontWeight: 800, color: 'var(--color-primary)' }}>
                  {fmt(efectivoEsperado)}
                </span>
              </div>

              {/* Monto a retirar */}
              <div>
                <label style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-muted)', display: 'block', marginBottom: 6 }}>
                  MONTO A RETIRAR
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
                  style={{ width: '100%', fontSize: 26, textAlign: 'center' }}
                />
              </div>

              {/* Quedará en caja — la info clave del flujo */}
              {montoNum > 0 && (
                <div style={{
                  padding: '14px 16px', borderRadius: 10,
                  background: excede
                    ? 'rgba(239, 68, 68, 0.1)'
                    : 'rgba(34, 197, 94, 0.1)',
                  border: `1px solid ${excede ? 'var(--color-danger)' : 'var(--color-success)'}`,
                  display: 'flex', justifyContent: 'space-between', alignItems: 'center',
                }}>
                  <span style={{ fontSize: 13, fontWeight: 700 }}>
                    {excede ? '⚠️ Excede el efectivo en caja' : 'Quedará en caja'}
                  </span>
                  <span className="mono" style={{
                    fontSize: 24, fontWeight: 800,
                    color: excede ? 'var(--color-danger)' : 'var(--color-success)',
                  }}>
                    {fmt(restante)}
                  </span>
                </div>
              )}

              {/* Concepto */}
              <div>
                <label style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-muted)', display: 'block', marginBottom: 6 }}>
                  CONCEPTO
                </label>
                <input
                  className="input"
                  placeholder="Ej: Pago a proveedor, gastos, depósito al banco..."
                  value={concepto}
                  onChange={e => setConcepto(e.target.value)}
                  onKeyDown={e => { if (e.key === 'Enter' && !requierePin) handleConfirmar(); }}
                  style={{ width: '100%' }}
                />
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

          {error && <p style={{ fontSize: 12, color: 'var(--color-danger)' }}>{error}</p>}

          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-ghost" style={{ flex: 1 }} onClick={onClose}>Cancelar</button>
            <button
              className="btn btn-primary"
              style={{ flex: 2 }}
              onClick={handleConfirmar}
              disabled={cargandoDatos || guardando || !datos}
            >
              {guardando ? 'Guardando...' : 'Confirmar retiro'}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

// ─── Modal 3: Corte del Día ───────────────────────────────

function ModalCorteDelDia({ onClose, onSuccess, fechaObjetivo }: {
  onClose: () => void;
  onSuccess: () => void;
  fechaObjetivo?: string | null;
}) {
  const { usuario } = useAuthStore();
  const { calcularDatosCorte, crearCorte, cargando } = useCortesStore();

  const [datos, setDatos] = useState<DatosCorte | null>(null);
  const [cargandoDatos, setCargandoDatos] = useState(true);
  const [usarDenominaciones, setUsarDenominaciones] = useState(false);
  const [cantidades, setCantidades] = useState<Record<string, number>>({});
  const [efectivoContadoDirecto, setEfectivoContadoDirecto] = useState('');
  const [nota, setNota] = useState('');
  const [fondoSiguiente, setFondoSiguiente] = useState('2000');
  const [error, setError] = useState('');

  const [fechaInicio] = useState(() => fechaObjetivo ? `${fechaObjetivo} 00:00:00` : fechaHoyInicio());
  const [fechaFin] = useState(() => fechaObjetivo ? `${fechaObjetivo} 23:59:59` : fechaHoyFin());
  const esExtemporaneo = !!fechaObjetivo;

  useEffect(() => {
    calcularDatosCorte(fechaInicio, fechaFin)
      .then(d => {
        setDatos(d);
      })
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

  const diferencia = datos ? efectivoContado - datos.efectivo_esperado : 0;
  const requiereNota = efectivoContado > 0 && diferencia !== 0;

  const handleConfirmar = async () => {
    if (!datos) return;
    if (efectivoContado < 0) { setError('El efectivo contado no puede ser negativo'); return; }
    if (requiereNota && !nota.trim()) { setError('La nota es obligatoria cuando hay diferencia'); return; }

    const fondoNum = parseFloat(fondoSiguiente) || 0;

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
    try {
      await crearCorte({
        tipo: 'DIA',
        usuario_id: usuario!.id,
        fecha_inicio: fechaInicio,
        fecha_fin: fechaFin,
        datos,
        efectivo_contado: efectivoContado,
        nota_diferencia: nota.trim() || null,
        fondo_siguiente: fondoNum,
        denominaciones,
      });
      await onSuccess();
      onClose();
    } catch (e: any) {
      setError(String(e));
    }
  };

  return (
    <div style={{
      position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.75)',
      display: 'flex', alignItems: 'flex-start', justifyContent: 'center',
      zIndex: 200, overflowY: 'auto', padding: '20px 0',
    }} onClick={onClose}>
      <div className="card animate-fade-in" style={{ width: 600, padding: 0, overflow: 'hidden', margin: 'auto' }}
        onClick={e => e.stopPropagation()}>

        {/* Header */}
        <div style={{
          padding: '16px 20px', borderBottom: '1px solid var(--color-border)',
          display: 'flex', alignItems: 'center', gap: 10,
          background: 'rgba(34,197,94,0.08)',
        }}>
          <CheckCircle size={18} style={{ color: 'var(--color-success)' }} />
          <div style={{ flex: 1 }}>
            <h3 style={{ fontSize: 15, fontWeight: 700 }}>
              {esExtemporaneo ? 'Corte del Día — Extemporáneo' : 'Corte del Día — Cierre'}
            </h3>
            <p style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>
              {esExtemporaneo
                ? `Cerrando: ${fechaObjetivo} · ${usuario?.nombre_completo}`
                : `${new Date().toLocaleDateString('es-MX', { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric' })} · ${usuario?.nombre_completo}`}
            </p>
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

              {/* CAJA */}
              <SeccionResumen titulo="MOVIMIENTOS DE CAJA">
                <FilaResumen label="Fondo inicial" valor={fmt(datos.fondo_inicial)} />
                <FilaResumen label="(+) Ventas efectivo" valor={fmt(datos.total_ventas_efectivo)} color="var(--color-success)" />
                {datos.total_entradas_efectivo > 0 && (
                  <FilaResumen label="(+) Entradas" valor={fmt(datos.total_entradas_efectivo)} color="var(--color-success)" />
                )}
                {datos.total_retiros_efectivo > 0 && (
                  <FilaResumen label="(-) Retiros" valor={`-${fmt(datos.total_retiros_efectivo)}`} color="var(--color-danger)" />
                )}
                <div style={{ margin: '6px 0', borderTop: '1px solid var(--color-border)' }} />
                <FilaResumen label="Efectivo esperado" valor={fmt(datos.efectivo_esperado)} bold color="var(--color-primary)" />
              </SeccionResumen>

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
                  style={{ width: '100%', justifyContent: 'space-between' }}
                  onClick={() => setUsarDenominaciones(!usarDenominaciones)}
                >
                  <span>Contar por denominación (opcional)</span>
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
  const { crearApertura, obtenerFondoSugerido } = useCortesStore();

  const [fondo, setFondo] = useState('');
  const [nota, setNota] = useState('');
  const [error, setError] = useState('');
  const [guardando, setGuardando] = useState(false);
  const [cargando, setCargando] = useState(true);

  useEffect(() => {
    obtenerFondoSugerido()
      .then(s => setFondo(String(s)))
      .catch(() => setFondo('2000'))
      .finally(() => setCargando(false));
  }, []);

  const fondoNum = parseFloat(fondo) || 0;

  const handleConfirmar = async () => {
    if (fondoNum < 0) { setError('El fondo no puede ser negativo'); return; }
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
    <div style={{
      position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.85)',
      display: 'flex', alignItems: 'center', justifyContent: 'center',
      zIndex: 9999,
    }} onClick={handleBackdrop}>
      <div className="card animate-fade-in" style={{ width: 440, padding: 0, overflow: 'hidden' }}
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
            Antes de empezar las operaciones del día, declara el efectivo con el que abres la caja.
            Este será el <strong>fondo inicial</strong> contra el que se cuadrará el corte de hoy.
          </p>

          {cargando ? (
            <p style={{ textAlign: 'center', color: 'var(--color-text-dim)', padding: 20 }}>Cargando sugerencia...</p>
          ) : (
            <>
              <div>
                <label style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-muted)', display: 'block', marginBottom: 6 }}>
                  FONDO DECLARADO
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
                <p style={{ fontSize: 11, color: 'var(--color-text-dim)', marginTop: 4, textAlign: 'center' }}>
                  Sugerencia basada en el último cierre del día
                </p>
              </div>

              <div>
                <label style={{ fontSize: 11, fontWeight: 700, color: 'var(--color-text-muted)', display: 'block', marginBottom: 6 }}>
                  NOTA (opcional)
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
