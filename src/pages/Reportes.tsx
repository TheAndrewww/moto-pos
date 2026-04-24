// pages/Reportes.tsx — Reportes avanzados con gráficas
// Ventas por día, Top 10 productos, Por vendedor, Por método de pago

import { useState, useEffect, useCallback } from 'react';
import { invoke } from '../lib/invokeCompat';
import { TrendingUp } from 'lucide-react';
import {
  BarChart, Bar, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer,
  PieChart, Pie, Cell, Legend,
} from 'recharts';

// ─── Tipos ────────────────────────────────────────────────

interface VentaResumen {
  id: number;
  folio: string;
  usuario_nombre: string;
  cliente_nombre: string | null;
  total: number;
  metodo_pago: string;
  anulada: boolean;
  fecha: string;
  num_productos: number;
}

interface VentaDetalleItem {
  id: number;
  producto_id: number;
  codigo: string;
  nombre: string;
  cantidad: number;
  cantidad_devuelta: number;
  cantidad_disponible: number;
  precio_original: number;
  descuento_porcentaje: number;
  descuento_monto: number;
  precio_final: number;
  subtotal: number;
}

interface VentaDetalleCompleto {
  id: number;
  folio: string;
  usuario_id: number;
  usuario_nombre: string;
  total: number;
  metodo_pago: string;
  anulada: boolean;
  fecha: string;
  items: VentaDetalleItem[];
}

type TabReporte = 'ventas_dia' | 'top_productos' | 'por_vendedor' | 'por_metodo';

// ─── Helpers ──────────────────────────────────────────────

function fmt(n: number) { return `$${n.toFixed(2)}`; }

