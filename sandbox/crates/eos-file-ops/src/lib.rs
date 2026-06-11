#![forbid(unsafe_code)]

use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::Value;
use thiserror::Error;

mod direct;
mod isolated;

pub use direct::DirectBackend;
pub use isolated::IsolatedBackend;

use std::collections::BTreeMap;

pub type WorkspaceTimings = BTreeMap<String, Value>;

pub type ChangedPathKinds = BTreeMap<String, String>;

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

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct FileOpsError(String);

impl FileOpsError {
    #[must_use]
    pub fn new(_kind: &str, message: String) -> Self {
        Self(message)
    }

    #[must_use]
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message.into())
    }
}

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mutation {
    pub kind: MutationKind,
    pub path: ResolvedWorkspacePath,
    pub content: Vec<u8>,
    pub base: ReadBytes,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MutationOutcome {
    pub workspace_kind: String,
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
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub applied_edits: i64,
}

pub trait FileBackend {
    fn workspace_kind(&self) -> &'static str;

    fn mutation_source(&self, kind: MutationKind) -> &'static str;

    fn resolve_path(&self, request_path: &str) -> Result<ResolvedWorkspacePath, FileOpsError>;

    fn read_bytes(&self, path: &ResolvedWorkspacePath) -> Result<ReadBytes, FileOpsError>;

    fn apply(&self, mutation: Mutation) -> Result<MutationOutcome, FileOpsError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFileRequest {
    pub path: String,
    pub max_read_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteFileRequest {
    pub path: String,
    pub content: Vec<u8>,
    pub overwrite: bool,
    pub max_file_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchReplaceEdit {
    pub old_text: String,
    pub new_text: String,
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditFileRequest {
    pub path: String,
    pub edits: Vec<SearchReplaceEdit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadFileOutcome {
    pub workspace_kind: String,
    pub success: bool,
    pub content: String,
    pub exists: bool,
    pub encoding: String,
    #[serde(default)]
    pub timings: WorkspaceTimings,
}

pub type WriteFileOutcome = MutationOutcome;
pub type EditFileOutcome = MutationOutcome;

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
        workspace_kind: backend.workspace_kind().to_owned(),
        success: true,
        content,
        exists: read.exists,
        encoding: "utf-8".to_owned(),
        timings,
    })
}

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
        return Ok(conflict_outcome(
            backend,
            MutationKind::Write,
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
    Ok(outcome)
}

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
        return Ok(conflict_outcome(
            backend,
            MutationKind::Edit,
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
                return Ok(conflict_outcome(
                    backend,
                    MutationKind::Edit,
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
    outcome.applied_edits = i64::try_from(request.edits.len()).unwrap_or(i64::MAX);
    Ok(outcome)
}

fn conflict_outcome<B: FileBackend>(
    backend: &B,
    kind: MutationKind,
    path: &str,
    status: &str,
    reason: &str,
    message: &str,
    timings: WorkspaceTimings,
) -> MutationOutcome {
    MutationOutcome {
        workspace_kind: backend.workspace_kind().to_owned(),
        success: false,
        published: false,
        status: status.to_owned(),
        conflict: Some(WorkspaceConflict::path(reason, path, message)),
        conflict_reason: Some(reason.to_owned()),
        changed_paths: Vec::new(),
        changed_path_kinds: ChangedPathKinds::new(),
        mutation_source: backend.mutation_source(kind).to_owned(),
        timings,
        ..MutationOutcome::default()
    }
}

const fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

fn insert_total(timings: &mut WorkspaceTimings, verb: &str, start: Instant) {
    timings.insert(
        format!("api.{verb}.total_s"),
        json!(start.elapsed().as_secs_f64()),
    );
}

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
#[path = "../tests/unit/lib.rs"]
mod tests;
