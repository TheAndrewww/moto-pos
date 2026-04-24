import { useState, useEffect } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { pairingRedeem } from '../lib/api';
import { setSession } from '../lib/auth';

export default function Pairing() {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const [token, setToken] = useState<string>('');
  const [nombre, setNombre] = useState('');
  const [pin, setPin] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [enviando, setEnviando] = useState(false);

  useEffect(() => {
    const t = params.get('token');
    if (!t) {
      setError('No hay token de emparejamiento. Escanea el QR del POS.');
    } else {
      setToken(t);
    }
  }, [params]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!nombre.trim() || !pin.trim() || !token) return;
    setEnviando(true);
    setError(null);
    try {
      const auth = await pairingRedeem(token, pin, nombre.trim());
      setSession(auth.jwt, auth.device_id, nombre.trim(), auth.usuario);
      navigate('/', { replace: true });
    } catch (e: any) {
      setError(e.message || 'Error al emparejar');
    } finally {
      setEnviando(false);
    }
  };

  return (
    <div className="screen">
      <div className="topbar">
        <h1>Emparejar dispositivo</h1>
      </div>

      <div className="card">
        <p style={{ fontSize: 14, color: '#4b5563', marginBottom: 16 }}>
          Este celular se va a emparejar con el POS de la tienda. Dale un nombre reconocible e
          ingresa tu PIN de usuario.
        </p>

        {error && <div className="banner-error">{error}</div>}

        <form onSubmit={handleSubmit}>
          <label className="label">Nombre del dispositivo</label>
          <input
            className="input"
            type="text"
            value={nombre}
            onChange={e => setNombre(e.target.value)}
            placeholder="Ej. iPhone de Miguel"
            autoFocus
            style={{ marginBottom: 16 }}
          />

          <label className="label">Tu PIN (4 dígitos)</label>
          <input
            className="input pin-input"
            type="tel"
            inputMode="numeric"
            pattern="[0-9]*"
            maxLength={4}
            value={pin}
            onChange={e => setPin(e.target.value.replace(/\D/g, ''))}
            placeholder="••••"
            style={{ marginBottom: 20 }}
          />

          <button
            type="submit"
            className="btn btn-primary"
            disabled={enviando || !nombre.trim() || pin.length !== 4 || !token}
          >
            {enviando ? 'Emparejando...' : 'Emparejar'}
          </button>
        </form>
      </div>
    </div>
  );
}
