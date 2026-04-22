// pages/Clientes.tsx — Gestión de clientes (CRUD + tipo de precio + estado)

import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { UserPlus, Plus, Edit2, X, ShieldCheck, ShieldOff, Search } from 'lucide-react';

interface ClienteInfo {
  id: number;
  nombre: string;
  telefono: string | null;
  email: string | null;
  descuento_porcentaje: number;
  notas: string | null;
  activo: boolean;
}

export default function ClientesPage() {
  const [clientes, setClientes] = useState<ClienteInfo[]>([]);
  const [showForm, setShowForm] = useState(false);
  const [editando, setEditando] = useState<ClienteInfo | null>(null);
  const [cargando, setCargando] = useState(true);
  const [busqueda, setBusqueda] = useState('');
  const [mostrarInactivos, setMostrarInactivos] = useState(false);

  const cargarDatos = async () => {
    setCargando(true);
    try {
      const c = await invoke<ClienteInfo[]>('listar_clientes');
      setClientes(c);
    } catch {}
    setCargando(false);
  };

  useEffect(() => { cargarDatos(); }, []);

  const handleToggle = async (c: ClienteInfo) => {
    try {
      await invoke('toggle_cliente_activo', { id: c.id });
      cargarDatos();
    } catch (err: any) {
      alert(err?.toString());
    }
  };

  const q = busqueda.toLowerCase().trim();
  const filtrados = clientes
    .filter(c => mostrarInactivos || c.activo)
    .filter(c => !q || (
      c.nombre.toLowerCase().includes(q) ||
      (c.telefono || '').toLowerCase().includes(q) ||
      (c.email || '').toLowerCase().includes(q)
    ));

  // ─── Form ────
  const FormCliente = () => {
    const [form, setForm] = useState({
      nombre: editando?.nombre || '',
      telefono: editando?.telefono || '',
      email: editando?.email || '',
      descuento_porcentaje: editando?.descuento_porcentaje || 0,
      notas: editando?.notas || '',
    });
    const [guardando, setGuardando] = useState(false);
    const [error, setError] = useState('');

    const handleSubmit = async (e: React.FormEvent) => {
      e.preventDefault();
      if (!form.nombre.trim()) return setError('El nombre es obligatorio');

      setGuardando(true);
      setError('');

      try {
        if (editando) {
          await invoke('actualizar_cliente', {
            datos: {
              id: editando.id,
              nombre: form.nombre,
              telefono: form.telefono || null,
              email: form.email || null,
              descuento_porcentaje: Number(form.descuento_porcentaje) || 0,
              notas: form.notas || null,
            },
          });
        } else {
          await invoke('crear_cliente', {
            nombre: form.nombre,
            telefono: form.telefono || null,
            email: form.email || null,
            descuentoPorcentaje: Number(form.descuento_porcentaje) || 0,
            notas: form.notas || null,
          });
        }
        setShowForm(false);
        setEditando(null);
        cargarDatos();
      } catch (err: any) {
        setError(err?.toString() || 'Error al guardar');
      }
      setGuardando(false);
    };

    return (
      <div style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.6)',
        display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 100,
      }} onClick={() => { setShowForm(false); setEditando(null); }}>
        <div className="card animate-fade-in" style={{ width: 480, padding: 24 }}
          onClick={e => e.stopPropagation()}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 20 }}>
            <h2 style={{ fontSize: 18, fontWeight: 700 }}>
              {editando ? 'Editar Cliente' : 'Nuevo Cliente'}
            </h2>
            <button className="btn btn-ghost btn-sm" onClick={() => { setShowForm(false); setEditando(null); }}>
              <X size={18} />
            </button>
          </div>

          <form onSubmit={handleSubmit} style={{ display: 'flex', flexDirection: 'column', gap: 14 }}>
            <div>
              <label style={labelStyle}>NOMBRE *</label>
              <input className="input" value={form.nombre}
                onChange={e => setForm(f => ({ ...f, nombre: e.target.value }))}
                placeholder="Nombre del cliente" autoFocus />
            </div>

            <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
              <div>
                <label style={labelStyle}>TELÉFONO</label>
                <input className="input" value={form.telefono}
                  onChange={e => setForm(f => ({ ...f, telefono: e.target.value }))}
                  placeholder="Opcional" />
              </div>
              <div>
                <label style={labelStyle}>EMAIL</label>
                <input className="input" value={form.email}
                  onChange={e => setForm(f => ({ ...f, email: e.target.value }))}
                  placeholder="Opcional" />
              </div>
            </div>

            <div style={{ display: 'grid', gridTemplateColumns: '1fr', gap: 12 }}>
              <div>
                <label style={labelStyle}>DESCUENTO % (sobre precio de venta)</label>
                <input className="input mono" type="number" step="0.5" min="0" max="100"
                  value={form.descuento_porcentaje}
                  onChange={e => setForm(f => ({ ...f, descuento_porcentaje: parseFloat(e.target.value) || 0 }))} />
              </div>
            </div>

            <div>
              <label style={labelStyle}>NOTAS</label>
              <textarea className="input" rows={2} value={form.notas}
                onChange={e => setForm(f => ({ ...f, notas: e.target.value }))}
                placeholder="Notas opcionales" />
            </div>

            {error && <p style={{ color: 'var(--color-danger)', fontSize: 13 }}>{error}</p>}

            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', marginTop: 4 }}>
              <button type="button" className="btn btn-ghost"
                onClick={() => { setShowForm(false); setEditando(null); }}>
                Cancelar
              </button>
              <button type="submit" className="btn btn-primary" disabled={guardando}>
                {guardando ? 'Guardando...' : editando ? 'Guardar Cambios' : 'Crear Cliente'}
              </button>
            </div>
          </form>
        </div>
      </div>
    );
  };

  // ─── Render ────

  if (cargando) {
    return (
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', color: 'var(--color-text-dim)' }}>
        <span className="animate-pulse-soft">Cargando clientes...</span>
      </div>
    );
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      {/* Toolbar */}
      <div style={{
        padding: '12px 16px', borderBottom: '1px solid var(--color-border)',
        display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12,
      }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <UserPlus size={18} style={{ color: 'var(--color-primary)' }} />
          <h2 style={{ fontSize: 16, fontWeight: 700 }}>Clientes</h2>
          <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>
            {filtrados.length} / {clientes.length}
          </span>
        </div>

        <div style={{ flex: 1, display: 'flex', alignItems: 'center', gap: 8, maxWidth: 420 }}>
          <div style={{ position: 'relative', flex: 1 }}>
            <Search size={14} style={{ position: 'absolute', left: 10, top: '50%', transform: 'translateY(-50%)', color: 'var(--color-text-dim)' }} />
            <input className="input" value={busqueda}
              onChange={e => setBusqueda(e.target.value)}
              placeholder="Buscar por nombre, teléfono o email..."
              style={{ paddingLeft: 32 }} />
          </div>
          <label style={{ display: 'flex', alignItems: 'center', gap: 4, fontSize: 12, color: 'var(--color-text-muted)', whiteSpace: 'nowrap' }}>
            <input type="checkbox" checked={mostrarInactivos}
              onChange={e => setMostrarInactivos(e.target.checked)} />
            Inactivos
          </label>
        </div>

        <button className="btn btn-primary" onClick={() => { setEditando(null); setShowForm(true); }}>
          <Plus size={16} /> Nuevo Cliente
        </button>
      </div>

      {/* Lista */}
      <div style={{ flex: 1, overflow: 'auto', padding: 16 }}>
        {filtrados.length === 0 ? (
          <div style={{ textAlign: 'center', color: 'var(--color-text-dim)', padding: 40 }}>
            {q ? 'Sin resultados para la búsqueda.' : 'No hay clientes registrados.'}
          </div>
        ) : (
          <div style={{ display: 'grid', gap: 8 }}>
            {filtrados.map(c => (
              <div key={c.id} className="card" style={{
                padding: '14px 18px',
                display: 'flex', alignItems: 'center', gap: 14,
                opacity: c.activo ? 1 : 0.5,
                transition: 'opacity 0.2s',
              }}>
                <div style={{
                  width: 40, height: 40, borderRadius: '50%',
                  background: 'var(--color-surface-2)',
                  display: 'flex', alignItems: 'center', justifyContent: 'center',
                  fontSize: 16, fontWeight: 700,
                  color: 'var(--color-text-muted)',
                  flexShrink: 0,
                }}>
                  {c.nombre.charAt(0).toUpperCase()}
                </div>

                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
                    <span style={{ fontWeight: 600, fontSize: 14 }}>{c.nombre}</span>
                    {c.descuento_porcentaje > 0 && (
                      <span style={{
                        fontSize: 10, padding: '1px 8px', borderRadius: 10,
                        background: 'rgba(16,185,129,0.12)', color: '#10b981', fontWeight: 600,
                      }}>
                        -{c.descuento_porcentaje}%
                      </span>
                    )}
                    {!c.activo && (
                      <span className="tag tag-danger" style={{ fontSize: 10 }}>Inactivo</span>
                    )}
                  </div>
                  <div style={{ fontSize: 12, color: 'var(--color-text-dim)', display: 'flex', gap: 12, flexWrap: 'wrap' }}>
                    {c.telefono && <span>📞 {c.telefono}</span>}
                    {c.email && <span>✉️ {c.email}</span>}
                    {c.notas && <span style={{ fontStyle: 'italic' }}>· {c.notas}</span>}
                  </div>
                </div>

                <div style={{ display: 'flex', gap: 6 }}>
                  <button className="btn btn-ghost btn-sm"
                    onClick={() => { setEditando(c); setShowForm(true); }}
                    title="Editar">
                    <Edit2 size={14} />
                  </button>
                  <button
                    className="btn btn-ghost btn-sm"
                    onClick={() => handleToggle(c)}
                    title={c.activo ? 'Desactivar' : 'Activar'}
                    style={{ color: c.activo ? 'var(--color-danger)' : 'var(--color-success)' }}
                  >
                    {c.activo ? <ShieldOff size={14} /> : <ShieldCheck size={14} />}
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {showForm && <FormCliente />}
    </div>
  );
}

const labelStyle: React.CSSProperties = {
  fontSize: 11,
  fontWeight: 600,
  color: 'var(--color-text-muted)',
  display: 'block',
  marginBottom: 4,
  textTransform: 'uppercase',
  letterSpacing: '0.3px',
};
