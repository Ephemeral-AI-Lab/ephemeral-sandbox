//! Runtime input/output DTOs for the `read`/`write`/`edit` file operations.
//! Field names mirror the local-os tools of the same name; sandbox-only
//! `workspace_session_id` selects the session backend, and the host-only
//! `mtime` fields are dropped (a layerstack publish has no faithful analog).

use crate::workspace_crate::WorkspaceSessionId;

#[derive(Debug, Clone)]
pub struct ListInput {
    pub path: Option<String>,
    pub limit: Option<usize>,
    pub workspace_session_id: Option<WorkspaceSessionId>,
}

/// Kind of one listed directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileListEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

impl FileListEntryKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileListEntry {
    pub name: String,
    pub kind: FileListEntryKind,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListOutput {
    pub path: String,
    pub entries: Vec<FileListEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct ReadInput {
    pub path: String,
    pub offset: Option<u64>,
    pub limit: Option<usize>,
    pub workspace_session_id: Option<WorkspaceSessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOutput {
    pub path: String,
    pub content: String,
    pub start_line: u64,
    pub num_lines: usize,
    pub total_lines: u64,
    pub bytes_read: usize,
    pub total_bytes: u64,
    pub next_offset: Option<u64>,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct WriteInput {
    pub path: String,
    pub content: String,
    pub request_id: String,
    pub workspace_session_id: Option<WorkspaceSessionId>,
}

/// Whether a write created a new file or overwrote an existing regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    Create,
    Update,
}

impl WriteKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutput {
    pub kind: WriteKind,
    pub path: String,
    pub bytes_written: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditOp {
    pub old_string: String,
    pub new_string: String,
    pub replace_all: bool,
}

#[derive(Debug, Clone)]
pub struct EditInput {
    pub path: String,
    pub edits: Vec<EditOp>,
    pub request_id: String,
    pub workspace_session_id: Option<WorkspaceSessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditOutput {
    pub path: String,
    pub edits_applied: usize,
    pub replacements: usize,
    pub bytes_written: usize,
}
