// pages/Dashboard.tsx — Layout principal del POS (post-login)
// Navegación lateral + contenido dinámico

import { useState, useEffect } from 'react';
import { useAuthStore } from '../store/authStore';
import { invoke } from '../lib/invokeCompat';
import PuntoDeVenta from './PuntoDeVenta';
import Catalogo from './Catalogo';
import UsuariosPage from './Usuarios';
import ClientesPage from './Clientes';
import Bitacora from './Bitacora';
import Presupuestos from './Presupuestos';
import RecepcionPage from './Recepcion';
import PedidosPage from './Pedidos';
import EtiquetasPage from './Etiquetas';
import CortesCaja, { ModalAperturaCaja } from './CortesCaja';
import HistorialVentas from './HistorialVentas';
import Reportes from './Reportes';
import Ajustes from './Ajustes';
import ConexionMovil from './ConexionMovil';
import Sincronizacion from './Sincronizacion';
import { useCortesStore } from '../store/cortesStore';
import {
  ShoppingCart, Package, BarChart3, LogOut, ClipboardList,
  TruckIcon, Tag, Users, ScrollText, DollarSign, History, Settings, UserPlus, TrendingUp,
  Smartphone, Menu, X, Cloud,
} from 'lucide-react';

type Modulo = 'venta' | 'catalogo' | 'dashboard' | 'presupuestos' | 'recepcion' | 'pedidos' | 'etiquetas' | 'bitacora' | 'usuarios' | 'clientes' | 'cortes' | 'historial' | 'reportes' | 'ajustes' | 'conexion' | 'sincronizacion';

interface EstadisticasDia {
  total_ventas: number;
  num_transacciones: number;
  efectivo: number;
  tarjeta: number;
  transferencia: number;
  producto_top_nombre: string | null;
  producto_top_cantidad: number;
}

