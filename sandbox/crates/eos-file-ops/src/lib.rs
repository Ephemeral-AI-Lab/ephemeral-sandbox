//! The file tool family: read / write / edit semantics over one backend.
//!
//! The semantics (size caps, create-only conflicts, exact search/replace with
//! occurrence checks, base-content pinning) live here once; a [`FileBackend`]
//! decides what a read resolves against and what an apply means durably:
//!
//! - [`DirectBackend`] — the fast path: reads resolve against the latest
//!   merged state of the layer stack, applies commit through the per-root
//!   single writer gated by the base hash the read observed. No overlay is
//!   involved at any point.
//! - [`IsolatedBackend`] — reads resolve upperdir-first then through the
//!   frozen snapshot layers; applies write into the workspace's private
//!   upperdir and never publish.
#![forbid(unsafe_code)]

use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

mod direct;
mod isolated;

pub use direct::DirectBackend;
pub use isolated::IsolatedBackend;

use std::collections::BTreeMap;
use serde_json::Value;

/// Workspace mode that produced a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    /// Shared publish-capable workspace path.
    #[default]
    Ephemeral,
    /// Caller-private no-publish workspace path.
    Isolated,
}

impl WorkspaceMode {
    /// Stable daemon/API string for this mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ephemeral => "ephemeral",
            Self::Isolated => "isolated",
        }
    }
}

/// Timing/telemetry map keyed by stable wire strings.
pub type WorkspaceTimings = BTreeMap<String, Value>;

/// `path -> kind` map for changed paths (wire-stable kind strings).
pub type ChangedPathKinds = BTreeMap<String, String>;

/// A per-path conflict surfaced on the response envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConflict {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_file: Option<String>,
    pub message: String,
}

impl WorkspaceConflict {
    #[must_use]
    pub fn path(reason: &str, conflict_file: &str, message: &str) -> Self {
        Self {
            reason: reason.to_owned(),
            conflict_file: Some(conflict_file.to_owned()),
            message: message.to_owned(),
        }
    }
}

/// File-tier API error carrying a stable wire kind.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct FileOpsError {
    pub kind: String,
    pub message: String,
}

impl FileOpsError {
    #[must_use]
    pub fn new(kind: &str, message: String) -> Self {
        Self {
            kind: kind.to_owned(),
            message,
        }
    }

    #[must_use]
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message.into())
    }
}

/// A workspace-relative path after backend-specific root resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedWorkspacePath {
    pub path: String,
}

impl ResolvedWorkspacePath {
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

/// Bytes read from the backend's view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadBytes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<Vec<u8>>,
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_version: Option<i64>,
    #[serde(default)]
    pub timings: WorkspaceTimings,
}

/// Mutation kind produced by a file operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationKind {
    Write,
    Edit,
}

impl MutationKind {
    #[must_use]
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Edit => "edit",
        }
    }
}

/// Mutation passed to the backend's apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mutation {
    pub kind: MutationKind,
    pub path: ResolvedWorkspacePath,
    pub content: Vec<u8>,
    pub base: ReadBytes,
}

/// Normalized mutation outcome before daemon JSON conversion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MutationOutcome {
    pub mode: WorkspaceMode,
    pub success: bool,
    /// True only when the mutation reached shared workspace truth.
    pub published: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<WorkspaceConflict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_reason: Option<String>,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub changed_path_kinds: ChangedPathKinds,
    #[serde(default)]
    pub mutation_source: String,
    #[serde(default)]
    pub timings: WorkspaceTimings,
}

/// What a read resolves against and what an apply means durably.
///
/// The two real implementations are [`DirectBackend`] (latest merged state +
/// gated single-writer commit) and [`IsolatedBackend`] (upperdir-first reads,
/// private upperdir writes, never published).
pub trait FileBackend {
    fn mode(&self) -> WorkspaceMode;

    /// Stable `mutation_source` string for outcomes of `kind`.
    fn mutation_source(&self, kind: MutationKind) -> &'static str;

    /// Normalize a request path into the backend's workspace-relative form.
    ///
    /// # Errors
    ///
    /// Returns [`FileOpsError`] when the path is invalid for the workspace.
    fn resolve_path(&self, request_path: &str) -> Result<ResolvedWorkspacePath, FileOpsError>;

    /// Read the path's current bytes in the backend's view.
    ///
    /// # Errors
    ///
    /// Returns [`FileOpsError`] when the view cannot be read.
    fn read_bytes(&self, path: &ResolvedWorkspacePath) -> Result<ReadBytes, FileOpsError>;

    /// Make `mutation` durable per the backend's policy.
    ///
    /// # Errors
    ///
    /// Returns [`FileOpsError`] when the apply fails (a publish conflict is an
    /// `Ok` outcome carrying `conflict`, not an error).
    fn apply(&self, mutation: Mutation) -> Result<MutationOutcome, FileOpsError>;
}

