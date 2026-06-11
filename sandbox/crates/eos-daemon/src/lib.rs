//! Daemon: the `AF_UNIX` + loopback-TCP RPC server that owns the async runtime,
//! the op dispatcher, and every inverted port implementation.
//!
//! # Invariant this crate owns
//!
//! This is the primary tokio control-plane crate. It runs the
//! newline-delimited compact-JSON protocol-v1 RPC server on an `AF_UNIX` socket
//! AND a 127.0.0.1 TCP listener ([`DaemonServer`]), routes ops through the
//! [`dispatcher`] op table, tracks in-flight invocations with a TTL reaper
//! ([`invocation_registry`]), and orchestrates background execution.
//!
//! It ORCHESTRATES but NEVER enters a namespace. The kernel requires the
//! `unshare(CLONE_NEWUSER)` / `setns`-into-a-userns caller to be single-threaded,
//! and the daemon is multi-threaded (tokio); so it SPAWNS the dedicated
//! single-threaded `eosd ns-holder` / `eosd ns-runner` children and wires their
//! pinned namespace FDs in — it does the namespace syscalls only by delegation.
//!
//! Op handlers live under [`ops`], while implementation state lives with the
//! owning service modules or sibling crates. [`dispatch::registry`] is the
//! single table binding wire op names to those handlers. Overlay and workspace
//! helpers live in the sibling crates that own those domains, with daemon code
//! keeping only process launch and service orchestration. Write-capable
//! shared-workspace operations route through `eos_layerstack::service`, the
//! per-root single writer shared with the live dispatcher.
//!
//! # The single-writer / no-lock-across-await discipline (§5)
//!
//! The live commit single-writer path is `eos_layerstack::service`'s per-root
//! writer cache, not a second daemon queue. The async server runs request
//! dispatch in a spawned task and keeps synchronous mutex guards out of await
//! points. Shutdown is a [`tokio_util::sync::CancellationToken`]; cancellation
//! tears down the full child process group for spawned background work.
//!
#![forbid(unsafe_code)]

pub(crate) mod dispatch;
pub(crate) mod ops;
pub(crate) mod runtime;
pub(crate) mod services;
pub(crate) mod transport;
pub mod wire;

pub(crate) use dispatch::dispatcher;
pub use dispatcher::OpTable;
pub use invocation_registry::InFlightRegistry;
pub(crate) use invocation_registry::{DEFAULT_REAPER_INTERVAL_S, DEFAULT_TTL_S};
pub use runtime::context::DispatchContext;
pub(crate) use runtime::{config, error, invocation_registry};
pub(crate) use runtime::{request_args, response};
pub use transport::server::{DaemonServer, ServerConfig};
