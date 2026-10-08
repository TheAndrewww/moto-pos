// components/ModalModoCaja.tsx — Selector del modo de caja del equipo web.
//
// Se muestra:
//   - Automáticamente al primer login en un nuevo navegador (modo_configurado=false).
//   - Manualmente desde el chip de modo de caja de la barra superior.
//
// Dos modos (ver server-remoto/src/caja.rs):
//   🪞 Espejo (recomendado): vende con el dinero del cajón de la tienda. Sus
//      ventas, entradas y retiros entran a la caja de la tienda y se cuentan
//      en el corte del POS de escritorio; aquí no se hacen cortes.
//   🧾 Caja propia: dinero fuera del cajón de la tienda. Todos los equipos en
//      este modo comparten la caja web, con su propia apertura y sus cortes.
//
// El servidor hace cumplir el cambio: no deja volver a espejo si la caja web
// tiene dinero sin cortar. Aquí solo se captura la elección.

import { useState } from 'react';
import { invoke } from '../lib/invokeCompat';
import { setModoCajaLocal } from '../store/authStore';
import { useCortesStore, infoCajaDeRespuesta } from '../store/cortesStore';
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
  // Primera vez en este equipo: se sugiere espejo (la web se usa como
  // respaldo del mismo cajón de la tienda).
  const [seleccion, setSeleccion] = useState<'espejo' | 'individual' | null>(
    bloqueante ? 'espejo' : (modoActual ?? 'espejo'),
  );
  const { setInfoCaja } = useCortesStore();
  const [guardando, setGuardando] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const confirmar = async () => {
    if (!seleccion) return;
    setGuardando(true);
    setError(null);
    try {
      const r = await invoke<Parameters<typeof infoCajaDeRespuesta>[0]>('configurar_modo_caja', { modo: seleccion });
      setModoCajaLocal(seleccion, true);
      const info = infoCajaDeRespuesta(r);
      if (info) setInfoCaja(info);
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
              Elige de qué cajón sale y a qué cajón entra el dinero de este equipo.
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
            titulo="Caja de la tienda (recomendado)"
            descripcion="Vendes con el dinero de la caja de la tienda. Tus ventas, entradas y retiros se suman al corte del POS de escritorio; los cortes y la apertura se hacen allá."
            ejemplo="Útil cuando: el POS de escritorio falló, o atiendes desde el celular o una tablet junto al mismo cajón."
            seleccionado={seleccion === 'espejo'}
            onClick={() => setSeleccion('espejo')}
          />
          <OpcionModo
            id="individual"
            icon={<Wallet size={32} />}
            titulo="Caja propia"
            descripcion="El dinero de este equipo va fuera del cajón de la tienda (otro cajón o una bolsa). Tiene su propia apertura y sus propios cortes, que se hacen aquí y no se mezclan con el escritorio."
            ejemplo="Útil cuando: hay un segundo mostrador con su propio cajón o el dueño vende fuera de la tienda. Todos los equipos en caja propia comparten la misma caja web."
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
