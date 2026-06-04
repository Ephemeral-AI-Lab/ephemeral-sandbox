//! Daemon: the `AF_UNIX` + loopback-TCP RPC server that owns the async runtime,
//! the op dispatcher, and every inverted port implementation.
//!
//! # Invariant this crate owns
//!
//! This is the primary tokio control-plane crate. It runs the
//! newline-delimited compact-JSON protocol-v1 RPC server on an `AF_UNIX` socket
//! AND a 127.0.0.1 TCP listener ([`server`]), routes ops through the
//! [`dispatcher`] op table, tracks in-flight invocations with a TTL reaper
//! ([`invocation_registry`]), houses the audit RING BUFFER plus the impure emit
//! bridges ([`audit_buffer`]), and orchestrates background execution.
//!
//! It ORCHESTRATES but NEVER enters a namespace. The kernel requires the
//! `unshare(CLONE_NEWUSER)` / `setns`-into-a-userns caller to be single-threaded,
//! and the daemon is multi-threaded (tokio); so it SPAWNS the dedicated
//! single-threaded `eosd ns-holder` / `eosd ns-runner` children and wires their
//! pinned namespace FDs in — it does the namespace syscalls only by delegation.
//!
//! Concrete Phase 3/3T handlers own the direct LayerStack/OCC/overlay runtime
//! paths in [`dispatcher`], [`command`], and [`isolated`]. There is no parallel
//! daemon port-injector layer: write-capable shared-workspace operations
//! route through the same per-root OCC service cache and single writer used by
//! the live dispatcher.
//!
//! # The single-writer / no-lock-across-await discipline (§5)
//!
//! The live OCC single-writer path is the dispatcher-owned per-root
//! `OccService` cache, not a second daemon queue. The async server runs request
//! dispatch in a spawned task and keeps synchronous mutex guards out of await
//! points. Shutdown is a [`tokio_util::sync::CancellationToken`]; the cancel
//! path kills the full child process group (the Python `start_new_session=True`).
//!
#![forbid(unsafe_code)]

pub mod audit_buffer;
pub(crate) mod command;
pub mod dispatcher;
pub mod error;
pub mod invocation_registry;
pub(crate) mod isolated;
pub(crate) mod occ_writer;
pub(crate) mod overlay_runner;
pub(crate) mod plugin;
pub(crate) mod response_timings;
pub mod server;

pub use audit_buffer::{safe_emit, safe_record_phase, AuditBuffer, BufferedEvent, LaneCounters};
pub use dispatcher::{error_envelope, DispatchContext, OpTable, AUDIT_ALLOW_FLOOR_RESET_ENV};
pub use error::{DaemonError, Result};
pub use invocation_registry::{
    ActiveCallGuard, InFlightInvocation, InFlightRegistry, DEFAULT_REAPER_INTERVAL_S,
    DEFAULT_TTL_S, ENV_REAPER_INTERVAL_S, ENV_TTL_S,
};
pub use server::{DaemonServer, ServerConfig, MAX_REQUEST_BYTES, REQUEST_READ_TIMEOUT_S};
