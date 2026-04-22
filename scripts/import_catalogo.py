#!/usr/bin/env python3
"""
import_catalogo.py — Importa productos desde el PDF de catálogo MyTPV
                     a la base de datos del POS Moto Refaccionaria.

Uso:
    python3 scripts/import_catalogo.py /ruta/al/catalogo.pdf [--db /ruta/db.sqlite] [--dry-run]

Por defecto la base de datos se busca en:
    ~/Library/Application Support/com.motorefaccionaria.pos/pos_database.db   (macOS)
    ~/.local/share/com.motorefaccionaria.pos/pos_database.db                  (Linux)

Requisitos: poppler (pdftotext)  →  brew install poppler

El script:
  1. Convierte el PDF a texto plano respetando el layout (pdftotext -layout).
  2. Detecta cada fila de producto por la firma de columnas
     (precio_costo, precio_venta, stock_tienda, sección, proveedor, stock_almacén, "Almacen N").
  3. Reconstruye descripciones multilínea uniendo las líneas huérfanas
     adyacentes (sin código ni precios) según una heurística basada en
     si la fila de datos trae descripción inline o no.
  4. Resuelve / crea proveedores por nombre (sección "PROVEEDOR" del PDF).
  5. Inserta en `productos` con INSERT OR IGNORE sobre `codigo` (re-ejecutable).

Imprime un resumen al final: insertados, actualizados, omitidos por código duplicado, errores.
"""

from __future__ import annotations

import argparse
import os
import re
import sqlite3
import subprocess
import sys
import unicodedata
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


# ─── Configuración ─────────────────────────────────────────

DEFAULT_DB_MAC = Path.home() / "Library/Application Support/com.motorefaccionaria.pos/pos_database.db"
DEFAULT_DB_LINUX = Path.home() / ".local/share/com.motorefaccionaria.pos/pos_database.db"

# Cola fija de cada fila de datos (precios + stock + proveedor + almacén)
DATA_TAIL_RE = re.compile(
    r"\s+(?P<costo>\d+\.\d+)\s+"
    r"(?P<venta>\d+\.\d+)\s+"
    r"(?P<stock_t>-?\d+)\s+"
    r"(?P<sec_t>\S+)\s+"
    r"(?P<proveedor>\S+)\s+"
    r"(?P<stock_a>-?\d+\.\d+)\s+"
    r"Almacen\s+\d+\s*$"
)

# Líneas a ignorar (encabezado, pie, totales)
SKIP_PATTERNS = [
    re.compile(r"^\s*LB MOTOREFACCIONES"),
    re.compile(r"^\s*RFC:"),
    re.compile(r"^\s*Prol\. Juarez"),
    re.compile(r"^\s*Tel[eé]fono:"),
    re.compile(r"^\s*CATALOGO DE PRODUCTOS"),
    re.compile(r"^\s*Generado por MyTPV"),
    re.compile(r"^\s*PRECIO\s+PRECIO\s+EXIST"),
    re.compile(r"^\s*CODIGO\s+DESCRIPCION"),
    re.compile(r"^\s*COSTO\s+VENTA"),
    re.compile(r"^\s*Total Registros:"),
    re.compile(r"^\s*Total a Precio"),
    re.compile(r"^\s*Total Existencias:"),
    re.compile(r"^\s*$"),
]


# ─── Modelo ────────────────────────────────────────────────

@dataclass
class Producto:
    codigo: str
    descripcion_partes: list[str] = field(default_factory=list)
    precio_costo: float = 0.0
    precio_venta: float = 0.0
    stock_tienda: float = 0.0
    seccion_tienda: str = ""
    proveedor: str = ""
    stock_almacen: float = 0.0

    @property
    def descripcion(self) -> str:
        # Une fragmentos colapsando espacios
        partes = [p.strip() for p in self.descripcion_partes if p.strip()]
        return re.sub(r"\s+", " ", " ".join(partes)).strip()

    @property
    def stock_total(self) -> float:
        return self.stock_tienda + self.stock_almacen


# ─── Parsing ───────────────────────────────────────────────

def es_linea_ignorable(line: str) -> bool:
    return any(p.match(line) for p in SKIP_PATTERNS)


def extraer_texto_pdf(pdf_path: Path) -> str:
    """Llama pdftotext -layout y devuelve el contenido."""
    if not pdf_path.exists():
        sys.exit(f"❌ No existe el PDF: {pdf_path}")
    try:
        out = subprocess.run(
            ["pdftotext", "-layout", str(pdf_path), "-"],
            check=True, capture_output=True, text=True,
        )
        return out.stdout
    except FileNotFoundError:
        sys.exit("❌ Falta `pdftotext`. Instala poppler:  brew install poppler")
    except subprocess.CalledProcessError as e:
        sys.exit(f"❌ Error al convertir PDF: {e.stderr}")


