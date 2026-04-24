import { useEffect, useState } from 'react'
import { listarVentas, detalleVenta, VentaRow } from '../lib/api'

export default function Ventas() {
  const [items, setItems] = useState<VentaRow[]>([])
  const [error, setError] = useState<string | null>(null)
  const [sel, setSel] = useState<string | null>(null)
  const [detalle, setDetalle] = useState<any | null>(null)

  useEffect(() => {
    listarVentas().then(r => setItems(r.items)).catch(e => setError(e.message))
  }, [])

  useEffect(() => {
    if (!sel) { setDetalle(null); return }
    detalleVenta(sel).then(setDetalle).catch(e => setError(e.message))
  }, [sel])

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-bold">Ventas</h1>
      {error && <div className="text-red-600 text-sm">{error}</div>}

      <div className="bg-white rounded-lg shadow overflow-hidden">
        <table className="w-full text-sm">
          <thead className="bg-gray-100 text-xs uppercase text-gray-600">
            <tr>
              <th className="px-3 py-2 text-left">Folio</th>
              <th className="px-3 py-2 text-left">Fecha</th>
              <th className="px-3 py-2 text-left">Usuario</th>
              <th className="px-3 py-2 text-left">Cliente</th>
              <th className="px-3 py-2 text-right">Total</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {items.map(v => (
              <tr key={v.uuid}
                  className={`border-b hover:bg-gray-50 ${v.anulada ? 'opacity-50 line-through' : ''}`}>
                <td className="px-3 py-2 font-mono">{v.folio}</td>
                <td className="px-3 py-2 text-gray-500">{v.fecha?.replace('T',' ').slice(0, 19)}</td>
                <td className="px-3 py-2">{v.usuario ?? '—'}</td>
                <td className="px-3 py-2">{v.cliente ?? 'Público general'}</td>
                <td className="px-3 py-2 text-right">${Number(v.total).toFixed(2)}</td>
                <td className="px-3 py-2 text-right">
                  <button onClick={() => setSel(v.uuid)} className="text-blue-600 hover:underline">Ver</button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {sel && detalle && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center p-4 z-50">
          <div className="bg-white rounded-lg shadow-lg w-full max-w-2xl p-6 space-y-4">
            <div className="flex items-center">
              <h2 className="text-lg font-bold flex-1">
                Venta {detalle.venta?.folio}
              </h2>
              <button onClick={() => setSel(null)} className="text-gray-500 hover:text-gray-900">✕</button>
            </div>
            <table className="w-full text-sm">
              <thead className="text-xs uppercase text-gray-500">
                <tr><th className="text-left">Producto</th><th className="text-right">Cant.</th><th className="text-right">Precio</th><th className="text-right">Subtotal</th></tr>
              </thead>
              <tbody>
                {detalle.detalle.map((d: any) => (
                  <tr key={d.id} className="border-t">
                    <td className="py-1">{d.producto_nombre ?? d.producto_id}</td>
                    <td className="text-right">{Number(d.cantidad).toFixed(0)}</td>
                    <td className="text-right">${Number(d.precio_final).toFixed(2)}</td>
                    <td className="text-right">${Number(d.subtotal).toFixed(2)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            <div className="text-right font-bold">
              Total: ${Number(detalle.venta?.total ?? 0).toFixed(2)}
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
