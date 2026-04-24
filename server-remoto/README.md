# Moto Refaccionaria — Servidor remoto (Fase 3.2)

Servidor HTTP que acepta `POST /sync/push` y `GET /sync/pull` del POS,
expone REST `/api/*` para el panel web y hace login email+password vía
`POST /auth/login`.

Stack: Rust · axum 0.7 · sqlx 0.8 · Postgres.

## Rutas

| Método | Ruta                  | Quién          |
|--------|-----------------------|----------------|
| GET    | `/health`             | cualquiera     |
| POST   | `/auth/login`         | web admin      |
| POST   | `/sync/push`          | POS (Bearer)   |
| GET    | `/sync/pull`          | POS (Bearer)   |
| GET    | `/api/dashboard`      | web (Bearer)   |
| GET    | `/api/sucursales`     | web            |
| GET    | `/api/productos`      | web            |
| POST   | `/api/productos`      | web            |
| PUT    | `/api/productos/:uuid`| web            |
| DELETE | `/api/productos/:uuid`| web (soft)     |
| GET    | `/api/categorias`     | web            |
| GET    | `/api/proveedores`    | web            |
| GET    | `/api/clientes`       | web            |
| GET    | `/api/ventas`         | web            |
| GET    | `/api/ventas/:uuid`   | web            |

## Correr local

```bash
# 1. Postgres local (docker)
docker run --name moto-pg -e POSTGRES_PASSWORD=postgres \
  -p 5432:5432 -d postgres:16

# 2. Env
cp .env.example .env
# edita DATABASE_URL y JWT_SECRET (mínimo 16 chars)

# 3. Correr
cargo run --bin moto-server
# Migraciones se corren automáticamente en arranque.

# 4. Crear admin inicial (hasta que haya UI de signup)
#   - password "admin123" se hashea con bcrypt
#   - reemplaza por tu email/password
psql $DATABASE_URL <<'SQL'
INSERT INTO admin_users (email, password_hash, nombre, es_super_admin)
VALUES (
  'admin@moto.mx',
  '$2b$12$N9qo8uLOickgx2ZMRZoMyeIjZAgcfl7p92ldGxad68LJZdL17lhWy',
  'Administrador',
  TRUE
);
SQL
# El hash de arriba corresponde a "admin123". Generar uno real:
#   python3 -c "import bcrypt; print(bcrypt.hashpw(b'tu-pass', bcrypt.gensalt(12)).decode())"
```

Health:
```bash
curl http://localhost:3000/health    # -> ok
```

Login:
```bash
curl -X POST http://localhost:3000/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"admin@moto.mx","password":"admin123"}'
```

## Deploy en Railway

1. `railway init` en este directorio.
2. Agrega un plugin **Postgres** → Railway inyecta `DATABASE_URL`.
3. Variables:
   - `JWT_SECRET` — random 32+ bytes (ej. `openssl rand -hex 32`)
   - `PORT` — Railway lo inyecta, el binario lo lee
   - `RUST_LOG` — `info,moto_server=debug` (opcional)
4. Railway detecta Cargo.toml y compila con `cargo build --release`.
5. Start command: `./target/release/moto-server`
6. Las migraciones corren solas en el primer `run_migrations()` del arranque.

Tras el deploy:
- El POS se configura con `obtener_estado_sync` / `configurar_sync`
  (comandos Tauri) apuntando a `https://tu-app.up.railway.app`.
- El panel web (pos/web-admin) se despliega aparte (Vercel, Netlify, o
  estáticos en Railway) apuntando `VITE` o `BASE_KEY` al dominio del servidor.

## Notas de diseño

- **LWW sobre `updated_at`** como TEXTO (`YYYY-MM-DD HH24:MI:SS`) para
  que la comparación sea idéntica a la del POS (SQLite) sin tocar tipos.
- **sync_cursor** es la fuente única de verdad para `pull`: cada escritura
  (desde push POS, o desde endpoints /api/*) inserta una fila aquí.
  Otros dispositivos hacen pull ordenado por `id` y excluyen los que
  ellos mismos originaron (`origen_device`).
- **Agregados** (ventas, cortes, etc.): el padre se envía con
  `children: { tabla_hijo: [...] }`; el servidor reemplaza atomicamente.
- **Soft delete**: `deleted_at = now()`, nunca `DELETE` físico, para que
  otros nodos reciban el tombstone.

## Seguridad

- Bearer JWT de 30 días en todas las rutas excepto `/health` y `/auth/login`.
- CORS permissive en dev — **bloquear en prod** editando `CorsLayer`
  para whitelistear el dominio del panel web.
- El admin inicial se crea a mano; agregar gestión de admins desde el
  panel es trabajo posterior.