def es_desc_fragmento(desc: str) -> bool:
    """Detecta si una descripción inline es un fragmento (ej: '18-', '/', vacío)."""
    desc = desc.strip()
    if not desc:
        return True
    if len(desc) <= 4:
        return True
    if desc.endswith("-") and len(desc) <= 6:
        return True
    return False


def parsear_data_line(line: str) -> Optional[dict]:
    """
    Detecta la cola fija (precios+stock+almacén). Si está, separa el prefijo
    en (código, descripción inline). El código puede estar vacío si la fila
    no incluye código (los reportes de MyTPV a veces parten el código en
    líneas separadas cuando es muy largo).
    """
    m = DATA_TAIL_RE.search(line)
    if not m:
        return None
    prefix = line[: m.start()].rstrip()
    if not prefix.strip():
        codigo = ""
        desc_inline = ""
    elif prefix[0].isspace():
        # Sin código: solo descripción precedida de espacios
        codigo = ""
        desc_inline = prefix.strip()
    else:
        partes = prefix.split(None, 1)
        codigo = partes[0]
        desc_inline = partes[1].strip() if len(partes) > 1 else ""
    d = m.groupdict()
    d["codigo"] = codigo
    d["desc"] = desc_inline
    return d


def es_codigo_prefijo(raw: str) -> Optional[tuple[str, str]]:
    """
    Si la línea (sin recortar) empieza en columna 0 con un token tipo 'XXX-' (que
    sugiere un código truncado), devuelve (prefijo_codigo, desc_resto).
    """
    if not raw or raw[0].isspace():
        return None
    m = re.match(r"^(\S+-)\s+(.*)$", raw)
    if m:
        return m.group(1), m.group(2).strip()
    m = re.match(r"^(\S+-)\s*$", raw)
    if m:
        return m.group(1), ""
    return None


def es_codigo_sufijo(raw: str) -> Optional[tuple[str, str]]:
    """
    Detecta una línea que empieza en columna 0 con un token corto (sufijo de código
    como '1001', '2XL', 'XL', 'L'). Devuelve (sufijo, desc_resto).
    """
    if not raw or raw[0].isspace():
        return None
    m = re.match(r"^(\S+)\s+(.*)$", raw)
    if m:
        return m.group(1), m.group(2).strip()
    m = re.match(r"^(\S+)\s*$", raw)
    if m:
        return m.group(1), ""
    return None


