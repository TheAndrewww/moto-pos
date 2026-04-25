// pages/Sincronizacion.tsx — Configuración del sync con el servidor remoto (Railway)

import { useState, useEffect } from 'react';
import { invoke } from '../lib/invokeCompat';
import { Cloud, CloudOff, CheckCircle2, AlertTriangle, RefreshCw, Wifi, Save, Upload } from 'lucide-react';

interface EstadoSync {
  activo: boolean;
  remote_url: string | null;
  device_uuid: string;
  sucursal_id: number;
  last_push_at: string | null;
  last_pull_at: string | null;
  pendientes: number;
}

export default function Sincronizacion() {
  const [estado, setEstado] = useState<EstadoSync | null>(null);
  const [cargando, setCargando] = useState(true);
  const [conectando, setConectando] = useState(false);
  const [desactivando, setDesactivando] = useState(false);
  const [probando, setProbando] = useState(false);
  const [reenviando, setReenviando] = useState(false);
  const [mensaje, setMensaje] = useState<{ tipo: 'ok' | 'error' | 'info'; texto: string } | null>(null);

  // Form
  const [remoteUrl, setRemoteUrl] = useState('https://moto-pos-production.up.railway.app');
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [sucursalId, setSucursalId] = useState<string>('');

  const cargarEstado = () => {
    invoke<EstadoSync>('obtener_estado_sync')
      .then(s => {
        setEstado(s);
        if (s.remote_url) setRemoteUrl(s.remote_url);
      })
      .catch(() => setEstado(null))
      .finally(() => setCargando(false));
  };

  useEffect(() => {
    cargarEstado();
    const i = setInterval(cargarEstado, 10_000);
    return () => clearInterval(i);
  }, []);

  const conectar = async () => {
    if (!remoteUrl.trim() || !email.trim() || !password) {
      setMensaje({ tipo: 'error', texto: 'Faltan campos: URL, email y contraseña son requeridos.' });
      return;
    }
    setConectando(true);
    setMensaje(null);
    try {
      const sid = sucursalId.trim() ? parseInt(sucursalId, 10) : null;
      const nuevo = await invoke<EstadoSync>('configurar_sync', {
        input: {
          remote_url: remoteUrl.trim().replace(/\/+$/, ''),
          email: email.trim(),
          password,
          sucursal_id: sid,
        },
      });
      setEstado(nuevo);
      setPassword('');
      setMensaje({ tipo: 'ok', texto: 'Conectado al servidor remoto. La sincronización empezará automáticamente.' });
    } catch (e: any) {
      const msg = typeof e === 'string' ? e : e?.message ?? 'Error desconocido';
      setMensaje({ tipo: 'error', texto: msg });
    } finally {
      setConectando(false);
    }
  };

  const desactivar = async () => {
    if (!confirm('¿Desactivar la sincronización? Los datos locales se conservan, pero dejarán de subirse al servidor.')) return;
    setDesactivando(true);
    try {
      await invoke('desactivar_sync');
      cargarEstado();
      setMensaje({ tipo: 'info', texto: 'Sincronización desactivada.' });
    } catch (e: any) {
      setMensaje({ tipo: 'error', texto: typeof e === 'string' ? e : 'No se pudo desactivar.' });
    } finally {
      setDesactivando(false);
    }
  };

  const reenviarTodo = async () => {
    if (!confirm('¿Reenviar todos los datos existentes al servidor? Útil tras configurar el sync por primera vez. Los datos ya sincronizados no se duplican.')) return;
    setReenviando(true);
    setMensaje(null);
    try {
      const total = await invoke<number>('backfill_outbox');
      cargarEstado();
      setMensaje({
        tipo: 'ok',
        texto: `${total} registros encolados. Empezarán a subirse en los próximos 30 segundos.`,
      });
    } catch (e: any) {
      setMensaje({ tipo: 'error', texto: typeof e === 'string' ? e : 'Error encolando datos.' });
    } finally {
      setReenviando(false);
    }
  };

  const probar = async () => {
    setProbando(true);
    setMensaje(null);
    try {
      const ok = await invoke<boolean>('probar_conexion_sync');
      setMensaje({ tipo: ok ? 'ok' : 'error', texto: ok ? 'Servidor responde correctamente.' : 'No hay respuesta del servidor.' });
    } catch (e: any) {
      setMensaje({ tipo: 'error', texto: typeof e === 'string' ? e : 'Error probando conexión.' });
    } finally {
      setProbando(false);
    }
  };

  const fmtFecha = (s: string | null) => {
    if (!s) return 'Nunca';
    try {
      const d = new Date(s);
      return d.toLocaleString('es-MX', { dateStyle: 'short', timeStyle: 'short' });
    } catch { return s; }
  };

  if (cargando) {
    return <div style={{ padding: 24 }}>Cargando…</div>;
  }

  const conectado = estado?.activo && estado.remote_url;

  return (
    <div style={{ padding: 20, maxWidth: 720, margin: '0 auto', overflow: 'auto' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 18 }}>
        <Cloud size={22} />
        <h2 style={{ margin: 0, fontSize: 20 }}>Sincronización con servidor</h2>
      </div>

      <p style={{ fontSize: 13, color: 'var(--color-text-muted)', marginTop: 0 }}>
        Conecta este POS al servidor remoto para que las ventas, productos y demás datos se respalden y queden disponibles
        desde la web/celular. Si pierdes internet, el POS sigue trabajando: los cambios se sincronizan cuando vuelva la red.
      </p>

      {/* ─── Estado actual ─── */}
      <div style={{
        padding: 14, borderRadius: 8,
        background: conectado ? 'rgba(34,197,94,0.10)' : 'rgba(158,122,126,0.10)',
        border: `1px solid ${conectado ? 'rgba(34,197,94,0.4)' : 'rgba(158,122,126,0.3)'}`,
        marginBottom: 18,
      }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 8 }}>
          {conectado
            ? <CheckCircle2 size={18} color="rgb(34,197,94)" />
            : <CloudOff size={18} color="var(--color-text-muted)" />}
          <strong style={{ fontSize: 14 }}>
            {conectado ? 'Conectado' : 'No conectado'}
          </strong>
        </div>
        {conectado && (
          <div style={{ fontSize: 12, color: 'var(--color-text-muted)', display: 'grid', gap: 4 }}>
            <div><strong>Servidor:</strong> {estado!.remote_url}</div>
            <div><strong>Sucursal:</strong> #{estado!.sucursal_id}</div>
            <div><strong>Último push:</strong> {fmtFecha(estado!.last_push_at)}</div>
            <div><strong>Último pull:</strong> {fmtFecha(estado!.last_pull_at)}</div>
            <div>
              <strong>Pendientes de subir:</strong>{' '}
              <span style={{ color: estado!.pendientes > 0 ? 'var(--color-warning)' : 'var(--color-text-muted)' }}>
                {estado!.pendientes}
              </span>
            </div>
            <div style={{ fontSize: 11, opacity: 0.7 }}>
              Device: <code>{estado!.device_uuid}</code>
            </div>
          </div>
        )}
      </div>

      {/* ─── Mensaje ─── */}
      {mensaje && (
        <div style={{
          padding: 12, borderRadius: 6, marginBottom: 14, fontSize: 13,
          display: 'flex', alignItems: 'center', gap: 8,
          background: mensaje.tipo === 'ok' ? 'rgba(34,197,94,0.10)'
            : mensaje.tipo === 'error' ? 'rgba(239,68,68,0.10)'
            : 'rgba(59,130,246,0.10)',
          color: mensaje.tipo === 'ok' ? 'rgb(34,197,94)'
            : mensaje.tipo === 'error' ? 'var(--color-danger)'
            : 'rgb(59,130,246)',
          border: `1px solid ${mensaje.tipo === 'ok' ? 'rgba(34,197,94,0.4)'
            : mensaje.tipo === 'error' ? 'rgba(239,68,68,0.4)'
            : 'rgba(59,130,246,0.4)'}`,
        }}>
          {mensaje.tipo === 'error' ? <AlertTriangle size={16} /> : <CheckCircle2 size={16} />}
          <span>{mensaje.texto}</span>
        </div>
      )}

      {/* ─── Form ─── */}
      <div style={{
        padding: 18, borderRadius: 8,
        background: 'var(--color-card)',
        border: '1px solid var(--color-border)',
      }}>
        <h3 style={{ margin: '0 0 14px', fontSize: 15 }}>
          {conectado ? 'Reconfigurar' : 'Conectar al servidor'}
        </h3>

        <div style={{ display: 'grid', gap: 12 }}>
          <Field label="URL del servidor" hint="Sin barra final. Ej: https://moto-pos-production.up.railway.app">
            <input
              type="text"
              className="input"
              value={remoteUrl}
              onChange={e => setRemoteUrl(e.target.value)}
              placeholder="https://..."
              autoComplete="off"
            />
          </Field>

          <Field label="Email">
            <input
              type="email"
              className="input"
              value={email}
              onChange={e => setEmail(e.target.value)}
              placeholder="ga.andrewww@gmail.com"
              autoComplete="username"
            />
          </Field>

          <Field label="Contraseña">
            <input
              type="password"
              className="input"
              value={password}
              onChange={e => setPassword(e.target.value)}
              autoComplete="current-password"
            />
          </Field>

          <Field label="Sucursal (opcional)" hint="Déjalo vacío para usar la sucursal por defecto del usuario.">
            <input
              type="number"
              className="input"
              value={sucursalId}
              onChange={e => setSucursalId(e.target.value)}
              placeholder="1"
              min={1}
            />
          </Field>
        </div>

        <div style={{ display: 'flex', gap: 8, marginTop: 16, flexWrap: 'wrap' }}>
          <button
            className="btn btn-primary"
            onClick={conectar}
            disabled={conectando}
          >
            {conectando
              ? <><RefreshCw size={14} className="spin" /> Conectando…</>
              : <><Save size={14} /> {conectado ? 'Guardar cambios' : 'Conectar'}</>}
          </button>

          {conectado && (
            <>
              <button
                className="btn btn-ghost"
                onClick={probar}
                disabled={probando}
              >
                {probando
                  ? <><RefreshCw size={14} className="spin" /> Probando…</>
                  : <><Wifi size={14} /> Probar conexión</>}
              </button>
              <button
                className="btn btn-ghost"
                onClick={reenviarTodo}
                disabled={reenviando}
                title="Encola todos los datos locales para subirlos al servidor. Útil la primera vez."
              >
                {reenviando
                  ? <><RefreshCw size={14} className="spin" /> Encolando…</>
                  : <><Upload size={14} /> Reenviar todo</>}
              </button>
              <button
                className="btn btn-ghost"
                onClick={desactivar}
                disabled={desactivando}
                style={{ color: 'var(--color-danger)' }}
              >
                <CloudOff size={14} /> Desactivar
              </button>
            </>
          )}
        </div>
      </div>

      <div style={{ marginTop: 18, fontSize: 11, color: 'var(--color-text-muted)', lineHeight: 1.5 }}>
        <strong>Nota:</strong> los usuarios con PIN del POS se sincronizan automáticamente con el servidor.
        El email/contraseña es solo para autenticar este dispositivo contra el servidor remoto.
      </div>
    </div>
  );
}

function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <label style={{ display: 'flex', flexDirection: 'column', gap: 4, fontSize: 12 }}>
      <span style={{ fontWeight: 600, color: 'var(--color-text)' }}>{label}</span>
      {children}
      {hint && <span style={{ fontSize: 11, color: 'var(--color-text-muted)' }}>{hint}</span>}
    </label>
  );
}
