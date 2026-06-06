//! `eos-backend-api` — the axum router, request/task/sandbox/stats handlers,
//! SSE / `WebSocket` milestone streaming, and the `OpenAPI` document.
//!
//! The crate exposes one composition surface: [`AppState`] (assembled by the
//! backend main from the runtime capabilities and store handles) and
//! [`build_router`], which wires every route in `SPEC.md`. Two narrow ports —
//! [`RunControl`] and [`SandboxRegistry`] — abstract the stateful runtime
//! capabilities so the production `RunLauncher` / `SandboxManager` drive the API
//! in deployment while test doubles drive it in the contract tests.
//!
//! Two contracts are load-bearing: sandbox responses ([`SandboxView`]) never
//! carry daemon connection material or credentials (AC4), and the milestone
//! stream replays persisted `event_log` rows before tailing live with no gap at
//! the handoff (AC5).
//!
//! [`SandboxView`]: eos_backend_types::SandboxView
#![warn(missing_docs)]

mod error;
mod handlers;
mod openapi;
mod router;
mod stream;

pub use router::{build_router, AgentCoreReads, AppState, RunControl, SandboxRegistry};
