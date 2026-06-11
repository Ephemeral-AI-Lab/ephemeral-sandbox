//! The `commit_to_git` checkpoint pipeline.
//!
//! This is the intermediate host layer that sits *between* the daemon control
//! plane and the storage leaves (`eos-layerstack`, `eos-overlay`):
//! `crate::ops::checkpoint` -> this service -> leaves. It owns the parts of
//! `commit_to_git` that are pure host glue — pathspec policy,
//! overlay-or-projection worktree preparation, and the git staging/commit
//! subprocess pipeline — none of which belong in the OCC single writer or the
//! wire adapters. Formerly the standalone `eos-checkpoint` crate; absorbed
//! into the daemon once it had exactly one consumer (it added no dependency
//! edges — the daemon already depended on both leaves).
//!
//! The boundary is DTO-in / DTO-out: the adapter parses the request envelope
//! and hands over a typed [`CommitRequest`]; this module returns a typed
//! [`CommitOutcome`] (or a [`CheckpointError`]) and never touches the wire
//! `Value` shape. The adapter re-maps the outcome to the protocol response.

mod commit;

use std::collections::BTreeMap;
use std::path::Path;

pub(crate) use commit::commit_to_git;

/// Typed input for [`commit_to_git`].
///
/// `raw_paths` are the un-normalized pathspecs lifted from the request envelope
/// by the adapter; this module trims, resolves them against the workspace
/// binding, and rejects `.git` paths. Empty / `"."` entries normalize away.
pub(crate) struct CommitRequest<'a> {
    /// Absolute layer-stack root.
    pub layer_stack_root: &'a Path,
    /// Absolute workspace root; must match the LayerStack binding.
    pub workspace_root: &'a Path,
    /// Commit message passed to `git commit -m`.
    pub message: &'a str,
    /// Raw, un-normalized pathspecs (envelope order preserved).
    pub raw_paths: Vec<String>,
}

/// Typed result of a checkpoint commit.
///
/// Mirrors the historical `api.commit_to_git` response fields one-to-one so the
/// adapter can re-emit the exact wire shape. `commit_sha` is `None` only when
/// the repository has no `HEAD` yet (re-mapped to JSON `null`).
pub(crate) struct CommitOutcome {
    /// Whether a new commit was created (`false` for a no-op re-commit).
    pub committed: bool,
    /// The resulting / current `HEAD` sha, or `None` when no `HEAD` exists.
    pub commit_sha: Option<String>,
    /// Active manifest version observed under the snapshot lease.
    pub manifest_version: i64,
    /// Active manifest root hash observed under the snapshot lease.
    pub manifest_root_hash: String,
    /// Normalized layer paths that were staged (empty means "all").
    pub paths: Vec<String>,
    /// `"overlay"` or `"projection"` depending on the worktree backend used.
    pub worktree_mode: &'static str,
    /// Phase timings, keyed exactly as the daemon emits them.
    pub timings: BTreeMap<String, f64>,
}

/// Failures raised by the checkpoint commit pipeline.
///
/// The runtime translates each variant onto its own `DaemonError` (preserving
/// variant identity and message text), which the dispatcher then maps to the
/// wire error envelope.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CheckpointError {
    /// The request was structurally invalid (binding mismatch, non-git
    /// workspace root, malformed pathspec resolution).
    #[error("invalid envelope: {0}")]
    InvalidEnvelope(String),

    /// A pathspec policy refusal (e.g. attempting to stage a `.git` path).
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// The overlay mount or git subprocess pipeline failed.
    #[error("overlay pipeline failure: {0}")]
    OverlayPipeline(String),

    /// The layer-stack snapshot / lease / projection layer failed.
    #[error(transparent)]
    LayerStack(#[from] eos_layerstack::LayerStackError),

    /// A filesystem operation in the worktree pipeline failed.
    #[error("checkpoint io error: {0}")]
    Io(#[from] std::io::Error),
}
