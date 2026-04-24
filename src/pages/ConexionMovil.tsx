// pages/ConexionMovil.tsx — Configurar y gestionar dispositivos móviles (Fase 3.1)

import { useState, useEffect, useCallback } from 'react';
import { invoke } from '../lib/invokeCompat';
import { useAuthStore } from '../store/authStore';
import { Smartphone, Wifi, WifiOff, RefreshCw, Trash2, Copy, Clock } from 'lucide-react';

interface ServerInfo {
  activo: boolean;
  port: number;
  ips: string[];
}

interface PairingQr {
  token: string;
  url: string;
  qr_svg: string;
  expires_in: number;
}

interface Dispositivo {
  id: number;
  nombre: string;
  user_agent: string | null;
  usuario_id: number;
  usuario_nombre: string;
  ultimo_ping: string | null;
  ip_ultima: string | null;
  created_at: string;
  revocado: boolean;
}

export default function ConexionMovil() {
  const { usuario } = useAuthStore();
  const [info, setInfo] = useState<ServerInfo | null>(null);
  const [ipElegida, setIpElegida] = useState<string>('');
  const [qr, setQr] = useState<PairingQr | null>(null);
  const [dispositivos, setDispositivos] = useState<Dispositivo[]>([]);
  const [segundosRestantes, setSegundosRestantes] = useState<number>(0);
  const [cargando, setCargando] = useState(false);
  const [copiado, setCopiado] = useState(false);

  const cargarInfo = useCallback(async () => {
    try {
      const i = await invoke<ServerInfo>('obtener_info_servidor');
      setInfo(i);
      if (i.ips.length > 0 && !ipElegida) setIpElegida(i.ips[0]);
    } catch {}
  }, [ipElegida]);

  const cargarDispositivos = useCallback(async () => {
    try {
      const list = await invoke<Dispositivo[]>('listar_dispositivos');
      setDispositivos(list);
    } catch {}
  }, []);

  useEffect(() => {
    cargarInfo();
    cargarDispositivos();
    const interval = setInterval(() => {
      cargarInfo();
      cargarDispositivos();
    }, 10000);
    return () => clearInterval(interval);
  }, [cargarInfo, cargarDispositivos]);

  // Timer del QR
  useEffect(() => {
    if (!qr) { setSegundosRestantes(0); return; }
    setSegundosRestantes(qr.expires_in);
    const t = setInterval(() => {
      setSegundosRestantes(s => {
        if (s <= 1) { clearInterval(t); setQr(null); return 0; }
        return s - 1;
      });
    }, 1000);
    return () => clearInterval(t);
  }, [qr]);

  const generarQr = async () => {
    if (!ipElegida || !info?.activo) return;
    setCargando(true);
    try {
      const nuevo = await invoke<PairingQr>('generar_qr_emparejamiento', { ip: ipElegida });
      setQr(nuevo);
    } catch (e: any) {
      alert(`Error generando QR: ${e}`);
    } finally {
      setCargando(false);
    }
  };

  const revocar = async (id: number, nombre: string) => {
    if (!confirm(`¿Revocar acceso del dispositivo "${nombre}"?`)) return;
    try {
      await invoke('revocar_dispositivo', { id, usuarioId: usuario?.id });
      await cargarDispositivos();
    } catch (e: any) {
      alert(`Error: ${e}`);
    }
  };

  const copiar = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopiado(true);
      setTimeout(() => setCopiado(false), 1500);
    } catch {}
  };

  const activos = dispositivos.filter(d => !d.revocado);
  const revocados = dispositivos.filter(d => d.revocado);

  return (
    <div style={{ padding: 24, overflow: 'auto', height: '100%' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 20 }}>
        <Smartphone size={22} />
        <h2 style={{ margin: 0 }}>Conexión móvil</h2>
        {info?.activo ? (
          <span className="pill" style={{ background: '#dcfce7', color: '#166534' }}>
            <Wifi size={12} style={{ marginRight: 4 }} /> Servidor activo
          </span>
        ) : (
          <span className="pill" style={{ background: '#fee2e2', color: '#991b1b' }}>
            <WifiOff size={12} style={{ marginRight: 4 }} /> Detenido
          </span>
        )}
      </div>

      {!info?.activo && (
        <div style={{
          padding: 16, background: '#fef3c7', border: '1px solid #fde68a',
          borderRadius: 8, marginBottom: 20, fontSize: 13,
        }}>
          El servidor móvil aún está arrancando o falló al iniciar. Espera unos segundos o revisa los logs.
        </div>
      )}

      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 24 }}>

        {/* === IZQ: QR + IP === */}
        <div className="card">
          <h3 style={{ marginTop: 0 }}>Conectar un celular</h3>

          {info && info.ips.length > 1 && (
            <div style={{ marginBottom: 12 }}>
              <label style={{ fontSize: 12, fontWeight: 600, marginBottom: 4, display: 'block' }}>
                IP a usar (varias interfaces disponibles)
              </label>
              <select
                value={ipElegida}
                onChange={e => { setIpElegida(e.target.value); setQr(null); }}
                style={{ width: '100%', padding: 8 }}
              >
                {info.ips.map(ip => <option key={ip} value={ip}>{ip}</option>)}
              </select>
            </div>
          )}

          <div style={{
            padding: 12, background: '#f3f4f6', borderRadius: 6, fontSize: 13,
            display: 'flex', alignItems: 'center', gap: 8, marginBottom: 16,
          }}>
            <code style={{ flex: 1 }}>
              https://{ipElegida || '—'}:{info?.port || '—'}
            </code>
            <button
              className="btn btn-ghost"
              onClick={() => copiar(`https://${ipElegida}:${info?.port}`)}
              style={{ padding: '4px 8px' }}
            >
              <Copy size={14} /> {copiado ? '✓' : ''}
            </button>
          </div>

          {qr ? (
            <div style={{ textAlign: 'center' }}>
              <div
                style={{
                  background: '#fff', padding: 12, border: '1px solid #e5e7eb',
                  borderRadius: 8, display: 'inline-block', marginBottom: 12,
                }}
                dangerouslySetInnerHTML={{ __html: qr.qr_svg }}
              />
              <div style={{
                fontSize: 13, color: '#6b7280', marginBottom: 12,
                display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 6,
              }}>
                <Clock size={14} /> Expira en {segundosRestantes}s
              </div>
              <button className="btn btn-ghost" onClick={generarQr} disabled={cargando}>
                <RefreshCw size={14} /> Generar otro
              </button>
            </div>
          ) : (
            <div style={{ textAlign: 'center' }}>
              <button
                className="btn btn-primary"
                onClick={generarQr}
                disabled={cargando || !info?.activo || !ipElegida}
                style={{ padding: '12px 24px', fontSize: 14 }}
              >
                {cargando ? 'Generando...' : 'Generar código QR'}
              </button>
              <p style={{ fontSize: 12, color: '#6b7280', marginTop: 12, lineHeight: 1.5 }}>
                El celular debe estar en la misma red WiFi que este POS.
                Al escanear el QR se abrirá la app móvil y pedirá un PIN para emparejarse.
              </p>
            </div>
          )}
        </div>

        {/* === DER: Dispositivos === */}
        <div className="card">
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 12 }}>
            <h3 style={{ margin: 0 }}>Dispositivos emparejados</h3>
            <button className="btn btn-ghost" onClick={cargarDispositivos} style={{ padding: '4px 10px' }}>
              <RefreshCw size={14} />
            </button>
          </div>

          {activos.length === 0 && (
            <div style={{ padding: 24, textAlign: 'center', color: '#9ca3af', fontSize: 13 }}>
              Aún no hay celulares emparejados.
            </div>
          )}

          {activos.map(d => (
            <div key={d.id} style={{
              padding: 12, border: '1px solid #e5e7eb', borderRadius: 6,
              marginBottom: 8, display: 'flex', gap: 12, alignItems: 'center',
            }}>
              <div style={{
                width: 32, height: 32, borderRadius: '50%',
                background: '#dbeafe', display: 'flex', alignItems: 'center', justifyContent: 'center',
              }}>
                <Smartphone size={16} />
              </div>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontWeight: 600, fontSize: 13 }}>{d.nombre}</div>
                <div style={{ fontSize: 11, color: '#6b7280' }}>
                  {d.usuario_nombre} · {pingLabel(d.ultimo_ping)}
                </div>
              </div>
              <button
                className="btn btn-ghost"
                onClick={() => revocar(d.id, d.nombre)}
                title="Revocar acceso"
                style={{ color: '#dc2626', padding: '4px 8px' }}
              >
                <Trash2 size={14} />
              </button>
            </div>
          ))}

          {revocados.length > 0 && (
            <details style={{ marginTop: 12 }}>
              <summary style={{ fontSize: 12, color: '#6b7280', cursor: 'pointer' }}>
                Revocados ({revocados.length})
              </summary>
              {revocados.map(d => (
                <div key={d.id} style={{
                  padding: 8, fontSize: 12, color: '#9ca3af',
                  borderBottom: '1px solid #f3f4f6',
                }}>
                  {d.nombre} · {d.usuario_nombre}
                </div>
              ))}
            </details>
          )}
        </div>
      </div>

      {/* === Instrucciones === */}
      <div className="card" style={{ marginTop: 24 }}>
        <h3 style={{ marginTop: 0 }}>Primer uso — configuración</h3>
        <ol style={{ fontSize: 13, lineHeight: 1.8, margin: 0, paddingLeft: 20 }}>
          <li>Asegúrate que el celular y este POS estén en la <b>misma red WiFi</b>.</li>
          <li>Abre la cámara del celular → escanea el QR de arriba.</li>
          <li>El navegador mostrará un aviso de "conexión no privada" (normal, el certificado es local). Toca <b>Avanzado → Continuar</b>.</li>
          <li>Ingresa un nombre para el dispositivo y tu PIN de usuario.</li>
          <li>Agrega la página a tu pantalla de inicio para tenerla como app.</li>
        </ol>
      </div>
    </div>
  );
}

function pingLabel(iso: string | null): string {
  if (!iso) return 'nunca';
  const d = new Date(iso.replace(' ', 'T'));
  const diff = (Date.now() - d.getTime()) / 1000;
  if (diff < 60) return `activo ahora`;
  if (diff < 3600) return `hace ${Math.floor(diff / 60)} min`;
  if (diff < 86400) return `hace ${Math.floor(diff / 3600)} h`;
  return `hace ${Math.floor(diff / 86400)} días`;
}
