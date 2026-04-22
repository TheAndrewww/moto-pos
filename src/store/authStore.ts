// store/authStore.ts — Estado global de autenticación (Zustand)

import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';

export interface Permiso {
  modulo: string;
  accion: string;
  permitido: boolean;
}

export interface UsuarioSesion {
  id: number;
  nombre_completo: string;
  nombre_usuario: string;
  rol_id: number;
  rol_nombre: string;
  es_admin: boolean;
  sesion_id: number;
  permisos: Permiso[];
}

interface AuthState {
  usuario: UsuarioSesion | null;
  cargando: boolean;
  error: string | null;

  // Verificar si el usuario tiene un permiso específico
  tienePermiso: (modulo: string, accion: string) => boolean;

  // Acciones
  loginPin: (pin: string) => Promise<boolean>;
  loginPassword: (usuario: string, password: string) => Promise<boolean>;
  logout: () => Promise<void>;
  verificarPinDueno: (pin: string) => Promise<boolean>;
  crearUsuarioInicial: (datos: {
    nombre_completo: string;
    nombre_usuario: string;
    pin: string;
    password: string;
  }) => Promise<boolean>;
  limpiarError: () => void;
}

export const useAuthStore = create<AuthState>((set, get) => ({
  usuario: null,
  cargando: false,
  error: null,

  tienePermiso: (modulo, accion) => {
    const { usuario } = get();
    if (!usuario) return false;
    if (usuario.es_admin) return true; // dueño tiene todo
    return usuario.permisos.some(
      p => p.modulo === modulo && p.accion === accion && p.permitido
    );
  },

  loginPin: async (pin) => {
    set({ cargando: true, error: null });
    try {
      const result = await invoke<{ ok: boolean; usuario?: UsuarioSesion; error?: string }>(
        'login_pin', { pin }
      );
      if (result.ok && result.usuario) {
        set({ usuario: result.usuario, cargando: false, error: null });
        return true;
      } else {
        set({ cargando: false, error: result.error || 'PIN incorrecto' });
        return false;
      }
    } catch (e) {
      set({ cargando: false, error: 'Error de conexión con el sistema' });
      return false;
    }
  },

  loginPassword: async (nombre_usuario, password) => {
    set({ cargando: true, error: null });
    try {
      const result = await invoke<{ ok: boolean; usuario?: UsuarioSesion; error?: string }>(
        'login_password', { nombreUsuario: nombre_usuario, password }
      );
      if (result.ok && result.usuario) {
        set({ usuario: result.usuario, cargando: false, error: null });
        return true;
      } else {
        set({ cargando: false, error: result.error || 'Credenciales incorrectas' });
        return false;
      }
    } catch (e) {
      set({ cargando: false, error: 'Error de sistema' });
      return false;
    }
  },

  logout: async () => {
    const { usuario } = get();
    if (usuario) {
      await invoke('logout', {
        usuarioId: usuario.id,
        sesionId: usuario.sesion_id,
        nombreUsuario: usuario.nombre_usuario,
      }).catch(() => {});
    }
    set({ usuario: null, error: null });
  },

  verificarPinDueno: async (pin) => {
    try {
      return await invoke<boolean>('verificar_pin_dueno', { pin });
    } catch {
      return false;
    }
  },

  crearUsuarioInicial: async (datos) => {
    set({ cargando: true, error: null });
    try {
      await invoke('crear_usuario_inicial', datos);
      set({ cargando: false });
      return true;
    } catch (e: any) {
      set({ cargando: false, error: e?.toString() || 'Error al crear usuario' });
      return false;
    }
  },

  limpiarError: () => set({ error: null }),
}));
