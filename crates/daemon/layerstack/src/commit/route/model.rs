use crate::model::LayerPath;

use super::super::model::{CommitStatus, FileResult};

pub(crate) const GIT_METADATA_UNSUPPORTED_DROP_REASON: &str = "git_metadata_unsupported";
pub(crate) const GIT_INDEX_STAT_REFRESH_DROP_REASON: &str = "git_index_stat_refresh";
pub(crate) const GIT_INDEX_STAGED_STATE_REJECT_REASON: &str = "git_index_staged_state";
pub(crate) const GIT_LOCK_FILE_REJECT_REASON: &str = "git_lock_file";
pub(crate) const GIT_INCOMPLETE_OPERATION_REJECT_REASON: &str = "git_incomplete_operation";
pub(crate) const GIT_HOOK_WRITE_REJECT_REASON: &str = "git_hook_write";
pub(crate) const GIT_METADATA_DELETE_REJECT_REASON: &str = "git_metadata_delete";
pub(crate) const GIT_METADATA_OPAQUE_REPLACE_REJECT_REASON: &str = "git_metadata_opaque_replace";
pub(crate) const GIT_REF_WRITE_REJECT_REASON: &str = "git_ref_write";
pub(crate) const GIT_OBJECT_REWRITE_REJECT_REASON: &str = "git_object_rewrite";
pub(crate) const GIT_REFLOG_REWRITE_REJECT_REASON: &str = "git_reflog_rewrite";
pub(crate) const DAEMON_CONTROL_PATH_DROP_REASON: &str = "daemon_control_path";
pub(crate) const COMMAND_SCRATCH_PATH_DROP_REASON: &str = "command_scratch_path";
pub(crate) const OPAQUE_DIR_PROTECTED_DESCENDANT_DROP_REASON: &str =
    "opaque_dir_protected_descendant";
pub(crate) const OPAQUE_DIR_MIXED_ROUTES_DROP_REASON: &str = "opaque_dir_mixed_routes";
pub(crate) const OPAQUE_DIR_EXPANSION_LIMIT_DROP_REASON: &str = "opaque_dir_expansion_limit";

pub(super) const OPAQUE_DIR_EXPANSION_LIMIT: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteDropReason {
    GitMetadataUnsupported,
    GitIndexStatRefresh,
    GitIndexStagedState,
    GitLockFile,
    GitIncompleteOperation,
    GitHookWrite,
    GitMetadataDelete,
    GitMetadataOpaqueReplace,
    GitRefWrite,
    GitObjectRewrite,
    GitReflogRewrite,
    DaemonControlPath,
    CommandScratchPath,
    OpaqueDirProtectedDescendant,
    OpaqueDirMixedRoutes,
    OpaqueDirExpansionLimit,
}

