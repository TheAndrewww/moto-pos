// utils/print.ts — Impresión vía iframe oculto (sin abrir ventanas externas)
//
// Estrategia: renderizar el HTML en un iframe invisible y llamar window.print()
// dentro de él. La ruta ideal es la impresora térmica ESC/POS configurada en
// Ajustes (silenciosa). Este fallback solo entra si no hay térmica.
//
// Comportamiento por plataforma del fallback:
//   - Windows (WebView2): imprime silenciosamente sin diálogo.
//   - macOS (WebKit):     muestra el diálogo de impresión del sistema (1 clic).
//                         No abre ventana de navegador.
//
// Si la impresión via iframe falla (raro), mostramos un aviso. Antes el fallback
// abría una pestaña en el navegador con el ticket; eso fue eliminado a petición.

export async function printHTML(html: string): Promise<void> {
  const ok = await tryWebViewPrint(html);
  if (!ok) {
    // No abrimos navegador — solo notificamos al usuario.
    console.warn('Impresión silenciosa falló. Configurar impresora térmica en Ajustes.');
    alert(
      'No se pudo imprimir el ticket automáticamente.\n\n' +
      'Para impresión silenciosa configura una impresora térmica ESC/POS ' +
      'en Ajustes → Impresora.'
    );
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

      // Esperar a que renderice y luego imprimir directo desde el iframe.
      setTimeout(() => {
        try {
          const win = iframe.contentWindow;
          if (!win) { iframe.remove(); resolve(false); return; }

          win.focus();
          win.print();

          // Quitar el iframe después; si la diálogo de macOS bloquea el
          // hilo de UI, este timeout solo corre cuando se cierra.
          setTimeout(() => iframe.remove(), 1500);
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
