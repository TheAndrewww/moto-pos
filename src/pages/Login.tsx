// pages/Login.tsx — Pantalla de login del POS
// Dos modos: PIN rápido (día a día) y usuario+contraseña

import { useState, useEffect, useCallback, useRef } from 'react';
import { useAuthStore } from '../store/authStore';
import clsx from 'clsx';

// ─── Tipos ────────────────────────────────────────────────
type LoginMode = 'pin' | 'password' | 'setup';

// ─── Componente principal ─────────────────────────────────
export default function Login() {
  const { loginPin, loginPassword, cargando, error, limpiarError } = useAuthStore();
  const [mode, setMode] = useState<LoginMode>('pin');
  const [pin, setPin] = useState('');
  const [pinError, setPinError] = useState(false);

  // Password mode
  const [credentials, setCredentials] = useState({ usuario: '', password: '' });
  const passwordRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    // Limpiar error al cambiar de modo
    limpiarError();
    setPin('');
    setPinError(false);
  }, [mode]);

  // Manejar teclas numéricas del teclado físico en modo PIN
  useEffect(() => {
    if (mode !== 'pin') return;

    const handleKey = (e: KeyboardEvent) => {
      if (e.key >= '0' && e.key <= '9') {
        handlePinInput(e.key);
      } else if (e.key === 'Backspace') {
        setPin(p => p.slice(0, -1));
        setPinError(false);
      } else if (e.key === 'Escape') {
        setPin('');
        setPinError(false);
      }
    };

    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [mode, pin]);

  const handlePinInput = useCallback((digit: string) => {
    setPinError(false);
    limpiarError();

    setPin(prev => {
      const next = prev + digit;
      if (next.length === 4) {
        // Triggerear login cuando se completan 4 dígitos
        setTimeout(() => attemptPinLogin(next), 80);
        return next;
      }
      return next;
    });
  }, []);

  const attemptPinLogin = async (pinValue: string) => {
    const ok = await loginPin(pinValue);
    if (!ok) {
      setPinError(true);
      setTimeout(() => {
        setPin('');
        setPinError(false);
      }, 600);
    }
  };

  const handlePasswordLogin = async (e: React.FormEvent) => {
    e.preventDefault();
    await loginPassword(credentials.usuario, credentials.password);
  };


  // ─── Render ───────────────────────────────────────────────
  return (
    <div className="flex items-center justify-center h-screen" style={{ background: 'var(--color-bg)' }}>
      {/* Logo / Header */}
      <div className="flex flex-col items-center gap-8 animate-fade-in" style={{ width: 380 }}>

        {/* Logo */}
        <div className="text-center">
          <div className="flex items-center justify-center gap-3 mb-2">
            <span style={{ fontSize: 36 }}>🏍️</span>
            <h1 style={{
              fontSize: 22,
              fontWeight: 800,
              color: 'var(--color-text)',
              letterSpacing: '-0.5px',
            }}>
              MOTO REFACCIONARIA
            </h1>
          </div>
          <p style={{ color: 'var(--color-text-muted)', fontSize: 13 }}>
            Sistema de Punto de Venta
          </p>
        </div>

        {/* Card de login */}
        <div className="card" style={{ width: '100%', padding: 28 }}>

          {/* Tabs de modo */}
          <div style={{
            display: 'flex',
            gap: 4,
            background: 'var(--color-bg)',
            borderRadius: 10,
            padding: 4,
            marginBottom: 24,
          }}>
            <TabBtn active={mode === 'pin'} onClick={() => setMode('pin')}>
              PIN Rápido
            </TabBtn>
            <TabBtn active={mode === 'password'} onClick={() => setMode('password')}>
              Usuario / Contraseña
            </TabBtn>
          </div>

          {/* Modo PIN */}
          {mode === 'pin' && (
            <PinMode
              pin={pin}
              pinError={pinError}
              onDigit={handlePinInput}
              onDelete={() => { setPin(p => p.slice(0, -1)); setPinError(false); }}
              onClear={() => { setPin(''); setPinError(false); }}
              cargando={cargando}
              error={error}
            />
          )}

          {/* Modo contraseña */}
          {mode === 'password' && (
            <form onSubmit={handlePasswordLogin} style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
              <div>
                <label style={{ fontSize: 12, fontWeight: 600, color: 'var(--color-text-muted)', display: 'block', marginBottom: 6 }}>
                  USUARIO
                </label>
                <input
                  className="input"
                  placeholder="nombre de usuario"
                  value={credentials.usuario}
                  onChange={e => setCredentials(c => ({ ...c, usuario: e.target.value }))}
                  autoFocus
                  autoComplete="username"
                />
              </div>
              <div>
                <label style={{ fontSize: 12, fontWeight: 600, color: 'var(--color-text-muted)', display: 'block', marginBottom: 6 }}>
                  CONTRASEÑA
                </label>
                <input
                  className="input"
                  type="password"
                  placeholder="••••••••"
                  value={credentials.password}
                  onChange={e => setCredentials(c => ({ ...c, password: e.target.value }))}
                  ref={passwordRef}
                  autoComplete="current-password"
                />
              </div>

              {error && (
                <p style={{ color: 'var(--color-danger)', fontSize: 13, textAlign: 'center' }}>
                  {error}
                </p>
              )}

              <button
                className="btn btn-primary btn-lg"
                type="submit"
                disabled={cargando || !credentials.usuario || !credentials.password}
                style={{ marginTop: 4, width: '100%', justifyContent: 'center' }}
              >
                {cargando ? 'Verificando...' : 'Entrar'}
              </button>
            </form>
          )}
        </div>

        {/* Footer */}
        <p style={{ color: 'var(--color-text-dim)', fontSize: 11 }}>
          v0.1.0 — Fase 1 MVP
        </p>
      </div>
    </div>
  );
}

