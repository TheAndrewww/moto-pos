// pages/Ajustes.tsx — Configuración del negocio (datos para tickets)

import { useState, useEffect } from 'react';
import { invoke } from '../lib/invokeCompat';
import { Settings, Save, Printer, CheckCircle2, Database, Download, Upload, RefreshCw, AlertTriangle, Clock, ToggleLeft, ToggleRight } from 'lucide-react';
import { imprimirTicket, type ConfigNegocio } from '../utils/ticket';

// Extensión local de ConfigNegocio con campos de respaldo automático
// Estos campos se persisten vía actualizar_config_negocio una vez que Track A los agregue al struct Rust.
// Mientras tanto, se manejan con fallback local.
interface ConfigNegocioExtended extends ConfigNegocio {
  respaldo_auto_activo?: boolean;
  respaldo_auto_hora?: string;
}

interface Respaldo {
  nombre: string;
  ruta: string;
  tamanio_bytes: number;
  created_at: string;
}

interface ImpresoraInfo {
  nombre: string;
  default: boolean;
}

export default function Ajustes() {
  const [config, setConfig] = useState<ConfigNegocioExtended>({
    nombre: '', direccion: '', telefono: '', rfc: '', mensaje_pie: '',
    respaldo_auto_activo: false, respaldo_auto_hora: '23:00',
    impresora_termica: '',
  });
  const [impresoras, setImpresoras] = useState<ImpresoraInfo[]>([]);
  const [cargando, setCargando] = useState(true);
  const [guardando, setGuardando] = useState(false);
  const [guardado, setGuardado] = useState(false);

  // Respaldos
  const [respaldos, setRespaldos] = useState<Respaldo[]>([]);
  const [creandoRespaldo, setCreandoRespaldo] = useState(false);
  const [restaurando, setRestaurando] = useState<string | null>(null);
  const [confirmarRestaurar, setConfirmarRestaurar] = useState<Respaldo | null>(null);
  const [tamanioBD, setTamanioBD] = useState<number | null>(null);

  const cargarRespaldos = () => {
    invoke<Respaldo[]>('listar_respaldos').then(setRespaldos).catch(() => {});
    invoke<number>('obtener_info_bd').then(setTamanioBD).catch(() => {});
  };

  useEffect(() => {
    invoke<ConfigNegocioExtended>('obtener_config_negocio')
      .then(c => setConfig(prev => ({
        ...prev,
        ...c,
        respaldo_auto_activo: (c as any).respaldo_auto_activo ?? prev.respaldo_auto_activo,
        respaldo_auto_hora: (c as any).respaldo_auto_hora ?? prev.respaldo_auto_hora,
        impresora_termica: (c as any).impresora_termica ?? '',
      })))
      .catch(() => {})
      .finally(() => setCargando(false));
    cargarRespaldos();
    invoke<ImpresoraInfo[]>('listar_impresoras').then(setImpresoras).catch(() => {});
  }, []);

  const handleCrearRespaldo = async () => {
    setCreandoRespaldo(true);
    try {
      await invoke('crear_respaldo');
      cargarRespaldos();
    } catch (e: any) {
      alert(e?.toString() || 'Error al crear respaldo');
    } finally {
      setCreandoRespaldo(false);
    }
  };

  const handleRestaurar = async (r: Respaldo) => {
    setRestaurando(r.ruta);
    try {
      await invoke('restaurar_respaldo', { ruta: r.ruta });
      alert('Restauración completada. Se recomienda cerrar y volver a abrir la aplicación para evitar inconsistencias en pantalla.');
      cargarRespaldos();
    } catch (e: any) {
      alert(e?.toString() || 'Error al restaurar');
    } finally {
      setRestaurando(null);
      setConfirmarRestaurar(null);
    }
  };

  const fmtSize = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
  };

  const handleGuardar = async () => {
    setGuardando(true);
    try {
      const actualizado = await invoke<ConfigNegocioExtended>('actualizar_config_negocio', { datos: config });
      setConfig(prev => ({ ...prev, ...actualizado }));
      setGuardado(true);
      setTimeout(() => setGuardado(false), 2000);
    } catch (e: any) {
      alert(e?.toString() || 'Error al guardar');
    } finally {
      setGuardando(false);
    }
  };

  const handlePrueba = () => {
    imprimirTicket(config, {
      folio: 'PRUEBA-001',
      fecha: new Date().toLocaleString('es-MX'),
      usuario: 'Vendedor de prueba',
      cliente: 'Cliente Ejemplo',
      items: [
        { nombre: 'Producto de ejemplo 1', codigo: 'MR-00001', cantidad: 2, precio_final: 150.00, subtotal: 300.00 },
        { nombre: 'Producto de ejemplo 2', codigo: 'MR-00002', cantidad: 1, precio_final: 85.50, subtotal: 85.50, descuento_porcentaje: 10 },
      ],
      subtotal: 385.50,
      descuento: 9.50,
      total: 385.50,
      metodo_pago: 'efectivo',
      monto_recibido: 400.00,
      cambio: 14.50,
    });
  };

  if (cargando) {
    return <div style={{ padding: 40, textAlign: 'center', color: 'var(--color-text-dim)' }}>Cargando...</div>;
  }

  const setField = <K extends keyof ConfigNegocioExtended>(k: K, v: ConfigNegocioExtended[K]) =>
    setConfig(prev => ({ ...prev, [k]: v }));

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      <div className="pos-page-header" style={{
        padding: '12px 20px', borderBottom: '1px solid var(--color-border)',
        background: 'var(--color-surface)', display: 'flex', alignItems: 'center', gap: 10,
      }}>
        <Settings size={20} style={{ color: 'var(--color-primary)' }} />
        <h2 style={{ fontSize: 17, fontWeight: 800, color: 'var(--color-text)' }}>Ajustes del Negocio</h2>
      </div>

      <div style={{ flex: 1, overflow: 'auto', padding: 24, display: 'flex', justifyContent: 'center' }}>
        <div style={{ width: '100%', maxWidth: 560, display: 'flex', flexDirection: 'column', gap: 20 }}>
          <section className="card" style={{ padding: 20 }}>
            <h3 style={{ fontSize: 14, fontWeight: 700, marginBottom: 4 }}>Datos para el ticket</h3>
            <p style={{ fontSize: 12, color: 'var(--color-text-muted)', marginBottom: 16 }}>
              Esta información aparece en la cabecera y pie de cada ticket impreso.
            </p>

            <Campo label="Nombre del negocio *"
              value={config.nombre}
              onChange={v => setField('nombre', v)}
              placeholder="Moto Refaccionaria LB" />

            <Campo label="Dirección"
              value={config.direccion}
              onChange={v => setField('direccion', v)}
              placeholder="Calle 123, Col. Centro, Ciudad, CP" />

            <Campo label="Teléfono"
              value={config.telefono}
              onChange={v => setField('telefono', v)}
              placeholder="555-123-4567" />

            <Campo label="RFC"
              value={config.rfc}
              onChange={v => setField('rfc', v.toUpperCase())}
              placeholder="XAXX010101000" />

            <Campo label="Mensaje al pie del ticket"
              value={config.mensaje_pie}
              onChange={v => setField('mensaje_pie', v)}
              placeholder="¡Gracias por su compra!" />
          </section>

          <section className="card" style={{ padding: 20 }}>
            <h3 style={{ fontSize: 14, fontWeight: 700, marginBottom: 4, display: 'flex', alignItems: 'center', gap: 6 }}>
              <Printer size={14} /> Impresora térmica
            </h3>
            <p style={{ fontSize: 12, color: 'var(--color-text-muted)', marginBottom: 16 }}>
              Al cobrar, el ticket se enviará directo a esta impresora (sin abrir ventana del navegador).
              Dejar en blanco para usar la impresión HTML en el navegador.
            </p>

            <label style={{ display: 'block', fontSize: 12, fontWeight: 600, color: 'var(--color-text)', marginBottom: 6 }}>
              Impresora del sistema
            </label>
            <select
              value={config.impresora_termica || ''}
              onChange={e => setField('impresora_termica', e.target.value)}
              style={{
                width: '100%', padding: '8px 10px', fontSize: 13,
                border: '1px solid var(--color-border)', borderRadius: 6,
                background: 'var(--color-surface)', color: 'var(--color-text)',
              }}
            >
              <option value="">— Ninguna (usar HTML en navegador) —</option>
              {impresoras.map(i => (
                <option key={i.nombre} value={i.nombre}>
                  {i.nombre}{i.default ? ' (predeterminada)' : ''}
                </option>
              ))}
            </select>
            {impresoras.length === 0 && (
              <p style={{ fontSize: 11, color: 'var(--color-text-muted)', marginTop: 8 }}>
                No se detectaron impresoras instaladas en el sistema.
              </p>
            )}
          </section>

          <div style={{ display: 'flex', gap: 10, justifyContent: 'flex-end' }}>
            <button className="btn btn-ghost" onClick={handlePrueba}>
              <Printer size={14} /> Imprimir prueba
            </button>
            <button className="btn btn-primary" onClick={handleGuardar} disabled={guardando || !config.nombre.trim()}>
              {guardado ? <><CheckCircle2 size={14} /> Guardado</> : <><Save size={14} /> Guardar</>}
            </button>
          </div>

          {/* ─── Sección: Respaldos de Base de Datos ─── */}
          <section className="card" style={{ padding: 20, marginTop: 8 }}>
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 4 }}>
              <h3 style={{ fontSize: 14, fontWeight: 700, display: 'flex', alignItems: 'center', gap: 6 }}>
                <Database size={14} /> Respaldos de Base de Datos
              </h3>
              <span style={{ fontSize: 11, color: 'var(--color-text-muted)' }}>
                {tamanioBD !== null && `BD actual: ${fmtSize(tamanioBD)}`}
              </span>
            </div>
            <p style={{ fontSize: 12, color: 'var(--color-text-muted)', marginBottom: 14 }}>
              Se crea un respaldo automático al abrir la app cada día. Se conservan los últimos 30.
            </p>

            <div style={{ display: 'flex', gap: 8, marginBottom: 14 }}>
              <button className="btn btn-primary btn-sm" onClick={handleCrearRespaldo} disabled={creandoRespaldo}>
                <Download size={14} /> {creandoRespaldo ? 'Creando...' : 'Crear respaldo ahora'}
              </button>
              <button className="btn btn-ghost btn-sm" onClick={cargarRespaldos}>
                <RefreshCw size={14} /> Actualizar
              </button>
            </div>

            {respaldos.length === 0 ? (
              <div style={{ padding: 16, textAlign: 'center', color: 'var(--color-text-dim)', fontSize: 13 }}>
                No hay respaldos aún
              </div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4, maxHeight: 260, overflow: 'auto' }}>
                {respaldos.map(r => (
                  <div key={r.ruta} style={{
                    display: 'flex', alignItems: 'center', gap: 10,
                    padding: '8px 12px', borderRadius: 6,
                    background: 'var(--color-surface-2)',
                    border: '1px solid var(--color-border)',
                  }}>
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div className="mono" style={{ fontSize: 12, fontWeight: 600, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                        {r.nombre}
                      </div>
                      <div style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>
                        {r.created_at} · {fmtSize(r.tamanio_bytes)}
                      </div>
                    </div>
                    <button
                      className="btn btn-ghost btn-sm"
                      onClick={() => setConfirmarRestaurar(r)}
                      disabled={restaurando !== null}
                      title="Restaurar este respaldo"
                    >
                      <Upload size={12} /> Restaurar
                    </button>
                  </div>
                ))}
              </div>
            )}
          </section>

          {/* ─── Sección: Respaldo Automático ─── */}
          <section className="card" style={{ padding: 20, marginTop: 8 }}>
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 4 }}>
              <h3 style={{ fontSize: 14, fontWeight: 700, display: 'flex', alignItems: 'center', gap: 6 }}>
                <Clock size={14} /> Respaldo Automático
              </h3>
            </div>
            <p style={{ fontSize: 12, color: 'var(--color-text-muted)', marginBottom: 14 }}>
              Se ejecutará al iniciar la app si han pasado 24h del último respaldo.
            </p>

            <div style={{ display: 'flex', flexDirection: 'column', gap: 14 }}>
              {/* Toggle activar/desactivar */}
              <div
                style={{
                  display: 'flex', alignItems: 'center', justifyContent: 'space-between',
                  padding: '10px 14px', borderRadius: 8,
                  background: config.respaldo_auto_activo ? 'rgba(34,197,94,0.08)' : 'var(--color-surface-2)',
                  border: `1px solid ${config.respaldo_auto_activo ? 'rgba(34,197,94,0.3)' : 'var(--color-border)'}`,
                  cursor: 'pointer',
                  transition: 'all 0.2s',
                }}
                onClick={() => setField('respaldo_auto_activo', !config.respaldo_auto_activo)}
              >
                <div>
                  <p style={{ fontSize: 13, fontWeight: 600, color: 'var(--color-text)' }}>
                    Activar respaldo automático diario
                  </p>
                  <p style={{ fontSize: 11, color: 'var(--color-text-dim)', marginTop: 2 }}>
                    {config.respaldo_auto_activo ? 'El respaldo se ejecutará automáticamente' : 'El respaldo está desactivado'}
                  </p>
                </div>
                {config.respaldo_auto_activo
                  ? <ToggleRight size={28} style={{ color: 'var(--color-success, #22c55e)', flexShrink: 0 }} />
                  : <ToggleLeft size={28} style={{ color: 'var(--color-text-dim)', flexShrink: 0 }} />
                }
              </div>

              {/* Hora preferida */}
              {config.respaldo_auto_activo && (
                <div style={{
                  padding: '10px 14px', borderRadius: 8,
                  background: 'var(--color-surface-2)',
                  border: '1px solid var(--color-border)',
                }}>
                  <label style={{
                    display: 'block', fontSize: 11, fontWeight: 600,
                    color: 'var(--color-text-muted)', marginBottom: 6,
                    textTransform: 'uppercase', letterSpacing: 0.4,
                  }}>Hora preferida</label>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
                    <input
                      type="time"
                      className="input mono"
                      value={config.respaldo_auto_hora || '23:00'}
                      onChange={e => setField('respaldo_auto_hora', e.target.value)}
                      style={{ width: 140 }}
                    />
                    <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>
                      El respaldo se intentará a esta hora o al abrir la app.
                    </span>
                  </div>
                </div>
              )}
            </div>
          </section>
        </div>
      </div>

      {/* ─── Modal confirmar restaurar ─── */}
      {confirmarRestaurar && (
        <div className="pos-modal-overlay" style={{
          position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.6)',
          display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 200,
        }} onClick={() => setConfirmarRestaurar(null)}>
          <div className="card pos-modal-content pos-modal-fluid animate-fade-in" style={{ width: 420, maxWidth: '100%', padding: 24 }} onClick={e => e.stopPropagation()}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 10 }}>
              <AlertTriangle size={20} style={{ color: 'var(--color-warning)' }} />
              <h3 style={{ fontSize: 16, fontWeight: 700, margin: 0 }}>Restaurar respaldo</h3>
            </div>
            <p style={{ fontSize: 13, color: 'var(--color-text-muted)', marginBottom: 8 }}>
              Vas a restaurar: <strong className="mono">{confirmarRestaurar.nombre}</strong>
            </p>
            <p style={{ fontSize: 13, color: 'var(--color-text)', marginBottom: 16 }}>
              Esto <strong>reemplazará todos los datos actuales</strong> con el contenido del respaldo.
              Se creará un respaldo de seguridad automático del estado actual antes de restaurar.
            </p>
            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
              <button className="btn btn-ghost" onClick={() => setConfirmarRestaurar(null)} disabled={restaurando !== null}>
                Cancelar
              </button>
              <button className="btn btn-danger" onClick={() => handleRestaurar(confirmarRestaurar)} disabled={restaurando !== null}>
                {restaurando !== null ? 'Restaurando...' : 'Sí, restaurar'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function Campo({ label, value, onChange, placeholder }: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <div style={{ marginBottom: 12 }}>
      <label style={{
        display: 'block', fontSize: 11, fontWeight: 600,
        color: 'var(--color-text-muted)', marginBottom: 4,
        textTransform: 'uppercase', letterSpacing: 0.4,
      }}>{label}</label>
      <input
        className="input"
        value={value}
        onChange={e => onChange(e.target.value)}
        placeholder={placeholder}
        style={{ width: '100%' }}
      />
    </div>
  );
}
