// pages/Proveedores.tsx — Gestión de proveedores (CRUD + estado)

import { useState, useEffect } from 'react';
import { invoke } from '../lib/invokeCompat';
import { useProductStore } from '../store/productStore';
import { Building2, Plus, Edit2, X, ShieldCheck, ShieldOff, Search } from 'lucide-react';

interface ProveedorInfo {
  id: number;
  nombre: string;
  contacto: string | null;
  telefono: string | null;
  email: string | null;
  notas: string | null;
  activo: boolean;
}

export default function ProveedoresPage() {
  const [proveedores, setProveedores] = useState<ProveedorInfo[]>([]);
  const [showForm, setShowForm] = useState(false);
  const [editando, setEditando] = useState<ProveedorInfo | null>(null);
  const [cargando, setCargando] = useState(true);
  const [busqueda, setBusqueda] = useState('');
  const [mostrarInactivos, setMostrarInactivos] = useState(false);

  // Recepción y Catálogo leen los proveedores del store, así que se refresca
  // ahí también para que un proveedor nuevo aparezca sin reiniciar el POS.
  const cargarProveedoresStore = useProductStore(s => s.cargarProveedores);

  const cargarDatos = async () => {
    setCargando(true);
    try {
      const p = await invoke<ProveedorInfo[]>('listar_proveedores');
      setProveedores(p);
      cargarProveedoresStore();
    } catch {}
    setCargando(false);
  };

  useEffect(() => { cargarDatos(); }, []);

  const handleToggle = async (p: ProveedorInfo) => {
    try {
      await invoke('toggle_proveedor_activo', { id: p.id });
      cargarDatos();
    } catch (err: any) {
      alert(err?.toString());
    }
  };

  const q = busqueda.toLowerCase().trim();
  const filtrados = proveedores
    .filter(p => mostrarInactivos || p.activo)
    .filter(p => !q || (
      p.nombre.toLowerCase().includes(q) ||
      (p.contacto || '').toLowerCase().includes(q) ||
      (p.telefono || '').toLowerCase().includes(q) ||
      (p.email || '').toLowerCase().includes(q)
    ));

  // ─── Form ────
  const FormProveedor = () => {
    const [form, setForm] = useState({
      nombre: editando?.nombre || '',
      contacto: editando?.contacto || '',
      telefono: editando?.telefono || '',
      email: editando?.email || '',
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
          await invoke('actualizar_proveedor', {
            datos: {
              id: editando.id,
              nombre: form.nombre,
              contacto: form.contacto || null,
              telefono: form.telefono || null,
              email: form.email || null,
              notas: form.notas || null,
            },
          });
        } else {
          await invoke('crear_proveedor', {
            nombre: form.nombre,
            contacto: form.contacto || null,
            telefono: form.telefono || null,
            email: form.email || null,
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
      <div className="pos-modal-overlay" style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.6)',
        display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 100,
      }} onClick={() => { setShowForm(false); setEditando(null); }}>
        <div className="card animate-fade-in pos-modal-content" style={{ width: 480, maxWidth: 480, padding: 24 }}
          onClick={e => e.stopPropagation()}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 20 }}>
            <h2 style={{ fontSize: 18, fontWeight: 700 }}>
              {editando ? 'Editar Proveedor' : 'Nuevo Proveedor'}
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
                placeholder="Nombre del proveedor" autoFocus />
            </div>

            <div>
              <label style={labelStyle}>CONTACTO</label>
              <input className="input" value={form.contacto}
                onChange={e => setForm(f => ({ ...f, contacto: e.target.value }))}
                placeholder="Opcional — con quién se trata" />
            </div>

            <div className="pos-2col-grid" style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
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

            <div>
              <label style={labelStyle}>NOTAS</label>
              <textarea className="input" rows={2} value={form.notas}
                onChange={e => setForm(f => ({ ...f, notas: e.target.value }))}
                placeholder="Qué vende, días de visita, condiciones de pago..." />
            </div>

            {error && <p style={{ color: 'var(--color-danger)', fontSize: 13 }}>{error}</p>}

            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', marginTop: 4 }}>
              <button type="button" className="btn btn-ghost"
                onClick={() => { setShowForm(false); setEditando(null); }}>
                Cancelar
              </button>
              <button type="submit" className="btn btn-primary" disabled={guardando}>
                {guardando ? 'Guardando...' : editando ? 'Guardar Cambios' : 'Crear Proveedor'}
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
        <span className="animate-pulse-soft">Cargando proveedores...</span>
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
          <Building2 size={18} style={{ color: 'var(--color-primary)' }} />
          <h2 style={{ fontSize: 16, fontWeight: 700 }}>Proveedores</h2>
          <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>
            {filtrados.length} / {proveedores.length}
          </span>
        </div>

        <div style={{ flex: 1, display: 'flex', alignItems: 'center', gap: 8, maxWidth: 420 }}>
          <div style={{ position: 'relative', flex: 1 }}>
            <Search size={14} style={{ position: 'absolute', left: 10, top: '50%', transform: 'translateY(-50%)', color: 'var(--color-text-dim)' }} />
            <input className="input" value={busqueda}
              onChange={e => setBusqueda(e.target.value)}
              placeholder="Buscar por nombre, contacto o teléfono..."
              style={{ paddingLeft: 32 }} />
          </div>
          <label style={{ display: 'flex', alignItems: 'center', gap: 4, fontSize: 12, color: 'var(--color-text-muted)', whiteSpace: 'nowrap' }}>
            <input type="checkbox" checked={mostrarInactivos}
              onChange={e => setMostrarInactivos(e.target.checked)} />
            Inactivos
          </label>
        </div>

        <button className="btn btn-primary" onClick={() => { setEditando(null); setShowForm(true); }}>
          <Plus size={16} /> Nuevo Proveedor
        </button>
      </div>

      {/* Lista */}
      <div style={{ flex: 1, overflow: 'auto', padding: 16 }}>
        {filtrados.length === 0 ? (
          <div style={{ textAlign: 'center', color: 'var(--color-text-dim)', padding: 40 }}>
            {q ? 'Sin resultados para la búsqueda.' : 'No hay proveedores registrados.'}
          </div>
        ) : (
          <div style={{ display: 'grid', gap: 8 }}>
            {filtrados.map(p => (
              <div key={p.id} className="card pos-list-row" style={{
                padding: '14px 18px',
                display: 'flex', alignItems: 'center', gap: 14,
                opacity: p.activo ? 1 : 0.5,
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
                  {p.nombre.charAt(0).toUpperCase()}
                </div>

                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
                    <span style={{ fontWeight: 600, fontSize: 14 }}>{p.nombre}</span>
                    {!p.activo && (
                      <span className="tag tag-danger" style={{ fontSize: 10 }}>Inactivo</span>
                    )}
                  </div>
                  <div style={{ fontSize: 12, color: 'var(--color-text-dim)', display: 'flex', gap: 12, flexWrap: 'wrap' }}>
                    {p.contacto && <span>👤 {p.contacto}</span>}
                    {p.telefono && <span>📞 {p.telefono}</span>}
                    {p.email && <span>✉️ {p.email}</span>}
                    {p.notas && <span style={{ fontStyle: 'italic' }}>· {p.notas}</span>}
                  </div>
                </div>

                <div style={{ display: 'flex', gap: 6 }}>
                  <button className="btn btn-ghost btn-sm"
                    onClick={() => { setEditando(p); setShowForm(true); }}
                    title="Editar">
                    <Edit2 size={14} />
                  </button>
                  <button
                    className="btn btn-ghost btn-sm"
                    onClick={() => handleToggle(p)}
                    title={p.activo ? 'Desactivar' : 'Activar'}
                    style={{ color: p.activo ? 'var(--color-danger)' : 'var(--color-success)' }}
                  >
                    {p.activo ? <ShieldOff size={14} /> : <ShieldCheck size={14} />}
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {showForm && <FormProveedor />}
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
