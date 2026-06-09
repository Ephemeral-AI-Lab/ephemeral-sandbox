//! Fresh per-operation workspace policy.
//!
//! This module owns the publish-capable ephemeral workspace lifecycle pieces that
//! are unique to fresh overlay operations: allocate scratch, capture upperdir
//! changes, classify path/resource data, prepare runner requests for fresh
//! command workspaces, and call an injected publisher. Daemon RPC routing,
//! process supervision, command-session registry state, public JSON envelopes,
//! and generic OCC publisher ownership stay outside this module.

pub mod capture;
pub mod command;
pub mod dirs;
pub mod error;
pub mod finalize;
mod ops;
pub mod ports;
pub mod timings;
pub mod types;

pub use capture::CapturedUpperdir;
pub use command::{
    discard_ephemeral_command, finalize_ephemeral_command, prepare_ephemeral_command,
    EphemeralCommandPrepareContext, PreparedEphemeralCommand,
};
pub use dirs::{EphemeralDirAllocator, RunDirCleanup};
pub use error::EphemeralWorkspaceError;
pub use finalize::{finalize_publishable_workspace, FinalizeOutcome, FinalizeRequest};
pub use ops::EphemeralWorkspaceOps;
pub use ports::WorkspacePublisherPort;
pub use timings::{PublishTiming, TreeResourceStats};
pub use types::{
    CallerId, EphemeralRunDirs, SnapshotLease, EphemeralWorkspace, InvocationId, PathChange,
    PathChangeKind, PublishOutcome, PublishStatus, WorkspaceRoot,
};
