// pages/Bitacora.tsx — Visor de auditoría del sistema

import { useState, useEffect } from 'react';
import { invoke } from '../lib/invokeCompat';
import { ScrollText, Search, RefreshCw, Filter } from 'lucide-react';

interface EntradaBitacora {
  id: number;
  usuario_nombre: string | null;
  accion: string;
  tabla_afectada: string | null;
  registro_id: number | null;
  descripcion_legible: string;
  origen: string;
  fecha: string;
}

const ACCIONES_POSIBLES = [
  'LOGIN_PIN', 'LOGIN_PASSWORD', 'LOGOUT',
  'PRODUCTO_CREADO', 'PRODUCTO_EDITADO',
  'VENTA_CREADA', 'VENTA_ANULADA',
  'USUARIO_CREADO', 'USUARIO_EDITADO', 'USUARIO_TOGGLE',
];

function getBadgeColor(accion: string): { bg: string; color: string } {
  if (accion.includes('LOGIN') || accion.includes('LOGOUT')) return { bg: 'rgba(108,117,246,0.1)', color: '#6c75f6' };
  if (accion.includes('VENTA')) return { bg: 'rgba(34,179,120,0.1)', color: '#22b378' };
  if (accion.includes('PRODUCTO')) return { bg: 'rgba(216,56,77,0.1)', color: 'var(--color-primary)' };
  if (accion.includes('USUARIO')) return { bg: 'rgba(230,187,128,0.1)', color: 'var(--color-secondary)' };
  return { bg: 'var(--color-surface-2)', color: 'var(--color-text-muted)' };
}

export default function Bitacora() {
  const [entradas, setEntradas] = useState<EntradaBitacora[]>([]);
  const [cargando, setCargando] = useState(true);
  const [filtroAccion, setFiltroAccion] = useState<string>('');
  const [busqueda, setBusqueda] = useState('');
  const [limite, setLimite] = useState(200);

  const cargarDatos = async () => {
    setCargando(true);
    try {
      const data = await invoke<EntradaBitacora[]>('listar_bitacora', {
        limite,
        accionFiltro: filtroAccion || null,
      });
      setEntradas(data);
    } catch {}
    setCargando(false);
  };

  useEffect(() => { cargarDatos(); }, [filtroAccion, limite]);

  // Filtro local por texto
  const entradasFiltradas = busqueda
    ? entradas.filter(e =>
        e.descripcion_legible.toLowerCase().includes(busqueda.toLowerCase()) ||
        (e.usuario_nombre || '').toLowerCase().includes(busqueda.toLowerCase()) ||
        e.accion.toLowerCase().includes(busqueda.toLowerCase())
      )
    : entradas;

  const formatFecha = (fecha: string) => {
    try {
      const d = new Date(fecha + 'Z');
      return d.toLocaleString('es-MX', {
        day: '2-digit', month: '2-digit', year: 'numeric',
        hour: '2-digit', minute: '2-digit', second: '2-digit',
        hour12: false,
      });
    } catch { return fecha; }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      {/* Header */}
      <div style={{
        padding: '12px 20px',
        borderBottom: '1px solid var(--color-border)',
        background: 'var(--color-surface)',
        display: 'flex', flexDirection: 'column', gap: 10,
      }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
            <ScrollText size={20} style={{ color: 'var(--color-primary)' }} />
            <h2 style={{ fontSize: 17, fontWeight: 800, color: 'var(--color-text)' }}>Bitácora de Auditoría</h2>
            <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>·</span>
            <span style={{ fontSize: 12, color: 'var(--color-text-muted)' }}>{entradasFiltradas.length} registros</span>
          </div>
          <button className="btn btn-ghost" onClick={cargarDatos} disabled={cargando}>
            <RefreshCw size={14} className={cargando ? 'animate-pulse-soft' : ''} /> Actualizar
          </button>
        </div>

        <div style={{ display: 'flex', gap: 8 }}>
          <div style={{ position: 'relative', flex: 1 }}>
            <Search size={16} style={{
              position: 'absolute', left: 12, top: '50%', transform: 'translateY(-50%)',
              color: 'var(--color-text-dim)',
            }} />
            <input
              className="input"
              placeholder="Buscar en descripciones, usuarios, acciones..."
              value={busqueda}
              onChange={e => setBusqueda(e.target.value)}
              style={{ paddingLeft: 36, width: '100%' }}
            />
          </div>
          <select
            className="input"
            value={filtroAccion}
            onChange={e => setFiltroAccion(e.target.value)}
            style={{ width: 200 }}
          >
            <option value="">Todas las acciones</option>
            {ACCIONES_POSIBLES.map(a => <option key={a} value={a}>{a}</option>)}
          </select>
          <select
            className="input"
            value={limite}
            onChange={e => setLimite(Number(e.target.value))}
            style={{ width: 120 }}
          >
            <option value={50}>Últimos 50</option>
            <option value={200}>Últimos 200</option>
            <option value={500}>Últimos 500</option>
            <option value={1000}>Últimos 1000</option>
          </select>
        </div>
      </div>

      {/* Log list */}
      <div style={{ flex: 1, overflow: 'auto' }}>
        {cargando ? (
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', color: 'var(--color-text-dim)' }}>
            <span className="animate-pulse-soft">Cargando bitácora...</span>
          </div>
        ) : entradasFiltradas.length === 0 ? (
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', flexDirection: 'column', gap: 8, color: 'var(--color-text-dim)' }}>
            <Filter size={40} strokeWidth={1.2} />
            <p style={{ fontSize: 14 }}>No hay registros</p>
          </div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column' }}>
            {entradasFiltradas.map((e) => {
              const badge = getBadgeColor(e.accion);
              return (
                <div
                  key={e.id}
                  style={{
                    display: 'flex', alignItems: 'center', gap: 14,
                    padding: '10px 20px',
                    borderBottom: '1px solid var(--color-border)',
                    transition: 'background 0.1s',
                  }}
                  onMouseEnter={ev => (ev.currentTarget.style.background = 'var(--color-surface)')}
                  onMouseLeave={ev => (ev.currentTarget.style.background = 'transparent')}
                >
                  {/* Fecha */}
                  <span className="mono" style={{
                    fontSize: 11, color: 'var(--color-text-dim)',
                    minWidth: 140, flexShrink: 0,
                  }}>
                    {formatFecha(e.fecha)}
                  </span>

                  {/* Acción badge */}
                  <span style={{
                    fontSize: 10, fontWeight: 700, padding: '2px 10px',
                    borderRadius: 10, minWidth: 120, textAlign: 'center',
                    flexShrink: 0,
                    background: badge.bg, color: badge.color,
                  }}>
                    {e.accion}
                  </span>

                  {/* Descripción */}
                  <span style={{ flex: 1, fontSize: 13, color: 'var(--color-text)' }}>
                    {e.descripcion_legible}
                  </span>

                  {/* Usuario */}
                  <span style={{
                    fontSize: 12, color: 'var(--color-text-muted)',
                    minWidth: 100, textAlign: 'right', flexShrink: 0,
                  }}>
                    {e.usuario_nombre || '—'}
                  </span>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
