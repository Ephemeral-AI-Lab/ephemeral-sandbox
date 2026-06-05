use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use crate::mode::WorkspaceMode;
use crate::mutation::{
    WorkspaceMutationKind, WorkspaceMutationOutcome, WorkspaceMutationRequest,
    WorkspaceMutationSink,
};
use crate::read_view::WorkspaceReadView;
use crate::response::{ChangedPathKinds, WorkspaceApiError, WorkspaceConflict, WorkspaceTimings};

/// Read one text file from a workspace mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFileRequest {
    pub path: String,
    pub max_read_bytes: usize,
}

/// Write one file into a workspace mode.
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

impl From<WorkspaceMutationOutcome> for WriteFileOutcome {
    fn from(outcome: WorkspaceMutationOutcome) -> Self {
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
    pub fn from_mutation(outcome: WorkspaceMutationOutcome, applied_edits: i64) -> Self {
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

/// Shared direct file API.
pub trait WorkspaceFileOps {
    fn read_file(&self, request: ReadFileRequest) -> Result<ReadFileOutcome, WorkspaceApiError>;
    fn write_file(&self, request: WriteFileRequest) -> Result<WriteFileOutcome, WorkspaceApiError>;
    fn edit_file(&self, request: EditFileRequest) -> Result<EditFileOutcome, WorkspaceApiError>;
}

/// Read one text file through a mode-specific read view.
///
/// # Errors
///
/// Returns [`WorkspaceApiError`] when the path cannot be resolved, the file
/// cannot be read, or the read exceeds `max_read_bytes`.
pub fn read_file<P>(
    ports: &P,
    mode: WorkspaceMode,
    request: ReadFileRequest,
) -> Result<ReadFileOutcome, WorkspaceApiError>
where
    P: WorkspaceReadView,
{
    let total_start = Instant::now();
    let path = ports.resolve_path(&request.path)?;
    let read = ports.read_bytes(&path)?;
    let content = if read.exists {
        let bytes = read.bytes.unwrap_or_default();
        if bytes.len() > request.max_read_bytes {
            return Err(WorkspaceApiError::invalid_request(format!(
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
    Ok(read_outcome(mode, content, read.exists, timings))
}

/// Write one file through a mode-specific mutation sink.
///
/// # Errors
///
/// Returns [`WorkspaceApiError`] when the path cannot be resolved, the base
/// cannot be read, the request is too large, or the mutation sink fails.
pub fn write_file<P>(
    ports: &P,
    mode: WorkspaceMode,
    mutation_source: &str,
    request: WriteFileRequest,
) -> Result<WriteFileOutcome, WorkspaceApiError>
where
    P: WorkspaceReadView + WorkspaceMutationSink,
{
    let total_start = Instant::now();
    if request.content.len() > request.max_file_bytes {
        return Err(WorkspaceApiError::invalid_request(format!(
            "file too large: {} > {} bytes",
            request.content.len(),
            request.max_file_bytes
        )));
    }
    let path = ports.resolve_path(&request.path)?;
    let base = ports.read_bytes(&path)?;
    if !request.overwrite && base.exists {
        let mut timings = base.timings;
        insert_total(&mut timings, "write", total_start);
        return Ok(write_conflict(
            mode,
            mutation_source,
            &path.path,
            "rejected",
            "create_only_existing",
            "file already exists",
            timings,
        ));
    }
    let mut outcome = ports.commit_or_record(WorkspaceMutationRequest {
        kind: WorkspaceMutationKind::Write,
        path,
        content: request.content,
        base,
    })?;
    insert_total(&mut outcome.timings, "write", total_start);
    Ok(outcome.into())
}

/// Apply exact search/replace edits through a mode-specific mutation sink.
///
/// # Errors
///
/// Returns [`WorkspaceApiError`] when the path cannot be resolved, the base
/// cannot be read, the file is not UTF-8, or the mutation sink fails.
pub fn edit_file<P>(
    ports: &P,
    mode: WorkspaceMode,
    mutation_source: &str,
    request: EditFileRequest,
) -> Result<EditFileOutcome, WorkspaceApiError>
where
    P: WorkspaceReadView + WorkspaceMutationSink,
{
    let total_start = Instant::now();
    let path = ports.resolve_path(&request.path)?;
    let base = ports.read_bytes(&path)?;
    if !base.exists {
        let mut timings = base.timings;
        insert_total(&mut timings, "edit", total_start);
        return Ok(edit_conflict(
            mode,
            mutation_source,
            &path.path,
            "aborted_version",
            "aborted_version",
            "file does not exist",
            timings,
        ));
    }
    let bytes = base.bytes.clone().unwrap_or_default();
    let mut content = String::from_utf8(bytes).map_err(|err| {
        WorkspaceApiError::invalid_request(format!("file is not utf-8 text: {err}"))
    })?;
    for edit in &request.edits {
        if edit.old_text.is_empty() {
            return Err(WorkspaceApiError::invalid_request(
                "edit anchor old_text must be non-empty",
            ));
        }
        match apply_search_replace(&content, &edit.old_text, &edit.new_text, edit.replace_all) {
            Ok(next) => content = next,
            Err(err) => {
                let mut timings = base.timings;
                insert_total(&mut timings, "edit", total_start);
                return Ok(edit_conflict(
                    mode,
                    mutation_source,
                    &path.path,
                    "aborted_overlap",
                    "aborted_overlap",
                    search_replace_message(&err),
                    timings,
                ));
            }
        }
    }
    let mut outcome = ports.commit_or_record(WorkspaceMutationRequest {
        kind: WorkspaceMutationKind::Edit,
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

#[must_use]
pub fn read_outcome(
    mode: WorkspaceMode,
    content: String,
    exists: bool,
    timings: WorkspaceTimings,
) -> ReadFileOutcome {
    ReadFileOutcome {
        mode,
        success: true,
        content,
        exists,
        encoding: "utf-8".to_owned(),
        timings,
    }
}

#[must_use]
pub fn write_conflict(
    mode: WorkspaceMode,
    mutation_source: &str,
    path: &str,
    status: &str,
    reason: &str,
    message: &str,
    timings: WorkspaceTimings,
) -> WriteFileOutcome {
    WriteFileOutcome {
        mode,
        success: false,
        published: false,
        status: status.to_owned(),
        conflict: Some(WorkspaceConflict::path(reason, path, message)),
        conflict_reason: Some(reason.to_owned()),
        changed_paths: Vec::new(),
        changed_path_kinds: ChangedPathKinds::new(),
        mutation_source: mutation_source.to_owned(),
        timings,
    }
}

#[must_use]
pub fn edit_conflict(
    mode: WorkspaceMode,
    mutation_source: &str,
    path: &str,
    status: &str,
    reason: &str,
    message: &str,
    timings: WorkspaceTimings,
) -> EditFileOutcome {
    EditFileOutcome {
        mode,
        success: false,
        published: false,
        status: status.to_owned(),
        conflict: Some(WorkspaceConflict::path(reason, path, message)),
        conflict_reason: Some(reason.to_owned()),
        changed_paths: Vec::new(),
        changed_path_kinds: ChangedPathKinds::new(),
        mutation_source: mutation_source.to_owned(),
        timings,
        applied_edits: 0,
    }
}

pub fn insert_total(timings: &mut WorkspaceTimings, verb: &str, start: Instant) {
    timings.insert(
        format!("api.{verb}.total_s"),
        json!(start.elapsed().as_secs_f64()),
    );
}

/// Search/replace failure. Message strings are part of the public conflict
/// contract and match `eos-protocol`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SearchReplaceError {
    #[error("edit anchor old_text must be non-empty")]
    EmptyAnchor,
    #[error("anchor not found")]
    NotFound,
    #[error("anchor occurrence count mismatch")]
    CountMismatch,
}

const fn search_replace_message(err: &SearchReplaceError) -> &'static str {
    match err {
        SearchReplaceError::EmptyAnchor => "edit anchor old_text must be non-empty",
        SearchReplaceError::NotFound => "anchor not found",
        SearchReplaceError::CountMismatch => "anchor occurrence count mismatch",
    }
}

/// Apply one search/replace edit with Python `str.count` semantics.
///
/// # Errors
///
/// Returns [`SearchReplaceError`] when the anchor is empty, absent, or ambiguous
/// with `replace_all=false`.
pub fn apply_search_replace(
    text: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, SearchReplaceError> {
    if old.is_empty() {
        return Err(SearchReplaceError::EmptyAnchor);
    }
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
