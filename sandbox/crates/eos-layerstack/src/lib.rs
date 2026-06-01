//! Layer-stack storage: durable truth for the sandbox.
//!
//! # Invariant owned by this crate
//!
//! The manifest CAS is the **SINGLE linearization point**: ONE mutable
//! `manifest.json` over immutable, content-addressed layer directories, swapped
//! by an ATOMIC pointer write. There is no other place state becomes durable.
//!
//! - A **snapshot is O(1)**: it returns a [`Lease`] + the manifest's EXISTING
//!   `layer_paths`, NEVER a rendered tree. Rendering is the caller's
//!   overlay/projection concern.
//! - [`LeaseRegistry::leased_layers`] (the FULL on-disk retention set) and
//!   [`LeaseRegistry::lease_head_layers`] (the squash-keep barrier set) are
//!   **DISTINCT** sets — see [`lease`].
//! - **Squash is NON-DESTRUCTIVE** until the retaining lease releases: a layer
//!   below a lease head folds into a checkpoint, but the underlying directory
//!   stays on disk for that lease's frozen reads until release GCs it.
//!
//! # This crate OWNS the HINGE
//!
//! The snapshot/lease port lives HERE, not in `eos-occ`. It is deliberately
//! SPLIT from the publish-side transaction so the no-publish guarantee holds at
//! the type level: [`SnapshotLeasePort`] (what `eos-isolated` + `eos-plugin`
//! need — NEVER publishes) vs [`LayerCommitTransaction`] (what `eos-occ` +
//! `eos-ephemeral` need). Because the HINGE is here, isolated/plugin link
//! `eos-layerstack` and never `eos-occ`. See [`port`].
//!
//! # Build-time / threading guarantee
//!
//! Single-threaded core plus a per-root reentrant write lease (the dual-layer
//! `flock` cross-process lease + in-process reentrant mutex). No tokio. The
//! reentrant-RLock → non-reentrant-Mutex DEADLOCK TRAP is documented in
//! [`storage_lock`] — do NOT 1:1-port it.
#![forbid(unsafe_code)]

pub mod commit_staging;
pub mod error;
pub mod lease;
pub mod port;
pub mod squash;
pub mod stack;
pub mod storage_lock;
pub mod workspace_base;
pub mod workspace_binding;

// CAS types are owned by eos-protocol; re-export so downstream crates use ONE
// set of hashes/types and never redefine them.
pub use eos_protocol::{
    aggregate_layer_changes, layer_digest, manifest_root_hash, LayerChange, LayerPath, LayerRef,
    Manifest,
};

pub use commit_staging::{allocate_commit_staging, drop_commit_staging, CommitStagingArea};
pub use error::LayerStackError;
pub use lease::{LayerStackLeaseRecord, LeaseRegistry};
pub use port::{LayerCommitTransaction, LayerStackRuntimePort, SnapshotLeasePort};
pub use squash::{
    manifest_prefix_before_plan, CheckpointSegment, LayerCheckpointSquasher, SquashPlan,
    SquashPlanEntry,
};
pub use stack::{LayerStack, Lease, MergedView};
pub use storage_lock::{StorageWriterLockLease, STORAGE_WRITER_LOCK_FILE};
pub use workspace_base::{build_workspace_base, ensure_workspace_base, WORKSPACE_BASE_LAYER_ID};
pub use workspace_binding::{
    read_workspace_binding, require_workspace_binding, WorkspaceBinding, WORKSPACE_BINDING_FILE,
};

/// Auto-squash depth ceiling — distinct from the 16-layer overlay mount ceiling.
/// `// PORT backend/src/sandbox/occ/service.py:34 — AUTO_SQUASH_MAX_DEPTH`
pub const AUTO_SQUASH_MAX_DEPTH: usize = 100;

/// Storage layout subdirectory for immutable layer directories.
/// `// PORT backend/src/sandbox/layer_stack/manifest.py:24 — LAYERS_DIR`
pub const LAYERS_DIR: &str = "layers";

/// Storage layout subdirectory for in-flight commit/checkpoint staging dirs.
/// `// PORT backend/src/sandbox/layer_stack/manifest.py:25 — STAGING_DIR`
pub const STAGING_DIR: &str = "staging";

/// Active-manifest pointer filename under a storage root.
/// `// PORT backend/src/sandbox/layer_stack/manifest.py:23 — ACTIVE_MANIFEST_FILE`
pub const ACTIVE_MANIFEST_FILE: &str = "manifest.json";

/// Sidecar directory for per-layer digests used by head-layer idempotency.
/// `// PORT backend/src/sandbox/layer_stack/manifest.py:26 — LAYER_METADATA_DIR`
pub const LAYER_METADATA_DIR: &str = ".layer-metadata";
