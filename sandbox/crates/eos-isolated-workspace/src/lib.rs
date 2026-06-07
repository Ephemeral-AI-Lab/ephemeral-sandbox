//! Isolated workspace: a persistent, network-isolated PRIVATE session that
//! captures writes for AUDIT ONLY and NEVER publishes.
//!
//! # The build-time no-publish guarantee (STATE LOUDLY)
//!
//! The single sharpest invariant of this crate is: **isolated writes are
//! captured for audit but NEVER published.** This is enforced at BUILD TIME by
//! this crate NOT depending on `eos-occ`. The publish path lives behind the OCC
//! commit queue; isolated never links it. The only layer-stack surface this
//! crate models is the snapshot/lease HINGE (read-only snapshot + lease
//! acquire/release), exposed here as [`session::LayerStackSnapshotPort`] and
//! injected by `eos-daemon`. On `exit`, the overlay upperdir is DISCARDED.
//!
//! If `eos-occ` ever appears in this crate's `Cargo.toml`, the guarantee is
//! silently broken — guard that edge.
//!
//! # What this crate owns
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
pub mod command_session;
pub mod error;
pub mod network;
mod ops;
pub mod session;

pub mod config {
    pub use eos_config::configs::isolated_workspace::*;
}

pub use audit::{AuditSink, JsonlAuditSink};
pub use caps::{
    ResourceCaps, Rfc1918Egress, CGROUP_ROOT, HANDLE_PREFIX, PERSISTED_HANDLES_SCHEMA_VERSION,
};
pub use error::IsolatedError;
pub use network::{
    veth_names, BridgeAddressPool, IsolatedNetwork, VethAllocation, BRIDGE_CIDR, BRIDGE_NAME,
    BRIDGE_PREFIX_LEN, GATEWAY, IMDS_ADDR, NFT_FILTER_TABLE, NFT_NAT_TABLE, RFC1918_NETS,
    VETH_PREFIX,
};
pub use ops::IsolatedWorkspaceOps;
pub use session::{
    CallerId, IsolatedSession, LayerStackSnapshotPort, NamespaceRuntimePort, SnapshotLease,
    WorkspaceHandle, WorkspaceHandleId, DEFAULT_ISOLATED_SCRATCH_ROOT,
};
