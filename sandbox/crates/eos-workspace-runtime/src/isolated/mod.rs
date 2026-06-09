//! Isolated workspace: a persistent, network-isolated PRIVATE session that
//! captures writes for AUDIT ONLY and NEVER publishes.
//!
//! # The build-time no-publish guarantee (STATE LOUDLY)
//!
//! The single sharpest invariant of this module is: **isolated writes are
//! captured for audit but NEVER published.** This is enforced at BUILD TIME by
//! `eos-workspace-runtime` NOT depending on `eos-occ`. The publish path lives
//! behind the OCC commit queue; isolated never links it. The only layer-stack
//! surface this module models is the snapshot/lease HINGE (read-only snapshot +
//! lease acquire/release), exposed here as [`session::LayerStackSnapshotPort`]
//! and injected by `eos-daemon`. On `exit`, the overlay upperdir is DISCARDED.
//!
//! If `eos-occ` ever appears in `eos-workspace-runtime`'s `Cargo.toml`, the
//! guarantee is silently broken — guard that edge.
//!
//! # What this module owns
//!
//! - [`caps::ResourceCaps`] — env-sourced lifecycle caps (TTL, cap, upperdir
//!   bytes, memavail fraction, fallback DNS).
//! - [`network`] — the `eos-shared0` bridge, per-workspace veth allocation, and
//!   the shell-free IPv6 hardening contract (rtnetlink + `/proc/sys` writes, NO
//!   `ip`/`sysctl` binaries).
//! - [`session::IsolatedSession`] — the enter/exit lifecycle plus the inverted
//!   namespace-runtime and snapshot/lease ports it orchestrates (daemon-spawned
//!   `eosd ns-holder` / `eosd ns-runner` children).
//! - [`audit`] — an append-only JSONL audit sink (audit-only, no OCC).
#![forbid(unsafe_code)]

pub mod audit;
pub mod caps;
pub mod command;
pub mod error;
pub mod network;
mod ops;
pub mod session;

pub use audit::JsonlAuditSink;
pub use caps::{ResourceCaps, Rfc1918Egress, CGROUP_ROOT, HANDLE_PREFIX};
pub use command::{
    finalize_isolated_command, prepare_isolated_command, take_isolated_audit,
    IsolatedCommandFinalizeContext, IsolatedCommandPrepareContext,
};
pub use error::IsolatedError;
pub use network::{BRIDGE_PREFIX_LEN, GATEWAY};
pub use ops::IsolatedWorkspaceOps;
pub use session::{
    CallerId, IsolatedSession, LayerStackSnapshotPort, NamespaceRuntimePort, SnapshotLease,
    WorkspaceHandle, WorkspaceHandleId,
};
