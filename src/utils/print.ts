// utils/print.ts — Impresión híbrida: WebView (Windows) → Navegador fallback (macOS)

import { invoke } from '@tauri-apps/api/core';

export async function printHTML(html: string): Promise<void> {
  // Intentar primero con WebView (funciona en Windows/WebView2)
  const ok = await tryWebViewPrint(html);
  if (!ok) {
    // Fallback: abrir en navegador del sistema (macOS/WebKit)
    try {
      await invoke('imprimir_html', { html });
    } catch (e) {
      console.error('Error al imprimir:', e);
      alert('Error al imprimir: ' + String(e));
    }
  }
}

function tryWebViewPrint(html: string): Promise<boolean> {
  return new Promise((resolve) => {
    try {
      const iframe = document.createElement('iframe');
      iframe.style.cssText = 'position:fixed;right:0;bottom:0;width:0;height:0;border:0;opacity:0;';
      document.body.appendChild(iframe);

      const doc = iframe.contentDocument || iframe.contentWindow?.document;
      if (!doc) {
        iframe.remove();
        resolve(false);
        return;
      }

      doc.open();
      doc.write(html);
      doc.close();

      // Esperar a que renderice y luego intentar imprimir
      setTimeout(() => {
        try {
          const win = iframe.contentWindow;
          if (!win) { iframe.remove(); resolve(false); return; }

          // En WebView2 (Windows), esto funciona perfectamente
          // En WebKit (macOS), falla silenciosamente
          // Detectar si estamos en macOS para ir directo al fallback
          const isMac = navigator.userAgent.includes('Macintosh') ||
                        navigator.platform?.includes('Mac');

          if (isMac) {
            iframe.remove();
            resolve(false); // Ir directo al fallback en macOS
            return;
          }

          win.focus();
          win.print();
          setTimeout(() => iframe.remove(), 1000);
          resolve(true);
        } catch {
          iframe.remove();
          resolve(false);
        }
      }, 300);
    } catch {
      resolve(false);
    }
  });
}

export function escapeHTML(s: string): string {
  return s.replace(/[&<>"']/g, c =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]!));
}
