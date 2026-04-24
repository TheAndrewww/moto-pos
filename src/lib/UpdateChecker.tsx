// UpdateChecker — detecta nuevas versiones del POS y las instala.
//
// Solo corre dentro de Tauri (en navegador no aplica — la web siempre tiene
// la última versión porque sirve el bundle reciente).
//
// Flujo:
//   1. Al montar, llama a `check()` del plugin updater.
//   2. Si hay update: muestra banner discreto con [Actualizar ahora] / [Después].
//   3. "Actualizar ahora" → descarga, verifica firma, instala y reinicia.
//   4. Re-checa cada 30 min mientras la app está abierta (por si lanzaste un
//      hotfix mientras una caja lleva horas abierta).

import { useEffect, useState } from 'react';
import { isTauri } from './invokeCompat';
import { Download, X } from 'lucide-react';

interface UpdateInfo {
  version: string;
  body?: string;
}

export default function UpdateChecker() {
  const [available, setAvailable] = useState<UpdateInfo | null>(null);
  const [installing, setInstalling] = useState(false);
  const [progress, setProgress] = useState<{ downloaded: number; total: number } | null>(null);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isTauri()) return;

    let mounted = true;
    let updateRef: any = null;

    const checkOnce = async () => {
      try {
        const { check } = await import('@tauri-apps/plugin-updater');
        const update = await check();
        if (!mounted) return;
        if (update) {
          updateRef = update;
          setAvailable({ version: update.version, body: update.body });
        }
      } catch (e: any) {
        // Sin conexión, sin release publicado, etc. → silencioso (no es crítico)
        console.warn('[updater] check failed:', e);
      }
    };

    // Check al arrancar y cada 30 min después
    checkOnce();
    const id = window.setInterval(checkOnce, 30 * 60 * 1000);

    // Exponer la referencia para el handler de instalación
    (window as any).__motoUpdateRef = () => updateRef;

    return () => {
      mounted = false;
      window.clearInterval(id);
    };
  }, []);

  const handleInstall = async () => {
    const update = (window as any).__motoUpdateRef?.();
    if (!update) return;

    setInstalling(true);
    setError(null);

    try {
      let downloaded = 0;
      let total = 0;
      await update.downloadAndInstall((event: any) => {
        switch (event.event) {
          case 'Started':
            total = event.data.contentLength ?? 0;
            setProgress({ downloaded: 0, total });
            break;
          case 'Progress':
            downloaded += event.data.chunkLength;
            setProgress({ downloaded, total });
            break;
          case 'Finished':
            setProgress({ downloaded: total, total });
            break;
        }
      });

      // Reinicia para que tome la nueva versión
      const { relaunch } = await import('@tauri-apps/plugin-process');
      await relaunch();
    } catch (e: any) {
      setError(e?.toString() || 'Error al instalar la actualización');
      setInstalling(false);
    }
  };

  if (!isTauri()) return null;
  if (!available) return null;
  if (dismissed === available.version) return null;

  const pct = progress && progress.total > 0
    ? Math.round((progress.downloaded / progress.total) * 100)
    : 0;

  return (
    <div style={{
      position: 'fixed',
      bottom: 16,
      right: 16,
      zIndex: 9999,
      background: 'var(--color-surface, #1a1d2e)',
      border: '1px solid var(--color-primary, #6c75f6)',
      borderRadius: 12,
      padding: '14px 16px',
      boxShadow: '0 8px 32px rgba(0,0,0,0.4)',
      maxWidth: 360,
      color: 'var(--color-text, #fff)',
      fontSize: 13,
      fontFamily: 'inherit',
    }}>
      <div style={{ display: 'flex', alignItems: 'flex-start', gap: 10 }}>
        <Download size={18} style={{ color: 'var(--color-primary, #6c75f6)', flexShrink: 0, marginTop: 2 }} />
        <div style={{ flex: 1 }}>
          <div style={{ fontWeight: 700, marginBottom: 2 }}>
            Nueva versión disponible: v{available.version}
          </div>
          {available.body && (
            <div style={{ fontSize: 11, color: 'var(--color-text-muted, #aaa)', marginBottom: 8, maxHeight: 60, overflow: 'auto' }}>
              {available.body.slice(0, 200)}
            </div>
          )}

          {installing && progress && (
            <div style={{ marginBottom: 8 }}>
              <div style={{ fontSize: 11, marginBottom: 4 }}>
                Descargando... {pct}%
              </div>
              <div style={{ height: 4, background: 'rgba(255,255,255,0.1)', borderRadius: 2, overflow: 'hidden' }}>
                <div style={{ height: '100%', width: `${pct}%`, background: 'var(--color-primary, #6c75f6)', transition: 'width 0.2s' }} />
              </div>
            </div>
          )}

          {error && (
            <div style={{ fontSize: 11, color: 'var(--color-danger, #dc3545)', marginBottom: 6 }}>
              {error}
            </div>
          )}

          {!installing && (
            <div style={{ display: 'flex', gap: 6 }}>
              <button
                className="btn btn-primary btn-sm"
                onClick={handleInstall}
                style={{ fontSize: 12, padding: '6px 12px' }}
              >
                Actualizar ahora
              </button>
              <button
                className="btn btn-ghost btn-sm"
                onClick={() => setDismissed(available.version)}
                style={{ fontSize: 12, padding: '6px 12px' }}
              >
                Después
              </button>
            </div>
          )}
        </div>
        {!installing && (
          <button
            onClick={() => setDismissed(available.version)}
            style={{ background: 'none', border: 'none', color: 'var(--color-text-dim, #888)', cursor: 'pointer', padding: 0 }}
            aria-label="Cerrar"
          >
            <X size={14} />
          </button>
        )}
      </div>
    </div>
  );
}
