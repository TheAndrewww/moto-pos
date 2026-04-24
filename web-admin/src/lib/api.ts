// lib/api.ts — Cliente HTTP del panel web contra el servidor remoto.

const TOKEN_KEY = 'moto_admin_token'
const BASE_KEY  = 'moto_admin_base'

export function getToken(): string | null {
  return localStorage.getItem(TOKEN_KEY)
}

export function getBase(): string {
  // En dev, Vite proxy a :3000; en prod se usa el origen actual.
  return localStorage.getItem(BASE_KEY) ?? ''
}

export function setSession(token: string, base?: string) {
  localStorage.setItem(TOKEN_KEY, token)
  if (base) localStorage.setItem(BASE_KEY, base)
}

export function clearSession() {
  localStorage.removeItem(TOKEN_KEY)
  localStorage.removeItem(BASE_KEY)
}

async function req<T>(path: string, init: RequestInit = {}): Promise<T> {
  const token = getToken()
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...(init.headers as Record<string, string> ?? {}),
  }
  if (token) headers['Authorization'] = `Bearer ${token}`
  const r = await fetch(getBase() + path, { ...init, headers })
  if (r.status === 401) {
    clearSession()
    if (!location.pathname.startsWith('/login')) location.replace('/login')
    throw new Error('no autorizado')
  }
  if (!r.ok) {
    const txt = await r.text().catch(() => '')
    throw new Error(`${r.status}: ${txt}`)
  }
  return r.json() as Promise<T>
}

// ─── Auth ────────────────────────────────────────────────────────────────
export interface LoginOut {
  token: string
  sucursal_id: number
  nombre: string
  es_super_admin: boolean
}
export function login(email: string, password: string) {
  return req<LoginOut>('/auth/login', {
    method: 'POST',
    body: JSON.stringify({ email, password }),
  })
}

// ─── Dashboard ──────────────────────────────────────────────────────────
export interface DashboardRes {
  ventas_hoy: { total: string; cuenta: number }
  stock_bajo: number
  dispositivos: number
  ultimas_ventas: Array<{ uuid: string; folio: string; total: string; fecha: string; usuario?: string }>
}
export function dashboard(sucursal_id?: number) {
  const q = sucursal_id ? `?sucursal_id=${sucursal_id}` : ''
  return req<DashboardRes>(`/api/dashboard${q}`)
}

// ─── Productos ──────────────────────────────────────────────────────────
export interface Producto {
  id: number
  uuid: string
  codigo: string
  codigo_tipo: string
  nombre: string
  descripcion?: string
  categoria_id?: number
  precio_costo: string
  precio_venta: string
  stock_actual: string
  stock_minimo: string
  proveedor_id?: number
  activo?: number | boolean
}
export function listarProductos(q = '', limit = 100, offset = 0) {
  const p = new URLSearchParams()
  if (q) p.set('q', q)
  p.set('limit', String(limit))
  p.set('offset', String(offset))
  return req<{ items: Producto[]; count: number }>(`/api/productos?${p}`)
}
export function crearProducto(data: Partial<Producto>) {
  return req<Producto>('/api/productos', {
    method: 'POST', body: JSON.stringify(data),
  })
}
export function actualizarProducto(uuid: string, data: Partial<Producto>) {
  return req<Producto>(`/api/productos/${uuid}`, {
    method: 'PUT', body: JSON.stringify(data),
  })
}
export function eliminarProducto(uuid: string) {
  return req<{ ok: boolean }>(`/api/productos/${uuid}`, { method: 'DELETE' })
}

// ─── Ventas ─────────────────────────────────────────────────────────────
export interface VentaRow {
  uuid: string; folio: string; total: string; metodo_pago: string
  anulada: number; fecha: string; usuario?: string; cliente?: string
  sucursal_id: number
}
export function listarVentas(sucursal_id?: number, limit = 50, offset = 0) {
  const p = new URLSearchParams()
  if (sucursal_id) p.set('sucursal_id', String(sucursal_id))
  p.set('limit', String(limit))
  p.set('offset', String(offset))
  return req<{ items: VentaRow[] }>(`/api/ventas?${p}`)
}
export function detalleVenta(uuid: string) {
  return req<{ venta: any; detalle: any[] }>(`/api/ventas/${uuid}`)
}

// ─── Catálogos auxiliares ───────────────────────────────────────────────
export function listarCategorias() {
  return req<{ items: any[] }>('/api/categorias')
}
export function listarProveedores() {
  return req<{ items: any[] }>('/api/proveedores')
}
export function listarClientes(q = '') {
  const s = q ? `?q=${encodeURIComponent(q)}` : ''
  return req<{ items: any[] }>(`/api/clientes${s}`)
}
export function listarSucursales() {
  return req<{ items: any[] }>('/api/sucursales')
}
