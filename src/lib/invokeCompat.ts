// invokeCompat — adaptador transparente para los comandos del POS.
//
// En Tauri (app de escritorio): pasa-through directo al `invoke` nativo.
//   Cero overhead, cero cambios de comportamiento vs código legacy.
//
// En navegador (versión web del POS): traduce a HTTP POST /rpc/{cmd}
//   contra el server-remoto, con Bearer token del login web.
//
// Uso (drop-in replacement de '@tauri-apps/api/core'):
//     import { invoke } from '../lib/invokeCompat';
//     await invoke<Producto[]>('listar_productos');

import type { InvokeArgs } from '@tauri-apps/api/core';

/**
 * Detecta si el bundle corre dentro de Tauri (ventana nativa).
 * Tauri 2 expone `window.__TAURI_INTERNALS__`.
 */
export function isTauri(): boolean {
  return typeof window !== 'undefined' &&
    typeof (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !== 'undefined';
}

/**
 * Base URL del server-remoto cuando corre en navegador.
 * Configurable via Vite env `VITE_API_BASE` (ej. https://moto.up.railway.app).
 * Default: mismo origen que la SPA (útil si el server sirve los estáticos).
 */
function apiBase(): string {
  const env = (import.meta as unknown as { env?: Record<string, string> }).env;
  return env?.VITE_API_BASE || (typeof window !== 'undefined' ? window.location.origin : '');
}

const TOKEN_KEY = 'moto_token';
const DEVICE_KEY = 'moto_device_uuid';

export function setAuthToken(token: string | null): void {
  if (token) localStorage.setItem(TOKEN_KEY, token);
  else localStorage.removeItem(TOKEN_KEY);
}

export function getAuthToken(): string | null {
  if (typeof localStorage === 'undefined') return null;
  return localStorage.getItem(TOKEN_KEY);
}

/**
 * Identifica de manera estable a esta instancia del navegador (web POS).
 * Solo aplica en modo web — en Tauri devuelve null (el desktop ya tiene
 * su propio sistema de identidad).
 *
 * Generamos un UUID v4 la primera vez y lo guardamos en localStorage.
 * Persistente entre recargas. Si el usuario borra storage, se genera uno
 * nuevo y `pos_devices` lo trata como dispositivo nuevo (le pide configurar
 * su modo de caja con el modal de bienvenida).
 */
export function getOrCreateDeviceUuid(): string | null {
  if (isTauri()) return null;
  if (typeof localStorage === 'undefined') return null;

  const existing = localStorage.getItem(DEVICE_KEY);
  if (existing) return existing;

  // crypto.randomUUID() requiere contexto seguro (HTTPS o localhost).
  // Como fallback hacemos un v4 manual con Math.random — suficiente para
  // identificar dispositivos (no es input de seguridad).
  const uuid =
    typeof crypto !== 'undefined' && 'randomUUID' in crypto
      ? crypto.randomUUID()
      : 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, (c) => {
          const r = (Math.random() * 16) | 0;
          const v = c === 'x' ? r : (r & 0x3) | 0x8;
          return v.toString(16);
        });
  localStorage.setItem(DEVICE_KEY, uuid);
  return uuid;
}

/**
 * Drop-in replacement de `invoke` de Tauri.
 *
 * En Tauri:  delega al invoke nativo (importado dinámicamente para que el
 *            bundle web no jale el módulo Tauri).
 * En web:    POST {base}/rpc/{cmd} con body JSON = args.
 */
export async function invoke<T = unknown>(
  cmd: string,
  args?: InvokeArgs,
): Promise<T> {
  if (isTauri()) {
    // Import dinámico: así el bundle web puede excluir '@tauri-apps/api' si quiere.
    const mod = await import('@tauri-apps/api/core');
    return mod.invoke<T>(cmd, args);
  }
  return rpcWeb<T>(cmd, args);
}

async function rpcWeb<T>(cmd: string, args?: InvokeArgs): Promise<T> {
  const url = `${apiBase()}/rpc/${cmd}`;
  const token = getAuthToken();
  const res = await fetch(url, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: JSON.stringify(args ?? {}),
  });
  if (!res.ok) {
    // Token expirado o inválido → limpiar sesión y forzar login
    if (res.status === 401 && cmd !== 'login_pin' && cmd !== 'login_password') {
      setAuthToken(null);
      localStorage.removeItem('moto_usuario');
      window.location.reload();
      // Never resolves — the page reloads
      return new Promise(() => {}) as T;
    }
    const text = await res.text().catch(() => '');
    throw new Error(`RPC ${cmd} failed: ${res.status} ${text || res.statusText}`);
  }
  // El server puede devolver null, número, string, array u objeto.
  // Preservar el tipo de retorno que espera el cliente.
  const ct = res.headers.get('content-type') ?? '';
  if (ct.includes('application/json')) {
    return (await res.json()) as T;
  }
  const text = await res.text();
  return (text === '' ? (null as T) : (text as unknown as T));
}
