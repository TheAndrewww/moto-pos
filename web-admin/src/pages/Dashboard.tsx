import { useEffect, useState } from 'react'
import { dashboard, DashboardRes } from '../lib/api'

function Stat({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="bg-white rounded-lg shadow p-5">
      <div className="text-xs text-gray-500 uppercase">{label}</div>
      <div className="text-2xl font-bold mt-1">{value}</div>
      {hint && <div className="text-xs text-gray-400 mt-1">{hint}</div>}
    </div>
  )
}

export default function Dashboard() {
  const [data, setData] = useState<DashboardRes | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    dashboard().then(setData).catch(e => setError(String(e)))
  }, [])

  if (error) return <div className="text-red-600">{error}</div>
  if (!data) return <div className="text-gray-500">Cargando…</div>

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold">Dashboard</h1>

      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        <Stat label="Ventas hoy"  value={`$${Number(data.ventas_hoy.total).toFixed(2)}`} />
        <Stat label="# Ventas"    value={String(data.ventas_hoy.cuenta)} />
        <Stat label="Stock bajo"  value={String(data.stock_bajo)} hint="productos" />
        <Stat label="Dispositivos" value={String(data.dispositivos)} hint="POS conectados" />
      </div>

      <section className="bg-white rounded-lg shadow p-5">
        <h2 className="font-semibold mb-3">Últimas ventas</h2>
        {data.ultimas_ventas.length === 0 ? (
          <p className="text-sm text-gray-500">Sin ventas recientes.</p>
        ) : (
          <table className="w-full text-sm">
            <thead>
              <tr className="text-left text-gray-500 text-xs uppercase border-b">
                <th className="py-2">Folio</th>
                <th>Usuario</th>
                <th>Fecha</th>
                <th className="text-right">Total</th>
              </tr>
            </thead>
            <tbody>
              {data.ultimas_ventas.map(v => (
                <tr key={v.uuid} className="border-b last:border-0">
                  <td className="py-2 font-mono">{v.folio}</td>
                  <td>{v.usuario ?? '—'}</td>
                  <td className="text-gray-500">{v.fecha?.replace('T', ' ').slice(0, 19)}</td>
                  <td className="text-right">${Number(v.total).toFixed(2)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </div>
  )
}
