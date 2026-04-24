import { FormEvent, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { login, setSession } from '../lib/api'

export default function Login() {
  const nav = useNavigate()
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [cargando, setCargando] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function submit(e: FormEvent) {
    e.preventDefault()
    setCargando(true); setError(null)
    try {
      const out = await login(email, password)
      setSession(out.token)
      nav('/', { replace: true })
    } catch (err: any) {
      setError(err?.message ?? 'Error')
    } finally {
      setCargando(false)
    }
  }

  return (
    <div className="min-h-screen bg-gray-900 flex items-center justify-center p-6">
      <form
        onSubmit={submit}
        className="bg-white rounded-lg shadow-lg p-8 w-full max-w-sm space-y-4"
      >
        <h1 className="text-xl font-bold">Moto Refaccionaria</h1>
        <p className="text-sm text-gray-500">Panel de administración</p>

        <div>
          <label className="block text-xs font-medium text-gray-600 mb-1">Email</label>
          <input
            type="email" required
            value={email} onChange={e => setEmail(e.target.value)}
            className="w-full px-3 py-2 border rounded"
            autoFocus
          />
        </div>
        <div>
          <label className="block text-xs font-medium text-gray-600 mb-1">Contraseña</label>
          <input
            type="password" required
            value={password} onChange={e => setPassword(e.target.value)}
            className="w-full px-3 py-2 border rounded"
          />
        </div>

        {error && <div className="text-sm text-red-600">{error}</div>}

        <button
          type="submit" disabled={cargando}
          className="w-full py-2 bg-gray-900 text-white rounded disabled:opacity-50"
        >
          {cargando ? 'Ingresando…' : 'Ingresar'}
        </button>
      </form>
    </div>
  )
}
