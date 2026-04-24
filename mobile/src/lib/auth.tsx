import { Navigate, useLocation } from 'react-router-dom';
import { ReactNode } from 'react';

const KEY_JWT = 'jwt';
const KEY_DEVICE = 'device_id';
const KEY_DEVICE_NAME = 'device_name';
const KEY_USUARIO = 'usuario';

export function setSession(jwt: string, device_id: number, device_name: string, usuario: object) {
  localStorage.setItem(KEY_JWT, jwt);
  localStorage.setItem(KEY_DEVICE, String(device_id));
  localStorage.setItem(KEY_DEVICE_NAME, device_name);
  localStorage.setItem(KEY_USUARIO, JSON.stringify(usuario));
}

export function clearSession() {
  localStorage.removeItem(KEY_JWT);
  localStorage.removeItem(KEY_USUARIO);
  // mantenemos device_id / device_name para re-login sin QR
}

export function getJwt() { return localStorage.getItem(KEY_JWT); }
export function getDeviceId(): number | null {
  const v = localStorage.getItem(KEY_DEVICE);
  return v ? Number(v) : null;
}
export function getDeviceName() { return localStorage.getItem(KEY_DEVICE_NAME) ?? ''; }
export function getUsuario(): { id: number; nombre_completo: string; rol: string; es_admin: boolean } | null {
  const raw = localStorage.getItem(KEY_USUARIO);
  return raw ? JSON.parse(raw) : null;
}

export function RequireAuth({ children }: { children: ReactNode }) {
  const loc = useLocation();
  if (!getJwt()) {
    const destino = getDeviceId() ? '/login' : '/pairing';
    return <Navigate to={destino} replace state={{ from: loc }} />;
  }
  return <>{children}</>;
}
