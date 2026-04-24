// sync/mod.rs — Infraestructura de sincronización con servidor remoto (Fase 3.2)
//
// Arquitectura:
//   - POS mantiene `sync_outbox` con uuids de filas que cambiaron (via triggers SQL).
//   - Worker corre en background cada N segundos:
//       1. Empuja cambios pendientes al remoto (POST /sync/push).
//       2. Jala cambios del remoto (GET /sync/pull?cursor=X).
//   - Cambios recibidos se aplican a SQLite con `sync_suppress` creado para que
//     los triggers NO generen nuevas entradas de outbox (evita eco).

pub mod payload;
pub mod outbox;
pub mod state;
pub mod apply;
pub mod worker;
pub mod client;

pub use state::SyncConfig;
