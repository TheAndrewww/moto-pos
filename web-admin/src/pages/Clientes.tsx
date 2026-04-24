import { useEffect, useState } from 'react'
import { listarClientes } from '../lib/api'

export default function Clientes() {
  const [q, setQ] = useState('')
  const [items, setItems] = useState<any[]>([])
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const t = setTimeout(() => {
      listarClientes(q).then(r => setItems(r.items)).catch(e => setError(e.message))
    }, 250)
    return () => clearTimeout(t)
  }, [q])

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-3">
        <h1 className="text-2xl font-bold flex-1">Clientes</h1>
        <input
          placeholder="Buscar…"
          value={q} onChange={e => setQ(e.target.value)}
          className="px-3 py-2 border rounded w-72"
        />
      </div>
      {error && <div className="text-red-600 text-sm">{error}</div>}
      <div className="bg-white rounded-lg shadow overflow-hidden">
        <table className="w-full text-sm">
          <thead className="bg-gray-100 text-xs uppercase text-gray-600">
            <tr>
              <th className="px-3 py-2 text-left">Nombre</th>
              <th className="px-3 py-2 text-left">Teléfono</th>
              <th className="px-3 py-2 text-left">Email</th>
              <th className="px-3 py-2 text-right">Descuento</th>
              <th className="px-3 py-2">Estado</th>
            </tr>
          </thead>
          <tbody>
            {items.map((c: any) => (
              <tr key={c.uuid} className="border-b last:border-0">
                <td className="px-3 py-2">{c.nombre}</td>
                <td className="px-3 py-2">{c.telefono ?? '—'}</td>
                <td className="px-3 py-2">{c.email ?? '—'}</td>
                <td className="px-3 py-2 text-right">{Number(c.descuento_porcentaje).toFixed(1)}%</td>
                <td className="px-3 py-2">{c.activo ? 'Activo' : 'Inactivo'}</td>
              </tr>
            ))}
            {items.length === 0 && (
              <tr><td colSpan={5} className="text-center text-gray-400 py-8">Sin resultados</td></tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  )
}