export default function Dashboard() {
  const { usuario, logout, tienePermiso } = useAuthStore();
  const [modulo, setModulo] = useState<Modulo>('venta');
  const [stats, setStats] = useState<EstadisticasDia | null>(null);
  const [cortePendiente, setCortePendiente] = useState<string | null>(null);
  const [necesitaApertura, setNecesitaApertura] = useState<boolean>(false);
  const [verificandoApertura, setVerificandoApertura] = useState<boolean>(true);
  const [stockBajoCount, setStockBajoCount] = useState<number>(0);
  const [stockAlertDismiss, setStockAlertDismiss] = useState<boolean>(false);
  const [mobileMenuOpen, setMobileMenuOpen] = useState<boolean>(false);
  const { obtenerAperturaHoy } = useCortesStore();

  // Triggers para abrir modales de cortes desde shortcuts globales
  const [triggerMovimiento, setTriggerMovimiento] = useState(0);
  const [triggerParcial, setTriggerParcial] = useState(0);
  const [triggerDia, setTriggerDia] = useState(0);

  const esAdmin = usuario?.es_admin ?? false;

  // Verificar corte del día pendiente al iniciar
  useEffect(() => {
    import('@tauri-apps/api/core').then(({ invoke }) => {
      invoke<string | null>('verificar_corte_dia_pendiente')
        .then(fecha => setCortePendiente(fecha))
        .catch(() => {});
    });
  }, []);

  // Auto-respaldo diario (una vez al arrancar si no hay uno de hoy)
  useEffect(() => {
    invoke('respaldo_auto_si_necesario').catch(() => {});
  }, []);

  // Alerta de stock bajo (recuento al iniciar y al cambiar a Dashboard)
  useEffect(() => {
    invoke<any[]>('listar_productos_stock_bajo')
      .then(lista => setStockBajoCount(lista.length))
      .catch(() => {});
  }, [modulo]);

  // Verificar apertura de caja del día — bloquea operación si no hay
  useEffect(() => {
    obtenerAperturaHoy()
      .then(apertura => setNecesitaApertura(apertura === null))
      .catch(() => setNecesitaApertura(true))
      .finally(() => setVerificandoApertura(false));
  }, []);

  // Atajos de teclado globales
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'F1') { e.preventDefault(); setModulo('venta'); }
      if (e.key === 'F4') { e.preventDefault(); setModulo('catalogo'); }
      if (e.key === 'F8') { e.preventDefault(); setModulo('dashboard'); }
      if (e.key === 'F6') { e.preventDefault(); setModulo('cortes'); setTriggerMovimiento(n => n + 1); }
      if (e.key === 'F7') { e.preventDefault(); setModulo('historial'); }
      if (e.key === 'F11' && !e.shiftKey) { e.preventDefault(); setModulo('cortes'); setTriggerParcial(n => n + 1); }
      if (e.key === 'F11' && e.shiftKey) { e.preventDefault(); setModulo('cortes'); setTriggerDia(n => n + 1); }
      if (e.key === 'F10') { e.preventDefault(); setModulo('reportes'); }
      if (e.key === 'F12') { e.preventDefault(); logout(); }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  // Cargar stats cuando se abre el dashboard
  useEffect(() => {
    if (modulo === 'dashboard') {
      invoke<EstadisticasDia>('obtener_estadisticas_dia')
        .then(setStats)
        .catch(() => {});
    }
  }, [modulo]);

  const fmt = (n: number) => `$${n.toFixed(2)}`;

  // Menú lateral
  const menuItems: { id: Modulo; label: string; icon: React.ReactNode; key: string; visible: boolean }[] = [
    { id: 'venta', label: 'Venta Rápida', icon: <ShoppingCart size={18} />, key: 'F1', visible: tienePermiso('ventas', 'crear') },
    { id: 'catalogo', label: 'Inventario', icon: <Package size={18} />, key: 'F4', visible: tienePermiso('inventario', 'ver') },
    { id: 'dashboard', label: 'Dashboard', icon: <BarChart3 size={18} />, key: 'F8', visible: true },
    { id: 'presupuestos', label: 'Presupuestos', icon: <ClipboardList size={18} />, key: 'F2', visible: tienePermiso('ventas', 'crear') },
    { id: 'recepcion', label: 'Recepción', icon: <TruckIcon size={18} />, key: 'F3', visible: tienePermiso('inventario', 'crear') },
    { id: 'pedidos', label: 'Pedidos', icon: <ScrollText size={18} />, key: '', visible: tienePermiso('pedidos', 'ver') },
    { id: 'etiquetas', label: 'Etiquetas', icon: <Tag size={18} />, key: 'F5', visible: tienePermiso('inventario', 'ver') },
    { id: 'historial', label: 'Historial Ventas', icon: <History size={18} />, key: 'F7', visible: tienePermiso('ventas', 'ver') },
    { id: 'bitacora', label: 'Bitácora', icon: <ScrollText size={18} />, key: 'F9', visible: esAdmin },
    { id: 'clientes', label: 'Clientes', icon: <UserPlus size={18} />, key: '', visible: tienePermiso('ventas', 'crear') },
    { id: 'usuarios', label: 'Usuarios', icon: <Users size={18} />, key: '', visible: esAdmin },
    { id: 'cortes', label: 'Cortes de Caja', icon: <DollarSign size={18} />, key: 'F11', visible: true },
    { id: 'reportes', label: 'Reportes', icon: <TrendingUp size={18} />, key: 'F10', visible: esAdmin },
    { id: 'conexion', label: 'Conexión móvil', icon: <Smartphone size={18} />, key: '', visible: esAdmin },
    { id: 'sincronizacion', label: 'Sincronización', icon: <Cloud size={18} />, key: '', visible: esAdmin },
    { id: 'ajustes', label: 'Ajustes', icon: <Settings size={18} />, key: '', visible: esAdmin },
  ];

  return (
    <>
    {/* Modal bloqueante de apertura de caja — debe completarse antes de operar */}
    {!verificandoApertura && necesitaApertura && (
      <ModalAperturaCaja
        bloqueante
        onSuccess={() => setNecesitaApertura(false)}
      />
    )}
    <div className="pos-dashboard">

      {/* ─── Top Bar ─── */}
      <div className="pos-topbar" style={{ gridColumn: '1 / -1' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          <button
            className="pos-hamburger btn-ghost"
            aria-label="Menú"
            onClick={() => setMobileMenuOpen(v => !v)}
          >
            {mobileMenuOpen ? <X size={20} /> : <Menu size={20} />}
          </button>
          <img src="/logo.png" alt="LB" style={{ height: 30, width: 'auto' }} draggable={false} />
          <span className="pos-topbar-title" style={{ fontWeight: 700, fontSize: 14, color: 'var(--color-text)' }}>
            MOTO REFACCIONARIA LB
          </span>
        </div>

        <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
          {/* Sync indicator */}
          <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <span className="sync-dot sync-ok" />
            <span style={{ fontSize: 11, color: 'var(--color-text-muted)' }}>Local</span>
          </div>

          {/* Usuario */}
          <div style={{
            display: 'flex', alignItems: 'center', gap: 8,
            padding: '4px 10px', background: 'var(--color-surface-2)',
            borderRadius: 8, border: '1px solid var(--color-border)',
          }}>
            <div style={{
              width: 24, height: 24, borderRadius: '50%', background: 'var(--color-primary)',
              display: 'flex', alignItems: 'center', justifyContent: 'center',
              fontSize: 11, fontWeight: 700, color: '#fff',
            }}>
              {usuario?.nombre_completo.charAt(0).toUpperCase()}
            </div>
            <span style={{ fontSize: 13, fontWeight: 600 }}>{usuario?.nombre_completo}</span>
            <span style={{
              fontSize: 10, padding: '1px 6px', borderRadius: 10,
              background: esAdmin ? 'rgba(216,56,77,0.12)' : 'rgba(158,122,126,0.12)',
              color: esAdmin ? 'var(--color-primary)' : 'var(--color-text-muted)',
              fontWeight: 600,
            }}>
              {usuario?.rol_nombre}
            </span>
          </div>

          <button className="btn btn-ghost btn-sm" onClick={logout}>
            <LogOut size={14} /> F12
          </button>
        </div>
      </div>

      {/* Backdrop para cerrar el menú al tocar afuera (solo mobile) */}
      {mobileMenuOpen && (
        <div className="pos-sidebar-backdrop" onClick={() => setMobileMenuOpen(false)} />
      )}

      {/* ─── Sidebar ─── */}
      <div className={`pos-sidebar${mobileMenuOpen ? ' open' : ''}`} style={{
        background: 'var(--color-surface)',
        borderRight: '1px solid var(--color-border)',
        display: 'flex', flexDirection: 'column',
        padding: '8px',
        gap: 2,
        overflow: 'auto',
      }}>
        {menuItems.filter(m => m.visible).map((item) => (
          <button
            key={item.id}
            onClick={() => { setModulo(item.id); setMobileMenuOpen(false); }}
            style={{
              display: 'flex', alignItems: 'center', gap: 10,
              padding: '10px 12px', borderRadius: 8, border: 'none',
              cursor: 'pointer', fontSize: 13, fontWeight: 600,
              transition: 'all 0.1s',
              textAlign: 'left', width: '100%',
              background: modulo === item.id ? 'var(--color-primary-soft)' : 'transparent',
              color: modulo === item.id ? 'var(--color-primary)' : 'var(--color-text-muted)',
            }}
          >
            {item.icon}
            <span style={{ flex: 1 }}>{item.label}</span>
            {item.key && (
              <span style={{ fontSize: 10, opacity: 0.5 }}>{item.key}</span>
            )}
          </button>
        ))}
      </div>

      {/* ─── Contenido ─── */}
      <div style={{ overflow: 'hidden', display: 'flex', flexDirection: 'column', minHeight: 0, minWidth: 0 }}>

        {/* Alerta de corte del día pendiente */}
        {cortePendiente && (
          <div style={{
            padding: '10px 20px', background: 'rgba(245,158,11,0.12)',
            borderBottom: '1px solid rgba(245,158,11,0.4)',
            display: 'flex', alignItems: 'center', gap: 12,
            flexShrink: 0,
          }}>
            <span style={{ fontSize: 15 }}>⚠️</span>
            <span style={{ fontSize: 13, fontWeight: 600, flex: 1, color: 'var(--color-warning)' }}>
              No se hizo el corte del día del {cortePendiente}. Realiza el cierre antes de continuar.
            </span>
            <button
              className="btn btn-sm"
              style={{ background: 'var(--color-warning)', color: '#fff', border: 'none' }}
              onClick={() => { setModulo('cortes'); setTriggerDia(n => n + 1); }}
            >
              Hacer corte ahora
            </button>
            <button
              className="btn btn-ghost btn-sm"
              style={{ fontSize: 11 }}
              onClick={() => setCortePendiente(null)}
            >
              Ignorar
            </button>
          </div>
        )}

        {/* Alerta de stock bajo */}
        {stockBajoCount > 0 && !stockAlertDismiss && tienePermiso('inventario', 'ver') && (
          <div style={{
            padding: '10px 20px', background: 'rgba(239,68,68,0.10)',
            borderBottom: '1px solid rgba(239,68,68,0.4)',
            display: 'flex', alignItems: 'center', gap: 12,
            flexShrink: 0,
          }}>
            <span style={{ fontSize: 15 }}>📉</span>
            <span style={{ fontSize: 13, fontWeight: 600, flex: 1, color: 'var(--color-danger)' }}>
              {stockBajoCount} producto{stockBajoCount !== 1 ? 's' : ''} con stock bajo o agotado.
            </span>
            <button
              className="btn btn-sm"
              style={{ background: 'var(--color-danger)', color: '#fff', border: 'none' }}
              onClick={() => setModulo('catalogo')}
            >
              Ver inventario
            </button>
            <button
              className="btn btn-ghost btn-sm"
              style={{ fontSize: 11 }}
              onClick={() => setStockAlertDismiss(true)}
            >
              Ignorar
            </button>
          </div>
        )}

        {modulo === 'venta' && <PuntoDeVenta />}
        {modulo === 'catalogo' && <Catalogo />}
        {modulo === 'usuarios' && <UsuariosPage />}
        {modulo === 'clientes' && <ClientesPage />}
        {modulo === 'dashboard' && (
          <DashboardHome stats={stats} fmt={fmt} stockBajo={stockBajoCount} onVerInventario={() => setModulo('catalogo')} />
        )}
        {modulo === 'bitacora' && <Bitacora />}
        {modulo === 'presupuestos' && <Presupuestos onIrAVenta={() => setModulo('venta')} />}
        {modulo === 'recepcion' && <RecepcionPage />}
        {modulo === 'pedidos' && <PedidosPage />}
        {modulo === 'etiquetas' && <EtiquetasPage />}
        {modulo === 'historial' && <HistorialVentas />}
        {modulo === 'reportes' && <Reportes />}
        {modulo === 'cortes' && (
          <CortesCaja
            triggerMovimiento={triggerMovimiento}
            triggerParcial={triggerParcial}
            triggerDia={triggerDia}
            fechaObjetivoDia={cortePendiente}
            onCorteDiaHecho={() => setCortePendiente(null)}
          />
        )}
        {modulo === 'ajustes' && <Ajustes />}
        {modulo === 'conexion' && <ConexionMovil />}
        {modulo === 'sincronizacion' && <Sincronizacion />}
      </div>
    </div>
    </>
  );
}

// ─── Dashboard Home ───────────────────────────────────────

function DashboardHome({ stats, fmt, stockBajo, onVerInventario }: { stats: EstadisticasDia | null; fmt: (n: number) => string; stockBajo: number; onVerInventario: () => void }) {
  if (!stats) {
    return (
      <div style={{
        display: 'flex', alignItems: 'center', justifyContent: 'center',
        height: '100%', color: 'var(--color-text-dim)',
      }}>
        <span className="animate-pulse-soft">Cargando estadísticas...</span>
      </div>
    );
  }

  return (
    <div className="animate-fade-in" style={{ padding: 24, overflow: 'auto' }}>
      <h2 style={{ fontSize: 20, fontWeight: 800, marginBottom: 20, color: 'var(--color-text)' }}>
        📊 Resumen del Día
      </h2>

      {stockBajo > 0 && (
        <div className="card" style={{
          padding: 14, marginBottom: 16, display: 'flex', alignItems: 'center', gap: 12,
          background: 'rgba(239,68,68,0.08)', borderColor: 'rgba(239,68,68,0.3)',
        }}>
          <span style={{ fontSize: 20 }}>⚠️</span>
          <div style={{ flex: 1 }}>
            <p style={{ fontSize: 13, fontWeight: 700, color: 'var(--color-danger)' }}>
              Stock bajo: {stockBajo} producto{stockBajo !== 1 ? 's' : ''} necesitan reorden
            </p>
            <p style={{ fontSize: 11, color: 'var(--color-text-dim)' }}>
              Revisa el inventario para planificar tu próxima compra.
            </p>
          </div>
          <button className="btn btn-sm" style={{ background: 'var(--color-danger)', color: '#fff', border: 'none' }}
            onClick={onVerInventario}>
            Ver inventario
          </button>
        </div>
      )}

      {/* Cards de stats */}
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: 16, marginBottom: 24 }}>
        <StatCard label="Total Ventas" value={fmt(stats.total_ventas)} color="var(--color-success)" />
        <StatCard label="Transacciones" value={String(stats.num_transacciones)} color="var(--color-primary)" />
        <StatCard label="Producto Top" value={stats.producto_top_nombre || '—'} sub={stats.producto_top_cantidad > 0 ? `${stats.producto_top_cantidad} unidades` : ''} color="var(--color-warning)" />
        <StatCard label="Promedio / Venta" value={stats.num_transacciones > 0 ? fmt(stats.total_ventas / stats.num_transacciones) : '$0.00'} color="var(--color-text-muted)" />
      </div>

      {/* Desglose por método */}
      <div className="card" style={{ padding: 20 }}>
        <h3 style={{ fontSize: 14, fontWeight: 700, marginBottom: 16, color: 'var(--color-text-muted)' }}>
          DESGLOSE POR MÉTODO DE PAGO
        </h3>
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(3, 1fr)', gap: 16 }}>
          <PaymentBar label="Efectivo" value={stats.efectivo} total={stats.total_ventas} color="var(--color-success)" fmt={fmt} />
          <PaymentBar label="Tarjeta" value={stats.tarjeta} total={stats.total_ventas} color="var(--color-primary)" fmt={fmt} />
          <PaymentBar label="Transferencia" value={stats.transferencia} total={stats.total_ventas} color="var(--color-warning)" fmt={fmt} />
        </div>
      </div>
    </div>
  );
}

