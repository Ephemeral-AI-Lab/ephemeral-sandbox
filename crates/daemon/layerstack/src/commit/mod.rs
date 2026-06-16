use std::collections::{BTreeMap, BTreeSet};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use ignore::gitignore::GitignoreBuilder;
use ignore::Match;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::capture::{ProtectedPathDrop, ProtectedPathDropReason};
use crate::fs::resolve_layer_path;
use crate::model::{hex_lower, CasError, LayerChange, LayerPath};
use crate::{LayerStack, LayerStackError, Manifest, MergedView};

mod worker;

use worker::{CommitQueue, CommitTransaction, PreparedChangeset};

pub const GIT_METADATA_UNSUPPORTED_DROP_REASON: &str = "git_metadata_unsupported";
pub const DAEMON_CONTROL_PATH_DROP_REASON: &str = "daemon_control_path";
pub const COMMAND_SCRATCH_PATH_DROP_REASON: &str = "command_scratch_path";
pub const UNSUPPORTED_SPECIAL_FILE_DROP_REASON: &str = "unsupported_special_file";
pub const INVALID_LAYER_PATH_DROP_REASON: &str = "invalid_layer_path";
pub const OPAQUE_DIR_PROTECTED_DESCENDANT_DROP_REASON: &str = "opaque_dir_protected_descendant";
pub const OPAQUE_DIR_MIXED_ROUTES_DROP_REASON: &str = "opaque_dir_mixed_routes";
pub const OPAQUE_DIR_EXPANSION_LIMIT_DROP_REASON: &str = "opaque_dir_expansion_limit";

const OPAQUE_DIR_EXPANSION_LIMIT: usize = 4096;
const LOGICAL_WHITEOUT_PREFIX: &str = ".wh.";
const OPAQUE_MARKER: &str = ".wh..wh..opq";
#[cfg(target_os = "linux")]
const TRUSTED_OVERLAY_WHITEOUT_XATTR: &str = "trusted.overlay.whiteout";
#[cfg(target_os = "linux")]
const USER_OVERLAY_WHITEOUT_XATTR: &str = "user.overlay.whiteout";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteDropReason {
    GitMetadataUnsupported,
    DaemonControlPath,
    CommandScratchPath,
    UnsupportedSpecialFile,
    InvalidLayerPath,
    OpaqueDirProtectedDescendant,
    OpaqueDirMixedRoutes,
    OpaqueDirExpansionLimit,
}

impl RouteDropReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GitMetadataUnsupported => GIT_METADATA_UNSUPPORTED_DROP_REASON,
            Self::DaemonControlPath => DAEMON_CONTROL_PATH_DROP_REASON,
            Self::CommandScratchPath => COMMAND_SCRATCH_PATH_DROP_REASON,
            Self::UnsupportedSpecialFile => UNSUPPORTED_SPECIAL_FILE_DROP_REASON,
            Self::InvalidLayerPath => INVALID_LAYER_PATH_DROP_REASON,
            Self::OpaqueDirProtectedDescendant => OPAQUE_DIR_PROTECTED_DESCENDANT_DROP_REASON,
            Self::OpaqueDirMixedRoutes => OPAQUE_DIR_MIXED_ROUTES_DROP_REASON,
            Self::OpaqueDirExpansionLimit => OPAQUE_DIR_EXPANSION_LIMIT_DROP_REASON,
        }
    }
}

