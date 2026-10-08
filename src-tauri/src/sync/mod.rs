// sync/mod.rs — Infraestructura de sincronización con servidor remoto (Fase 3.2)
//
// Arquitectura:
//   - POS mantiene `sync_outbox` con uuids de filas que cambiaron (via triggers SQL).
//   - Worker corre en background cada N segundos:
//       1. Empuja cambios pendientes al remoto (POST /sync/push).
//       2. Jala cambios del remoto (GET /sync/pull?cursor=X).
//   - Cambios recibidos se aplican a SQLite con la tabla `sync_suppress_flag`
//     poblada DENTRO de la transacción para que los triggers NO generen
//     nuevas entradas de outbox (evita eco).
//
// Sync v2: las FK viajan como uuids (fk.rs, mapa compartido con el servidor),
// los cambios que no se pueden aplicar esperan en `sync_inbox` (inbox.rs),
// hay tareas únicas de reparación (tareas.rs) y los productos viajan con su
// `base` para la fusión de 3 vías del servidor (base.rs).

pub mod fk;
pub mod base;
pub mod payload;
pub mod outbox;
pub mod state;
pub mod apply;
pub mod inbox;
pub mod tareas;
pub mod worker;
pub mod client;

#[cfg(test)]
mod sync_tests;
#[cfg(test)] mod e2e_tests;
