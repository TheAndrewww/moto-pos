import { useEffect, useState } from 'react';
import { Link } from 'react-router-dom';
import { buscarProductos, Producto } from '../lib/api';

export default function Consulta() {
  const [q, setQ] = useState('');
  const [resultados, setResultados] = useState<Producto[]>([]);
  const [cargando, setCargando] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (q.trim().length < 2) {
      setResultados([]);
      return;
    }
    const id = setTimeout(async () => {
      setCargando(true);
      setError(null);
      try {
        const list = await buscarProductos(q.trim());
        setResultados(list);
      } catch (e: any) {
        setError(e.message || 'Error al buscar');
      } finally {
        setCargando(false);
      }
    }, 300);
    return () => clearTimeout(id);
  }, [q]);

  return (
    <div className="screen">
      <div className="topbar">
        <Link to="/" className="btn-back">←</Link>
        <h1>Consulta de productos</h1>
      </div>

      <div className="card">
        <input
          className="input"
          type="search"
          value={q}
          onChange={e => setQ(e.target.value)}
          placeholder="Código o nombre..."
          autoFocus
        />
      </div>

      {error && <div className="banner-error">{error}</div>}
      {cargando && <p style={{ textAlign: 'center', color: '#6b7280' }}>Buscando...</p>}

      {!cargando && q.trim().length >= 2 && resultados.length === 0 && !error && (
        <p style={{ textAlign: 'center', color: '#6b7280' }}>Sin resultados.</p>
      )}

      {resultados.map(p => (
        <div key={p.id} className="card" style={{ marginBottom: 8 }}>
          <div style={{ fontSize: 15, fontWeight: 600, marginBottom: 4 }}>{p.nombre}</div>
          <div style={{ fontSize: 12, color: '#6b7280', fontFamily: 'monospace', marginBottom: 10 }}>
            {p.codigo}
          </div>
          <div style={{ display: 'flex', justifyContent: 'space-between', gap: 12 }}>
            <div>
              <div style={{ fontSize: 11, color: '#6b7280' }}>Stock</div>
              <div style={{ fontSize: 20, fontWeight: 700, color: p.stock_actual > 0 ? '#059669' : '#dc2626' }}>
                {p.stock_actual}
              </div>
            </div>
            <div style={{ textAlign: 'right' }}>
              <div style={{ fontSize: 11, color: '#6b7280' }}>Precio venta</div>
              <div style={{ fontSize: 20, fontWeight: 700 }}>
                ${p.precio_venta.toFixed(2)}
              </div>
            </div>
          </div>
          {p.proveedor_nombre && (
            <div style={{ fontSize: 11, color: '#6b7280', marginTop: 8 }}>
              Proveedor: {p.proveedor_nombre}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
