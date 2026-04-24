# Guía de Producción — Moto Refaccionaria POS

Este documento cubre todo lo necesario para llevar el POS a producción y mantener
las cajas actualizadas sin tocarlas físicamente.

---

## 1. Auto-actualización (Tauri Updater)

### Cómo funciona

```
┌──────────────┐    push tag v0.1.1    ┌─────────────────┐
│  Tu máquina  │ ────────────────────▶ │  GitHub Actions │
└──────────────┘                       └────────┬────────┘
                                                │ compila + firma
                                                ▼
                                       ┌─────────────────┐
                                       │ GitHub Release  │
                                       │ - Setup.exe     │
                                       │ - Setup.exe.sig │
                                       │ - latest.json   │
                                       └────────┬────────┘
                                                │ chequea cada 30 min
                                                ▼
                                       ┌─────────────────┐
                                       │  POS en caja    │
                                       │ → banner update │
                                       │ → instala+reboot│
                                       └─────────────────┘
```

### Setup inicial — UNA SOLA VEZ

#### 1.1 Guardar la llave privada en lugar seguro

La llave fue generada en `~/.tauri/moto-pos-updater.key`.
**Si la pierdes, los POS instalados no podrán actualizarse jamás** (porque la
pubkey está hardcodeada en cada instalador).

Cópiala a 1Password / Bitwarden / lo que uses, junto con la password:

```
Password: ****REDACTED — ver tu password manager****
Llave privada (archivo): ~/.tauri/moto-pos-updater.key
Llave pública (archivo): ~/.tauri/moto-pos-updater.key.pub
```

#### 1.2 Configurar secrets de GitHub

Ve a: https://github.com/TheAndrewww/moto-pos/settings/secrets/actions

Crea estos dos secrets:

| Nombre | Valor |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | Contenido completo del archivo `~/.tauri/moto-pos-updater.key` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | `****REDACTED — ver tu password manager****` |

Para copiar la llave privada:
```bash
cat ~/.tauri/moto-pos-updater.key | pbcopy
```

#### 1.3 Verificar que el endpoint apunta a tu repo

Ya está configurado en `src-tauri/tauri.conf.json`:
```json
"endpoints": [
  "https://github.com/TheAndrewww/moto-pos/releases/latest/download/latest.json"
]
```

GitHub Releases es público — cualquier POS con internet podrá descargar el update.
Si quieres releases privados, hay que cambiar a un servidor propio con auth.

---

### Proceso de release (cada vez que quieras publicar)

```bash
# 1. Asegúrate de que main esté limpio y los cambios commiteados
git status

# 2. Bump de versión en LOS DOS lados (deben coincidir)
#    - package.json     → "version": "0.1.1"
#    - src-tauri/tauri.conf.json → "version": "0.1.1"
#    - src-tauri/Cargo.toml → version = "0.1.1"

# 3. Commit del bump
git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml
git commit -m "chore: release v0.1.1"

# 4. Tag y push
git tag v0.1.1
git push origin main --tags
```

GitHub Actions automáticamente:
- Compila el `.exe` para Windows
- Lo firma con tu llave privada
- Genera `latest.json` con metadata + firma
- Crea un GitHub Release con todo

A los pocos minutos, cualquier POS abierto verá el banner y podrá actualizarse.

---

### Probar el updater (primera vez)

1. Compila localmente la versión actual e instálala en una PC de prueba:
   ```bash
   npx tauri build
   # El .exe queda en: src-tauri/target/release/bundle/nsis/
   ```
2. Sube tag `v0.1.1` y espera el release.
3. Abre el POS — debe aparecer el banner "Nueva versión disponible: v0.1.1".
4. Click "Actualizar ahora" → descarga, instala, reinicia.

Si el banner no aparece:
- Revisa la consola del Tauri (devtools): `console.warn '[updater] check failed:'`
- Verifica que el release tenga `latest.json` adjunto.
- Confirma que la pubkey en `tauri.conf.json` coincide con la de tu llave.

