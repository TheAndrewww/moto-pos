import { useEffect, useState } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import { health } from '../lib/api';
import { clearSession, getUsuario, getDeviceName } from '../lib/auth';

export default function Home() {
  const navigate = useNavigate();
  const usuario = getUsuario();
  const deviceName = getDeviceName();
  const [online, setOnline] = useState<boolean | null>(null);

  useEffect(() => {
    let cancel = false;
    const ping = async () => {
      const ok = await health();
      if (!cancel) setOnline(ok);
    };
    ping();
    const id = setInterval(ping, 10_000);
    return () => { cancel = true; clearInterval(id); };
  }, []);

  const handleLogout = () => {
    clearSession();
    navigate('/login', { replace: true });
  };

  return (
    <div className="screen">
      <div className="topbar">
        <h1>{usuario?.nombre_completo ?? 'Usuario'}</h1>
        <button
          onClick={handleLogout}
          style={{
            marginLeft: 'auto',
            background: 'none',
            border: '1px solid rgba(255,255,255,0.3)',
            color: '#fff',
            padding: '4px 10px',
            borderRadius: 6,
            fontSize: 12,
          }}
        >
          Salir
        </button>
      </div>

      <div className="home-grid">
        <Link to="/recepcion" className="tile">
          <div className="tile-icon">📦</div>
          <div className="tile-label">Recepción</div>
        </Link>
        <Link to="/consulta" className="tile">
          <div className="tile-icon">🔍</div>
          <div className="tile-label">Consulta</div>
        </Link>
      </div>

      <div className="status-bar">
        <span>
          <span className={`dot ${online === null ? 'warn' : online ? 'ok' : 'err'}`} />
          {online === null ? 'Conectando...' : online ? 'Conectado al POS' : 'Sin conexión'}
        </span>
        <span style={{ color: '#6b7280' }}>{deviceName}</span>
      </div>
    </div>
  );
}
