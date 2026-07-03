use crate::model::{LayerChange, LayerPath, Manifest};

use super::merge::{LineRange, Origin};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishValidatedChangesRequest {
    pub base: PublishBase,
    pub changes: Vec<LayerChange>,
    pub protected_drops: Vec<LayerProtectedDrop>,
}

/// The bytes a publish commits plus, per resolved path, each final line's
/// structural [`Origin`]. The owner is **not** here (boundary law): the runtime
/// above layerstack maps origin to an owner string after the layer commits.
///
/// An empty origin range list for a path is *wholesale* attribution — a
/// non-text clean write or an ignored path with no line-level claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedChangeset {
    pub changes: Vec<LayerChange>,
    pub origin: Vec<(LayerPath, Vec<(LineRange, Origin)>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishBase {
    pub manifest: Manifest,
    pub revision: PublishBaseRevision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishBaseRevision {
    pub manifest_version: i64,
    pub root_hash: String,
    pub layer_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerProtectedDrop {
    pub path: String,
    pub reason: LayerProtectedDropReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerProtectedDropReason {
    UnsupportedSpecialFile,
    InvalidLayerPath,
    CommandScratchPath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishValidatedChangesResult {
    pub manifest: Manifest,
    pub route_summary: PublishRouteSummary,
    pub no_op: bool,
    /// Per committed path, each final line's structural origin. Empty when the
    /// publish was a no-op (nothing committed, so nothing to attribute).
    pub origin: Vec<(LayerPath, Vec<(LineRange, Origin)>)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublishRouteSummary {
    pub source_count: usize,
    pub ignored_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishReject {
    pub path: Option<LayerPath>,
    pub reason: PublishRejectReason,
    pub source_conflict: Option<SourceConflict>,
    pub protected_drop: Option<LayerProtectedDrop>,
    pub message: Option<String>,
}

impl PublishReject {
    pub(crate) fn at_path(path: LayerPath, reason: PublishRejectReason) -> Self {
        Self {
            path: Some(path),
            reason,
            source_conflict: None,
            protected_drop: None,
            message: None,
        }
    }

    pub(crate) fn with_message(reason: PublishRejectReason, message: impl Into<String>) -> Self {
        Self {
            path: None,
            reason,
            source_conflict: None,
            protected_drop: None,
            message: Some(message.into()),
        }
    }

    pub(crate) fn protected_drop(drop: LayerProtectedDrop) -> Self {
        Self {
            path: None,
            reason: PublishRejectReason::ProtectedPath,
            source_conflict: None,
            protected_drop: Some(drop),
            message: None,
        }
    }

    pub(crate) fn source_conflict(conflict: SourceConflict) -> Self {
        Self {
            path: Some(conflict.path.clone()),
            reason: PublishRejectReason::SourceConflict,
            source_conflict: Some(conflict),
            protected_drop: None,
            message: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishRejectReason {
    InvalidBaseRevision,
    ProtectedPath,
    SourceConflict,
    OpaqueDirProtectedDescendant,
    OpaqueDirMixedRoutes,
    OpaqueDirExpansionLimit,
    RoutePreparationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceConflict {
    pub path: LayerPath,
    pub expected: ContentFingerprint,
    pub actual: ContentFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentFingerprint {
    Absent,
    File {
        digest: String,
        executable: Option<bool>,
    },
    Symlink {
        target: String,
    },
    Directory,
}