def parsear_catalogo(texto: str) -> list[Producto]:
    """
    Pasada 1: extrae cada data line con sus huérfanos posteriores. Si la data
              line no trae código, intenta reconstruirlo desde un "prefijo"
              (línea anterior tipo 'XXX-') más un "sufijo" (próxima línea no-data
              en columna 0).
    Pasada 2: distribuye huérfanos entre productos consecutivos por heurística.
    """
    raw_records: list[tuple[dict, list[str]]] = []
    pre_orphans: list[str] = []
    current_orphans: list[str] = []          # strings ya .strip()ados (para descripción)
    current_orphans_raw: list[str] = []      # mismas líneas pero sin tocar (para detectar columna)
    visto_primero = False
    pending_suffix_for: Optional[int] = None  # índice del producto que espera sufijo de código

    for raw in texto.splitlines():
        line = raw.rstrip()
        if es_linea_ignorable(line):
            continue

        # ¿Es la línea esperada de "sufijo de código" para un producto pendiente?
        if pending_suffix_for is not None and not DATA_TAIL_RE.search(line):
            suf = es_codigo_sufijo(line)
            if suf:
                sufijo, desc_extra = suf
                d_pend = raw_records[pending_suffix_for][0]
                d_pend["codigo"] = (d_pend["codigo"] + sufijo).strip()
                if desc_extra:
                    d_pend["desc"] = (d_pend["desc"] + " " + desc_extra).strip()
                pending_suffix_for = None
                continue
            # si no parece sufijo, lo dejamos como huérfano normal
            pending_suffix_for = None

        d = parsear_data_line(line)
        if d:
            if visto_primero:
                raw_records[-1] = (raw_records[-1][0], current_orphans)
            else:
                pre_orphans = current_orphans
                visto_primero = True

            # Reconstruir código faltante
            if not d["codigo"]:
                # Buscar el último prefijo (línea col-0 terminada en '-') en los huérfanos
                idx_prefijo = None
                for j in range(len(current_orphans_raw) - 1, -1, -1):
                    pref = es_codigo_prefijo(current_orphans_raw[j])
                    if pref:
                        idx_prefijo = j
                        prefijo, desc_extra = pref
                        d["codigo"] = prefijo
                        if desc_extra:
                            d["desc"] = (desc_extra + " " + d["desc"]).strip()
                        break
                if idx_prefijo is not None:
                    # Recortar los huérfanos: lo anterior queda como continuación del previo
                    current_orphans = [s.strip() for s in current_orphans_raw[:idx_prefijo] if s.strip()]
                    current_orphans_raw = current_orphans_raw[:idx_prefijo]
                # esperar sufijo en próxima línea col-0
                pending_suffix_for = len(raw_records)

            raw_records.append((d, []))
            current_orphans = []
            current_orphans_raw = []
        else:
            txt = line.strip()
            if txt:
                current_orphans.append(txt)
                current_orphans_raw.append(line)

    if raw_records:
        raw_records[-1] = (raw_records[-1][0], current_orphans)

    # Pasada 2 — construir productos asignando preámbulo / continuación
    productos: list[Producto] = []
    # huérfanos antes del primer producto → preámbulo del primero
    pending_preamble: list[str] = pre_orphans

    for i, (d, post_orphans) in enumerate(raw_records):
        desc_inline = d["desc"].strip()
        prod = Producto(codigo=d["codigo"])
        if pending_preamble:
            prod.descripcion_partes.extend(pending_preamble)
            pending_preamble = []
        if desc_inline:
            prod.descripcion_partes.append(desc_inline)

        prod.precio_costo   = float(d["costo"])
        prod.precio_venta   = float(d["venta"])
        prod.stock_tienda   = float(d["stock_t"])
        prod.seccion_tienda = d["sec_t"]
        prod.proveedor      = d["proveedor"]
        prod.stock_almacen  = float(d["stock_a"])
        productos.append(prod)

        # Distribuir post_orphans entre este producto y el siguiente
        if not post_orphans:
            continue

        siguiente = raw_records[i + 1] if i + 1 < len(raw_records) else None
        if siguiente is None:
            # último producto: todo es continuación
            prod.descripcion_partes.extend(post_orphans)
            continue

        sig_desc = siguiente[0]["desc"].strip()
        prev_desc_inline = d["desc"].strip()
        sig_es_frag = es_desc_fragmento(sig_desc)
        prev_es_frag = es_desc_fragmento(prev_desc_inline)

        if sig_es_frag and prev_es_frag:
            # Ambos productos necesitan descripción → repartir.
            # 1 huérfano al previo (su continuación de envoltura), resto al siguiente.
            if len(post_orphans) == 1:
                pending_preamble = post_orphans
            else:
                prod.descripcion_partes.append(post_orphans[0])
                pending_preamble = post_orphans[1:]
        elif sig_es_frag and not prev_es_frag:
            # Previo ya tiene descripción inline; siguiente no → todo al siguiente.
            pending_preamble = post_orphans
        else:
            # Siguiente tiene desc real → no necesita preámbulo. Todo al previo.
            prod.descripcion_partes.extend(post_orphans)

    return productos


# ─── Importación a SQLite ─────────────────────────────────

def normalizar(s: str) -> str:
    """Quita acentos y baja a minúsculas para `search_text`."""
    nfkd = unicodedata.normalize("NFKD", s)
    sin_acentos = "".join(c for c in nfkd if not unicodedata.combining(c))
    return sin_acentos.lower().strip()


def detectar_tipo_codigo(codigo: str) -> str:
    """Heurística: 13 dígitos = EAN13, todo dígitos otra longitud = CODE128, demás = INTERNO."""
    if codigo.isdigit() and len(codigo) == 13:
        return "EAN13"
    if codigo.isdigit():
        return "CODE128"
    return "INTERNO"


def upsert_proveedor(conn: sqlite3.Connection, nombre: str, cache: dict[str, int]) -> Optional[int]:
    nombre = nombre.strip()
    if not nombre or nombre == "-":
        return None
    if nombre in cache:
        return cache[nombre]
    row = conn.execute("SELECT id FROM proveedores WHERE nombre = ?", (nombre,)).fetchone()
    if row:
        cache[nombre] = row[0]
        return row[0]
    cur = conn.execute(
        "INSERT INTO proveedores (nombre) VALUES (?)",
        (nombre,),
    )
    cache[nombre] = cur.lastrowid
    return cur.lastrowid


