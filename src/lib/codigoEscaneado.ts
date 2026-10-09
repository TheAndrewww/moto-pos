// lib/codigoEscaneado.ts — Resolver lo que llega del lector de códigos.
//
// Dos problemas reales de la tienda:
//
// 1. Guion convertido en apóstrofo. Cuando el lector está configurado con
//    teclado en inglés y Windows en español, la tecla del guion llega como
//    "'": la etiqueta CDI-023 se lee "CDI'023". Ningún código del inventario
//    lleva apóstrofo (y 4 de cada 5 llevan guion), así que se convierte de
//    vuelta sin riesgo.
//
// 2. Códigos de proveedor con demasiada información. El proveedor Alessia
//    imprime en su código de barras su clave + la clave corta del producto +
//    la descripción, p.ej.:
//      S017'KEY26 CDI'023 UNIDAD DE CDI 12 Vcc DM'125 17'19 - DM'150 ...
//    La tienda registra el producto con la clave corta (CDI-023), que viene
//    DENTRO de esa cadena. Se busca cualquier código del inventario que
//    aparezca como palabra completa dentro de lo escaneado.
//
// Orden: código exacto (como siempre) → exacto tras normalizar → contenido.
// Si dentro de la cadena hay más de un código posible, quien llama debe dejar
// elegir al usuario (nunca se adivina).

/** Mínimo de caracteres de un código para buscarlo DENTRO de una cadena
 *  (evita que códigos como "12" o "DE" coincidan con palabras sueltas). */
const MIN_LARGO_CONTENIDO = 4;

/** Dentro de una cadena solo se buscan códigos con al menos un dígito: hay
 *  productos registrados con una palabra común como código ("cilindro",
 *  "LLAVERO CHICO") que aparecerían en cualquier descripción del proveedor. */
const TIENE_DIGITO = /[0-9]/;

/** Caracteres que separan "palabras" dentro de lo escaneado. El guion NO
 *  separa: DM-125 es una sola palabra (si separara, un código "DM" se
 *  detectaría dentro de "DM-125"). */
const SEPARADORES = new Set([' ', '\t', ',', ';', ':', '|', '(', ')', '[', ']', '{', '}', '"', '*']);

/** Lo escaneado tal como debe compararse: sin espacios en los extremos,
 *  apóstrofos/acentos sueltos convertidos a guion y espacios repetidos
 *  reducidos a uno. Mayúsculas/minúsculas se respetan aquí (la comparación
 *  las ignora). */
export function normalizarCodigoEscaneado(raw: string): string {
  return raw
    .replace(/['’‘´`]/g, '-')
    .replace(/\s+/g, ' ')
    .trim();
}

export interface ConCodigo {
  codigo: string;
}

export type ResultadoEscaneo<P extends ConCodigo> =
  | { tipo: 'producto'; producto: P; via: 'exacto' | 'normalizado' | 'contenido' }
  | { tipo: 'varios'; candidatos: P[] }
  | { tipo: 'ninguno'; texto: string };

function esSeparador(c: string | undefined): boolean {
  return c === undefined || SEPARADORES.has(c);
}

/** ¿`codigo` aparece en `texto` como palabra completa? (ambos en mayúsculas) */
function contieneComoPalabra(texto: string, codigo: string): number {
  let desde = 0;
  while (desde <= texto.length - codigo.length) {
    const i = texto.indexOf(codigo, desde);
    if (i < 0) return -1;
    if (esSeparador(texto[i - 1]) && esSeparador(texto[i + codigo.length])) return i;
    desde = i + 1;
  }
  return -1;
}

/**
 * Busca el producto que corresponde a lo escaneado.
 * `productos` debe ser la lista de productos ACTIVOS que la pantalla ya tiene.
 */
export function resolverCodigoEscaneado<P extends ConCodigo>(
  raw: string,
  productos: P[],
): ResultadoEscaneo<P> {
  const crudo = raw.trim();
  const texto = normalizarCodigoEscaneado(raw);
  if (!crudo) return { tipo: 'ninguno', texto };

  // 1. Exacto, como siempre.
  const exacto = productos.find(p => p.codigo === crudo);
  if (exacto) return { tipo: 'producto', producto: exacto, via: 'exacto' };

  // 2. Exacto tras normalizar (apóstrofo → guion), sin distinguir mayúsculas.
  const textoMay = texto.toUpperCase();
  const normalizado = productos.filter(p => normalizarCodigoEscaneado(p.codigo).toUpperCase() === textoMay);
  if (normalizado.length === 1) return { tipo: 'producto', producto: normalizado[0], via: 'normalizado' };
  if (normalizado.length > 1) return { tipo: 'varios', candidatos: normalizado };

  // 3. Algún código del inventario DENTRO de la cadena (como palabra completa).
  //    Solo tiene sentido si lo escaneado es más largo que un código normal.
  if (!textoMay.includes(' ')) return { tipo: 'ninguno', texto };
  const hallados: { p: P; pos: number; cod: string }[] = [];
  for (const p of productos) {
    const cod = normalizarCodigoEscaneado(p.codigo).toUpperCase();
    if (cod.length < MIN_LARGO_CONTENIDO || !TIENE_DIGITO.test(cod)) continue;
    const pos = contieneComoPalabra(textoMay, cod);
    if (pos >= 0) hallados.push({ p, pos, cod });
  }
  if (hallados.length === 0) return { tipo: 'ninguno', texto };

  // Si un código hallado está contenido en otro también hallado (p.ej.
  // "CDI-02" y "CDI-023"), el más corto es parte del más largo: se descarta.
  const utiles = hallados.filter(h =>
    !hallados.some(o => o !== h && o.cod.length > h.cod.length && o.cod.includes(h.cod)),
  );
  // Primero el que aparece antes en la cadena: en el formato de Alessia la
  // clave corta va al principio, así que la pantalla lo deja preseleccionado.
  utiles.sort((a, b) => a.pos - b.pos || b.cod.length - a.cod.length);
  if (utiles.length === 1) return { tipo: 'producto', producto: utiles[0].p, via: 'contenido' };
  return { tipo: 'varios', candidatos: utiles.map(h => h.p) };
}