function fechaStr(d: Date) {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

function haceNDias(n: number): Date {
  const d = new Date();
  d.setDate(d.getDate() - n);
  return d;
}

const COLORES = [
  'hsl(340, 65%, 47%)', // primary-like
  'hsl(210, 70%, 50%)',
  'hsl(45, 85%, 55%)',
  'hsl(150, 55%, 45%)',
  'hsl(280, 55%, 55%)',
  'hsl(20, 80%, 55%)',
  'hsl(180, 50%, 45%)',
  'hsl(350, 50%, 60%)',
  'hsl(100, 50%, 45%)',
  'hsl(260, 60%, 60%)',
];

const METODO_COLORES: Record<string, string> = {
  efectivo: 'hsl(150, 55%, 45%)',
  tarjeta: 'hsl(210, 70%, 50%)',
  transferencia: 'hsl(45, 85%, 55%)',
};

const METODO_LABELS: Record<string, string> = {
  efectivo: 'Efectivo',
  tarjeta: 'Tarjeta',
  transferencia: 'Transferencia',
};

// ─── Componente Principal ─────────────────────────────────

export default function Reportes() {
  const [tab, setTab] = useState<TabReporte>('ventas_dia');
  const [ventas, setVentas] = useState<VentaResumen[]>([]);
  const [detalles, setDetalles] = useState<VentaDetalleCompleto[]>([]);
  const [cargando, setCargando] = useState(false);

  // Rango de fechas predefinidos
  const [rangoLabel, setRangoLabel] = useState('7 días');
  const [fechaInicio, setFechaInicio] = useState(fechaStr(haceNDias(7)));
  const [fechaFin, setFechaFin] = useState(fechaStr(new Date()));

  const rangos = [
    { label: '7 días', dias: 7 },
    { label: '15 días', dias: 15 },
    { label: '30 días', dias: 30 },
    { label: '60 días', dias: 60 },
  ];

  const cargarDatos = useCallback(async () => {
    setCargando(true);
    try {
      const rows = await invoke<VentaResumen[]>('buscar_ventas', {
        folio: null,
        fechaInicio: `${fechaInicio} 00:00:00`,
        fechaFin: `${fechaFin} 23:59:59`,
        clienteTexto: null,
        limite: 5000,
      });
      setVentas(rows.filter(v => !v.anulada));

      // Para Top 10, necesitamos los detalles de cada venta
      if (tab === 'top_productos') {
        const detallesArr: VentaDetalleCompleto[] = [];
        // Cargamos detalles en lotes para evitar saturar
        const ventasActivas = rows.filter(v => !v.anulada).slice(0, 200);
        for (const v of ventasActivas) {
          try {
            const det = await invoke<VentaDetalleCompleto>('obtener_detalle_venta', { ventaId: v.id });
            detallesArr.push(det);
          } catch { /* skip */ }
        }
        setDetalles(detallesArr);
      }
    } catch (e) {
      console.error('Error cargando reportes:', e);
    } finally {
      setCargando(false);
    }
  }, [fechaInicio, fechaFin, tab]);

  useEffect(() => { cargarDatos(); }, [cargarDatos]);

  const setRango = (dias: number, label: string) => {
    setFechaInicio(fechaStr(haceNDias(dias)));
    setFechaFin(fechaStr(new Date()));
    setRangoLabel(label);
  };

  // ─── Datos procesados ───────────────────────────────────

  // Ventas por día
  const ventasPorDia = (() => {
    const mapa: Record<string, { fecha: string; total: number; count: number }> = {};
    for (const v of ventas) {
      const dia = v.fecha.substring(0, 10);
      if (!mapa[dia]) mapa[dia] = { fecha: dia, total: 0, count: 0 };
      mapa[dia].total += v.total;
      mapa[dia].count += 1;
    }
    return Object.values(mapa).sort((a, b) => a.fecha.localeCompare(b.fecha));
  })();

  // Top 10 productos
  const topProductos = (() => {
    if (tab !== 'top_productos') return [];
    const mapa: Record<string, { nombre: string; cantidad: number; total: number }> = {};
    for (const d of detalles) {
      for (const item of d.items) {
        const key = item.nombre;
        if (!mapa[key]) mapa[key] = { nombre: item.nombre, cantidad: 0, total: 0 };
        mapa[key].cantidad += item.cantidad;
        mapa[key].total += item.subtotal;
      }
    }
    return Object.values(mapa).sort((a, b) => b.cantidad - a.cantidad).slice(0, 10);
  })();

  // Ventas por vendedor
  const ventasPorVendedor = (() => {
    const mapa: Record<string, { nombre: string; total: number; count: number }> = {};
    for (const v of ventas) {
      const nombre = v.usuario_nombre;
      if (!mapa[nombre]) mapa[nombre] = { nombre, total: 0, count: 0 };
      mapa[nombre].total += v.total;
      mapa[nombre].count += 1;
    }
    return Object.values(mapa).sort((a, b) => b.total - a.total);
  })();

  // Ventas por método de pago
  const ventasPorMetodo = (() => {
    const mapa: Record<string, { metodo: string; label: string; total: number; count: number }> = {};
    for (const v of ventas) {
      const met = v.metodo_pago;
      if (!mapa[met]) mapa[met] = { metodo: met, label: METODO_LABELS[met] || met, total: 0, count: 0 };
      mapa[met].total += v.total;
      mapa[met].count += 1;
    }
    return Object.values(mapa).sort((a, b) => b.total - a.total);
  })();

  const totalGeneral = ventas.reduce((sum, v) => sum + v.total, 0);

  // ─── Tabs ───────────────────────────────────────────────

  const tabs: { id: TabReporte; label: string }[] = [
    { id: 'ventas_dia', label: 'Ventas por día' },
    { id: 'top_productos', label: 'Top 10 productos' },
    { id: 'por_vendedor', label: 'Por vendedor' },
    { id: 'por_metodo', label: 'Por método de pago' },
  ];

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      {/* Header */}
      <div style={{
        padding: '12px 20px', borderBottom: '1px solid var(--color-border)',
        background: 'var(--color-surface)', display: 'flex', alignItems: 'center',
        justifyContent: 'space-between',
      }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          <TrendingUp size={20} style={{ color: 'var(--color-primary)' }} />
          <h2 style={{ fontSize: 17, fontWeight: 800, color: 'var(--color-text)' }}>Reportes</h2>
          <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>·</span>
          <span style={{ fontSize: 12, color: 'var(--color-text-muted)' }}>
            {ventas.length} ventas en rango
          </span>
        </div>

        {/* Rango de fechas */}
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          {rangos.map(r => (
            <button
              key={r.label}
              className={`btn btn-sm ${rangoLabel === r.label ? 'btn-primary' : 'btn-ghost'}`}
              onClick={() => setRango(r.dias, r.label)}
              style={{ fontSize: 11 }}
            >
              {r.label}
            </button>
          ))}
          <span style={{ color: 'var(--color-border)', margin: '0 4px' }}>|</span>
          <input
            type="date"
            className="input"
            value={fechaInicio}
            onChange={e => { setFechaInicio(e.target.value); setRangoLabel(''); }}
            style={{ width: 130, fontSize: 12, padding: '4px 8px' }}
          />
          <span style={{ fontSize: 12, color: 'var(--color-text-dim)' }}>a</span>
          <input
            type="date"
            className="input"
            value={fechaFin}
            onChange={e => { setFechaFin(e.target.value); setRangoLabel(''); }}
            style={{ width: 130, fontSize: 12, padding: '4px 8px' }}
          />
        </div>
      </div>

      {/* Tabs */}
      <div style={{
        display: 'flex', gap: 0, borderBottom: '2px solid var(--color-border)',
        background: 'var(--color-surface)', padding: '0 20px',
      }}>
        {tabs.map(t => (
          <button
            key={t.id}
            onClick={() => setTab(t.id)}
            style={{
              padding: '10px 18px', border: 'none', background: 'transparent', cursor: 'pointer',
              fontSize: 13, fontWeight: 600,
              color: tab === t.id ? 'var(--color-primary)' : 'var(--color-text-muted)',
              borderBottom: tab === t.id ? '2px solid var(--color-primary)' : '2px solid transparent',
              marginBottom: -2,
              transition: 'all 0.15s',
            }}
          >
            {t.label}
          </button>
        ))}
      </div>

      {/* Contenido */}
      <div style={{ flex: 1, overflow: 'auto', padding: 24 }}>
        {cargando ? (
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', color: 'var(--color-text-dim)' }}>
            <span className="animate-pulse-soft">Cargando datos...</span>
          </div>
        ) : (
          <>
            {/* Resumen rápido */}
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: 14, marginBottom: 24 }}>
              <MiniCard label="Total Ventas" value={fmt(totalGeneral)} />
              <MiniCard label="Transacciones" value={String(ventas.length)} />
              <MiniCard label="Promedio / Venta" value={ventas.length > 0 ? fmt(totalGeneral / ventas.length) : '$0.00'} />
              <MiniCard label="Días con ventas" value={String(ventasPorDia.length)} />
            </div>

            {/* Gráficas */}
            {tab === 'ventas_dia' && <GraficaVentasDia data={ventasPorDia} />}
            {tab === 'top_productos' && <TablaTopProductos data={topProductos} />}
            {tab === 'por_vendedor' && <GraficaVendedores data={ventasPorVendedor} totalGeneral={totalGeneral} />}
            {tab === 'por_metodo' && <GraficaMetodoPago data={ventasPorMetodo} totalGeneral={totalGeneral} />}
          </>
        )}
      </div>
    </div>
  );
}

