import { Navigate, Route, Routes } from 'react-router-dom'
import Layout from './components/Layout'
import Login from './pages/Login'
import Dashboard from './pages/Dashboard'
import Productos from './pages/Productos'
import Ventas from './pages/Ventas'
import Clientes from './pages/Clientes'
import { getToken } from './lib/api'

function Guard({ children }: { children: JSX.Element }) {
  if (!getToken()) return <Navigate to="/login" replace />
  return children
}

export default function App() {
  return (
    <Routes>
      <Route path="/login" element={<Login />} />
      <Route path="/" element={<Guard><Layout /></Guard>}>
        <Route index element={<Dashboard />} />
        <Route path="productos" element={<Productos />} />
        <Route path="ventas" element={<Ventas />} />
        <Route path="clientes" element={<Clientes />} />
      </Route>
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  )
}
