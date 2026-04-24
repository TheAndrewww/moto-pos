import { Link, NavLink, Outlet, useNavigate } from 'react-router-dom'
import { clearSession } from '../lib/api'

const tabs = [
  { to: '/',          label: 'Dashboard' },
  { to: '/productos', label: 'Productos' },
  { to: '/ventas',    label: 'Ventas' },
  { to: '/clientes',  label: 'Clientes' },
]

export default function Layout() {
  const nav = useNavigate()
  return (
    <div className="min-h-screen bg-gray-50 text-gray-900 flex">
      <aside className="w-60 bg-gray-900 text-gray-100 flex flex-col">
        <div className="px-5 py-4 text-lg font-bold border-b border-gray-800">
          <Link to="/">Moto Admin</Link>
        </div>
        <nav className="flex-1 py-3">
          {tabs.map(t => (
            <NavLink
              key={t.to}
              to={t.to}
              end={t.to === '/'}
              className={({ isActive }) =>
                `block px-5 py-2 text-sm ${
                  isActive ? 'bg-gray-800 text-white' : 'text-gray-400 hover:text-white'
                }`
              }
            >
              {t.label}
            </NavLink>
          ))}
        </nav>
        <button
          className="m-3 px-3 py-2 rounded bg-gray-800 hover:bg-gray-700 text-sm"
          onClick={() => { clearSession(); nav('/login', { replace: true }) }}
        >
          Cerrar sesión
        </button>
      </aside>
      <main className="flex-1 p-6 overflow-auto">
        <Outlet />
      </main>
    </div>
  )
}
