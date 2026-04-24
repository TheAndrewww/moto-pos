import { useEffect, useState } from 'react'
import {
  listarProductos, crearProducto, actualizarProducto, eliminarProducto,
  Producto,
} from '../lib/api'

export default function Productos() {
  const [q, setQ] = useState('')
  const [items, setItems] = useState<Producto[]>([])
  const [edit, setEdit] = useState<Producto | null>(null)
  const [creando, setCreando] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function cargar() {
    try {
      const r = await listarProductos(q)
      setItems(r.items)
    } catch (e: any) {
      setError(e.message)
    }
  }

  useEffect(() => { cargar() }, [])
  useEffect(() => {
    const t = setTimeout(cargar, 300)
    return () => clearTimeout(t)
  }, [q])

  async function guardar(data: Partial<Producto>, uuid?: string) {
    try {
      if (uuid) await actualizarProducto(uuid, data)
      else      await crearProducto(data)
      setEdit(null); setCreando(false)
      await cargar()
    } catch (e: any) {
      alert(e.message)
    }
  }

  async function borrar(uuid: string) {
    if (!confirm('¿Eliminar este producto?')) return
    try {
      await eliminarProducto(uuid)
      await cargar()
    } catch (e: any) {
      alert(e.message)
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-3">
        <h1 className="text-2xl font-bold flex-1">Productos</h1>
        <input
          placeholder="Buscar por nombre o código…"
          value={q} onChange={e => setQ(e.target.value)}
          className="px-3 py-2 border rounded w-72"
        />
        <button
          onClick={() => { setCreando(true); setEdit(null) }}
          className="px-4 py-2 bg-gray-900 text-white rounded text-sm"
        >
          + Nuevo
        </button>
      </div>

      {error && <div className="text-red-600 text-sm">{error}</div>}

      <div className="bg-white rounded-lg shadow overflow-hidden">
        <table className="w-full text-sm">
          <thead className="bg-gray-100 text-xs uppercase text-gray-600">
            <tr>
              <th className="px-3 py-2 text-left">Código</th>
              <th className="px-3 py-2 text-left">Nombre</th>
              <th className="px-3 py-2 text-right">Costo</th>
              <th className="px-3 py-2 text-right">Venta</th>
              <th className="px-3 py-2 text-right">Stock</th>
              <th className="px-3 py-2"></th>
            </tr>
          </thead>
          <tbody>
            {items.map(p => (
              <tr key={p.uuid} className="border-b last:border-0 hover:bg-gray-50">
                <td className="px-3 py-2 font-mono">{p.codigo}</td>
                <td className="px-3 py-2">{p.nombre}</td>
                <td className="px-3 py-2 text-right">${Number(p.precio_costo).toFixed(2)}</td>
                <td className="px-3 py-2 text-right">${Number(p.precio_venta).toFixed(2)}</td>
                <td className="px-3 py-2 text-right">{Number(p.stock_actual).toFixed(0)}</td>
                <td className="px-3 py-2 text-right space-x-2">
                  <button onClick={() => setEdit(p)} className="text-blue-600 hover:underline">Editar</button>
                  <button onClick={() => borrar(p.uuid)} className="text-red-600 hover:underline">Borrar</button>
                </td>
              </tr>
            ))}
            {items.length === 0 && (
              <tr><td colSpan={6} className="text-center text-gray-400 py-8">Sin resultados</td></tr>
            )}
          </tbody>
        </table>
      </div>

      {(edit || creando) && (
        <ProductoModal
          initial={edit ?? undefined}
          onClose={() => { setEdit(null); setCreando(false) }}
          onSave={(data) => guardar(data, edit?.uuid)}
        />
      )}
    </div>
  )
}

function ProductoModal(
  { initial, onClose, onSave }:
  { initial?: Producto; onClose: () => void; onSave: (d: Partial<Producto>) => void },
) {
  const [codigo, setCodigo]   = useState(initial?.codigo ?? '')
  const [nombre, setNombre]   = useState(initial?.nombre ?? '')
  const [costo,  setCosto]    = useState(String(initial?.precio_costo ?? '0'))
  const [venta,  setVenta]    = useState(String(initial?.precio_venta ?? '0'))
  const [minimo, setMinimo]   = useState(String(initial?.stock_minimo ?? '0'))
  const [descr,  setDescr]    = useState(initial?.descripcion ?? '')

  function submit(e: React.FormEvent) {
    e.preventDefault()
    onSave({
      codigo, nombre, descripcion: descr || undefined,
      precio_costo: Number(costo) as any,
      precio_venta: Number(venta) as any,
      stock_minimo: Number(minimo) as any,
    })
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4">
      <form onSubmit={submit} className="bg-white rounded-lg shadow-lg w-full max-w-md p-6 space-y-3">
        <h2 className="text-lg font-bold">{initial ? 'Editar producto' : 'Nuevo producto'}</h2>
        <Field label="Código"><input className="w-full px-3 py-2 border rounded" required value={codigo} onChange={e => setCodigo(e.target.value)} /></Field>
        <Field label="Nombre"><input className="w-full px-3 py-2 border rounded" required value={nombre} onChange={e => setNombre(e.target.value)} /></Field>
        <Field label="Descripción"><input className="w-full px-3 py-2 border rounded" value={descr} onChange={e => setDescr(e.target.value)} /></Field>
        <div className="grid grid-cols-3 gap-3">
          <Field label="Costo"><input className="w-full px-3 py-2 border rounded" type="number" step="0.01" value={costo} onChange={e => setCosto(e.target.value)} /></Field>
          <Field label="Venta"><input className="w-full px-3 py-2 border rounded" type="number" step="0.01" value={venta} onChange={e => setVenta(e.target.value)} /></Field>
          <Field label="Stock mín"><input className="w-full px-3 py-2 border rounded" type="number" step="1" value={minimo} onChange={e => setMinimo(e.target.value)} /></Field>
        </div>
        <div className="flex gap-2 pt-3">
          <button type="button" onClick={onClose} className="flex-1 py-2 border rounded">Cancelar</button>
          <button type="submit" className="flex-1 py-2 bg-gray-900 text-white rounded">Guardar</button>
        </div>
      </form>
    </div>
  )
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <span className="block text-xs font-medium text-gray-600 mb-1">{label}</span>
      {children}
    </label>
  )
}
