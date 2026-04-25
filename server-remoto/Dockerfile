# Dockerfile para server-remoto (Railway).
# Multi-stage: compila Rust en imagen builder, corre binario en imagen slim.

# ---------- Stage 1: Builder ----------
FROM rust:1.82-slim AS builder

# Dependencias de sistema para compilar (sqlx-postgres usa OpenSSL via rustls,
# pero pkg-config y libssl-dev no estorban y dan compatibilidad).
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copiar manifests primero para cachear dependencias
COPY Cargo.toml Cargo.lock ./

# Trick para cachear dependencias: dummy main + build, luego sustituir source real
RUN mkdir src && echo "fn main(){}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Copiar source real y compilar
COPY src ./src
COPY migrations ./migrations

# Toca main.rs para forzar rebuild solo del crate (deps quedan cacheadas)
RUN touch src/main.rs && cargo build --release

# ---------- Stage 2: Runtime ----------
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Binario compilado
COPY --from=builder /app/target/release/moto-server-remoto /app/moto-server-remoto

# SPA estática (servida por el binario)
COPY static ./static

# Migraciones (sqlx::migrate! las embebe en el binario, pero las copiamos
# por si en el futuro se cargan en runtime)
COPY migrations ./migrations

ENV STATIC_DIR=/app/static
ENV RUST_LOG=info

EXPOSE 8080

CMD ["/app/moto-server-remoto"]
