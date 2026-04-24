import { useEffect, useState } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import {
  buscarProductos, productoPorCodigo, listarProveedores, listarOrdenes, ordenDetalle,
  crearRecepcion, Producto, Proveedor, OrdenResumen, OrdenDetalle,
} from '../lib/api';
import Scanner from '../lib/scanner';

interface Linea {
  producto_id: number;
  codigo: string;
  nombre: string;
  cantidad: number;
  precio_costo: number;
  pendiente?: number;
}

export default function Recepcion() {
  const navigate = useNavigate();

  const [orden, setOrden] = useState<OrdenDetalle | null>(null);
  const [ordenes, setOrdenes] = useState<OrdenResumen[]>([]);
  const [mostrarOrdenes, setMostrarOrdenes] = useState(false);

  const [proveedores, setProveedores] = useState<Proveedor[]>([]);
  const [proveedorId, setProveedorId] = useState<number | ''>('');

  const [notas, setNotas] = useState('');
  const [lineas, setLineas] = useState<Linea[]>([]);
  const [busqueda, setBusqueda] = useState('');
  const [resultados, setResultados] = useState<Producto[]>([]);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [enviando, setEnviando] = useState(false);

  useEffect(() => {
    listarProveedores().then(setProveedores).catch(() => {});
  }, []);

  useEffect(() => {
    if (busqueda.trim().length < 2) {
      setResultados([]);
      return;
    }
    const id = setTimeout(async () => {
      try {
        const list = await buscarProductos(busqueda.trim(), proveedorId || undefined);
        setResultados(list.slice(0, 20));
      } catch {}
    }, 300);
    return () => clearTimeout(id);
  }, [busqueda, proveedorId]);

  const abrirListaOrdenes = async () => {
    try {
      const lista = await listarOrdenes(true);
      setOrdenes(lista);
      setMostrarOrdenes(true);
    } catch (e: any) {
      setError(e.message || 'Error al cargar órdenes');
    }
  };

  const vincularOrden = async (id: number) => {
    try {
      const detalle = await ordenDetalle(id);
      setOrden(detalle);
      setProveedorId(detalle.proveedor_id ?? '');
      setMostrarOrdenes(false);
      const nuevas: Linea[] = detalle.items
        .filter(it => it.pendiente > 0)
        .map(it => ({
          producto_id: it.producto_id,
          codigo: it.codigo,
          nombre: it.nombre,
          cantidad: 0,
          precio_costo: it.precio_costo,
          pendiente: it.pendiente,
        }));
      setLineas(nuevas);
    } catch (e: any) {
      setError(e.message || 'Error al cargar orden');
    }
  };

  const desvincularOrden = () => {
    setOrden(null);
    setLineas([]);
  };

  const agregarProducto = (p: Producto, cantidad = 1) => {
    setLineas(prev => {
      const i = prev.findIndex(l => l.producto_id === p.id);
      if (i >= 0) {
        const copia = [...prev];
        copia[i] = { ...copia[i], cantidad: copia[i].cantidad + cantidad };
        return copia;
      }
      return [...prev, {
        producto_id: p.id, codigo: p.codigo, nombre: p.nombre,
        cantidad, precio_costo: p.precio_costo,
      }];
    });
    setBusqueda('');
    setResultados([]);
  };

  const onScan = async (codigo: string) => {
    setScanning(false);
    try {
      const p = await productoPorCodigo(codigo);
      if (!p) {
        setError(`Código no encontrado: ${codigo}`);
        return;
      }
      if (orden) {
        const idx = lineas.findIndex(l => l.producto_id === p.id);
        if (idx >= 0) {
          setLineas(prev => {
            const copia = [...prev];
            copia[idx] = { ...copia[idx], cantidad: copia[idx].cantidad + 1 };
            return copia;
          });
        } else {
          setError(`${p.nombre} no está en la orden vinculada.`);
        }
      } else {
        agregarProducto(p);
      }
    } catch (e: any) {
      setError(e.message || 'Error al buscar código');
    }
  };

  const cambiarCantidad = (idx: number, delta: number) => {
    setLineas(prev => {
      const copia = [...prev];
      const nueva = copia[idx].cantidad + delta;
      if (nueva <= 0) {
        if (orden) { copia[idx] = { ...copia[idx], cantidad: 0 }; return copia; }
        return copia.filter((_, i) => i !== idx);
      }
      copia[idx] = { ...copia[idx], cantidad: nueva };
      return copia;
    });
  };

  const editarCostoYCantidad = (idx: number, cantidad: number, costo: number) => {
    setLineas(prev => {
      const copia = [...prev];
      copia[idx] = { ...copia[idx], cantidad, precio_costo: costo };
      return copia;
    });
  };

  const confirmar = async () => {
    const items = lineas.filter(l => l.cantidad > 0);
    if (!items.length) {
      setError('Agrega al menos un producto con cantidad.');
      return;
    }
    setEnviando(true);
    setError(null);
    try {
      await crearRecepcion({
        proveedor_id: proveedorId || null,
        orden_id: orden?.id ?? null,
        notas: notas.trim() || null,
        items: items.map(l => ({
          producto_id: l.producto_id,
          cantidad: l.cantidad,
          precio_costo: l.precio_costo,
        })),
      });
      alert('Recepción guardada.');
      navigate('/', { replace: true });
    } catch (e: any) {
      setError(e.message || 'Error al guardar');
    } finally {
      setEnviando(false);
    }
  };

  const totalItems = lineas.reduce((s, l) => s + l.cantidad, 0);

  if (scanning) {
    return <Scanner onDetected={onScan} onClose={() => setScanning(false)} />;
  }

  if (mostrarOrdenes) {
    return (
      <div className="screen">
        <div className="topbar">
          <button className="btn-back" onClick={() => setMostrarOrdenes(false)}>←</button>
          <h1>Vincular orden</h1>
        </div>
        {ordenes.length === 0 && (
          <p style={{ textAlign: 'center', color: '#6b7280' }}>No hay órdenes abiertas.</p>
        )}
        {ordenes.map(o => (
          <div key={o.id} className="card" onClick={() => vincularOrden(o.id)} style={{ cursor: 'pointer' }}>
            <div style={{ display: 'flex', justifyContent: 'space-between' }}>
              <div style={{ fontWeight: 600 }}>{o.folio}</div>
              <div style={{ fontSize: 12, color: '#6b7280' }}>{o.estado}</div>
            </div>
            <div style={{ fontSize: 13, color: '#4b5563', marginTop: 4 }}>
              {o.proveedor_nombre || 'Sin proveedor'} · {o.total_items} items
            </div>
          </div>
        ))}
      </div>
    );
  }

  return (
    <div className="screen">
      <div className="topbar">
        <Link to="/" className="btn-back">←</Link>
        <h1>Recepción</h1>
      </div>

      {error && <div className="banner-error">{error}</div>}

      <div className="card">
        {orden ? (
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
            <div>
              <div style={{ fontSize: 12, color: '#6b7280' }}>Vinculada a orden</div>
              <div style={{ fontWeight: 600 }}>{orden.folio}</div>
              <div style={{ fontSize: 12, color: '#4b5563' }}>{orden.proveedor_nombre}</div>
            </div>
            <button className="btn btn-secondary" style={{ width: 'auto', padding: '8px 12px' }} onClick={desvincularOrden}>
              Quitar
            </button>
          </div>
        ) : (
          <>
            <button className="btn btn-secondary" onClick={abrirListaOrdenes} style={{ marginBottom: 12 }}>
              Vincular a orden (opcional)
            </button>
            <label className="label">Proveedor (opcional)</label>
            <select
              className="select"
              value={proveedorId}
              onChange={e => setProveedorId(e.target.value ? Number(e.target.value) : '')}
            >
              <option value="">— Sin proveedor —</option>
              {proveedores.map(p => (
                <option key={p.id} value={p.id}>{p.nombre}</option>
              ))}
            </select>
          </>
        )}
      </div>

      <div className="card">
        <button className="btn btn-primary" onClick={() => setScanning(true)} style={{ marginBottom: 12 }}>
          📷 Escanear código
        </button>
        <input
          className="input"
          type="search"
          value={busqueda}
          onChange={e => setBusqueda(e.target.value)}
          placeholder="O busca por código o nombre..."
        />
        {resultados.length > 0 && (
          <div style={{ marginTop: 8, border: '1px solid #e5e7eb', borderRadius: 8, overflow: 'hidden' }}>
            {resultados.map(p => {
              const yaEnOrden = orden && !lineas.some(l => l.producto_id === p.id);
              if (orden && yaEnOrden) return null;
              return (
                <div
                  key={p.id}
                  className="list-item"
                  onClick={() => agregarProducto(p)}
                  style={{ cursor: 'pointer' }}
                >
                  <div className="item-info">
                    <div className="name">{p.nombre}</div>
                    <div className="code">{p.codigo} · Stock {p.stock_actual}</div>
                  </div>
                  <div style={{ fontSize: 20 }}>＋</div>
                </div>
              );
            })}
          </div>
        )}
      </div>

      <div style={{ marginBottom: 12 }}>
        {lineas.map((l, idx) => (
          <div key={l.producto_id} className="card" style={{ padding: 12, marginBottom: 8 }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 8 }}>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontWeight: 600, fontSize: 14 }}>{l.nombre}</div>
                <div style={{ fontSize: 11, color: '#6b7280', fontFamily: 'monospace' }}>
                  {l.codigo}{l.pendiente !== undefined && ` · pendiente ${l.pendiente}`}
                </div>
              </div>
            </div>
            <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
              <button
                type="button"
                onClick={() => cambiarCantidad(idx, -1)}
                style={{ width: 40, height: 40, borderRadius: 8, border: '1px solid #d1d5db', background: '#fff', fontSize: 20 }}
              >−</button>
              <input
                type="number"
                inputMode="numeric"
                className="input"
                style={{ textAlign: 'center', width: 70, padding: 8 }}
                value={l.cantidad}
                onChange={e => editarCostoYCantidad(idx, Math.max(0, Number(e.target.value) || 0), l.precio_costo)}
              />
              <button
                type="button"
                onClick={() => cambiarCantidad(idx, +1)}
                style={{ width: 40, height: 40, borderRadius: 8, border: '1px solid #d1d5db', background: '#fff', fontSize: 20 }}
              >＋</button>
              <span style={{ marginLeft: 'auto', fontSize: 11, color: '#6b7280' }}>Costo</span>
              <input
                type="number"
                inputMode="decimal"
                className="input"
                style={{ width: 90, padding: 8 }}
                value={l.precio_costo}
                onChange={e => editarCostoYCantidad(idx, l.cantidad, Number(e.target.value) || 0)}
              />
            </div>
          </div>
        ))}
      </div>

      <div className="card">
        <label className="label">Notas (opcional)</label>
        <textarea
          className="input"
          rows={2}
          value={notas}
          onChange={e => setNotas(e.target.value)}
          placeholder="Observaciones..."
        />
      </div>

      <div className="status-bar">
        <span>{totalItems} items · {lineas.length} productos</span>
        <button
          className="btn btn-primary"
          style={{ width: 'auto', padding: '10px 18px' }}
          disabled={enviando || totalItems === 0}
          onClick={confirmar}
        >
          {enviando ? 'Guardando...' : 'Confirmar'}
        </button>
      </div>
    </div>
  );
}