// ─── Sub-componentes ──────────────────────────────────────

function TabBtn({ active, onClick, children }: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      style={{
        flex: 1,
        padding: '7px 12px',
        borderRadius: 7,
        border: 'none',
        cursor: 'pointer',
        fontSize: 13,
        fontWeight: 600,
        transition: 'all 0.15s',
        background: active ? 'var(--color-primary)' : 'transparent',
        color: active ? '#fff' : 'var(--color-text-muted)',
      }}
    >
      {children}
    </button>
  );
}

function PinMode({ pin, pinError, onDigit, onDelete, onClear, cargando, error }: {
  pin: string;
  pinError: boolean;
  onDigit: (d: string) => void;
  onDelete: () => void;
  onClear: () => void;
  cargando: boolean;
  error: string | null;
}) {
  const digits = ['1', '2', '3', '4', '5', '6', '7', '8', '9', 'C', '0', '⌫'];

  return (
    <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 20 }}>
      {/* Indicador de PIN ingresado */}
      <div className={clsx(pinError && 'animate-pin-shake')} style={{ display: 'flex', gap: 12 }}>
        {[0, 1, 2, 3].map(i => (
          <div key={i} style={{
            width: 16,
            height: 16,
            borderRadius: '50%',
            border: `2px solid ${pinError ? 'var(--color-danger)' : 'var(--color-border-2)'}`,
            background: i < pin.length
              ? (pinError ? 'var(--color-danger)' : 'var(--color-primary)')
              : 'transparent',
            transition: 'all 0.1s',
          }} />
        ))}
      </div>

      {/* Error */}
      {(error || pinError) && (
        <p style={{ color: 'var(--color-danger)', fontSize: 13, margin: '-8px 0' }}>
          {error || 'PIN incorrecto'}
        </p>
      )}

      {/* Teclado numérico */}
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(3, 1fr)', gap: 10 }}>
        {digits.map(d => (
          <button
            key={d}
            className="pin-key"
            onClick={() => {
              if (d === '⌫') onDelete();
              else if (d === 'C') onClear();
              else if (pin.length < 4) onDigit(d);
            }}
            disabled={cargando}
            style={{
              color: d === 'C' ? 'var(--color-warning)' : d === '⌫' ? 'var(--color-text-muted)' : undefined,
            }}
          >
            {d}
          </button>
        ))}
      </div>

      {cargando && (
        <p className="animate-pulse-soft" style={{ color: 'var(--color-text-muted)', fontSize: 13 }}>
          Verificando...
        </p>
      )}

      <p style={{ color: 'var(--color-text-dim)', fontSize: 11, textAlign: 'center' }}>
        Ingresa tu PIN de 4 dígitos<br/>o usa el teclado numérico físico
      </p>
    </div>
  );
}
