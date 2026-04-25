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

// Utiliza la ventana principal de la aplicación para disparar de manera garantizada
// el cuadro de diálogo de impresión del Sistema Operativo con soporte de @media print.
export function printHTMLDialogOverlay(htmlContent: string): void {
  const container = document.createElement('div');
  container.id = 'print-dialog-overlay';
  container.innerHTML = htmlContent;
  document.body.appendChild(container);

  const style = document.createElement('style');
  style.id = 'print-dialog-style';
  style.innerHTML = `
    @media print {
      body > :not(#print-dialog-overlay) {
        display: none !important;
      }
      #print-dialog-overlay {
        display: block !important;
        position: absolute;
        left: 0;
        top: 0;
        width: 100%;
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

  // Lanzar el print nativo que forzosamente abre el diálogo
  setTimeout(() => window.print(), 100);
}
