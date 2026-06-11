use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ignore::gitignore::GitignoreBuilder;
use ignore::Match;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::{hex_lower, CasError, LayerChange, LayerPath};
use crate::{LayerStack, LayerStackError, Manifest, MergedView};

mod worker;

pub use worker::configure_auto_squash_max_depth;
use worker::{CommitQueue, CommitTransaction, PreparedChangeset};

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
    Storage(#[from] crate::LayerStackError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Gated,
    Direct,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CommitStatus {
    #[serde(rename = "accepted")]
    Accepted,
    #[serde(rename = "committed")]
    Committed,
    #[serde(rename = "aborted_version")]
    AbortedVersion,
    #[serde(rename = "aborted_overlap")]
    AbortedOverlap,
    #[serde(rename = "dropped")]
    Dropped,
    #[serde(rename = "rejected")]
    Rejected,
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
            Self::AbortedOverlap => "aborted_overlap",
            Self::Dropped => "dropped",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub const fn is_published(self) -> bool {
        matches!(self, Self::Accepted | Self::Committed)
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Accepted | Self::Committed | Self::Dropped)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublishDecision {
    pub(crate) path: LayerPath,
    pub(crate) route: Route,
    pub(crate) base_hash: Option<String>,
    pub(crate) message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResult {
    pub path: LayerPath,
    pub status: CommitStatus,
    pub message: String,
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
pub struct ChangesetResult {
    pub files: Vec<FileResult>,
    pub published_manifest_version: Option<u64>,
    pub timings: BTreeMap<String, f64>,
}

impl ChangesetResult {
    #[must_use]
    pub fn success(&self) -> bool {
        self.files.iter().all(|f| f.status.is_success())
    }

    #[must_use]
    pub fn first_conflict(&self) -> Option<&FileResult> {
        self.files.iter().find(|file| !file.status.is_success())
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
}

pub(crate) struct CommitWriter {
    root: PathBuf,
    commit_queue: CommitQueue,
}

impl CommitWriter {
    pub(crate) fn new(root: PathBuf) -> Result<Self, CommitError> {
        let transaction = CommitTransaction { root: root.clone() };
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
        let mut path_groups = Vec::with_capacity(changes.len());
        let mut publishable = Vec::with_capacity(changes.len());
        for change in changes {
            let path = change.path().clone();
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
            path_groups.push(PublishDecision {
                path,
                route,
                base_hash,
                message: drop_message(route),
            });
            if route != Route::Drop {
                publishable.push(change.clone());
            }
        }
        let receiver = self.commit_queue.submit(PreparedChangeset {
            path_groups,
            changes: publishable,
            atomic,
        })?;
        let mut result = receiver
            .recv()
            .map_err(|_| CommitError::ReplyDisconnected)??;
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

fn route_for_path(stack: &LayerStack, path: &LayerPath) -> Result<Route, LayerStackError> {
    if path.as_str() == ".git" || path.as_str().starts_with(".git/") {
        return Ok(Route::Drop);
    }
    if path_is_ignored(stack, path.as_str())? {
        Ok(Route::Direct)
    } else {
        Ok(Route::Gated)
    }
}

fn drop_message(route: Route) -> Option<String> {
    (route == Route::Drop).then(|| ".git paths are not mutable through OCC".to_owned())
}

fn stack_base_hash(stack: &LayerStack, path: &LayerPath) -> Result<Option<String>, CommitError> {
    let (bytes, exists) = stack
        .read_bytes(path.as_str())
        .map_err(|err| CommitError::RoutePreparation(err.to_string()))?;
    Ok(hash_current(bytes.as_deref(), exists))
}

fn path_is_ignored(stack: &LayerStack, path: &str) -> Result<bool, LayerStackError> {
    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        return Ok(false);
    }
    let parts: Vec<&str> = rel.split('/').collect();
    let mut accum = String::new();
    for part in &parts[..parts.len() - 1] {
        accum = join_rel(&accum, part);
        if dir_is_excluded(stack, &accum)? {
            return Ok(true);
        }
    }
    match_with_inheritance(stack, rel, false)
}

fn dir_is_excluded(stack: &LayerStack, dir_rel: &str) -> Result<bool, LayerStackError> {
    let mut accum = String::new();
    let mut excluded = false;
    for part in dir_rel.split('/').filter(|part| !part.is_empty()) {
        accum = join_rel(&accum, part);
        if !excluded {
            excluded = match_with_inheritance(stack, &accum, true)?;
        }
    }
    Ok(excluded)
}

fn match_with_inheritance(
    stack: &LayerStack,
    path: &str,
    as_dir: bool,
) -> Result<bool, LayerStackError> {
    let parts: Vec<&str> = path.split('/').collect();
    let mut ignored = false;
    let mut accum = String::new();
    for part in &parts {
        if let Some(matcher) = matcher_for(stack, &accum)? {
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
    stack: &LayerStack,
    dir_rel: &str,
) -> Result<Option<ignore::gitignore::Gitignore>, LayerStackError> {
    let rel = join_rel(dir_rel, ".gitignore");
    let (bytes, exists) = stack.read_bytes(&rel)?;
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
