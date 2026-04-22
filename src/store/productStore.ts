// store/productStore.ts — Estado global de productos (Zustand)

import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import MiniSearch from 'minisearch';

export interface Producto {
  id: number;
  codigo: string;
  codigo_tipo: string;
  nombre: string;
  descripcion: string | null;
  categoria_id: number | null;
  categoria_nombre: string | null;
  precio_costo: number;
  precio_venta: number;
  stock_actual: number;
  stock_minimo: number;
  proveedor_id: number | null;
  proveedor_nombre: string | null;
  foto_url: string | null;
  activo: boolean;
}

export interface Categoria {
  id: number;
  nombre: string;
  descripcion: string | null;
}

export interface Proveedor {
  id: number;
  nombre: string;
  contacto: string | null;
  telefono: string | null;
  email: string | null;
  notas: string | null;
}

export interface NuevoProducto {
  codigo?: string;
  codigo_tipo?: string;
  nombre: string;
  descripcion?: string;
  categoria_id?: number;
  precio_costo: number;
  precio_venta: number;
  stock_actual: number;
  stock_minimo: number;
  proveedor_id?: number;
  foto_url?: string;
}

interface ProductState {
  productos: Producto[];
  categorias: Categoria[];
  proveedores: Proveedor[];
  cargando: boolean;
  busqueda: string;

  // Getters
  productosFiltrados: () => Producto[];

  // Actions
  cargarProductos: () => Promise<void>;
  cargarCategorias: () => Promise<void>;
  cargarProveedores: () => Promise<void>;
  cargarTodo: () => Promise<void>;
  setBusqueda: (q: string) => void;
  buscarPorCodigo: (codigo: string) => Promise<Producto | null>;
  crearProducto: (producto: NuevoProducto, usuario_id: number) => Promise<Producto>;
  actualizarProducto: (producto: any, usuario_id: number) => Promise<boolean>;
}

function normalizar(s: string): string {
  return s.toLowerCase()
    .replace(/[áà]/g, 'a').replace(/[éè]/g, 'e').replace(/[íì]/g, 'i')
    .replace(/[óò]/g, 'o').replace(/[úù]/g, 'u').replace(/ñ/g, 'n');
}

let searchIndex: MiniSearch<Producto> | null = null;
let searchCache: Map<string, Producto[]> = new Map();
let cacheProductsLen = 0;

function rebuildIndex(productos: Producto[]) {
  searchIndex = new MiniSearch<Producto>({
    fields: ['codigo', 'nombre', 'descripcion', 'categoria_nombre'],
    storeFields: ['id'],
    processTerm: (term) => normalizar(term),
    searchOptions: {
      boost: { codigo: 3, nombre: 2 },
      prefix: true,
      fuzzy: 0.2,
      combineWith: 'AND',
    },
  });
  searchIndex.addAll(productos.map(p => ({
    ...p,
    descripcion: p.descripcion || '',
    categoria_nombre: p.categoria_nombre || '',
  })));
  // Invalidate cache when index rebuilds
  searchCache.clear();
  cacheProductsLen = productos.length;
}

export const useProductStore = create<ProductState>((set, get) => ({
  productos: [],
  categorias: [],
  proveedores: [],
  cargando: false,
  busqueda: '',

  productosFiltrados: () => {
    const { productos, busqueda } = get();
    const q = busqueda.trim();
    if (!q) return productos;
    if (!searchIndex) return productos;

    // Invalidate cache if products changed
    if (productos.length !== cacheProductsLen) {
      searchCache.clear();
      cacheProductsLen = productos.length;
    }

    // Return cached result if available
    const cacheKey = q.toLowerCase();
    const cached = searchCache.get(cacheKey);
    if (cached) return cached;

    const results = searchIndex.search(normalizar(q));
    const byId = new Map(productos.map(p => [p.id, p]));
    const seen = new Set<number>();
    const ordered: Producto[] = [];
    for (const r of results) {
      const id = (r as any).id as number;
      if (seen.has(id)) continue;
      const p = byId.get(id);
      if (p) { ordered.push(p); seen.add(id); }
    }

    // Keep cache small (max 20 entries) 
    if (searchCache.size > 20) searchCache.clear();
    searchCache.set(cacheKey, ordered);

    return ordered;
  },

  cargarProductos: async () => {
    set({ cargando: true });
    try {
      const productos = await invoke<Producto[]>('listar_productos');
      rebuildIndex(productos);
      set({ productos, cargando: false });
    } catch {
      set({ cargando: false });
    }
  },

  cargarCategorias: async () => {
    try {
      const categorias = await invoke<Categoria[]>('listar_categorias');
      set({ categorias });
    } catch {}
  },

  cargarProveedores: async () => {
    try {
      const proveedores = await invoke<Proveedor[]>('listar_proveedores');
      set({ proveedores });
    } catch {}
  },

  cargarTodo: async () => {
    set({ cargando: true });
    try {
      const [productos, categorias, proveedores] = await Promise.all([
        invoke<Producto[]>('listar_productos'),
        invoke<Categoria[]>('listar_categorias'),
        invoke<Proveedor[]>('listar_proveedores'),
      ]);
      rebuildIndex(productos);
      set({ productos, categorias, proveedores, cargando: false });
    } catch {
      set({ cargando: false });
    }
  },

  setBusqueda: (q) => set({ busqueda: q }),

  buscarPorCodigo: async (codigo) => {
    try {
      return await invoke<Producto | null>('obtener_producto_por_codigo', { codigo });
    } catch {
      return null;
    }
  },

  crearProducto: async (producto, usuario_id) => {
    const created = await invoke<Producto>('crear_producto', { producto, usuarioId: usuario_id });
    const productos = [...get().productos, created];
    rebuildIndex(productos);
    set({ productos });
    return created;
  },

  actualizarProducto: async (producto, usuario_id) => {
    const ok = await invoke<boolean>('actualizar_producto', { producto, usuarioId: usuario_id });
    if (ok) {
      const productos = await invoke<Producto[]>('listar_productos');
      rebuildIndex(productos);
      set({ productos });
    }
    return ok;
  },
}));
