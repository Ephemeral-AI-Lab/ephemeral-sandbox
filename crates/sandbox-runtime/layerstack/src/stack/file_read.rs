//! Classified single-path reads of the active manifest, plus `amend_path`: the
//! atomic read-modify-write that sessionless file write/edit publish through.
//!
//! `amend_path` holds the storage **exclusive writer lock** across read →
//! transform → resolve → commit, so head cannot move between the read and the
//! commit. Because the publish base is that same head, the three-way merge never
//! runs and no source/manifest conflict can occur — there is nothing to retry.

use crate::error::LayerStackError;
use crate::model::{manifest_root_hash, LayerChange, LayerPath};
use crate::stack::publish::merge::{LineRange, Origin};
use crate::stack::publish::model::{
    PublishBase, PublishBaseRevision, PublishValidatedChangesRequest,
};
use crate::stack::publish::{plan_publish, resolve_publish_changes};
use crate::stack::LayerStack;

/// Classified read of one path against the active manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestFileRead {
    Absent,
    /// A regular file. `bytes` is empty when the read was classify-only
    /// (`max_bytes == 0`); otherwise it is the full content.
    File {
        bytes: Vec<u8>,
        total_bytes: u64,
    },
    Directory,
    Symlink,
    TooLarge {
        size: u64,
        limit: usize,
    },
}

/// Outcome of a committed `amend_path`, including the resolved changes and each
/// committed line's structural origin so the caller can record blame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmendCommit {
    pub existed_before: bool,
    pub bytes_written: usize,
    pub origin: Vec<(LayerPath, Vec<(LineRange, Origin)>)>,
    pub changes: Vec<LayerChange>,
}

/// Failure of `amend_path`: either the caller's transform rejected the read, or
/// the layerstack read/commit failed.
#[derive(Debug)]
pub enum AmendError<E> {
    Transform(E),
    LayerStack(LayerStackError),
}

impl LayerStack {
    /// Classified read of `rel` from the active head under a shared lock.
    pub fn read_classified(
        &self,
        rel: &LayerPath,
        max_bytes: usize,
    ) -> Result<ManifestFileRead, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        let manifest = self.read_active_manifest_unlocked()?;
        self.view
            .read_classified(rel.as_str(), &manifest, max_bytes)
    }

    /// Atomic read-modify-write of `rel` on the active head: read it classified,
    /// run `transform` to produce the new bytes, then publish one `Write` with
    /// the base pinned to that same head. The exclusive writer lock is held for
    /// the whole sequence, so the publish never conflicts and never retries.
    ///
    /// # Errors
    /// [`AmendError::Transform`] when the caller's transform rejects the read;
    /// [`AmendError::LayerStack`] when the read or commit fails.
    pub fn amend_path<E>(
        &self,
        rel: &LayerPath,
        max_bytes: usize,
        transform: impl FnOnce(ManifestFileRead) -> Result<Vec<u8>, E>,
    ) -> Result<AmendCommit, AmendError<E>> {
        let _guard = self
            .writer_lock
            .exclusive()
            .map_err(AmendError::LayerStack)?;
        let active = self
            .read_active_manifest_unlocked()
            .map_err(AmendError::LayerStack)?;
        let read = self
            .view
            .read_classified(rel.as_str(), &active, max_bytes)
            .map_err(AmendError::LayerStack)?;
        let existed_before = matches!(read, ManifestFileRead::File { .. });
        let new_bytes = transform(read).map_err(AmendError::Transform)?;
        let bytes_written = new_bytes.len();
        let request = PublishValidatedChangesRequest {
            base: PublishBase {
                manifest: active.clone(),
                revision: PublishBaseRevision {
                    manifest_version: active.version,
                    root_hash: manifest_root_hash(&active),
                    layer_count: active.layers.len(),
                },
            },
            changes: vec![LayerChange::Write {
                path: rel.clone(),
                content: new_bytes,
            }],
            protected_drops: Vec::new(),
        };
        let plan = plan_publish(&self.view, &request).map_err(AmendError::LayerStack)?;
        let resolved = resolve_publish_changes(&self.view, &active, &request, &plan)
            .map_err(AmendError::LayerStack)?;
        let (origin, changes) = if resolved.changes.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            let outcome = self
                .publish_layer_unlocked(&active, &resolved.changes)
                .map_err(AmendError::LayerStack)?;
            if outcome.created {
                (resolved.origin, resolved.changes)
            } else {
                (Vec::new(), Vec::new())
            }
        };
        Ok(AmendCommit {
            existed_before,
            bytes_written,
            origin,
            changes,
        })
    }
}
