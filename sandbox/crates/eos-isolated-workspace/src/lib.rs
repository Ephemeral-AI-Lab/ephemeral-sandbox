//! The persistent private workspace subsystem.
//!
//! An isolated workspace is a caller-private overlay that operations are
//! performed **on** — never a thing that runs commands. [`IsolatedSessions`]
//! owns the caller-keyed registry (TTL, capacity, GC, persistence) and the
//! isolation envelope around each handle: the `eosd ns-holder` namespace stack,
//! the veth/bridge/nftables network, DNS, and the per-workspace cgroup.
//!
//! # No publish, by construction
//!
//! This crate has **zero storage edges** — it never links the layer-stack
//! engine, not even for types. A snapshot enters as plain fields
//! ([`IsolatedSnapshot`]); lease custody stays with the caller that acquired
//! it, and [`IsolatedSessions::exit`] hands the `lease_id` back for release.
//! The upperdir is DISCARDED on exit; nothing written inside an isolated
//! workspace can reach the shared layer stack.
#![forbid(unsafe_code)]

pub mod caps;
mod error;
mod manager;
pub(crate) mod namespace;
mod network;
mod sessions;

pub use caps::{ResourceCaps, Rfc1918Egress};
pub use error::IsolatedError;
pub use manager::IsolatedManager;
pub use sessions::{
    ExitOutcome, IsolatedSessions, IsolatedSnapshot, IsolatedWorkspaceId, WorkspaceHandle,
};
