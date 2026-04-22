// App.tsx — Router principal del POS

import { useEffect, useState } from 'react';
import { useAuthStore } from './store/authStore';
import Login from './pages/Login';
import Dashboard from './pages/Dashboard';

export default function App() {
  const { usuario } = useAuthStore();
  const [checking, setChecking] = useState(true);

  useEffect(() => {
    // Pequeño delay para que se inicialice el estado
    setChecking(false);
  }, []);

  if (checking) {
    return (
      <div style={{
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        height: '100vh',
        background: 'var(--color-bg)',
        color: 'var(--color-text-muted)',
        fontSize: 14,
      }}>
        <span className="animate-pulse-soft">Iniciando POS...</span>
      </div>
    );
  }

  // Si no hay usuario autenticado → Login
  if (!usuario) {
    return <Login />;
  }

  // Si hay usuario → Dashboard principal
  return <Dashboard />;
}
