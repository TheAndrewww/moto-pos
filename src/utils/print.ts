// utils/print.ts

export async function printHTML(html: string): Promise<void> {
  const ok = await tryWebViewPrint(html);
  if (!ok) {
    console.warn('Impresión silenciosa falló.');
    alert(
      'No se pudo imprimir automáticamente.\n\n' +
      'Configura una impresora térmica ESC/POS en Ajustes.'
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
      if (!doc) { iframe.remove(); resolve(false); return; }

      doc.open();
      doc.write(html);
      doc.close();

      setTimeout(() => {
        try {
          const win = iframe.contentWindow;
          if (!win) { iframe.remove(); resolve(false); return; }
          win.focus();
          win.print();
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

// Utiliza la ventana principal de la aplicación para disparar el diálogo de
// impresión del SO con soporte de @media print. Lo usa la impresión de
// etiquetas (lib/imprimirEtiquetas.ts); los tickets van por printHTML.
//
// IMPORTANTE para quien lo llame: window.print() se lanza en el mismo task en
// que se inserta el HTML, y la impresión por script de Chromium / WebView2
// (Windows) toma la página tal como está en ese instante. El contenido NO debe
// depender de cargas (<img>, fuentes web, CSS externo): usar SVG en línea y
// fuentes del sistema.
export function printHTMLDialogOverlay(htmlContent: string): void {
  // Si quedó un overlay de una impresión anterior (en macOS Tauri reemplaza
  // window.print por un invoke y 'afterprint' no llega, así que el overlay
  // vive hasta 2 min), quitarlo: si no, se imprimen los dos.
  document
    .querySelectorAll('#print-dialog-overlay, #print-dialog-style')
    .forEach(el => el.remove());

  const container = document.createElement('div');
  container.id = 'print-dialog-overlay';
  container.innerHTML = htmlContent;
  document.body.appendChild(container);

  const style = document.createElement('style');
  style.id = 'print-dialog-style';
  style.innerHTML = `
    @media print {
      /* index.css fija html, body, #root { height:100%; overflow:hidden }:
         sin esto el contenido impreso se recorta a una "pantalla". */
      html, body { height: auto !important; overflow: visible !important; }
      body > :not(#print-dialog-overlay) {
        display: none !important;
      }
      #print-dialog-overlay {
        display: block !important;
        position: static;
        width: auto;
        margin: 0;
        padding: 0;
        background: white;
      }
    }
    @media screen {
      #print-dialog-overlay {
        display: none !important;
      }
    }
  `;
  document.head.appendChild(style);

  const cleanup = () => {
    container.remove();
    style.remove();
    window.removeEventListener('afterprint', cleanup);
  };

  window.addEventListener('afterprint', cleanup);
  setTimeout(cleanup, 120000); // 2 minutos máximo

  // Red de seguridad para llamadores futuros: si el contenido trae <img> que
  // aún no cargan (un window.print() inmediato los imprime en blanco en
  // Chromium/WebView2), esperar a que decodifiquen. Ojo: imprimir después de
  // un await pierde el gesto del usuario en Safari/WebKit, por eso las
  // etiquetas usan SVG en línea y nunca pasan por aquí.
  const pendientes = Array.from(container.querySelectorAll('img'))
    .filter(img => !img.complete || img.naturalWidth === 0);
  if (pendientes.length === 0) {
    // Lanzar el print nativo de forma síncrona para no perder el contexto de
    // evento de usuario (WebKit) y con el contenido ya pintado.
    window.print();
    return;
  }
  Promise.all(pendientes.map(img => img.decode().catch(() => undefined)))
    .then(() => {
      // Si mientras tanto otra impresión reemplazó este overlay, no imprimir.
      if (container.isConnected) window.print();
    });
}