def importar(productos: list[Producto], db_path: Path, dry_run: bool) -> None:
    if dry_run:
        print(f"🧪 DRY RUN — sin escribir en DB. {len(productos)} productos parseados.\n")
        for p in productos[:5]:
            print(f"  · {p.codigo:20s} | {p.descripcion[:60]:60s} | ${p.precio_venta:>8.2f} | "
                  f"stock {p.stock_total:>5.1f} | prov: {p.proveedor:10s} | sec: {p.seccion_tienda}")
        if len(productos) > 5:
            print(f"  ... y {len(productos) - 5} más.\n")
        return

    if not db_path.exists():
        sys.exit(f"❌ No se encontró la BD: {db_path}\n   Inicia la app del POS al menos una vez para crearla.")

    print(f"🗄️  Conectando a {db_path}")
    conn = sqlite3.connect(str(db_path))
    conn.execute("PRAGMA foreign_keys = ON")
    conn.execute("PRAGMA journal_mode = WAL")

    proveedor_cache: dict[str, int] = {}
    insertados = 0
    actualizados = 0
    omitidos = 0
    errores: list[tuple[str, str]] = []

    try:
        conn.execute("BEGIN")
        for p in productos:
            try:
                proveedor_id = upsert_proveedor(conn, p.proveedor, proveedor_cache)
                desc = p.descripcion or p.codigo
                # nombre = primera línea / hasta 80 chars; descripción = todo
                nombre = desc[:120]
                search = normalizar(f"{p.codigo} {desc}")
                tipo_cod = detectar_tipo_codigo(p.codigo)
                stock = p.stock_total

                # ¿Ya existe?
                row = conn.execute(
                    "SELECT id FROM productos WHERE codigo = ?", (p.codigo,)
                ).fetchone()

                if row:
                    conn.execute(
                        """UPDATE productos
                           SET nombre = ?, descripcion = ?, precio_costo = ?, precio_venta = ?,
                               stock_actual = ?, proveedor_id = ?, search_text = ?,
                               codigo_tipo = ?, updated_at = datetime('now')
                           WHERE id = ?""",
                        (nombre, desc, p.precio_costo, p.precio_venta, stock,
                         proveedor_id, search, tipo_cod, row[0]),
                    )
                    actualizados += 1
                else:
                    conn.execute(
                        """INSERT INTO productos
                           (codigo, codigo_tipo, nombre, descripcion,
                            precio_costo, precio_venta, stock_actual, stock_minimo,
                            proveedor_id, search_text, activo)
                           VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?, 1)""",
                        (p.codigo, tipo_cod, nombre, desc,
                         p.precio_costo, p.precio_venta, stock,
                         proveedor_id, search),
                    )
                    insertados += 1
            except sqlite3.IntegrityError as e:
                omitidos += 1
                errores.append((p.codigo, str(e)))
            except Exception as e:
                errores.append((p.codigo, str(e)))
        conn.commit()
    except Exception:
        conn.rollback()
        raise
    finally:
        conn.close()

    print()
    print(f"✅ Insertados:   {insertados}")
    print(f"🔄 Actualizados: {actualizados}")
    print(f"⏭️  Omitidos:    {omitidos}")
    print(f"🏷️  Proveedores nuevos en cache: {len(proveedor_cache)}")
    if errores:
        print(f"\n⚠️  {len(errores)} errores. Primeros 10:")
        for c, e in errores[:10]:
            print(f"   {c}: {e}")


# ─── Main ─────────────────────────────────────────────────

def db_default() -> Path:
    if sys.platform == "darwin":
        return DEFAULT_DB_MAC
    return DEFAULT_DB_LINUX


def main() -> None:
    ap = argparse.ArgumentParser(description="Importa catálogo MyTPV (PDF) al POS")
    ap.add_argument("pdf", type=Path, help="Ruta al PDF del catálogo")
    ap.add_argument("--db", type=Path, default=db_default(),
                    help=f"Ruta a la BD SQLite (default: {db_default()})")
    ap.add_argument("--dry-run", action="store_true",
                    help="Solo parsea y muestra resumen, no escribe en BD")
    args = ap.parse_args()

    print(f"📄 Leyendo PDF: {args.pdf}")
    texto = extraer_texto_pdf(args.pdf)

    print("🔍 Parseando productos...")
    productos = parsear_catalogo(texto)
    print(f"   → {len(productos)} productos detectados\n")

    if not productos:
        sys.exit("❌ No se detectó ningún producto. Revisa el formato del PDF.")

    importar(productos, args.db, args.dry_run)


if __name__ == "__main__":
    main()