---

## 2. Servidor remoto (Railway)

### Variables de entorno requeridas

En Railway → Settings → Variables:

| Variable | Descripción | Ejemplo |
|---|---|---|
| `DATABASE_URL` | Inyectado automático por el plugin Postgres | (no tocar) |
| `JWT_SECRET` | Clave para firmar JWTs. Mínimo 32 chars. | `openssl rand -base64 48` |
| `PORT` | Inyectado por Railway | (no tocar) |
| `STATIC_DIR` | Dónde están los assets de la SPA | `./static` (default ok) |

### Backups de la BD

Railway Postgres incluye backups automáticos (revisa el plan). Adicional:
```bash
# Backup manual desde tu máquina
pg_dump $DATABASE_URL > backup-$(date +%F).sql
```

### Actualizar el server-remoto

Railway hace auto-deploy desde `main`. Cualquier push a `main` que toque
`server-remoto/` redespliega automáticamente.

Para actualizar la SPA web (lo que sirve el server):
```bash
npx vite build
cp -r dist/* server-remoto/static/
git add server-remoto/static
git commit -m "chore: rebuild SPA"
git push
```

---

## 3. Sincronización POS local ↔ Servidor

El POS local guarda todo primero en SQLite, y un worker async sincroniza con
Postgres en segundo plano cuando hay conexión. Si se cae internet, el POS sigue
operando — sincroniza cuando vuelva la red.

- **Cursor de sync**: tabla `sync_cursor` en ambos lados, con `origen_device`
  para evitar loops y aplicar LWW (last-write-wins).
- **Conflictos**: gana el más reciente por timestamp UTC.
- **Multi-caja**: cada caja tiene su `device_id` único; los push/pull son por
  device.

---

## 4. Checklist antes de poner en producción

- [ ] Secrets de GitHub configurados (`TAURI_SIGNING_PRIVATE_KEY` + password)
- [ ] Llave privada respaldada fuera de la máquina
- [ ] Railway: `JWT_SECRET` configurado con valor random fuerte (no el default)
- [ ] Railway: deploy verde y `/health` responde "ok"
- [ ] BD Postgres con backups habilitados
- [ ] Usuario admin/dueño creado en producción (no usar el seed de dev)
- [ ] Probar login PIN desde el celular contra el servidor de producción
- [ ] Probar `crear_venta` desde el POS local → confirmar que aparece en el dashboard web
- [ ] Compilar primer instalador firmado y probarlo en PC limpia
- [ ] Documentar IP/dominio del servidor para los empleados

---

## 5. Distribución a las cajas (primera instalación)

Solo se hace UNA VEZ por caja. Después, las actualizaciones son automáticas.

1. Bajar el `.exe` del último Release de GitHub:
   https://github.com/TheAndrewww/moto-pos/releases/latest
2. Buscar el archivo tipo `Moto Refaccionaria POS_0.1.0_x64-setup.exe`
3. Copiarlo a USB / red local de la tienda
4. Instalar en cada caja
5. Primer login con el dueño → crear PINs para vendedores
6. Listo. Las próximas versiones se instalarán solas.

---

## 6. Troubleshooting

| Síntoma | Causa probable | Solución |
|---|---|---|
| Banner de update no aparece | Sin internet o release no publicado | Revisar `https://github.com/TheAndrewww/moto-pos/releases/latest/download/latest.json` en navegador |
| "Failed to verify signature" | Pubkey en config no coincide con llave usada en CI | Verificar secret `TAURI_SIGNING_PRIVATE_KEY` |
| POS no sincroniza | Servidor caído o token expirado (7 días) | Logout + login. Revisar `/health` del server |
| `Address already in use :3000` (dev local) | Server-remoto previo no cerrado | `lsof -ti :3000 \| xargs kill -9` |
