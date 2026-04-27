// pages/Usuarios.tsx — Gestión de usuarios (solo dueño)

import { useState, useEffect } from 'react';
import { invoke } from '../lib/invokeCompat';
import { useAuthStore } from '../store/authStore';
import { Users, Plus, Edit2, X, ShieldCheck, ShieldOff, Eye, EyeOff } from 'lucide-react';

interface UsuarioInfo {
  id: number;
  nombre_completo: string;
  nombre_usuario: string;
  rol_id: number;
  rol_nombre: string;
  es_admin: boolean;
  activo: boolean;
  ultimo_login: string | null;
  created_at: string;
}

interface RolInfo {
  id: number;
  nombre: string;
  es_admin: boolean;
}

export default function UsuariosPage() {
  const { usuario } = useAuthStore();
  const [usuarios, setUsuarios] = useState<UsuarioInfo[]>([]);
  const [roles, setRoles] = useState<RolInfo[]>([]);
  const [showForm, setShowForm] = useState(false);
  const [editando, setEditando] = useState<UsuarioInfo | null>(null);
  const [cargando, setCargando] = useState(true);

  const cargarDatos = async () => {
    setCargando(true);
    try {
      const [u, r] = await Promise.all([
        invoke<UsuarioInfo[]>('listar_usuarios'),
        invoke<RolInfo[]>('listar_roles'),
      ]);
      setUsuarios(u);
      setRoles(r);
    } catch {}
    setCargando(false);
  };

  useEffect(() => { cargarDatos(); }, []);

  const handleToggle = async (u: UsuarioInfo) => {
    if (!usuario) return;
    try {
      await invoke('toggle_usuario_activo', {
        usuarioId: u.id,
        activo: !u.activo,
        adminId: usuario.id,
      });
      cargarDatos();
    } catch (err: any) {
      alert(err?.toString());
    }
  };

  // ─── Form ────
  const FormUsuario = () => {
    const [form, setForm] = useState({
      nombre_completo: editando?.nombre_completo || '',
      nombre_usuario: editando?.nombre_usuario || '',
      pin: '',
      password: '',
      rol_id: editando?.rol_id || (roles.find(r => !r.es_admin)?.id || 1),
    });
    const [showPin, setShowPin] = useState(false);
    const [showPass, setShowPass] = useState(false);
    const [guardando, setGuardando] = useState(false);
    const [error, setError] = useState('');

    const handleSubmit = async (e: React.FormEvent) => {
      e.preventDefault();
      if (!form.nombre_completo.trim()) return setError('El nombre es obligatorio');
      if (!form.nombre_usuario.trim()) return setError('El usuario es obligatorio');
      if (!editando && !form.pin) return setError('El PIN es obligatorio');
      if (!editando && form.pin.length !== 4) return setError('El PIN debe ser de 4 dígitos');
      if (!editando && !form.password) return setError('La contraseña es obligatoria');
      if (!usuario) return;

      setGuardando(true);
      setError('');

      try {
        if (editando) {
          await invoke('actualizar_usuario', {
            usuario: {
              id: editando.id,
              nombre_completo: form.nombre_completo,
              nombre_usuario: form.nombre_usuario,
              rol_id: Number(form.rol_id),
              nuevo_pin: form.pin || null,
              nuevo_password: form.password || null,
            },
            adminId: usuario.id,
          });
        } else {
          await invoke('crear_usuario', {
            usuario: {
              nombre_completo: form.nombre_completo,
              nombre_usuario: form.nombre_usuario,
              pin: form.pin,
              password: form.password,
              rol_id: Number(form.rol_id),
            },
            adminId: usuario.id,
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
        <div className="card animate-fade-in pos-modal-content" style={{ width: 460, maxWidth: 460, padding: 24 }}
          onClick={e => e.stopPropagation()}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 20 }}>
            <h2 style={{ fontSize: 18, fontWeight: 700 }}>
              {editando ? 'Editar Usuario' : 'Nuevo Usuario'}
            </h2>
            <button className="btn btn-ghost btn-sm" onClick={() => { setShowForm(false); setEditando(null); }}>
              <X size={18} />
            </button>
          </div>

          <form onSubmit={handleSubmit} style={{ display: 'flex', flexDirection: 'column', gap: 14 }}>
            <div>
              <label style={labelStyle}>NOMBRE COMPLETO *</label>
              <input className="input" value={form.nombre_completo}
                onChange={e => setForm(f => ({ ...f, nombre_completo: e.target.value }))}
                placeholder="Ej: Juan Pérez" autoFocus />
            </div>

            <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
              <div>
                <label style={labelStyle}>USUARIO *</label>
                <input className="input" value={form.nombre_usuario}
                  onChange={e => setForm(f => ({ ...f, nombre_usuario: e.target.value }))}
                  placeholder="Ej: juan" />
              </div>
              <div>
                <label style={labelStyle}>ROL *</label>
                <select className="input" value={form.rol_id}
                  onChange={e => setForm(f => ({ ...f, rol_id: Number(e.target.value) }))}>
                  {roles.map(r => (
                    <option key={r.id} value={r.id}>
                      {r.nombre} {r.es_admin ? '👑' : ''}
                    </option>
                  ))}
                </select>
              </div>
            </div>

            <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
              <div>
                <label style={labelStyle}>
                  PIN (4 dígitos) {editando ? '(vacío = sin cambio)' : '*'}
                </label>
                <div style={{ position: 'relative' }}>
                  <input className="input mono" value={form.pin}
                    type={showPin ? 'text' : 'password'}
                    maxLength={4} inputMode="numeric"
                    pattern="[0-9]*"
                    onChange={e => {
                      const v = e.target.value.replace(/\D/g, '');
                      setForm(f => ({ ...f, pin: v }));
                    }}
                    placeholder="••••" />
                  <button type="button" className="btn btn-ghost btn-sm"
                    style={{ position: 'absolute', right: 4, top: '50%', transform: 'translateY(-50%)' }}
                    onClick={() => setShowPin(!showPin)}>
                    {showPin ? <EyeOff size={14} /> : <Eye size={14} />}
                  </button>
                </div>
              </div>
              <div>
                <label style={labelStyle}>
                  CONTRASEÑA {editando ? '(vacío = sin cambio)' : '*'}
                </label>
                <div style={{ position: 'relative' }}>
                  <input className="input" value={form.password}
                    type={showPass ? 'text' : 'password'}
                    onChange={e => setForm(f => ({ ...f, password: e.target.value }))}
                    placeholder="••••••••" />
                  <button type="button" className="btn btn-ghost btn-sm"
                    style={{ position: 'absolute', right: 4, top: '50%', transform: 'translateY(-50%)' }}
                    onClick={() => setShowPass(!showPass)}>
                    {showPass ? <EyeOff size={14} /> : <Eye size={14} />}
                  </button>
                </div>
              </div>
            </div>

            {error && <p style={{ color: 'var(--color-danger)', fontSize: 13 }}>{error}</p>}

            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', marginTop: 4 }}>
              <button type="button" className="btn btn-ghost"
                onClick={() => { setShowForm(false); setEditando(null); }}>
                Cancelar
              </button>
              <button type="submit" className="btn btn-primary" disabled={guardando}>
                {guardando ? 'Guardando...' : editando ? 'Guardar Cambios' : 'Crear Usuario'}
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
        <span className="animate-pulse-soft">Cargando usuarios...</span>
      </div>
    );
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      {/* Toolbar */}
      <div style={{
        padding: '12px 16px', borderBottom: '1px solid var(--color-border)',
        display: 'flex', alignItems: 'center', justifyContent: 'space-between',
      }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <Users size={18} style={{ color: 'var(--color-primary)' }} />
          <h2 style={{ fontSize: 16, fontWeight: 700 }}>Gestión de Usuarios</h2>
          <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>
            {usuarios.length} usuario{usuarios.length !== 1 ? 's' : ''}
          </span>
        </div>
        <button className="btn btn-primary" onClick={() => { setEditando(null); setShowForm(true); }}>
          <Plus size={16} /> Nuevo Usuario
        </button>
      </div>

      {/* Lista */}
      <div style={{ flex: 1, overflow: 'auto', padding: 16 }}>
        <div style={{ display: 'grid', gap: 8 }}>
          {usuarios.map(u => (
            <div key={u.id} className="card" style={{
              padding: '14px 18px',
              display: 'flex', alignItems: 'center', gap: 14,
              opacity: u.activo ? 1 : 0.5,
              transition: 'opacity 0.2s',
            }}>
              {/* Avatar */}
              <div style={{
                width: 40, height: 40, borderRadius: '50%',
                background: u.es_admin ? 'var(--color-primary)' : 'var(--color-surface-2)',
                display: 'flex', alignItems: 'center', justifyContent: 'center',
                fontSize: 16, fontWeight: 700,
                color: u.es_admin ? '#fff' : 'var(--color-text-muted)',
                flexShrink: 0,
              }}>
                {u.nombre_completo.charAt(0).toUpperCase()}
              </div>

              {/* Info */}
              <div style={{ flex: 1 }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <span style={{ fontWeight: 600, fontSize: 14 }}>{u.nombre_completo}</span>
                  <span style={{
                    fontSize: 10, padding: '1px 8px', borderRadius: 10,
                    background: u.es_admin ? 'rgba(216,56,77,0.12)' : 'rgba(158,122,126,0.08)',
                    color: u.es_admin ? 'var(--color-primary)' : 'var(--color-text-muted)',
                    fontWeight: 600,
                  }}>
                    {u.rol_nombre} {u.es_admin && '👑'}
                  </span>
                  {!u.activo && (
                    <span className="tag tag-danger" style={{ fontSize: 10 }}>Inactivo</span>
                  )}
                </div>
                <div style={{ fontSize: 12, color: 'var(--color-text-dim)', display: 'flex', gap: 12 }}>
                  <span>@{u.nombre_usuario}</span>
                  {u.ultimo_login && <span>Último login: {u.ultimo_login}</span>}
                </div>
              </div>

              {/* Acciones — no permitir editar/desactivar al usuario actual */}
              <div style={{ display: 'flex', gap: 6 }}>
                <button className="btn btn-ghost btn-sm"
                  onClick={() => { setEditando(u); setShowForm(true); }}
                  title="Editar">
                  <Edit2 size={14} />
                </button>
                {u.id !== usuario?.id && (
                  <button
                    className={`btn btn-ghost btn-sm`}
                    onClick={() => handleToggle(u)}
                    title={u.activo ? 'Desactivar' : 'Activar'}
                    style={{ color: u.activo ? 'var(--color-danger)' : 'var(--color-success)' }}
                  >
                    {u.activo ? <ShieldOff size={14} /> : <ShieldCheck size={14} />}
                  </button>
                )}
              </div>
            </div>
          ))}
        </div>
      </div>

      {showForm && <FormUsuario />}
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
