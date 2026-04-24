import { useState, useEffect } from 'react';
import { useNavigate } from 'react-router-dom';
import { loginPin } from '../lib/api';
import { setSession, getDeviceId, getDeviceName, clearSession } from '../lib/auth';

export default function Login() {
  const navigate = useNavigate();
  const [pin, setPin] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [enviando, setEnviando] = useState(false);
  const deviceId = getDeviceId();
  const deviceName = getDeviceName();

  useEffect(() => {
    if (!deviceId) {
      navigate('/pairing', { replace: true });
    }
  }, [deviceId, navigate]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (pin.length !== 4 || !deviceId) return;
    setEnviando(true);
    setError(null);
    try {
      const auth = await loginPin(pin, deviceId);
      setSession(auth.jwt, auth.device_id, deviceName, auth.usuario);
      navigate('/', { replace: true });
    } catch (e: any) {
      setError(e.message || 'PIN incorrecto');
      setPin('');
    } finally {
      setEnviando(false);
    }
  };

  const handleDesemparejar = () => {
    if (confirm('¿Quitar el emparejamiento de este dispositivo? Necesitarás escanear otro QR para volver a usarlo.')) {
      localStorage.clear();
      clearSession();
      navigate('/pairing', { replace: true });
    }
  };

  return (
    <div className="screen">
      <div className="topbar">
        <h1>Ingresar</h1>
      </div>

      <div className="card">
        <p style={{ fontSize: 14, color: '#4b5563', marginBottom: 4 }}>Dispositivo:</p>
        <p style={{ fontSize: 16, fontWeight: 600, marginBottom: 20 }}>{deviceName || 'Sin nombre'}</p>

        {error && <div className="banner-error">{error}</div>}

        <form onSubmit={handleSubmit}>
          <label className="label">PIN del usuario</label>
          <input
            className="input pin-input"
            type="tel"
            inputMode="numeric"
            pattern="[0-9]*"
            maxLength={4}
            value={pin}
            onChange={e => setPin(e.target.value.replace(/\D/g, ''))}
            placeholder="••••"
            autoFocus
            style={{ marginBottom: 20 }}
          />

          <button
            type="submit"
            className="btn btn-primary"
            disabled={enviando || pin.length !== 4}
          >
            {enviando ? 'Ingresando...' : 'Ingresar'}
          </button>
        </form>
      </div>

      <button
        type="button"
        className="btn btn-secondary"
        onClick={handleDesemparejar}
        style={{ marginTop: 12 }}
      >
        Desemparejar este dispositivo
      </button>
    </div>
  );
}