// ─── Sub-componentes ──────────────────────────────────────

function MiniCard({ label, value }: { label: string; value: string }) {
  return (
    <div className="card" style={{ padding: 14 }}>
      <p style={{ fontSize: 10, fontWeight: 600, color: 'var(--color-text-dim)', textTransform: 'uppercase', marginBottom: 4 }}>
        {label}
      </p>
      <p className="mono" style={{ fontSize: 20, fontWeight: 800, color: 'var(--color-text)' }}>{value}</p>
    </div>
  );
}

function GraficaVentasDia({ data }: { data: { fecha: string; total: number; count: number }[] }) {
  if (data.length === 0) return <EmptyState mensaje="No hay ventas en este período" />;

  return (
    <div className="card" style={{ padding: 20 }}>
      <h3 style={{ fontSize: 14, fontWeight: 700, marginBottom: 16, color: 'var(--color-text-muted)' }}>
        VENTAS POR DÍA
      </h3>
      <ResponsiveContainer width="100%" height={360}>
        <BarChart data={data} margin={{ top: 5, right: 20, left: 10, bottom: 5 }}>
          <CartesianGrid strokeDasharray="3 3" stroke="var(--color-border)" />
          <XAxis
            dataKey="fecha"
            tick={{ fill: 'var(--color-text-muted)', fontSize: 11 }}
            tickFormatter={(v: string) => v.substring(5)}
          />
          <YAxis
            tick={{ fill: 'var(--color-text-muted)', fontSize: 11 }}
            tickFormatter={(v: number) => `$${v >= 1000 ? (v / 1000).toFixed(0) + 'k' : v}`}
          />
          <Tooltip
            formatter={(value: any) => [fmt(Number(value)), 'Total']}
            labelFormatter={(label: any) => `Fecha: ${label}`}
            contentStyle={{
              background: 'var(--color-surface)',
              border: '1px solid var(--color-border)',
              borderRadius: 8, fontSize: 13,
            }}
          />
          <Bar dataKey="total" fill="var(--color-primary)" radius={[4, 4, 0, 0]} />
        </BarChart>
      </ResponsiveContainer>

      {/* Tabla detalle debajo */}
      <div style={{ marginTop: 16, maxHeight: 200, overflow: 'auto' }}>
        <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 12 }}>
          <thead>
            <tr style={{ borderBottom: '2px solid var(--color-border)' }}>
              <th style={thStyle}>FECHA</th>
              <th style={{ ...thStyle, textAlign: 'right' }}>VENTAS</th>
              <th style={{ ...thStyle, textAlign: 'right' }}>TOTAL</th>
              <th style={{ ...thStyle, textAlign: 'right' }}>PROMEDIO</th>
            </tr>
          </thead>
          <tbody>
            {data.map(d => (
              <tr key={d.fecha} style={{ borderBottom: '1px solid var(--color-border)' }}>
                <td style={tdStyle}>{d.fecha}</td>
                <td style={{ ...tdStyle, textAlign: 'right' }}>{d.count}</td>
                <td className="mono" style={{ ...tdStyle, textAlign: 'right', fontWeight: 700 }}>{fmt(d.total)}</td>
                <td className="mono" style={{ ...tdStyle, textAlign: 'right', color: 'var(--color-text-dim)' }}>
                  {d.count > 0 ? fmt(d.total / d.count) : '$0.00'}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function TablaTopProductos({ data }: { data: { nombre: string; cantidad: number; total: number }[] }) {
  if (data.length === 0) return <EmptyState mensaje="No hay datos de productos en este período" />;

  const maxCant = Math.max(...data.map(d => d.cantidad));

  return (
    <div className="card" style={{ padding: 20 }}>
      <h3 style={{ fontSize: 14, fontWeight: 700, marginBottom: 16, color: 'var(--color-text-muted)' }}>
        TOP 10 PRODUCTOS MÁS VENDIDOS
      </h3>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
        {data.map((p, i) => {
          const pct = maxCant > 0 ? (p.cantidad / maxCant) * 100 : 0;
          return (
            <div key={p.nombre} style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
              <span style={{
                width: 28, height: 28, borderRadius: '50%',
                background: COLORES[i % COLORES.length],
                color: '#fff', display: 'flex', alignItems: 'center', justifyContent: 'center',
                fontSize: 12, fontWeight: 700, flexShrink: 0,
              }}>
                {i + 1}
              </span>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 4 }}>
                  <span style={{
                    fontSize: 13, fontWeight: 600, color: 'var(--color-text)',
                    overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                  }}>
                    {p.nombre}
                  </span>
                  <div style={{ display: 'flex', gap: 16, flexShrink: 0 }}>
                    <span className="mono" style={{ fontSize: 12, fontWeight: 700, color: 'var(--color-text)' }}>
                      {p.cantidad} uds
                    </span>
                    <span className="mono" style={{ fontSize: 12, fontWeight: 600, color: 'var(--color-text-dim)', width: 80, textAlign: 'right' }}>
                      {fmt(p.total)}
                    </span>
                  </div>
                </div>
                <div style={{ height: 6, background: 'var(--color-surface-2)', borderRadius: 3, overflow: 'hidden' }}>
                  <div style={{
                    height: '100%', borderRadius: 3,
                    background: COLORES[i % COLORES.length],
                    width: `${pct}%`,
                    transition: 'width 0.5s ease',
                  }} />
                </div>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function GraficaVendedores({ data, totalGeneral }: { data: { nombre: string; total: number; count: number }[]; totalGeneral: number }) {
  if (data.length === 0) return <EmptyState mensaje="No hay datos de vendedores en este período" />;

  const pieData = data.map(d => ({ name: d.nombre, value: d.total }));

  return (
    <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 20 }}>
      <div className="card" style={{ padding: 20 }}>
        <h3 style={{ fontSize: 14, fontWeight: 700, marginBottom: 16, color: 'var(--color-text-muted)' }}>
          VENTAS POR VENDEDOR
        </h3>
        <ResponsiveContainer width="100%" height={300}>
          <PieChart>
            <Pie
              data={pieData}
              cx="50%" cy="50%"
              innerRadius={60} outerRadius={110}
              paddingAngle={3}
              dataKey="value"
            >
              {pieData.map((_, i) => (
                <Cell key={i} fill={COLORES[i % COLORES.length]} />
              ))}
            </Pie>
            <Tooltip
              formatter={(value: any) => [fmt(Number(value)), 'Total']}
              contentStyle={{
                background: 'var(--color-surface)',
                border: '1px solid var(--color-border)',
                borderRadius: 8, fontSize: 13,
              }}
            />
            <Legend />
          </PieChart>
        </ResponsiveContainer>
      </div>

      <div className="card" style={{ padding: 20 }}>
        <h3 style={{ fontSize: 14, fontWeight: 700, marginBottom: 16, color: 'var(--color-text-muted)' }}>
          DESGLOSE
        </h3>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
          {data.map((d, i) => {
            const pct = totalGeneral > 0 ? (d.total / totalGeneral) * 100 : 0;
            return (
              <div key={d.nombre}>
                <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 4 }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                    <span style={{
                      width: 10, height: 10, borderRadius: '50%',
                      background: COLORES[i % COLORES.length], flexShrink: 0,
                    }} />
                    <span style={{ fontSize: 13, fontWeight: 600 }}>{d.nombre}</span>
                  </div>
                  <span className="mono" style={{ fontSize: 13, fontWeight: 700 }}>{fmt(d.total)}</span>
                </div>
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <div style={{ flex: 1, height: 6, background: 'var(--color-surface-2)', borderRadius: 3, overflow: 'hidden' }}>
                    <div style={{
                      height: '100%', borderRadius: 3,
                      background: COLORES[i % COLORES.length],
                      width: `${pct}%`,
                      transition: 'width 0.5s ease',
                    }} />
                  </div>
                  <span style={{ fontSize: 11, color: 'var(--color-text-dim)', width: 40, textAlign: 'right' }}>
                    {pct.toFixed(0)}%
                  </span>
                </div>
                <p style={{ fontSize: 11, color: 'var(--color-text-dim)', marginTop: 2 }}>
                  {d.count} transacciones · Prom: {d.count > 0 ? fmt(d.total / d.count) : '$0.00'}
                </p>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

function GraficaMetodoPago({ data, totalGeneral }: { data: { metodo: string; label: string; total: number; count: number }[]; totalGeneral: number }) {
  if (data.length === 0) return <EmptyState mensaje="No hay datos de pagos en este período" />;

  const pieData = data.map(d => ({
    name: d.label,
    value: d.total,
    fill: METODO_COLORES[d.metodo] || COLORES[0],
  }));

  return (
    <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 20 }}>
      <div className="card" style={{ padding: 20 }}>
        <h3 style={{ fontSize: 14, fontWeight: 700, marginBottom: 16, color: 'var(--color-text-muted)' }}>
          VENTAS POR MÉTODO DE PAGO
        </h3>
        <ResponsiveContainer width="100%" height={300}>
          <PieChart>
            <Pie
              data={pieData}
              cx="50%" cy="50%"
              innerRadius={60} outerRadius={110}
              paddingAngle={3}
              dataKey="value"
            >
              {pieData.map((entry, i) => (
                <Cell key={i} fill={entry.fill} />
              ))}
            </Pie>
            <Tooltip
              formatter={(value: any) => [fmt(Number(value)), 'Total']}
              contentStyle={{
                background: 'var(--color-surface)',
                border: '1px solid var(--color-border)',
                borderRadius: 8, fontSize: 13,
              }}
            />
            <Legend />
          </PieChart>
        </ResponsiveContainer>
      </div>

      <div className="card" style={{ padding: 20 }}>
        <h3 style={{ fontSize: 14, fontWeight: 700, marginBottom: 16, color: 'var(--color-text-muted)' }}>
          DESGLOSE
        </h3>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
          {data.map(d => {
            const pct = totalGeneral > 0 ? (d.total / totalGeneral) * 100 : 0;
            const color = METODO_COLORES[d.metodo] || COLORES[0];
            return (
              <div key={d.metodo}>
                <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 6 }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                    <span style={{
                      width: 12, height: 12, borderRadius: 3,
                      background: color, flexShrink: 0,
                    }} />
                    <span style={{ fontSize: 14, fontWeight: 700 }}>{d.label}</span>
                  </div>
                  <div style={{ textAlign: 'right' }}>
                    <span className="mono" style={{ fontSize: 18, fontWeight: 800, color }}>{fmt(d.total)}</span>
                  </div>
                </div>
                <div style={{ height: 8, background: 'var(--color-surface-2)', borderRadius: 4, overflow: 'hidden' }}>
                  <div style={{
                    height: '100%', borderRadius: 4,
                    background: color,
                    width: `${pct}%`,
                    transition: 'width 0.5s ease',
                  }} />
                </div>
                <div style={{ display: 'flex', justifyContent: 'space-between', marginTop: 4 }}>
                  <span style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>
                    {d.count} transacciones
                  </span>
                  <span style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>
                    {pct.toFixed(1)}%
                  </span>
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

function EmptyState({ mensaje }: { mensaje: string }) {
  return (
    <div className="card" style={{
      padding: 60, textAlign: 'center', color: 'var(--color-text-dim)',
      display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 8,
    }}>
      <TrendingUp size={40} strokeWidth={1.2} />
      <p style={{ fontSize: 15, fontWeight: 600 }}>{mensaje}</p>
      <p style={{ fontSize: 12 }}>Ajusta el rango de fechas o realiza ventas para ver datos.</p>
    </div>
  );
}

const thStyle: React.CSSProperties = {
  textAlign: 'left', padding: '8px 10px',
  fontSize: 10, color: 'var(--color-text-muted)',
  fontWeight: 600, textTransform: 'uppercase',
};

const tdStyle: React.CSSProperties = {
  padding: '8px 10px', fontSize: 12,
};
