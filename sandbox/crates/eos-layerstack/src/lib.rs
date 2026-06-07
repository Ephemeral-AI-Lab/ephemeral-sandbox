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
//! # The no-publish guarantee is enforced by the dependency graph
//!
//! `eos-isolated-workspace` captures writes for audit but can NEVER publish — guaranteed
//! structurally because it does not depend on `eos-occ` (a build-time edge, not
//! a convention). The snapshot/lease read surface ([`LayerStack`] +
//! [`MergedView`] + [`Lease`]) is owned here; the publish-side transaction is
//! daemon-owned. Lower crates that need a narrow read port define and inject it
//! at their own boundary rather than importing this crate.
//!
//! # Build-time / threading guarantee
//!
//! Single-threaded core plus a per-root reentrant write lease (the dual-layer
//! `flock` cross-process lease + in-process reentrant mutex). No tokio. The
//! reentrant-RLock → non-reentrant-Mutex DEADLOCK TRAP is documented in
//! [`storage_lock`] — do NOT 1:1-port it.
#![forbid(unsafe_code)]

pub mod error;
pub(crate) mod fsutil;
pub mod lease;
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

pub use error::LayerStackError;
pub use lease::{LayerStackLeaseRecord, LeaseRegistry};
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

/// Auto-squash depth target — distinct from the kernel overlayfs layer ceiling.
pub const AUTO_SQUASH_MAX_DEPTH: usize = 100;

/// Storage layout subdirectory for immutable layer directories.
pub(crate) const LAYERS_DIR: &str = "layers";

/// Storage layout subdirectory for in-flight commit/checkpoint staging dirs.
pub(crate) const STAGING_DIR: &str = "staging";

/// Active-manifest pointer filename under a storage root.
pub const ACTIVE_MANIFEST_FILE: &str = "manifest.json";

/// Sidecar directory for per-layer digests used by head-layer idempotency.
pub(crate) const LAYER_METADATA_DIR: &str = ".layer-metadata";
