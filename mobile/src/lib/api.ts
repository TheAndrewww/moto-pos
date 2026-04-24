// api.ts — Cliente HTTP del servidor LAN del POS.
// En producción, el PWA se sirve desde el mismo POS, así que usamos rutas relativas.
// En dev standalone, se puede apuntar a otro POS con VITE_API_BASE.

const BASE = import.meta.env.VITE_API_BASE ?? '';

export interface Producto {
  id: number;
  codigo: string;
  nombre: string;
  stock_actual: number;
  precio_costo: number;
  precio_venta: number;
  proveedor_id: number | null;
  proveedor_nombre: string | null;
}

export interface Proveedor { id: number; nombre: string; }

export interface OrdenResumen {
  id: number;
  folio: string;
  proveedor_id: number | null;
  proveedor_nombre: string | null;
  estado: string;
  fecha_pedido: string;
  total_items: number;
}

export interface OrdenItem {
  producto_id: number;
  codigo: string;
  nombre: string;
  cantidad_pedida: number;
  cantidad_recibida: number;
  pendiente: number;
  precio_costo: number;
}

export interface OrdenDetalle {
  id: number;
  folio: string;
  proveedor_id: number | null;
  proveedor_nombre: string | null;
  estado: string;
  fecha_pedido: string;
  items: OrdenItem[];
}

export interface AuthResponse {
  jwt: string;
  device_id: number;
  usuario: {
    id: number;
    nombre_completo: string;
    nombre_usuario: string;
    rol: string;
    es_admin: boolean;
  };
}

function authHeaders(): HeadersInit {
  const jwt = localStorage.getItem('jwt');
  return jwt ? { Authorization: `Bearer ${jwt}` } : {};
}

async function handle<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const body = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(body.error || `HTTP ${res.status}`);
  }
  return res.json();
}

export async function health(): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/api/health`);
    return r.ok;
  } catch { return false; }
}

export async function pairingRedeem(
  token: string, pin: string, device_name: string,
): Promise<AuthResponse> {
  const r = await fetch(`${BASE}/api/pairing/redeem`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ token, pin, device_name, user_agent: navigator.userAgent }),
  });
  return handle<AuthResponse>(r);
}

export async function loginPin(pin: string, device_id: number): Promise<AuthResponse> {
  const r = await fetch(`${BASE}/api/auth/login_pin`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ pin, device_id }),
  });
  return handle<AuthResponse>(r);
}

export async function me() {
  const r = await fetch(`${BASE}/api/me`, { headers: authHeaders() });
  return handle(r);
}

export async function buscarProductos(q: string, proveedor_id?: number): Promise<Producto[]> {
  const params = new URLSearchParams();
  if (q) params.set('q', q);
  if (proveedor_id) params.set('proveedor_id', String(proveedor_id));
  const r = await fetch(`${BASE}/api/productos?${params}`, { headers: authHeaders() });
  return handle<Producto[]>(r);
}

export async function productoPorCodigo(codigo: string): Promise<Producto | null> {
  const r = await fetch(`${BASE}/api/productos/por_codigo/${encodeURIComponent(codigo)}`, { headers: authHeaders() });
  if (r.status === 404) return null;
  return handle<Producto>(r);
}

export async function listarProveedores(): Promise<Proveedor[]> {
  const r = await fetch(`${BASE}/api/proveedores`, { headers: authHeaders() });
  return handle<Proveedor[]>(r);
}

export async function listarOrdenes(abiertas = true): Promise<OrdenResumen[]> {
  const q = abiertas ? '?abiertas=1' : '';
  const r = await fetch(`${BASE}/api/ordenes_pedido${q}`, { headers: authHeaders() });
  return handle<OrdenResumen[]>(r);
}

export async function ordenDetalle(id: number): Promise<OrdenDetalle> {
  const r = await fetch(`${BASE}/api/ordenes_pedido/${id}`, { headers: authHeaders() });
  return handle<OrdenDetalle>(r);
}

export interface RecepcionBody {
  proveedor_id: number | null;
  orden_id: number | null;
  notas: string | null;
  items: { producto_id: number; cantidad: number; precio_costo: number }[];
}

export async function crearRecepcion(body: RecepcionBody): Promise<{ id: number; total_items: number }> {
  const r = await fetch(`${BASE}/api/recepciones`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...authHeaders() },
    body: JSON.stringify(body),
  });
  return handle(r);
}