function StatCard({ label, value, sub, color }: { label: string; value: string; sub?: string; color: string }) {
  return (
    <div className="card" style={{ padding: 16 }}>
      <p style={{ fontSize: 11, fontWeight: 600, color: 'var(--color-text-dim)', textTransform: 'uppercase', marginBottom: 6 }}>
        {label}
      </p>
      <p className="mono" style={{ fontSize: 24, fontWeight: 800, color }}>{value}</p>
      {sub && <p style={{ fontSize: 12, color: 'var(--color-text-dim)', marginTop: 2 }}>{sub}</p>}
    </div>
  );
}

function PaymentBar({ label, value, total, color, fmt }: {
  label: string; value: number; total: number; color: string; fmt: (n: number) => string;
}) {
  const pct = total > 0 ? (value / total) * 100 : 0;
  return (
    <div>
      <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 6 }}>
        <span style={{ fontSize: 13, fontWeight: 600, color: 'var(--color-text)' }}>{label}</span>
        <span className="mono" style={{ fontSize: 13, fontWeight: 700, color }}>{fmt(value)}</span>
      </div>
      <div style={{ height: 8, background: 'var(--color-surface-2)', borderRadius: 4, overflow: 'hidden' }}>
        <div style={{
          height: '100%', borderRadius: 4, background: color,
          width: `${pct}%`, transition: 'width 0.5s ease',
        }} />
      </div>
      <p style={{ fontSize: 11, color: 'var(--color-text-dim)', marginTop: 2 }}>{pct.toFixed(0)}%</p>
    </div>
  );
}