impl From<ProtectedPathDropReason> for RouteDropReason {
    fn from(reason: ProtectedPathDropReason) -> Self {
        match reason {
            ProtectedPathDropReason::UnsupportedSpecialFile => Self::UnsupportedSpecialFile,
            ProtectedPathDropReason::InvalidLayerPath => Self::InvalidLayerPath,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CommitError {
    #[error("occ commit queue is closed")]
    QueueClosed,

    #[error("occ commit queue has not been started")]
    QueueNotStarted,

    #[error("occ commit queue worker failed to start: {0}")]
    WorkerStart(String),

    #[error("occ commit queue worker panicked")]
    WorkerPanicked,

    #[error("occ commit queue state lock poisoned: {0}")]
    QueueStatePoisoned(&'static str),

    #[error("occ commit reply channel disconnected")]
    ReplyDisconnected,

    #[error("occ route preparation failed: {0}")]
    RoutePreparation(String),

    #[error(transparent)]
    Cas(#[from] CasError),

    #[error(transparent)]
    Capture(#[from] crate::capture::CaptureError),

    #[error(transparent)]
    Storage(#[from] crate::LayerStackError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Gated,
    Direct,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitStatus {
    #[serde(rename = "accepted")]
    Accepted,
    #[serde(rename = "committed")]
    Committed,
    #[serde(rename = "aborted_version")]
    AbortedVersion,
    #[serde(rename = "dropped")]
    Dropped,
    #[serde(rename = "failed")]
    Failed,
}

impl CommitStatus {
    #[must_use]
    pub const fn wire_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Committed => "committed",
            Self::AbortedVersion => "aborted_version",
            Self::Dropped => "dropped",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub const fn is_published(self) -> bool {
        matches!(self, Self::Accepted | Self::Committed)
    }

    #[must_use]
    pub const fn is_non_conflicting(self) -> bool {
        matches!(self, Self::Accepted | Self::Committed | Self::Dropped)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublishDecision {
    pub(crate) path: LayerPath,
    pub(crate) route: Route,
    pub(crate) base_hash: Option<String>,
    pub(crate) drop_reason: Option<RouteDropReason>,
    pub(crate) reject_publish: bool,
    pub(crate) validation_base_hashes: Option<Vec<(LayerPath, Option<String>)>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResult {
    pub path: LayerPath,
    pub status: CommitStatus,
    pub message: String,
    pub observed_version: Option<u64>,
    pub observed_state: Option<String>,
}

impl FileResult {
    #[must_use]
    pub fn conflict_message<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.message.is_empty() {
            fallback
        } else {
            self.message.as_str()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OccTraceEvent {
    pub module: &'static str,
    pub name: &'static str,
    pub details: Value,
}

impl OccTraceEvent {
    #[must_use]
    pub fn new(module: &'static str, name: &'static str, details: Value) -> Self {
        Self {
            module,
            name,
            details,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChangesetResult {
    pub files: Vec<FileResult>,
    pub published_manifest_version: Option<u64>,
    pub timings: BTreeMap<String, f64>,
    pub events: Vec<OccTraceEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitOptions {
    pub auto_squash_max_depth: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureRouteStats {
    pub gated_path_count: usize,
    pub direct_path_count: usize,
    pub drop_path_count: usize,
    pub direct_bytes: u64,
    pub direct_spooled_bytes: u64,
    pub ignored_limit_drop_reason: Option<String>,
    pub drop_reason_counts: BTreeMap<String, usize>,
}

impl CaptureRouteStats {
    #[must_use]
    pub fn drop_reason_count(&self, reason: &str) -> usize {
        self.drop_reason_counts.get(reason).copied().unwrap_or(0)
    }

    pub(crate) fn record_drop_reason(&mut self, reason: &str) {
        *self
            .drop_reason_counts
            .entry(reason.to_owned())
            .or_default() += 1;
    }

    fn record_route_drop_reason(&mut self, reason: RouteDropReason) {
        self.record_drop_reason(reason.as_str());
    }
}

impl Default for CommitOptions {
    fn default() -> Self {
        Self {
            auto_squash_max_depth: crate::AUTO_SQUASH_MAX_DEPTH,
        }
    }
}

impl CommitOptions {
    #[must_use]
    pub fn new(auto_squash_max_depth: usize) -> Self {
        Self {
            auto_squash_max_depth: auto_squash_max_depth.max(1),
        }
    }
}

impl ChangesetResult {
    #[must_use]
    pub fn success(&self) -> bool {
        self.files.iter().all(|f| f.status.is_non_conflicting())
    }

    #[must_use]
    pub fn first_conflict(&self) -> Option<&FileResult> {
        self.files
            .iter()
            .find(|file| !file.status.is_non_conflicting())
    }

    #[must_use]
    pub fn published_paths(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|file| file.status.is_published())
            .map(|file| file.path.as_str().to_owned())
            .collect()
    }

    #[must_use]
    pub fn published_file_count(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.status.is_published())
            .count()
    }

    #[must_use]
    pub fn trace_events(&self) -> Vec<OccTraceEvent> {
        let mut events = vec![OccTraceEvent::new(
            "occ",
            "commit_started",
            json!({
                "file_count": self.files.len(),
                "gated_path_count": self.timings.get("occ.commit.gated_path_count").copied(),
                "direct_path_count": self.timings.get("occ.commit.direct_path_count").copied(),
            }),
        )];
        events.push(OccTraceEvent::new(
            "occ",
            "validate_groups_finished",
            json!({
                "file_count": self.files.len(),
                "accepted_file_count": self.status_count(CommitStatus::Accepted),
                "committed_file_count": self.status_count(CommitStatus::Committed),
                "dropped_file_count": self.status_count(CommitStatus::Dropped),
                "aborted_version_file_count": self.status_count(CommitStatus::AbortedVersion),
                "failed_file_count": self.status_count(CommitStatus::Failed),
                "duration_s": self.timings.get("occ.commit.validate_groups_s").copied(),
            }),
        ));
        events.push(OccTraceEvent::new(
            "occ",
            "commit_finished",
            json!({
                "success": self.success(),
                "published_manifest_version": self.published_manifest_version,
                "file_count": self.files.len(),
                "published_file_count": self.published_file_count(),
                "accepted_file_count": self.status_count(CommitStatus::Accepted),
                "committed_file_count": self.status_count(CommitStatus::Committed),
                "dropped_file_count": self.status_count(CommitStatus::Dropped),
                "aborted_version_file_count": self.status_count(CommitStatus::AbortedVersion),
                "failed_file_count": self.status_count(CommitStatus::Failed),
                "gated_path_count": self.timings.get("occ.commit.gated_path_count").copied(),
                "direct_path_count": self.timings.get("occ.commit.direct_path_count").copied(),
                "duration_s": self.timings.get("occ.commit.total_s").copied(),
            }),
        ));
        events.extend(self.events.clone());
        events.extend(
            self.files
                .iter()
                .filter(|file| !file.status.is_non_conflicting())
                .map(|file| {
                    OccTraceEvent::new(
                        "occ",
                        "conflict_detected",
                        json!({
                            "path": file.path.as_str(),
                            "reason": file.status.wire_str(),
                            "message": file.conflict_message(file.status.wire_str()),
                            "observed_version": file.observed_version,
                            "observed_state": file.observed_state,
                        }),
                    )
                }),
        );
        events
    }

    fn status_count(&self, status: CommitStatus) -> usize {
        self.files
            .iter()
            .filter(|file| file.status == status)
            .count()
    }
}

pub(crate) struct CommitWriter {
    root: PathBuf,
    commit_queue: CommitQueue,
}

impl CommitWriter {
    pub(crate) fn with_options(root: PathBuf, options: CommitOptions) -> Result<Self, CommitError> {
        let options = CommitOptions::new(options.auto_squash_max_depth);
        let transaction = CommitTransaction {
            root: root.clone(),
            options,
        };
        let mut commit_queue = CommitQueue::new(transaction);
        commit_queue.start()?;
        Ok(Self { root, commit_queue })
    }

    pub(crate) fn apply_changeset_with_base_hashes(
        &self,
        changes: &[LayerChange],
        snapshot_version: Option<u64>,
        atomic: bool,
        base_hashes: &[(LayerPath, Option<String>)],
    ) -> Result<ChangesetResult, CommitError> {
        let stack = self.open_stack()?;
        let manifest = stack
            .read_active_manifest()
            .map_err(|err| CommitError::RoutePreparation(err.to_string()))?;
        let view = MergedView::new(self.root.clone());
        let source = ManifestIgnoreSource {
            view: &view,
            manifest: &manifest,
        };
        let mut path_groups = Vec::with_capacity(changes.len());
        for change in changes {
            let path = change.path().clone();
            let decision = if matches!(change, LayerChange::OpaqueDir { .. }) {
                publish_decision_for_opaque_dir(
                    &self.root,
                    &source,
                    &view,
                    &manifest,
                    &path,
                    OPAQUE_DIR_EXPANSION_LIMIT,
                )?
            } else {
                let route = route_for_path(&stack, &path)
                    .map_err(|err| CommitError::RoutePreparation(err.to_string()))?;
                let base_hash = if route == Route::Gated {
                    match base_hashes.iter().find(|(candidate, _)| candidate == &path) {
                        Some((_, hash)) => hash.clone(),
                        None => stack_base_hash(&stack, &path)?,
                    }
                } else {
                    None
                };
                publish_decision(
                    path,
                    route,
                    base_hash,
                    drop_reason_code(route, change.path()),
                )
            };
            path_groups.push(decision);
        }
        self.apply_changeset_with_decisions(changes, snapshot_version, atomic, path_groups)
    }

    pub(crate) fn apply_changeset_with_decisions(
        &self,
        changes: &[LayerChange],
        snapshot_version: Option<u64>,
        atomic: bool,
        path_groups: Vec<PublishDecision>,
    ) -> Result<ChangesetResult, CommitError> {
        if changes.len() > path_groups.len() {
            return Err(CommitError::RoutePreparation(format!(
                "changeset has more payload changes than route decisions: {} changes, {} decisions",
                changes.len(),
                path_groups.len()
            )));
        }
        for (change, group) in changes.iter().zip(path_groups.iter()) {
            if change.path() != &group.path {
                return Err(CommitError::RoutePreparation(format!(
                    "changeset decision path mismatch: change {}, decision {}",
                    change.path().as_str(),
                    group.path.as_str()
                )));
            }
        }
        if let Some(group) = path_groups
            .iter()
            .skip(changes.len())
            .find(|group| group.route != Route::Drop)
        {
            return Err(CommitError::RoutePreparation(format!(
                "payload-less route decision must be dropped: {}",
                group.path.as_str()
            )));
        }
        let publishable = changes
            .iter()
            .zip(path_groups.iter())
            .filter(|(_, group)| group.route != Route::Drop)
            .map(|(change, _)| change.clone())
            .collect::<Vec<_>>();
        let handoff_event = worker_handoff_event(&path_groups, publishable.len(), atomic);
        let receiver = self.commit_queue.submit(PreparedChangeset {
            path_groups,
            changes: publishable,
            atomic,
        })?;
        let mut result = receiver
            .recv()
            .map_err(|_| CommitError::ReplyDisconnected)??;
        result.events.insert(0, handoff_event);
        if let (Some(published), Some(snapshot)) =
            (result.published_manifest_version, snapshot_version)
        {
            result.timings.insert(
                "occ.apply.manifest_lag".to_owned(),
                published.saturating_sub(snapshot + 1) as f64,
            );
        }
        Ok(result)
    }

    pub(crate) fn apply_command_lane_aware_changeset(
        &self,
        changes: &[LayerChange],
        snapshot_version: Option<u64>,
        path_groups: Vec<PublishDecision>,
    ) -> Result<ChangesetResult, CommitError> {
        self.apply_changeset_with_decisions(changes, snapshot_version, true, path_groups)
    }

    fn open_stack(&self) -> Result<LayerStack, CommitError> {
        LayerStack::open(self.root.clone())
            .map_err(|err| CommitError::RoutePreparation(err.to_string()))
    }
}

impl Drop for CommitWriter {
    fn drop(&mut self) {
        let _ = self.commit_queue.close();
    }
}

fn worker_handoff_event(
    path_groups: &[PublishDecision],
    publishable_change_count: usize,
    atomic: bool,
) -> OccTraceEvent {
    OccTraceEvent::new(
        "occ",
        "worker_handoff",
        json!({
            "path_count": path_groups.len(),
            "publishable_change_count": publishable_change_count,
            "atomic": atomic,
            "gated_path_count": route_count(path_groups, Route::Gated),
            "direct_path_count": route_count(path_groups, Route::Direct),
            "drop_path_count": route_count(path_groups, Route::Drop),
            "drop_reason_counts": route_drop_reason_counts(path_groups),
        }),
    )
}

fn route_count(path_groups: &[PublishDecision], route: Route) -> usize {
    path_groups
        .iter()
        .filter(|group| group.route == route)
        .count()
}

fn route_drop_reason_counts(path_groups: &[PublishDecision]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for group in path_groups
        .iter()
        .filter(|group| group.route == Route::Drop)
    {
        if let Some(reason) = group.drop_reason {
            *counts.entry(reason.as_str().to_owned()).or_default() += 1;
        }
    }
    counts
}

pub fn capture_route_stats_for_manifest_with_protected_drops(
    root: &Path,
    manifest: &Manifest,
    changes: &[LayerChange],
    protected_drops: &[ProtectedPathDrop],
) -> Result<CaptureRouteStats, CommitError> {
    let decisions = publish_decisions_for_manifest_with_protected_drops(
        root,
        manifest,
        changes,
        protected_drops,
    )?;
    let mut stats = CaptureRouteStats::default();
    for (index, decision) in decisions.iter().enumerate() {
        match decision.route {
            Route::Gated => stats.gated_path_count += 1,
            Route::Direct => {
                stats.direct_path_count += 1;
                if let Some(change) = changes.get(index) {
                    stats.direct_bytes = stats
                        .direct_bytes
                        .saturating_add(change.write_size().unwrap_or(0));
                    stats.direct_spooled_bytes = stats
                        .direct_spooled_bytes
                        .saturating_add(change.spooled_write_size().unwrap_or(0));
                }
            }
            Route::Drop => {
                stats.drop_path_count += 1;
                if let Some(reason) = decision.drop_reason {
                    stats.record_route_drop_reason(reason);
                }
            }
        }
    }
    Ok(stats)
}

pub(crate) fn publish_decisions_for_manifest_with_protected_drops(
    root: &Path,
    manifest: &Manifest,
    changes: &[LayerChange],
    protected_drops: &[ProtectedPathDrop],
) -> Result<Vec<PublishDecision>, CommitError> {
    let view = MergedView::new(root.to_path_buf());
    let source = ManifestIgnoreSource {
        view: &view,
        manifest,
    };
    let mut decisions = changes
        .iter()
        .map(|change| {
            if let LayerChange::OpaqueDir { path } = change {
                publish_decision_for_opaque_dir(
                    root,
                    &source,
                    &view,
                    manifest,
                    path,
                    OPAQUE_DIR_EXPANSION_LIMIT,
                )
            } else {
                let path = change.path().clone();
                let route = route_for_path_from_source(&source, &path)
                    .map_err(|err| CommitError::RoutePreparation(err.to_string()))?;
                let base_hash = if route == Route::Gated {
                    snapshot_base_hash(&view, manifest, change)?
                } else {
                    None
                };
                Ok(publish_decision(
                    path,
                    route,
                    base_hash,
                    drop_reason_code(route, change.path()),
                ))
            }
        })
        .collect::<std::result::Result<Vec<_>, CommitError>>()?;
    decisions.extend(
        protected_drops
            .iter()
            .map(publish_decision_for_protected_drop),
    );
    Ok(decisions)
}

fn publish_decision_for_protected_drop(drop: &ProtectedPathDrop) -> PublishDecision {
    PublishDecision {
        path: drop.path.clone(),
        route: Route::Drop,
        base_hash: None,
        drop_reason: Some(RouteDropReason::from(drop.reason)),
        reject_publish: false,
        validation_base_hashes: None,
    }
}

fn publish_decision(
    path: LayerPath,
    route: Route,
    base_hash: Option<String>,
    drop_reason: Option<RouteDropReason>,
) -> PublishDecision {
    PublishDecision {
        path,
        route,
        base_hash,
        drop_reason,
        reject_publish: false,
        validation_base_hashes: None,
    }
}

fn rejected_drop_decision(path: LayerPath, drop_reason: RouteDropReason) -> PublishDecision {
    PublishDecision {
        path,
        route: Route::Drop,
        base_hash: None,
        drop_reason: Some(drop_reason),
        reject_publish: true,
        validation_base_hashes: None,
    }
}

fn publish_decision_for_opaque_dir(
    root: &Path,
    source: &impl IgnoreSource,
    view: &MergedView,
    manifest: &Manifest,
    path: &LayerPath,
    expansion_limit: usize,
) -> Result<PublishDecision, CommitError> {
    let hidden = match visible_paths_hidden_by_opaque_dir(root, manifest, path, expansion_limit)? {
        OpaqueDirExpansion::Complete(paths) => paths,
        OpaqueDirExpansion::LimitExceeded => {
            return Ok(rejected_drop_decision(
                path.clone(),
                RouteDropReason::OpaqueDirExpansionLimit,
            ));
        }
    };

    if hidden.is_empty() {
        let route = route_for_path_from_source(source, path)
            .map_err(|err| CommitError::RoutePreparation(err.to_string()))?;
        let mut decision =
            publish_decision(path.clone(), route, None, drop_reason_code(route, path));
        if route == Route::Gated {
            decision.validation_base_hashes = Some(Vec::new());
        }
        return Ok(decision);
    }

    let mut gated_paths = Vec::new();
    let mut direct_paths = Vec::new();
    for hidden_path in &hidden {
        match route_for_path_from_source(source, hidden_path)
            .map_err(|err| CommitError::RoutePreparation(err.to_string()))?
        {
            Route::Drop => {
                return Ok(rejected_drop_decision(
                    path.clone(),
                    RouteDropReason::OpaqueDirProtectedDescendant,
                ));
            }
            Route::Gated => gated_paths.push(hidden_path.clone()),
            Route::Direct => direct_paths.push(hidden_path.clone()),
        }
    }

    if !gated_paths.is_empty() && !direct_paths.is_empty() {
        return Ok(rejected_drop_decision(
            path.clone(),
            RouteDropReason::OpaqueDirMixedRoutes,
        ));
    }

    if !direct_paths.is_empty() {
        return Ok(publish_decision(path.clone(), Route::Direct, None, None));
    }

    let validation_base_hashes = gated_paths
        .iter()
        .map(|hidden_path| {
            Ok((
                hidden_path.clone(),
                snapshot_base_hash_for_path(view, manifest, hidden_path)?,
            ))
        })
        .collect::<Result<Vec<_>, CommitError>>()?;
    let mut decision = publish_decision(path.clone(), Route::Gated, None, None);
    decision.validation_base_hashes = Some(validation_base_hashes);
    Ok(decision)
}

fn route_for_path(stack: &LayerStack, path: &LayerPath) -> Result<Route, LayerStackError> {
    route_for_path_from_source(stack, path)
}

fn route_for_path_from_source(
    source: &impl IgnoreSource,
    path: &LayerPath,
) -> Result<Route, LayerStackError> {
    if is_git_metadata_path(path) {
        return Ok(Route::Drop);
    }
    if protected_path_drop_reason(path).is_some() {
        return Ok(Route::Drop);
    }
    if path_is_ignored(source, path.as_str())? {
        Ok(Route::Direct)
    } else {
        Ok(Route::Gated)
    }
}

fn drop_reason_code(route: Route, path: &LayerPath) -> Option<RouteDropReason> {
    if route != Route::Drop {
        return None;
    }
    if is_git_metadata_path(path) {
        return Some(RouteDropReason::GitMetadataUnsupported);
    }
    protected_path_drop_reason(path)
}

fn is_git_metadata_path(path: &LayerPath) -> bool {
    path.as_str().split('/').any(|part| part == ".git")
}

fn protected_path_drop_reason(path: &LayerPath) -> Option<RouteDropReason> {
    let path = path.as_str();
    let mut parts = path.split('/');
    let first = parts.next()?;
    if matches!(
        first,
        "manifest.json" | "workspace.json" | "layers" | "staging"
    ) || first == ".layer-metadata"
        || parts.any(|part| part == ".layer-metadata")
    {
        return Some(RouteDropReason::DaemonControlPath);
    }
    if is_command_scratch_path(path) {
        return Some(RouteDropReason::CommandScratchPath);
    }
    None
}

fn is_command_scratch_path(path: &str) -> bool {
    if matches!(
        path,
        "command-runner-request.json"
            | "command-runner-result.json"
            | "runner-request.json"
            | "runner-result.json"
            | "metadata.json"
            | "final.json"
            | "transcript.log"
    ) {
        return true;
    }

    let mut parts = path.split('/');
    let Some(first) = parts.next() else {
        return false;
    };
    matches!(
        first,
        "spool"
            | "commands"
            | ".eos-command"
            | ".eos-commands"
            | ".eos-scratch"
            | ".eos-spool"
            | ".eos-transcripts"
    ) || parts.any(|part| {
        matches!(
            part,
            ".eos-command" | ".eos-commands" | ".eos-scratch" | ".eos-spool" | ".eos-transcripts"
        )
    })
}

fn stack_base_hash(stack: &LayerStack, path: &LayerPath) -> Result<Option<String>, CommitError> {
    let (bytes, exists) = stack
        .read_bytes(path.as_str())
        .map_err(|err| CommitError::RoutePreparation(err.to_string()))?;
    Ok(hash_current(bytes.as_deref(), exists))
}

fn snapshot_base_hash(
    view: &MergedView,
    manifest: &Manifest,
    change: &LayerChange,
) -> Result<Option<String>, CommitError> {
    if matches!(change, LayerChange::OpaqueDir { .. }) {
        return Ok(None);
    }
    snapshot_base_hash_for_path(view, manifest, change.path())
}

fn snapshot_base_hash_for_path(
    view: &MergedView,
    manifest: &Manifest,
    path: &LayerPath,
) -> Result<Option<String>, CommitError> {
    let (bytes, exists) = view
        .read_bytes(path.as_str(), manifest)
        .map_err(|err| CommitError::RoutePreparation(err.to_string()))?;
    Ok(hash_current(bytes.as_deref(), exists))
}

trait IgnoreSource {
    fn read_bytes(&self, path: &str) -> Result<(Option<Vec<u8>>, bool), LayerStackError>;
}

impl IgnoreSource for LayerStack {
    fn read_bytes(&self, path: &str) -> Result<(Option<Vec<u8>>, bool), LayerStackError> {
        Self::read_bytes(self, path)
    }
}

struct ManifestIgnoreSource<'a> {
    view: &'a MergedView,
    manifest: &'a Manifest,
}

impl IgnoreSource for ManifestIgnoreSource<'_> {
    fn read_bytes(&self, path: &str) -> Result<(Option<Vec<u8>>, bool), LayerStackError> {
        self.view.read_bytes(path, self.manifest)
    }
}

fn path_is_ignored(source: &impl IgnoreSource, path: &str) -> Result<bool, LayerStackError> {
    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        return Ok(false);
    }
    let parts: Vec<&str> = rel.split('/').collect();
    let mut accum = String::new();
    for part in &parts[..parts.len() - 1] {
        accum = join_rel(&accum, part);
        if dir_is_excluded(source, &accum)? {
            return Ok(true);
        }
    }
    match_with_inheritance(source, rel, false)
}

fn dir_is_excluded(source: &impl IgnoreSource, dir_rel: &str) -> Result<bool, LayerStackError> {
    let mut accum = String::new();
    let mut excluded = false;
    for part in dir_rel.split('/').filter(|part| !part.is_empty()) {
        accum = join_rel(&accum, part);
        if !excluded {
            excluded = match_with_inheritance(source, &accum, true)?;
        }
    }
    Ok(excluded)
}

fn match_with_inheritance(
    source: &impl IgnoreSource,
    path: &str,
    as_dir: bool,
) -> Result<bool, LayerStackError> {
    let parts: Vec<&str> = path.split('/').collect();
    let mut ignored = false;
    let mut accum = String::new();
    for part in &parts {
        if let Some(matcher) = matcher_for(source, &accum)? {
            let sub = if accum.is_empty() {
                path
            } else {
                path[accum.len()..].trim_start_matches('/')
            };
            if !sub.is_empty() {
                match matcher.matched(sub, as_dir) {
                    Match::Ignore(_) => ignored = true,
                    Match::Whitelist(_) => ignored = false,
                    Match::None => {}
                }
            }
        }
        accum = join_rel(&accum, part);
    }
    Ok(ignored)
}

fn matcher_for(
    source: &impl IgnoreSource,
    dir_rel: &str,
) -> Result<Option<ignore::gitignore::Gitignore>, LayerStackError> {
    let rel = join_rel(dir_rel, ".gitignore");
    let (bytes, exists) = source.read_bytes(&rel)?;
    if !exists {
        return Ok(None);
    }
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    let mut builder = GitignoreBuilder::new(".");
    for line in text.lines() {
        let _ = builder.add_line(None, line);
    }
    Ok(builder.build().ok())
}

fn join_rel(prefix: &str, child: &str) -> String {
    if prefix.is_empty() {
        child.to_owned()
    } else {
        format!("{prefix}/{child}")
    }
}

enum OpaqueDirExpansion {
    Complete(Vec<LayerPath>),
    LimitExceeded,
}

fn visible_paths_hidden_by_opaque_dir(
    root: &Path,
    manifest: &Manifest,
    opaque_path: &LayerPath,
    expansion_limit: usize,
) -> Result<OpaqueDirExpansion, CommitError> {
    let mut visible = BTreeSet::new();
    let mut blockers = Vec::<String>::new();
    for layer in &manifest.layers {
        let layer_dir = resolve_layer_path(root, &layer.path);
        if !layer_dir.is_dir() {
            return Err(CommitError::RoutePreparation(format!(
                "manifest references missing layer {}: {}",
                layer.layer_id, layer.path
            )));
        }
        let mut layer_blockers = Vec::new();
        collect_opaque_hidden_paths_from_layer(
            &layer_dir,
            opaque_path.as_str(),
            &blockers,
            &mut visible,
            &mut layer_blockers,
            expansion_limit,
        )?;
        if visible.len() > expansion_limit {
            return Ok(OpaqueDirExpansion::LimitExceeded);
        }
        blockers.extend(layer_blockers);
    }
    Ok(OpaqueDirExpansion::Complete(visible.into_iter().collect()))
}

fn collect_opaque_hidden_paths_from_layer(
    layer_dir: &Path,
    opaque_path: &str,
    older_blockers: &[String],
    visible: &mut BTreeSet<LayerPath>,
    layer_blockers: &mut Vec<String>,
    expansion_limit: usize,
) -> Result<(), CommitError> {
    if path_is_blocked(opaque_path, older_blockers) {
        return Ok(());
    }
    collect_logical_whiteout_for_exact_path(layer_dir, opaque_path, layer_blockers);

    let target = resolve_layer_path(layer_dir, opaque_path);
    let Ok(meta) = std::fs::symlink_metadata(&target) else {
        return Ok(());
    };
    if is_kernel_whiteout_meta(&target, &meta) {
        layer_blockers.push(opaque_path.to_owned());
        return Ok(());
    }
    if meta.file_type().is_symlink() || meta.is_file() {
        insert_visible_hidden_path(visible, opaque_path, expansion_limit)?;
        layer_blockers.push(opaque_path.to_owned());
        return Ok(());
    }
    if !meta.is_dir() {
        return Ok(());
    }

    let mut stack = vec![target];
    while let Some(dir) = stack.pop() {
        let mut entries = read_sorted_dir(&dir)?;
        for entry in entries.drain(..) {
            let path = entry.path();
            let rel = layer_relative_string(layer_dir, &path)?;
            if !is_equal_or_descendant(&rel, opaque_path) {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|err| CommitError::RoutePreparation(err.to_string()))?;
            if name == OPAQUE_MARKER {
                if let Some(target) = parent_rel(&rel) {
                    layer_blockers.push(target);
                }
                continue;
            }
            if let Some(target) = logical_whiteout_target(&rel, name) {
                layer_blockers.push(target);
                continue;
            }
            if is_kernel_whiteout_meta(&path, &meta) {
                layer_blockers.push(rel);
                continue;
            }
            if path_is_blocked(&rel, older_blockers) {
                continue;
            }
            if meta.file_type().is_symlink() || meta.is_file() {
                insert_visible_hidden_path(visible, &rel, expansion_limit)?;
                layer_blockers.push(rel);
            } else if meta.is_dir() {
                stack.push(path);
            }
        }
    }
    Ok(())
}

fn collect_logical_whiteout_for_exact_path(
    layer_dir: &Path,
    path: &str,
    layer_blockers: &mut Vec<String>,
) {
    let Some((parent, name)) = path.rsplit_once('/') else {
        let whiteout = resolve_layer_path(layer_dir, &format!("{LOGICAL_WHITEOUT_PREFIX}{path}"));
        if whiteout.exists() {
            layer_blockers.push(path.to_owned());
        }
        return;
    };
    let whiteout = resolve_layer_path(
        layer_dir,
        &format!("{parent}/{LOGICAL_WHITEOUT_PREFIX}{name}"),
    );
    if whiteout.exists() {
        layer_blockers.push(path.to_owned());
    }
}

fn read_sorted_dir(dir: &Path) -> Result<Vec<std::fs::DirEntry>, CommitError> {
    let mut entries = std::fs::read_dir(dir)
        .map_err(|err| CommitError::RoutePreparation(err.to_string()))?
        .collect::<io::Result<Vec<_>>>()
        .map_err(|err| CommitError::RoutePreparation(err.to_string()))?;
    entries.sort_by_key(std::fs::DirEntry::path);
    Ok(entries)
}

fn insert_visible_hidden_path(
    visible: &mut BTreeSet<LayerPath>,
    path: &str,
    _expansion_limit: usize,
) -> Result<(), CommitError> {
    visible.insert(LayerPath::parse(path)?);
    Ok(())
}

fn layer_relative_string(layer_dir: &Path, path: &Path) -> Result<String, CommitError> {
    let rel = path
        .strip_prefix(layer_dir)
        .map_err(|err| CommitError::RoutePreparation(err.to_string()))?;
    let mut parts = Vec::new();
    for component in rel.components() {
        let part = component.as_os_str().to_str().ok_or_else(|| {
            CommitError::RoutePreparation(format!(
                "layer path component is not valid UTF-8: {:?}",
                component.as_os_str().as_encoded_bytes()
            ))
        })?;
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn logical_whiteout_target(rel: &str, name: &str) -> Option<String> {
    if !name.starts_with(LOGICAL_WHITEOUT_PREFIX) || name == OPAQUE_MARKER {
        return None;
    }
    let target_name = name.strip_prefix(LOGICAL_WHITEOUT_PREFIX)?;
    Some(match rel.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{target_name}"),
        None => target_name.to_owned(),
    })
}

fn parent_rel(rel: &str) -> Option<String> {
    rel.rsplit_once('/')
        .map(|(parent, _)| parent.to_owned())
        .filter(|parent| !parent.is_empty())
}

fn path_is_blocked(path: &str, blockers: &[String]) -> bool {
    blockers
        .iter()
        .any(|blocker| is_equal_or_descendant(path, blocker))
}

fn is_equal_or_descendant(path: &str, ancestor: &str) -> bool {
    path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(unix)]
fn is_kernel_whiteout_meta(_path: &Path, meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_char_device() && meta.rdev() == 0 {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        meta.is_file()
            && meta.len() == 0
            && (has_xattr(_path, TRUSTED_OVERLAY_WHITEOUT_XATTR)
                || has_xattr(_path, USER_OVERLAY_WHITEOUT_XATTR))
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(not(unix))]
const fn is_kernel_whiteout_meta(_path: &Path, _meta: &std::fs::Metadata) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn has_xattr(path: &Path, name: &str) -> bool {
    let mut value = [0_u8; 1];
    rustix::fs::lgetxattr(path, name, &mut value).is_ok()
}

#[cfg(test)]
pub fn base_hashes_for_snapshot(
    root: &Path,
    manifest: &Manifest,
    changes: &[LayerChange],
) -> Result<Vec<(LayerPath, Option<String>)>, LayerStackError> {
    let view = MergedView::new(root.to_path_buf());
    changes
        .iter()
        .map(|change| {
            if matches!(change, LayerChange::OpaqueDir { .. }) {
                return Ok((change.path().clone(), None));
            }
            let (bytes, exists) = view.read_bytes(change.path().as_str(), manifest)?;
            Ok((
                change.path().clone(),
                hash_current(bytes.as_deref(), exists),
            ))
        })
        .collect()
}

#[must_use]
pub fn hash_current(content: Option<&[u8]>, exists: bool) -> Option<String> {
    if !exists {
        return None;
    }
    content.map(|content| {
        let mut hasher = Sha256::new();
        hasher.update(content);
        hex_lower(hasher.finalize())
    })
}

#[cfg(test)]
#[path = "../../tests/unit/route.rs"]
mod route_tests;
