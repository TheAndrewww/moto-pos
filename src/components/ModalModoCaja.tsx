// components/ModalModoCaja.tsx — Selector inicial del modo de caja del web.
//
// Se muestra:
//   - Automáticamente al primer login en un nuevo navegador (modo_configurado=false).
//   - Manualmente desde Ajustes → "Cambiar modo de caja".
//
// Dos modos:
//   🪞 Espejo:     comparte caja con el POS desktop (1 fondo, 1 corte único).
//   🧾 Individual: caja propia (fondo, ventas y corte separados del desktop).
//
// El backend (rpc.rs) hace cumplir el cambio: rechaza si hay movimientos
// pendientes del modo previo. Aquí solo capturamos la elección.

import { useState } from 'react';
import { invoke } from '../lib/invokeCompat';
import { setModoCajaLocal } from '../store/authStore';
import { Monitor, Wallet, X } from 'lucide-react';

interface Props {
  /** Si está activo, no permite cerrar el modal (primer login). */
  bloqueante?: boolean;
  modoActual?: 'espejo' | 'individual';
  onSeleccion: (modo: 'espejo' | 'individual') => void;
  onCerrar?: () => void;
}

export default function ModalModoCaja({
  bloqueante = false,
  modoActual,
  onSeleccion,
  onCerrar,
}: Props) {
  const [seleccion, setSeleccion] = useState<'espejo' | 'individual' | null>(modoActual ?? null);
  const [guardando, setGuardando] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const confirmar = async () => {
    if (!seleccion) return;
    setGuardando(true);
    setError(null);
    try {
      await invoke('configurar_modo_caja', { modo: seleccion });
      setModoCajaLocal(seleccion, true);
      onSeleccion(seleccion);
      onCerrar?.();
    } catch (e: any) {
      const msg = e?.message ?? String(e);
      setError(msg.replace(/^RPC[^:]+:\s*\d+\s*/, ''));
    } finally {
      setGuardando(false);
    }
  };

  return (
    <div
      className="pos-modal-overlay"
      style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.75)',
        display: 'flex', alignItems: 'center', justifyContent: 'center',
        zIndex: 300, padding: 20,
      }}
      onClick={() => { if (!bloqueante) onCerrar?.(); }}
    >
      <div
        className="card animate-fade-in pos-modal-content"
        style={{ width: 640, maxWidth: '100%', padding: 0, overflow: 'hidden' }}
        onClick={(e) => e.stopPropagation()}
      >
        <div style={{
          padding: '18px 22px', borderBottom: '1px solid var(--color-border)',
          display: 'flex', alignItems: 'center', justifyContent: 'space-between',
          background: 'rgba(59,130,246,0.08)',
        }}>
          <div>
            <h2 style={{ fontSize: 17, fontWeight: 700, margin: 0 }}>
              {bloqueante ? '¿Cómo quieres usar este equipo?' : 'Cambiar modo de caja'}
            </h2>
            <p style={{ fontSize: 12, color: 'var(--color-text-dim)', marginTop: 4 }}>
              Esta configuración determina cómo cuenta las ventas, el dinero y los cortes.
            </p>
          </div>
          {!bloqueante && (
            <button className="btn btn-ghost btn-sm" onClick={onCerrar}>
              <X size={16} />
            </button>
          )}
        </div>

        <div style={{ padding: 20, display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 14 }}>
          <OpcionModo
            id="espejo"
            icon={<Monitor size={32} />}
            titulo="Espejo de la caja"
            descripcion="Esta computadora vende junto con el POS desktop. Las ventas, el dinero y los cortes se cuentan en una sola caja."
            ejemplo="Útil cuando: alguien atiende desde el celular o una pestaña adicional pero el dinero físico está en la misma caja."
            seleccionado={seleccion === 'espejo'}
            onClick={() => setSeleccion('espejo')}
          />
          <OpcionModo
            id="individual"
            icon={<Wallet size={32} />}
            titulo="Caja propia independiente"
            descripcion="Esta computadora es su propia caja con su propio fondo, ventas y corte. No comparte con el desktop."
            ejemplo="Útil cuando: hay un segundo mostrador, una tablet en otra área o el dueño sale a vender afuera."
            seleccionado={seleccion === 'individual'}
            onClick={() => setSeleccion('individual')}
          />
        </div>

        {error && (
          <div style={{
            margin: '0 20px', padding: 10, fontSize: 13,
            background: 'rgba(239,68,68,0.10)', border: '1px solid rgba(239,68,68,0.4)',
            borderRadius: 6, color: 'var(--color-danger)',
          }}>
            {error}
          </div>
        )}

        <div style={{
          padding: '14px 20px', borderTop: '1px solid var(--color-border)',
          display: 'flex', justifyContent: 'flex-end', gap: 8,
        }}>
          {!bloqueante && (
            <button className="btn btn-ghost" onClick={onCerrar} disabled={guardando}>
              Cancelar
            </button>
          )}
          <button
            className="btn btn-primary"
            onClick={confirmar}
            disabled={!seleccion || guardando}
          >
            {guardando ? 'Guardando…' : 'Confirmar'}
          </button>
        </div>
      </div>
    </div>
  );
}

function OpcionModo({
  icon, titulo, descripcion, ejemplo, seleccionado, onClick,
}: {
  id: string;
  icon: React.ReactNode;
  titulo: string;
  descripcion: string;
  ejemplo: string;
  seleccionado: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      style={{
        textAlign: 'left',
        padding: 16,
        border: `2px solid ${seleccionado ? 'var(--color-primary)' : 'var(--color-border)'}`,
        borderRadius: 10,
        background: seleccionado ? 'rgba(59,130,246,0.08)' : 'var(--color-surface)',
        cursor: 'pointer',
        display: 'flex',
        flexDirection: 'column',
        gap: 8,
        transition: 'all 0.15s',
      }}
    >
      <div style={{
        width: 48, height: 48, borderRadius: '50%',
        background: seleccionado ? 'rgba(59,130,246,0.18)' : 'var(--color-surface-2)',
        display: 'flex', alignItems: 'center', justifyContent: 'center',
        color: seleccionado ? 'var(--color-primary)' : 'var(--color-text-muted)',
      }}>
        {icon}
      </div>
      <div style={{ fontSize: 15, fontWeight: 700 }}>{titulo}</div>
      <div style={{ fontSize: 12, color: 'var(--color-text-dim)', lineHeight: 1.5 }}>
        {descripcion}
      </div>
      <div style={{
        fontSize: 11, color: 'var(--color-text-muted)', fontStyle: 'italic',
        borderTop: '1px solid var(--color-border)', paddingTop: 8, marginTop: 4,
      }}>
        {ejemplo}
      </div>
    </button>
  );
}
