//! Daemon: the AF_UNIX + loopback-TCP RPC server that owns the async runtime,
//! the op dispatcher, and every inverted port implementation.
//!
//! # Invariant this crate owns
//!
//! This is the ONLY tokio crate. It runs the newline-delimited compact-JSON
//! protocol-v1 RPC server on an AF_UNIX socket AND a 127.0.0.1 TCP listener
//! ([`server`]), routes ops through the [`dispatcher`] op table, tracks in-flight
//! invocations with a TTL reaper ([`in_flight`]), houses the audit RING BUFFER
//! plus the impure emit bridges ([`audit_buffer`]), and orchestrates background
//! execution.
//!
//! It ORCHESTRATES but NEVER enters a namespace. The kernel requires the
//! `unshare(CLONE_NEWUSER)` / `setns`-into-a-userns caller to be single-threaded,
//! and the daemon is multi-threaded (tokio); so it SPAWNS the dedicated
//! single-threaded `eosd ns-holder` / `eosd ns-runner` children and wires their
//! pinned namespace FDs in — it does the namespace syscalls only by delegation.
//!
//! It IMPLEMENTS and injects the inverted port traits the lower crates DEFINE
//! ([`ports`]), so the crate graph stays leaf->root:
//!
//! * [`eos_occ::OccRuntimeServicesPort`] + [`eos_ephemeral::OccRuntimeServicesPort`]
//!   (severing #2) — both on one daemon injector keyed per `layer_stack_root`,
//!   so the WRITE_ALLOWED publish path always routes through the ONE
//!   `occ-commit-queue` writer per root (MF-1 single-writer).
//! * [`eos_layerstack::LayerStackRuntimePort`] (severing #3) — the daemon-side
//!   per-root manager cache + base construction.
//! * [`eos_ephemeral::ChangesetProjectionPort`] (severing #4) — published-file
//!   projection + the per-agent dispatch drain-gate.
//!
//! # The single-writer / no-lock-across-await discipline (§5)
//!
//! The OCC single writer is reached through an `mpsc` work queue with a single
//! consumer task and `oneshot` replies, NOT by holding a shared OCC lock across
//! an `.await`. No mutex guard is ever held across an await point. Shutdown is a
//! [`tokio_util::sync::CancellationToken`]; the cancel path kills the full child
//! process group (the Python `start_new_session=True`).
//!
//! `// PORT backend/src/sandbox/daemon/rpc/server.py — serve loop`
//! `// PORT backend/src/sandbox/daemon/rpc/dispatcher.py — OP_TABLE + dispatch`
//! `// PORT backend/src/sandbox/daemon/audit_buffer.py — ring buffer`
//! `// PORT backend/src/sandbox/daemon/audit_schema.py:294,310 — safe_emit / safe_record_phase`
//! `// PORT backend/src/sandbox/daemon/rpc/in_flight.py — in-flight registry + TTL reaper`
#![forbid(unsafe_code)]

pub mod audit_buffer;
pub(crate) mod command;
pub mod dispatcher;
pub mod error;
pub mod in_flight;
pub mod ports;
pub mod server;

pub use audit_buffer::{safe_emit, safe_record_phase, AuditBuffer, BufferedEvent, LaneCounters};
pub use dispatcher::{
    error_envelope, DispatchContext, Handler, OpTable, AUDIT_ALLOW_FLOOR_RESET_ENV,
};
pub use error::{DaemonError, Result};
pub use in_flight::{
    ActiveCallGuard, InFlightInvocation, InFlightRegistry, DEFAULT_REAPER_INTERVAL_S,
    DEFAULT_TTL_S, ENV_REAPER_INTERVAL_S, ENV_TTL_S,
};
pub use ports::{
    ChangesetProjectionInjector, DaemonCommitTransaction, LayerStackRuntimeInjector,
    OccServicesInjector,
};
pub use server::{
    DaemonServer, OccWork, OccWriterQueue, ServerConfig, MAX_OCC_QUEUE_DEPTH, MAX_REQUEST_BYTES,
    REQUEST_READ_TIMEOUT_S,
};
