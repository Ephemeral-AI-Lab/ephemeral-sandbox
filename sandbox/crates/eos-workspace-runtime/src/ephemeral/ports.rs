use eos_protocol::LayerChange;

use crate::ephemeral::error::EphemeralWorkspaceError;
use crate::ephemeral::types::{SnapshotLease, PathChange, PublishOutcome, WorkspaceRoot};

/// Publisher port supplied by the daemon's neutral OCC publisher adapter.
pub trait WorkspacePublisherPort {
    fn publish_upperdir_changes(
        &self,
        root: &WorkspaceRoot,
        snapshot: &SnapshotLease,
        changes: &[LayerChange],
        path_kinds: &[PathChange],
    ) -> Result<PublishOutcome, EphemeralWorkspaceError>;
}