/// Read one text file from a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFileRequest {
    pub path: String,
    pub max_read_bytes: usize,
}

/// Write one file into a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteFileRequest {
    pub path: String,
    pub content: Vec<u8>,
    pub overwrite: bool,
    pub max_file_bytes: usize,
}

/// One exact-match replacement for edit_file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchReplaceEdit {
    pub old_text: String,
    pub new_text: String,
    #[serde(default)]
    pub replace_all: bool,
}

/// Apply search/replace edits to one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditFileRequest {
    pub path: String,
    pub edits: Vec<SearchReplaceEdit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadFileOutcome {
    pub mode: WorkspaceMode,
    pub success: bool,
    pub content: String,
    pub exists: bool,
    pub encoding: String,
    #[serde(default)]
    pub timings: WorkspaceTimings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteFileOutcome {
    pub mode: WorkspaceMode,
    pub success: bool,
    pub published: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<WorkspaceConflict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_reason: Option<String>,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub changed_path_kinds: ChangedPathKinds,
    #[serde(default)]
    pub mutation_source: String,
    #[serde(default)]
    pub timings: WorkspaceTimings,
}

impl From<MutationOutcome> for WriteFileOutcome {
    fn from(outcome: MutationOutcome) -> Self {
        Self {
            mode: outcome.mode,
            success: outcome.success,
            published: outcome.published,
            status: outcome.status,
            conflict: outcome.conflict,
            conflict_reason: outcome.conflict_reason,
            changed_paths: outcome.changed_paths,
            changed_path_kinds: outcome.changed_path_kinds,
            mutation_source: outcome.mutation_source,
            timings: outcome.timings,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditFileOutcome {
    pub mode: WorkspaceMode,
    pub success: bool,
    pub published: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<WorkspaceConflict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_reason: Option<String>,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub changed_path_kinds: ChangedPathKinds,
    #[serde(default)]
    pub mutation_source: String,
    #[serde(default)]
    pub timings: WorkspaceTimings,
    pub applied_edits: i64,
}

impl EditFileOutcome {
    #[must_use]
    pub fn from_mutation(outcome: MutationOutcome, applied_edits: i64) -> Self {
        Self {
            mode: outcome.mode,
            success: outcome.success,
            published: outcome.published,
            status: outcome.status,
            conflict: outcome.conflict,
            conflict_reason: outcome.conflict_reason,
            changed_paths: outcome.changed_paths,
            changed_path_kinds: outcome.changed_path_kinds,
            mutation_source: outcome.mutation_source,
            timings: outcome.timings,
            applied_edits,
        }
    }
}

/// Read one text file through a backend.
///
/// # Errors
///
/// Returns [`FileOpsError`] when the path cannot be resolved, the file cannot
/// be read, or the read exceeds `max_read_bytes`.
pub fn read_file<B: FileBackend>(
    backend: &B,
    request: ReadFileRequest,
) -> Result<ReadFileOutcome, FileOpsError> {
    let total_start = Instant::now();
    let path = backend.resolve_path(&request.path)?;
    let read = backend.read_bytes(&path)?;
    let content = if read.exists {
        let bytes = read.bytes.unwrap_or_default();
        if bytes.len() > request.max_read_bytes {
            return Err(FileOpsError::invalid_request(format!(
                "file too large: {} > {} bytes",
                bytes.len(),
                request.max_read_bytes
            )));
        }
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::new()
    };
    let mut timings = read.timings;
    insert_total(&mut timings, "read", total_start);
    Ok(ReadFileOutcome {
        mode: backend.mode(),
        success: true,
        content,
        exists: read.exists,
        encoding: "utf-8".to_owned(),
        timings,
    })
}

/// Write one file through a backend.
///
/// # Errors
///
/// Returns [`FileOpsError`] when the path cannot be resolved, the base cannot
/// be read, the request is too large, or the apply fails.
pub fn write_file<B: FileBackend>(
    backend: &B,
    request: WriteFileRequest,
) -> Result<WriteFileOutcome, FileOpsError> {
    let total_start = Instant::now();
    if request.content.len() > request.max_file_bytes {
        return Err(FileOpsError::invalid_request(format!(
            "file too large: {} > {} bytes",
            request.content.len(),
            request.max_file_bytes
        )));
    }
    let path = backend.resolve_path(&request.path)?;
    let base = backend.read_bytes(&path)?;
    if !request.overwrite && base.exists {
        let mut timings = base.timings;
        insert_total(&mut timings, "write", total_start);
        return Ok(write_conflict(
            backend,
            &path.path,
            "rejected",
            "create_only_existing",
            "file already exists",
            timings,
        ));
    }
    let mut outcome = backend.apply(Mutation {
        kind: MutationKind::Write,
        path,
        content: request.content,
        base,
    })?;
    insert_total(&mut outcome.timings, "write", total_start);
    Ok(outcome.into())
}

/// Apply exact search/replace edits through a backend.
///
/// # Errors
///
/// Returns [`FileOpsError`] when the path cannot be resolved, the base cannot
/// be read, the file is not UTF-8, or the apply fails.
pub fn edit_file<B: FileBackend>(
    backend: &B,
    request: EditFileRequest,
) -> Result<EditFileOutcome, FileOpsError> {
    let total_start = Instant::now();
    let path = backend.resolve_path(&request.path)?;
    let base = backend.read_bytes(&path)?;
    if !base.exists {
        let mut timings = base.timings;
        insert_total(&mut timings, "edit", total_start);
        return Ok(edit_conflict(
            backend,
            &path.path,
            "aborted_version",
            "aborted_version",
            "file does not exist",
            timings,
        ));
    }
    let bytes = base.bytes.clone().unwrap_or_default();
    let mut content = String::from_utf8(bytes)
        .map_err(|err| FileOpsError::invalid_request(format!("file is not utf-8 text: {err}")))?;
    for edit in &request.edits {
        if edit.old_text.is_empty() {
            return Err(FileOpsError::invalid_request(
                "edit anchor old_text must be non-empty",
            ));
        }
        match apply_search_replace(&content, &edit.old_text, &edit.new_text, edit.replace_all) {
            Ok(next) => content = next,
            Err(err) => {
                let mut timings = base.timings;
                insert_total(&mut timings, "edit", total_start);
                return Ok(edit_conflict(
                    backend,
                    &path.path,
                    "aborted_overlap",
                    "aborted_overlap",
                    search_replace_message(&err),
                    timings,
                ));
            }
        }
    }
    let mut outcome = backend.apply(Mutation {
        kind: MutationKind::Edit,
        path,
        content: content.into_bytes(),
        base,
    })?;
    insert_total(&mut outcome.timings, "edit", total_start);
    Ok(EditFileOutcome::from_mutation(
        outcome,
        i64::try_from(request.edits.len()).unwrap_or(i64::MAX),
    ))
}

fn write_conflict<B: FileBackend>(
    backend: &B,
    path: &str,
    status: &str,
    reason: &str,
    message: &str,
    timings: WorkspaceTimings,
) -> WriteFileOutcome {
    WriteFileOutcome {
        mode: backend.mode(),
        success: false,
        published: false,
        status: status.to_owned(),
        conflict: Some(WorkspaceConflict::path(reason, path, message)),
        conflict_reason: Some(reason.to_owned()),
        changed_paths: Vec::new(),
        changed_path_kinds: ChangedPathKinds::new(),
        mutation_source: backend.mutation_source(MutationKind::Write).to_owned(),
        timings,
    }
}

fn edit_conflict<B: FileBackend>(
    backend: &B,
    path: &str,
    status: &str,
    reason: &str,
    message: &str,
    timings: WorkspaceTimings,
) -> EditFileOutcome {
    EditFileOutcome {
        mode: backend.mode(),
        success: false,
        published: false,
        status: status.to_owned(),
        conflict: Some(WorkspaceConflict::path(reason, path, message)),
        conflict_reason: Some(reason.to_owned()),
        changed_paths: Vec::new(),
        changed_path_kinds: ChangedPathKinds::new(),
        mutation_source: backend.mutation_source(MutationKind::Edit).to_owned(),
        timings,
        applied_edits: 0,
    }
}

fn insert_total(timings: &mut WorkspaceTimings, verb: &str, start: Instant) {
    timings.insert(
        format!("api.{verb}.total_s"),
        json!(start.elapsed().as_secs_f64()),
    );
}

/// Search/replace failure. Message strings are part of the public conflict
/// contract.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
enum SearchReplaceError {
    #[error("anchor not found")]
    NotFound,
    #[error("anchor occurrence count mismatch")]
    CountMismatch,
}

const fn search_replace_message(err: &SearchReplaceError) -> &'static str {
    match err {
        SearchReplaceError::NotFound => "anchor not found",
        SearchReplaceError::CountMismatch => "anchor occurrence count mismatch",
    }
}

/// Apply one search/replace edit with Rust `str.count` semantics. The anchor is
/// non-empty: `edit_file` rejects empty anchors before calling this.
fn apply_search_replace(
    text: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, SearchReplaceError> {
    let count = text.matches(old).count();
    if replace_all {
        if count == 0 {
            return Err(SearchReplaceError::NotFound);
        }
        Ok(text.replace(old, new))
    } else {
        match count {
            0 => Err(SearchReplaceError::NotFound),
            1 => Ok(text.replacen(old, new, 1)),
            _ => Err(SearchReplaceError::CountMismatch),
        }
    }
}

#[cfg(test)]
mod tests;
