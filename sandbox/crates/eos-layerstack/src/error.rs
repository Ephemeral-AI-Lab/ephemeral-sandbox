//! Layer-stack error algebra.
//!
//! `// PORT backend/src/sandbox/layer_stack/manifest.py:* — ManifestConflictError`
//! and the `RuntimeError`/`ValueError` raises scattered across `stack.py`,
//! `storage_lock.py`, `squash.py`.

use thiserror::Error;

use eos_protocol::CasError;

/// Errors raised by the durable layer-stack storage layer.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LayerStackError {
    /// The active manifest changed under a publish/squash transaction (CAS lost).
    /// `// PORT backend/src/sandbox/layer_stack/manifest.py — ManifestConflictError`
    #[error("active manifest changed: expected version {expected}, found version {found}")]
    ManifestConflict { expected: i64, found: i64 },

    /// The storage root is already owned by another daemon process (flock held).
    /// `// PORT backend/src/sandbox/layer_stack/storage_lock.py:74-77`
    #[error("layer-stack storage root is already owned by another process: {0}")]
    StorageRootOwned(String),

    /// The storage-writer lock lease has been closed.
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:369`
    #[error("layer-stack storage writer lock is closed")]
    StorageWriterLockClosed,

    /// A squash/checkpoint plan invariant was violated (e.g. <2-layer segment).
    /// `// PORT backend/src/sandbox/layer_stack/squash.py:24-26,38-44,69-72`
    #[error("invalid squash plan: {0}")]
    InvalidSquashPlan(String),

    /// Could not allocate a unique layer id within the attempt budget.
    /// `// PORT backend/src/sandbox/layer_stack/paths.py:111`
    #[error("could not allocate a unique layer id")]
    LayerIdAllocation,

    /// The active manifest could not be parsed or violated storage invariants.
    #[error("manifest error: {0}")]
    Manifest(String),

    /// The layer-stack workspace binding is missing or invalid.
    #[error("workspace binding error: {0}")]
    WorkspaceBinding(String),

    /// A manifest-referenced layer no longer contains the requested data.
    #[error("layer-stack storage error: {0}")]
    Storage(String),

    /// A CAS path / manifest value failed to parse or validate.
    #[error(transparent)]
    Cas(#[from] CasError),

    /// An underlying filesystem operation failed.
    #[error("layer-stack io error: {0}")]
    Io(#[from] std::io::Error),
}
