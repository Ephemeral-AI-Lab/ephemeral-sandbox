//! Optimistic-concurrency commit: the single-writer publish queue per root.
//!
//! Invariant (MF-1): OCC owns the publish DECISION gate — N disjoint file-API
//! writes batch into ONE manifest CAS attempt; each normalized path routes to
//! exactly one of [`Route::Drop`] (`.git`), [`Route::Direct`] (gitignored),
//! [`Route::Gated`] (tracked, base-hash checked), or [`Route::Reject`]
//! (disallowed). A stale base surfaces [`OccStatus::AbortedVersion`] after the
//! bounded CAS retry. EXACTLY ONE `occ-commit-queue` writer per
//! `layer_stack_root` serializes all publishes: any second OCC entry point
//! (e.g. the PPC self-managed plugin callback) MUST route through this same
//! single writer + storage lease, never a second [`CommitQueue`] instance.
//!
//! Build-time edges: the `occ -> overlay` edge is ONE-WAY and exists solely for
//! [`overlay_path_changes_to_occ_changes`] (overlay never links occ).
//! eos-isolated and eos-plugin do NOT link this crate — that omission is the
//! build-time no-publish guarantee, made possible because the snapshot/lease
//! HINGE lives in eos-layerstack, not here.

#![forbid(unsafe_code)]

pub mod commit_queue;
pub mod error;
pub mod overlay_change_conversion;
pub mod route;
pub mod service;

pub use commit_queue::{
    CommitQueue, CommitTransactionPort, PreparedChangeset, PublishConflict, BATCH_WINDOW_S,
    COMMIT_QUEUE_THREAD_NAME, MAX_BATCH_SIZE, MAX_OCC_CAS_RETRIES,
};
pub use error::OccError;
pub use overlay_change_conversion::{overlay_path_changes_to_occ_changes, OverlayPathChange};
pub use route::{ChangesetResult, FileResult, OccStatus, PublishDecision, Route};
pub use service::{OccRouteProvider, OccService};
