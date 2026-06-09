//! Daemon: the `AF_UNIX` + loopback-TCP RPC server that owns the async runtime,
//! the op dispatcher, and every inverted port implementation.
//!
//! # Invariant this crate owns
//!
//! This is the primary tokio control-plane crate. It runs the
//! newline-delimited compact-JSON protocol-v1 RPC server on an `AF_UNIX` socket
//! AND a 127.0.0.1 TCP listener ([`DaemonServer`]), routes ops through the
//! [`dispatcher`] op table, tracks in-flight invocations with a TTL reaper
//! ([`invocation_registry`]), houses the audit RING BUFFER plus the impure emit
//! bridges ([`audit`]), and orchestrates background execution.
//!
//! It ORCHESTRATES but NEVER enters a namespace. The kernel requires the
//! `unshare(CLONE_NEWUSER)` / `setns`-into-a-userns caller to be single-threaded,
//! and the daemon is multi-threaded (tokio); so it SPAWNS the dedicated
//! single-threaded `eosd ns-holder` / `eosd ns-runner` children and wires their
//! pinned namespace FDs in — it does the namespace syscalls only by delegation.
//!
//! Built-in daemon operations live under [`ops`]; the daemon-side adapter/seam
//! layer that binds the command, checkpoint, plugin, isolated-workspace, overlay,
//! workspace, and OCC sibling-crate contracts to daemon resources lives under
//! [`adapters`]. Write-capable shared-workspace operations route through the same
//! per-root OCC service cache and single writer used by the live dispatcher.
//!
//! # The single-writer / no-lock-across-await discipline (§5)
//!
//! The live OCC single-writer path is the dispatcher-owned per-root
//! `OccService` cache, not a second daemon queue. The async server runs request
//! dispatch in a spawned task and keeps synchronous mutex guards out of await
//! points. Shutdown is a [`tokio_util::sync::CancellationToken`]; cancellation
//! tears down the full child process group for spawned background work.
//!
#![forbid(unsafe_code)]

pub(crate) mod adapters;
pub(crate) mod audit;
pub(crate) mod dispatch;
pub(crate) mod ops;
pub(crate) mod runtime;
pub(crate) mod transport;

pub use dispatch::dispatcher;
pub use dispatcher::{DispatchContext, OpTable};
pub use error::DaemonError;
pub use invocation_registry::{InFlightRegistry, DEFAULT_REAPER_INTERVAL_S, DEFAULT_TTL_S};
pub use runtime::{config, error, invocation_registry};
pub(crate) use runtime::{request_args, response_timings};
pub use transport::server::{DaemonServer, ServerConfig};
