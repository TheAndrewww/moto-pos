// pages/Catalogo.tsx — Catálogo de productos con vista de tarjetas
// Diseño modular con cards, filtros y formulario de alta/edición

import { useState, useEffect, useRef } from 'react';
import { useProductStore, type Producto, type NuevoProducto } from '../store/productStore';
import { useAuthStore } from '../store/authStore';
import { Package, Plus, Search, Edit2, X, AlertTriangle, Tag, Hash, LayoutGrid, List, Upload } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useVirtualizer } from '@tanstack/react-virtual';

export default function Catalogo() {
  const {
    productosFiltrados, cargarTodo, busqueda, setBusqueda,
    categorias, proveedores, crearProducto, actualizarProducto,
  } = useProductStore();
  const { usuario, tienePermiso } = useAuthStore();

  const [showForm, setShowForm] = useState(false);
  const [showImportar, setShowImportar] = useState(false);
  const [editando, setEditando] = useState<Producto | null>(null);
  const [filtroCategoria, setFiltroCategoria] = useState<number | null>(null);
  const [filtroStock, setFiltroStock] = useState<'todos' | 'bajo' | 'cero'>('todos');
  const [vista, setVista] = useState<'grid' | 'lista'>('grid');
  const [localBusqueda, setLocalBusqueda] = useState(busqueda);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // React-virtual state
  const parentRef = useRef<HTMLDivElement>(null);
  const [columns, setColumns] = useState(1);

  useEffect(() => { cargarTodo(); }, []);

  // Compute responsive columns for grid virtualization
  useEffect(() => {
    if (!parentRef.current) return;
    const ro = new ResizeObserver((entries) => {
      if (vista === 'lista') {
        if (columns !== 1) setColumns(1);
        return;
      }
      for (let entry of entries) {
        // We use 260px as min-width for our grid item + 12px gap => 272
        const width = entry.contentRect.width - 32; // subtract 16px padding on left/right
        const newCols = Math.max(1, Math.floor((width + 12) / 272));
        if (newCols !== columns) setColumns(newCols);
      }
    });
    ro.observe(parentRef.current);
    return () => ro.disconnect();
  }, [columns, vista]);

  const esAdmin = usuario?.es_admin ?? false;
  const puedeEditar = esAdmin || tienePermiso('inventario', 'editar');
  const puedeCrear = esAdmin || tienePermiso('inventario', 'crear');

  // Filtrar productos
  let listaFinal = productosFiltrados();
  if (filtroCategoria) {
    listaFinal = listaFinal.filter(p => p.categoria_id === filtroCategoria);
  }
  if (filtroStock === 'bajo') {
    listaFinal = listaFinal.filter(p => p.stock_actual <= p.stock_minimo && p.stock_actual > 0);
  } else if (filtroStock === 'cero') {
    listaFinal = listaFinal.filter(p => p.stock_actual <= 0);
  }

  const fmt = (n: number) => `$${n.toFixed(2)}`;
  const totalProductos = productosFiltrados().length;
  const stockBajoCount = productosFiltrados().filter(p => p.stock_actual <= p.stock_minimo && p.stock_actual > 0).length;
  const sinStockCount = productosFiltrados().filter(p => p.stock_actual <= 0).length;

  const rowCount = Math.ceil(listaFinal.length / columns);
  const virtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => parentRef.current,
    estimateSize: () => vista === 'lista' ? 56 : 210, // 56px for list, 210px for grid
    overscan: 3,
  });

  // ──── Form ────

  const FormProducto = () => {
    const [form, setForm] = useState({
      nombre: editando?.nombre || '',
      descripcion: editando?.descripcion || '',
      categoria_id: editando?.categoria_id || '',
      codigo: editando?.codigo || '',
      codigo_tipo: editando?.codigo_tipo || 'INTERNO',
      precio_costo: editando?.precio_costo || 0,
      precio_venta: editando?.precio_venta || 0,
      stock_actual: editando?.stock_actual || 0,
      stock_minimo: editando?.stock_minimo || 0,
      proveedor_id: editando?.proveedor_id || '',
    });
    const [guardando, setGuardando] = useState(false);
    const [error, setError] = useState('');

    const handleSubmit = async (e: React.FormEvent) => {
      e.preventDefault();
      if (!form.nombre.trim()) return setError('El nombre es obligatorio');
      if (!usuario) return;

      setGuardando(true);
      setError('');

      try {
        if (editando) {
          await actualizarProducto({
            id: editando.id,
            nombre: form.nombre,
            descripcion: form.descripcion || null,
            categoria_id: form.categoria_id ? Number(form.categoria_id) : null,
            precio_costo: Number(form.precio_costo),
            precio_venta: Number(form.precio_venta),
            stock_minimo: Number(form.stock_minimo),
            proveedor_id: form.proveedor_id ? Number(form.proveedor_id) : null,
            foto_url: null,
          }, usuario.id);
        } else {
          const nuevo: NuevoProducto = {
            nombre: form.nombre,
            descripcion: form.descripcion || undefined,
            categoria_id: form.categoria_id ? Number(form.categoria_id) : undefined,
            codigo: form.codigo || undefined,
            codigo_tipo: form.codigo_tipo || undefined,
            precio_costo: Number(form.precio_costo),
            precio_venta: Number(form.precio_venta),
            stock_actual: Number(form.stock_actual),
            stock_minimo: Number(form.stock_minimo),
            proveedor_id: form.proveedor_id ? Number(form.proveedor_id) : undefined,
          };
          await crearProducto(nuevo, usuario.id);
        }
        setShowForm(false);
        setEditando(null);
      } catch (err: any) {
        setError(err?.toString() || 'Error al guardar');
      }
      setGuardando(false);
    };

    return (
      <div style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)',
        display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 100,
      }} onClick={() => { setShowForm(false); setEditando(null); }}>
        <div className="card animate-fade-in" style={{ width: 520, maxHeight: '85vh', overflow: 'auto', padding: 24 }}
          onClick={(e) => e.stopPropagation()}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 20 }}>
            <h2 style={{ fontSize: 18, fontWeight: 700 }}>
              {editando ? '✏️ Editar Producto' : '➕ Nuevo Producto'}
            </h2>
            <button className="btn btn-ghost btn-sm" onClick={() => { setShowForm(false); setEditando(null); }}>
              <X size={18} />
            </button>
          </div>

          <form onSubmit={handleSubmit} style={{ display: 'flex', flexDirection: 'column', gap: 14 }}>
            {/* Nombre */}
            <div>
              <label style={labelStyle}>NOMBRE *</label>
              <input className="input" value={form.nombre}
                onChange={e => setForm(f => ({ ...f, nombre: e.target.value }))}
                placeholder="Nombre del producto" autoFocus />
            </div>

            {/* Código + Categoría */}
            <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
              <div>
                <label style={labelStyle}>CÓDIGO {!editando && '(vacío = autogenerar MR-XXXXX)'}</label>
                <input className="input" value={form.codigo}
                  onChange={e => setForm(f => ({ ...f, codigo: e.target.value }))}
                  placeholder="Escanear o dejar vacío"
                  disabled={!!editando} />
              </div>
              <div>
                <label style={labelStyle}>CATEGORÍA</label>
                <select className="input" value={form.categoria_id}
                  onChange={e => setForm(f => ({ ...f, categoria_id: e.target.value }))}>
                  <option value="">Sin categoría</option>
                  {categorias.map(c => <option key={c.id} value={c.id}>{c.nombre}</option>)}
                </select>
              </div>
            </div>

            {/* Descripción */}
            <div>
              <label style={labelStyle}>DESCRIPCIÓN</label>
              <input className="input" value={form.descripcion}
                onChange={e => setForm(f => ({ ...f, descripcion: e.target.value }))}
                placeholder="Descripción opcional" />
            </div>

            {/* Precios */}
            <div style={{ background: 'var(--color-surface-2)', padding: 14, borderRadius: 10 }}>
              <p style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text-muted)', marginBottom: 10 }}>PRECIOS</p>
              {esAdmin && (
                <div style={{ marginBottom: 10 }}>
                  <label style={labelStyle}>💰 COSTO</label>
                  <input className="input mono" type="number" step="0.01" value={form.precio_costo}
                    onChange={e => setForm(f => ({ ...f, precio_costo: parseFloat(e.target.value) || 0 }))} />
                </div>
              )}
              <div>
                <label style={{ ...labelStyle, color: 'var(--color-primary)' }}>PRECIO DE VENTA *</label>
                <input className="input mono" type="number" step="0.01" value={form.precio_venta}
                  onChange={e => setForm(f => ({ ...f, precio_venta: parseFloat(e.target.value) || 0 }))} />
              </div>
            </div>

            {/* Stock */}
            <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr 1fr', gap: 12 }}>
              {!editando && (
                <div>
                  <label style={labelStyle}>STOCK INICIAL</label>
                  <input className="input mono" type="number" step="1" value={form.stock_actual}
                    onChange={e => setForm(f => ({ ...f, stock_actual: parseFloat(e.target.value) || 0 }))} />
                </div>
              )}
              <div>
                <label style={labelStyle}>STOCK MÍNIMO</label>
                <input className="input mono" type="number" step="1" value={form.stock_minimo}
                  onChange={e => setForm(f => ({ ...f, stock_minimo: parseFloat(e.target.value) || 0 }))} />
              </div>
              <div>
                <label style={labelStyle}>PROVEEDOR</label>
                <select className="input" value={form.proveedor_id}
                  onChange={e => setForm(f => ({ ...f, proveedor_id: e.target.value }))}>
                  <option value="">Sin proveedor</option>
                  {proveedores.map(p => <option key={p.id} value={p.id}>{p.nombre}</option>)}
                </select>
              </div>
            </div>

            {error && (
              <p style={{ color: 'var(--color-danger)', fontSize: 13 }}>{error}</p>
            )}

            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', marginTop: 4 }}>
              <button type="button" className="btn btn-ghost"
                onClick={() => { setShowForm(false); setEditando(null); }}>
                Cancelar
              </button>
              <button type="submit" className="btn btn-primary" disabled={guardando}>
                {guardando ? 'Guardando...' : editando ? 'Guardar Cambios' : 'Crear Producto'}
              </button>
            </div>
          </form>
        </div>
      </div>
    );
  };

  // ──── Render ────

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      {/* ─── Header toolbar ─── */}
      <div style={{
        padding: '12px 20px',
        borderBottom: '1px solid var(--color-border)',
        background: 'var(--color-surface)',
        display: 'flex', flexDirection: 'column', gap: 10,
      }}>
        {/* Title + button */}
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
            <Package size={20} style={{ color: 'var(--color-primary)' }} />
            <h2 style={{ fontSize: 17, fontWeight: 800, color: 'var(--color-text)' }}>Inventario</h2>
            <span style={{ fontSize: 12, color: 'var(--color-text-dim)', marginLeft: 4 }}>·</span>
            <span style={{ fontSize: 12, color: 'var(--color-text-muted)' }}>{totalProductos} productos</span>
            <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>·</span>
            <span style={{ fontSize: 12, color: stockBajoCount > 0 ? 'var(--color-warning)' : 'var(--color-text-dim)' }}>
              ⚠️ {stockBajoCount} stock bajo
            </span>
            <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>·</span>
            <span style={{ fontSize: 12, color: sinStockCount > 0 ? 'var(--color-danger)' : 'var(--color-text-dim)' }}>
              🔴 {sinStockCount} sin stock
            </span>
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            {esAdmin && (
              <button className="btn btn-secondary" onClick={() => setShowImportar(true)} title="Importar catálogo CSV">
                <Upload size={16} /> Importar
              </button>
            )}
            {puedeCrear && (
              <button className="btn btn-primary" onClick={() => { setEditando(null); setShowForm(true); }}>
                <Plus size={16} /> Nuevo Producto
              </button>
            )}
          </div>
        </div>

        {/* Search + filters + view toggle */}
        <div style={{ display: 'flex', gap: 8 }}>
          <div style={{ position: 'relative', flex: 1 }}>
            <Search size={16} style={{
              position: 'absolute', left: 12, top: '50%', transform: 'translateY(-50%)',
              color: 'var(--color-text-dim)',
            }} />
            <input
              className="input"
              placeholder="Buscar por nombre, código, descripción..."
              value={localBusqueda}
              onChange={e => {
                const v = e.target.value;
                setLocalBusqueda(v);
                if (debounceRef.current) clearTimeout(debounceRef.current);
                debounceRef.current = setTimeout(() => setBusqueda(v), 150);
              }}
              style={{ paddingLeft: 36, width: '100%' }}
            />
          </div>
          <select
            className="input"
            value={filtroCategoria || ''}
            onChange={e => setFiltroCategoria(e.target.value ? Number(e.target.value) : null)}
            style={{ width: 180 }}
          >
            <option value="">Todas las categorías</option>
            {categorias.map(c => <option key={c.id} value={c.id}>{c.nombre}</option>)}
          </select>
          <select
            className="input"
            value={filtroStock}
            onChange={e => setFiltroStock(e.target.value as any)}
            style={{ width: 140 }}
          >
            <option value="todos">Todo el stock</option>
            <option value="bajo">⚠️ Stock bajo</option>
            <option value="cero">🔴 Sin stock</option>
          </select>

          {/* View Toggle */}
          <div style={{ display: 'flex', background: 'var(--color-surface-2)', borderRadius: 8, padding: 2, border: '1px solid var(--color-border)' }}>
            <button
              onClick={() => setVista('grid')}
              style={{
                display: 'flex', alignItems: 'center', justifyContent: 'center',
                width: 32, height: 32, borderRadius: 6, border: 'none', cursor: 'pointer',
                background: vista === 'grid' ? 'var(--color-bg)' : 'transparent',
                color: vista === 'grid' ? 'var(--color-primary)' : 'var(--color-text-muted)',
                boxShadow: vista === 'grid' ? '0 2px 4px rgba(0,0,0,0.05)' : 'none',
              }}
            >
              <LayoutGrid size={16} />
            </button>
            <button
              onClick={() => setVista('lista')}
              style={{
                display: 'flex', alignItems: 'center', justifyContent: 'center',
                width: 32, height: 32, borderRadius: 6, border: 'none', cursor: 'pointer',
                background: vista === 'lista' ? 'var(--color-bg)' : 'transparent',
                color: vista === 'lista' ? 'var(--color-primary)' : 'var(--color-text-muted)',
                boxShadow: vista === 'lista' ? '0 2px 4px rgba(0,0,0,0.05)' : 'none',
              }}
            >
              <List size={18} />
            </button>
          </div>
        </div>
      </div>

      {/* ─── Product Grid ─── */}
      <div ref={parentRef} style={{ flex: 1, overflow: 'auto', padding: 16 }}>
        {listaFinal.length === 0 ? (
          <div style={{
            display: 'flex', alignItems: 'center', justifyContent: 'center',
            height: '100%', flexDirection: 'column', gap: 12,
            color: 'var(--color-text-dim)',
          }}>
            <Package size={48} strokeWidth={1.2} />
            <p style={{ fontSize: 16, fontWeight: 600 }}>No se encontraron productos</p>
            <p style={{ fontSize: 13 }}>
              {busqueda ? `Sin resultados para "${busqueda}"` : 'Agrega tu primer producto con el botón de arriba'}
            </p>
          </div>
        ) : (
          <div style={{
            height: `${virtualizer.getTotalSize()}px`,
            width: '100%',
            position: 'relative',
          }}>
            {virtualizer.getVirtualItems().map((virtualRow) => {
              const startIndex = virtualRow.index * columns;
              const rowItems = listaFinal.slice(startIndex, startIndex + columns);

              return (
                <div
                  key={virtualRow.index}
                  style={{
                    position: 'absolute',
                    top: 0,
                    left: 0,
                    width: '100%',
                    height: `${virtualRow.size}px`,
                    transform: `translateY(${virtualRow.start}px)`,
                    display: 'grid',
                    gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))`,
                    gap: 12,
                    paddingBottom: 12,
                  }}
                >
                  {rowItems.map((p) => {
                    const stockBajo = p.stock_actual <= p.stock_minimo && p.stock_actual > 0;
                    const sinStock = p.stock_actual <= 0;

                    if (vista === 'lista') {
                      return (
                        <div
                          key={p.id}
                          style={{
                            display: 'grid',
                            gridTemplateColumns: 'minmax(80px, 1fr) 3fr 1fr 1fr auto',
                            alignItems: 'center',
                            gap: 16,
                            padding: '12px 16px',
                            background: 'var(--color-surface)',
                            borderBottom: '1px solid var(--color-border)',
                            cursor: puedeEditar ? 'pointer' : 'default',
                            transition: 'background 0.15s',
                          }}
                          onClick={() => { if (puedeEditar) { setEditando(p); setShowForm(true); } }}
                          onMouseEnter={e => (e.currentTarget.style.background = 'var(--color-surface-2)')}
                          onMouseLeave={e => (e.currentTarget.style.background = 'var(--color-surface)')}
                        >
                          <div className="mono" style={{ fontSize: 12, fontWeight: 600, color: 'var(--color-text-muted)' }}>
                            {p.codigo}
                          </div>
                          <div>
                            <div style={{ fontSize: 13, fontWeight: 700, color: 'var(--color-text)', display: '-webkit-box', WebkitLineClamp: 1, WebkitBoxOrient: 'vertical', overflow: 'hidden' }}>
                              {p.nombre}
                            </div>
                            {p.categoria_nombre && (
                              <div style={{ fontSize: 11, color: 'var(--color-text-dim)', marginTop: 2 }}>
                                {p.categoria_nombre} {p.proveedor_nombre ? `· ${p.proveedor_nombre}` : ''}
                              </div>
                            )}
                          </div>
                          <div className="mono" style={{ fontSize: 15, fontWeight: 700, color: 'var(--color-primary)' }}>
                            {fmt(p.precio_venta)}
                          </div>
                          <div>
                            <span className={`tag ${sinStock ? 'tag-danger' : stockBajo ? 'tag-warning' : 'tag-success'}`}
                              style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12, padding: '2px 8px' }}>
                              {sinStock && <AlertTriangle size={10} style={{ marginRight: 4 }} />}
                              Stock: {p.stock_actual}
                            </span>
                          </div>
                          <div>
                            {puedeEditar && (
                              <button
                                className="btn btn-ghost btn-sm"
                                style={{ padding: '6px' }}
                                onClick={(e) => { e.stopPropagation(); setEditando(p); setShowForm(true); }}
                              >
                                <Edit2 size={14} />
                              </button>
                            )}
                          </div>
                        </div>
                      );
                    }

                    // Render Grid Card (existing code)
                    return (
                      <div
                        key={p.id}
                        className="card"
                        style={{
                          padding: 0,
                          overflow: 'hidden',
                          transition: 'all 0.15s ease',
                          cursor: puedeEditar ? 'pointer' : 'default',
                          border: sinStock ? '1px solid var(--color-danger)' :
                                  stockBajo ? '1px solid var(--color-warning)' :
                                  '1px solid var(--color-border)',
                          display: 'flex',
                          flexDirection: 'column',
                        }}
                        onClick={() => { if (puedeEditar) { setEditando(p); setShowForm(true); } }}
                        onMouseEnter={e => {
                          (e.currentTarget as HTMLDivElement).style.transform = 'translateY(-2px)';
                          (e.currentTarget as HTMLDivElement).style.boxShadow = '0 6px 20px rgba(0,0,0,0.08)';
                        }}
                        onMouseLeave={e => {
                          (e.currentTarget as HTMLDivElement).style.transform = 'translateY(0)';
                          (e.currentTarget as HTMLDivElement).style.boxShadow = 'none';
                        }}
                      >
                        {/* Card Header — Código + Categoría */}
                        <div style={{
                          padding: '10px 14px',
                          borderBottom: '1px solid var(--color-border)',
                          display: 'flex',
                          alignItems: 'center',
                          justifyContent: 'space-between',
                          background: sinStock ? 'rgba(220,53,69,0.04)' :
                                      stockBajo ? 'rgba(230,187,128,0.06)' :
                                      'var(--color-surface)',
                        }}>
                          <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                            <Hash size={12} style={{ color: 'var(--color-text-dim)' }} />
                            <span className="mono" style={{ fontSize: 12, fontWeight: 600, color: 'var(--color-text-muted)' }}>
                              {p.codigo}
                            </span>
                          </div>
                          {p.categoria_nombre && (
                            <span style={{
                              fontSize: 10, padding: '2px 8px', borderRadius: 10,
                              background: 'var(--color-primary-soft)',
                              color: 'var(--color-primary)',
                              fontWeight: 600,
                            }}>
                              {p.categoria_nombre}
                            </span>
                          )}
                        </div>

                        {/* Card Body */}
                        <div style={{ padding: '12px 14px', flex: 1 }}>
                          {/* Nombre */}
                          <h3 style={{
                            fontSize: 14, fontWeight: 700, color: 'var(--color-text)',
                            marginBottom: 4,
                            lineHeight: 1.3,
                            display: '-webkit-box',
                            WebkitLineClamp: 2,
                            WebkitBoxOrient: 'vertical',
                            overflow: 'hidden',
                          }}>
                            {p.nombre}
                          </h3>

                          {p.descripcion && (
                            <p style={{
                              fontSize: 12, color: 'var(--color-text-dim)',
                              marginBottom: 8,
                              lineHeight: 1.3,
                              display: '-webkit-box',
                              WebkitLineClamp: 1,
                              WebkitBoxOrient: 'vertical',
                              overflow: 'hidden',
                            }}>
                              {p.descripcion}
                            </p>
                          )}

                          {/* Precio + Stock */}
                          <div style={{
                            display: 'flex', justifyContent: 'space-between',
                            alignItems: 'flex-end', marginTop: 10,
                          }}>
                            <div>
                              <div style={{ fontSize: 10, color: 'var(--color-text-dim)', fontWeight: 600, marginBottom: 2 }}>
                                PRECIO
                              </div>
                              <span className="mono" style={{
                                fontSize: 20, fontWeight: 800,
                                color: 'var(--color-primary)',
                              }}>
                                {fmt(p.precio_venta)}
                              </span>
                              {esAdmin && (
                                <span className="mono" style={{
                                  fontSize: 11, color: 'var(--color-text-dim)',
                                  marginLeft: 8,
                                }}>
                                  Costo: {fmt(p.precio_costo)}
                                </span>
                              )}
                            </div>

                            <div style={{ textAlign: 'right' }}>
                              <div style={{ fontSize: 10, color: 'var(--color-text-dim)', fontWeight: 600, marginBottom: 2 }}>
                                STOCK
                              </div>
                              <span className={`tag ${sinStock ? 'tag-danger' : stockBajo ? 'tag-warning' : 'tag-success'}`}
                                style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 13, padding: '3px 10px' }}>
                                {sinStock && <AlertTriangle size={11} style={{ marginRight: 4 }} />}
                                {p.stock_actual}
                              </span>
                            </div>
                          </div>
                        </div>

                        {/* Footer — Proveedor + Edit */}
                        {(p.proveedor_nombre || puedeEditar) && (
                          <div style={{
                            padding: '8px 14px',
                            borderTop: '1px solid var(--color-border)',
                            display: 'flex',
                            alignItems: 'center',
                            justifyContent: 'space-between',
                            background: 'var(--color-surface)',
                            marginTop: 'auto',
                          }}>
                            {p.proveedor_nombre ? (
                              <span style={{ fontSize: 11, color: 'var(--color-text-dim)', display: 'flex', alignItems: 'center', gap: 4 }}>
                                <Tag size={10} /> {p.proveedor_nombre}
                              </span>
                            ) : <span />}
                            {puedeEditar && (
                              <button
                                className="btn btn-ghost btn-sm"
                                style={{ padding: '3px 8px', fontSize: 11 }}
                                onClick={(e) => { e.stopPropagation(); setEditando(p); setShowForm(true); }}
                              >
                                <Edit2 size={12} /> Editar
                              </button>
                            )}
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* Modal de form */}
      {showForm && <FormProducto />}
      {showImportar && <ImportarModal onClose={() => setShowImportar(false)} onDone={() => { setShowImportar(false); cargarTodo(); }} />}
    </div>
  );
}

// ─── Modal de importación de catálogo CSV ───────────────────────
interface ResultadoImportacion {
  total_lineas: number;
  insertados: number;
  actualizados: number;
  omitidos: number;
  proveedores_creados: number;
  errores: string[];
}

function ImportarModal({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const [ruta, setRuta] = useState('/Users/andrewww/Downloads/catalogo.csv');
  const [reemplazar, setReemplazar] = useState(true);
  const [ejecutando, setEjecutando] = useState(false);
  const [resultado, setResultado] = useState<ResultadoImportacion | null>(null);
  const [error, setError] = useState('');

  const ejecutar = async () => {
    setError('');
    setResultado(null);
    setEjecutando(true);
    try {
      const res = await invoke<ResultadoImportacion>('importar_catalogo_csv', { ruta, reemplazar });
      setResultado(res);
    } catch (e) {
      setError(String(e));
    } finally {
      setEjecutando(false);
    }
  };

  return (
    <div style={{
      position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.6)', zIndex: 1000,
      display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 20,
    }}>
      <div style={{ background: 'var(--color-surface)', borderRadius: 12, padding: 24, width: 560, maxHeight: '90vh', overflowY: 'auto' }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 16 }}>
          <h3 style={{ fontSize: 18, fontWeight: 800 }}>📥 Importar catálogo CSV</h3>
          <button className="btn-icon" onClick={onClose}><X size={18} /></button>
        </div>

        <p style={{ fontSize: 13, color: 'var(--color-text-muted)', marginBottom: 12 }}>
          Formato esperado (pipe-separado, sin cabecera): <br />
          <code style={{ fontSize: 11 }}>codigo | nombre | precio_venta | stock | proveedor</code>
        </p>

        <label style={{ fontSize: 12, fontWeight: 600, display: 'block', marginBottom: 4 }}>RUTA DEL ARCHIVO</label>
        <input className="input" value={ruta} onChange={e => setRuta(e.target.value)}
          placeholder="/Users/.../catalogo.csv" disabled={ejecutando} />

        <label style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 12, cursor: 'pointer' }}>
          <input type="checkbox" checked={reemplazar} onChange={e => setReemplazar(e.target.checked)} disabled={ejecutando} />
          <span style={{ fontSize: 13 }}>
            <strong>Reemplazar todo</strong> — borra productos, proveedores y ventas de prueba antes de importar
          </span>
        </label>
        {reemplazar && (
          <div style={{ marginTop: 8, padding: 10, background: 'rgba(255,180,0,0.1)', borderRadius: 8, fontSize: 12, color: 'var(--color-warning, orange)' }}>
            ⚠️ Esta operación es irreversible. Asegúrate de tener un respaldo.
          </div>
        )}

        {error && (
          <div style={{ marginTop: 12, padding: 10, background: 'var(--color-danger-bg, #fee)', color: 'var(--color-danger)', borderRadius: 8, fontSize: 13 }}>
            {error}
          </div>
        )}

        {resultado && (
          <div style={{ marginTop: 14, padding: 12, background: 'var(--color-surface-2)', borderRadius: 8 }}>
            <p style={{ fontWeight: 700, marginBottom: 6 }}>Resultado</p>
            <p style={{ fontSize: 13 }}>• Líneas procesadas: <strong>{resultado.total_lineas}</strong></p>
            <p style={{ fontSize: 13, color: 'var(--color-success, green)' }}>• Insertados: <strong>{resultado.insertados}</strong></p>
            <p style={{ fontSize: 13, color: 'var(--color-primary)' }}>• Actualizados: <strong>{resultado.actualizados}</strong></p>
            <p style={{ fontSize: 13, color: 'var(--color-warning, orange)' }}>• Omitidos: <strong>{resultado.omitidos}</strong></p>
            <p style={{ fontSize: 13 }}>• Proveedores creados: <strong>{resultado.proveedores_creados}</strong></p>
            {resultado.errores.length > 0 && (
              <details style={{ marginTop: 8 }}>
                <summary style={{ cursor: 'pointer', fontSize: 12 }}>Ver {resultado.errores.length} errores</summary>
                <pre style={{ fontSize: 11, maxHeight: 180, overflow: 'auto', marginTop: 6 }}>{resultado.errores.join('\n')}</pre>
              </details>
            )}
          </div>
        )}

        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', marginTop: 16 }}>
          {resultado ? (
            <button className="btn btn-primary" onClick={onDone}>Cerrar</button>
          ) : (
            <>
              <button className="btn btn-secondary" onClick={onClose} disabled={ejecutando}>Cancelar</button>
              <button className="btn btn-primary" onClick={ejecutar} disabled={ejecutando || !ruta.trim()}>
                {ejecutando ? 'Importando…' : 'Importar'}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

const labelStyle: React.CSSProperties = {
  fontSize: 11,
  fontWeight: 600,
  color: 'var(--color-text-muted)',
  display: 'block',
  marginBottom: 4,
  textTransform: 'uppercase',
  letterSpacing: '0.3px',
};