impl RouteDropReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GitMetadataUnsupported => GIT_METADATA_UNSUPPORTED_DROP_REASON,
            Self::GitIndexStatRefresh => GIT_INDEX_STAT_REFRESH_DROP_REASON,
            Self::GitIndexStagedState => GIT_INDEX_STAGED_STATE_REJECT_REASON,
            Self::GitLockFile => GIT_LOCK_FILE_REJECT_REASON,
            Self::GitIncompleteOperation => GIT_INCOMPLETE_OPERATION_REJECT_REASON,
            Self::GitHookWrite => GIT_HOOK_WRITE_REJECT_REASON,
            Self::GitMetadataDelete => GIT_METADATA_DELETE_REJECT_REASON,
            Self::GitMetadataOpaqueReplace => GIT_METADATA_OPAQUE_REPLACE_REJECT_REASON,
            Self::GitRefWrite => GIT_REF_WRITE_REJECT_REASON,
            Self::GitObjectRewrite => GIT_OBJECT_REWRITE_REJECT_REASON,
            Self::GitReflogRewrite => GIT_REFLOG_REWRITE_REJECT_REASON,
            Self::DaemonControlPath => DAEMON_CONTROL_PATH_DROP_REASON,
            Self::CommandScratchPath => COMMAND_SCRATCH_PATH_DROP_REASON,
            Self::OpaqueDirProtectedDescendant => OPAQUE_DIR_PROTECTED_DESCENDANT_DROP_REASON,
            Self::OpaqueDirMixedRoutes => OPAQUE_DIR_MIXED_ROUTES_DROP_REASON,
            Self::OpaqueDirExpansionLimit => OPAQUE_DIR_EXPANSION_LIMIT_DROP_REASON,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Gated,
    Direct,
    Drop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ValidationBase {
    Path(Option<String>),
    Paths(Vec<(LayerPath, Option<String>)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PublishDecision {
    Publish {
        path: LayerPath,
        route: PublishRoute,
    },
    Drop {
        path: LayerPath,
        reason: Option<RouteDropReason>,
        reject_publish: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PublishRoute {
    Direct,
    Gated(ValidationBase),
}

impl PublishDecision {
    pub(crate) fn direct(path: LayerPath) -> Self {
        Self::Publish {
            path,
            route: PublishRoute::Direct,
        }
    }

    pub(crate) fn gated(path: LayerPath, base_hash: Option<String>) -> Self {
        Self::Publish {
            path,
            route: PublishRoute::Gated(ValidationBase::Path(base_hash)),
        }
    }

    pub(crate) fn gated_paths(
        path: LayerPath,
        validation_base_hashes: Vec<(LayerPath, Option<String>)>,
    ) -> Self {
        Self::Publish {
            path,
            route: PublishRoute::Gated(ValidationBase::Paths(validation_base_hashes)),
        }
    }

    pub(crate) fn dropped(path: LayerPath, reason: Option<RouteDropReason>) -> Self {
        Self::Drop {
            path,
            reason,
            reject_publish: false,
        }
    }

    pub(crate) fn rejected_drop(path: LayerPath, reason: RouteDropReason) -> Self {
        Self::Drop {
            path,
            reason: Some(reason),
            reject_publish: true,
        }
    }

    pub(crate) fn path(&self) -> &LayerPath {
        match self {
            Self::Publish { path, .. } | Self::Drop { path, .. } => path,
        }
    }

    pub(crate) const fn route(&self) -> Route {
        match self {
            Self::Publish {
                route: PublishRoute::Direct,
                ..
            } => Route::Direct,
            Self::Publish {
                route: PublishRoute::Gated(_),
                ..
            } => Route::Gated,
            Self::Drop { .. } => Route::Drop,
        }
    }

    pub(crate) const fn is_publishable(&self) -> bool {
        matches!(self, Self::Publish { .. })
    }

    pub(crate) const fn drop_reason(&self) -> Option<RouteDropReason> {
        match self {
            Self::Drop { reason, .. } => *reason,
            Self::Publish { .. } => None,
        }
    }

    pub(crate) const fn reject_publish(&self) -> bool {
        match self {
            Self::Drop { reject_publish, .. } => *reject_publish,
            Self::Publish { .. } => false,
        }
    }

    pub(crate) fn validation_base(&self) -> Option<&ValidationBase> {
        match self {
            Self::Publish {
                route: PublishRoute::Gated(validation),
                ..
            } => Some(validation),
            Self::Publish { .. } | Self::Drop { .. } => None,
        }
    }

    pub(crate) fn drop_file_result(&self) -> Option<FileResult> {
        self.drop_file_result_with_default("change dropped")
    }

    pub(crate) fn drop_file_result_with_default(
        &self,
        default_message: &'static str,
    ) -> Option<FileResult> {
        let reject_publish = self.reject_publish();
        let Self::Drop { path, reason, .. } = self else {
            return None;
        };
        Some(FileResult {
            path: path.clone(),
            status: if reject_publish {
                CommitStatus::Failed
            } else {
                CommitStatus::Dropped
            },
            message: reason.map_or_else(
                || default_message.to_owned(),
                |reason| reason.as_str().to_owned(),
            ),
            observed_version: None,
            observed_state: reject_publish.then(|| "route_rejected".to_owned()),
        })
    }
}

pub(super) fn publish_decision(
    path: LayerPath,
    route: Route,
    base_hash: Option<String>,
    drop_reason: Option<RouteDropReason>,
) -> PublishDecision {
    match route {
        Route::Gated => PublishDecision::gated(path, base_hash),
        Route::Direct => PublishDecision::direct(path),
        Route::Drop => PublishDecision::dropped(path, drop_reason),
    }
}

pub(super) fn rejected_drop_decision(
    path: LayerPath,
    drop_reason: RouteDropReason,
) -> PublishDecision {
    PublishDecision::rejected_drop(path, drop_reason)
}
