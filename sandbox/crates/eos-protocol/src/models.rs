//! Shared verb request/response wire models + the search/replace primitive.
//!
//! Invariant: these model the WIRE shape (the daemon's recursive `asdict`
//! response), NOT the Python dataclass front door. Field order/name/type follow
//! the §6 serialization in `docs/contract/04-shared-models.md`. Two
//! representations of the same types, kept canonically-equal by fixtures.
//! `// PORT backend/src/sandbox/shared/models.py`
//! `// PORT backend/src/sandbox/shared/edit_apply.py:21-48 — apply_search_replace`

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// Read cap shared by `read_file` (over it raises `ValueError`).
/// `// PORT backend/src/sandbox/shared/tool_primitives/read.py:14`
pub const MAX_READ_BYTES: usize = 16 * 1024 * 1024;
/// Per-file grep cap; over it the file is silently skipped.
/// `// PORT backend/src/sandbox/shared/tool_primitives/grep.py:21`
pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
/// Glob result limit; results sorted then sliced, `truncated` if more.
/// `// PORT backend/src/sandbox/shared/tool_primitives/glob.py:17`
pub const DEFAULT_GLOB_LIMIT: usize = 100;

/// The single enum in the verb model; serialized as its `.value` string.
/// `// PORT backend/src/sandbox/shared/models.py:15-20`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// `"read_only"`
    ReadOnly,
    /// `"write_allowed"`
    WriteAllowed,
    /// `"lifecycle"`
    Lifecycle,
}

/// `{reason, conflict_file, message}` — serialized verbatim into guarded results.
/// `// PORT backend/src/sandbox/shared/models.py:115-133`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictInfo {
    pub reason: String,
    pub conflict_file: Option<String>,
    #[serde(default)]
    pub message: String,
}

impl ConflictInfo {
    /// `ConflictInfo.rejected(reason, message)` -> `conflict_file=None`.
    pub fn rejected(reason: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            conflict_file: None,
            message: message.into(),
        }
    }
    /// `ConflictInfo.overlap(path, message)` -> `reason="aborted_overlap"`.
    pub fn overlap(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            reason: "aborted_overlap".to_owned(),
            conflict_file: Some(path.into()),
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Verb request-args models (the WIRE `args` shape, NOT the Python dataclass).
//
// These carry only the verb-specific keys the daemon primitive reads out of
// `args`; the identity envelope (`agent_id`/`caller`/`invocation_id`) and the
// standard members (`layer_stack_root`, protocol version) are injected at the
// envelope layer (see `docs/contract/04-shared-models.md` §0/§1, the
// three-layer trap). Optional keys use `skip_serializing_if` so the wire form
// matches "sent only when non-None" exactly.
// ---------------------------------------------------------------------------

/// `read_file` request args. `// PORT api/tool/read.py:26`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFileArgs {
    pub path: String,
}

/// `write_file` request args. `overwrite` defaults `true` at the primitive.
/// `// PORT api/tool/write.py:26-31, tool_primitives/write.py:14-21`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
    pub overwrite: bool,
}

/// `edit_file` request args. `// PORT api/tool/edit.py:29-40`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditFileArgs {
    pub path: String,
    pub edits: Vec<SearchReplaceEdit>,
}

/// `shell` request args. Key rename: dataclass `timeout` -> wire
/// `timeout_seconds`. `cwd` defaults `"."` at the wrapper.
/// `// PORT api/tool/shell.py:29,49-56`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellArgs {
    pub command: String,
    pub cwd: String,
    pub timeout_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub background: bool,
}

/// Public command output payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
}

/// `exec_command` request args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecCommandArgs {
    pub cmd: String,
    pub tty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yield_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

/// Public `exec_command` / PTY-control result payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecCommandResult {
    pub status: String,
    pub exit_code: Option<i64>,
    pub output: CommandOutput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty_session_id: Option<String>,
}

/// `write_pty_command_stdin` request args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtyWriteArgs {
    pub pty_session_id: String,
    pub chars: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yield_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

/// `check_pty_command_progress` request args.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyProgressArgs {
    pub pty_session_id: String,
    pub time: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

/// `cancel_pty_command` request args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtyCancelArgs {
    pub pty_session_id: String,
}

/// `glob` request args. `path` sent only when non-None.
/// `// PORT api/tool/glob.py:26-28`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobArgs {
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// `grep` request args. `path`/`glob_filter`/`head_limit` sent only when
/// non-None; `head_limit`/`offset` are wire-present but primitive-inert.
/// `// PORT api/tool/grep.py:26-39`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepArgs {
    pub pattern: String,
    pub output_mode: String,
    pub offset: i64,
    pub case_insensitive: bool,
    pub line_numbers: bool,
    pub multiline: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob_filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_limit: Option<i64>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// `read_file` response (`SandboxResultBase` + content/exists/encoding).
/// `// PORT backend/src/sandbox/shared/models.py:162-166`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadFileResult {
    pub success: bool,
    pub workspace: String,
    pub timings: Map<String, Value>,
    pub conflict: Option<ConflictInfo>,
    pub conflict_reason: Option<String>,
    pub changed_paths: Vec<String>,
    pub error: Option<Value>,
    pub content: String,
    pub exists: bool,
    pub encoding: String,
}

/// `write_file` response (`GuardedResultBase`, no added fields).
/// `// PORT backend/src/sandbox/shared/models.py:176-178`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteFileResult {
    pub success: bool,
    pub workspace: String,
    pub timings: Map<String, Value>,
    pub conflict: Option<ConflictInfo>,
    pub conflict_reason: Option<String>,
    pub changed_paths: Vec<String>,
    pub error: Option<Value>,
    pub changed_path_kinds: Map<String, Value>,
    pub mutation_source: String,
    pub status: String,
}

/// `edit_file` response (`GuardedResultBase` + `applied_edits`).
/// `// PORT backend/src/sandbox/shared/models.py:196-198`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditFileResult {
    pub success: bool,
    pub workspace: String,
    pub timings: Map<String, Value>,
    pub conflict: Option<ConflictInfo>,
    pub conflict_reason: Option<String>,
    pub changed_paths: Vec<String>,
    pub error: Option<Value>,
    pub changed_path_kinds: Map<String, Value>,
    pub mutation_source: String,
    pub status: String,
    pub applied_edits: i64,
}

/// `shell` response (`GuardedResultBase` + exit/stdout/stderr/warnings).
/// `// PORT backend/src/sandbox/shared/models.py:212-217`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellResult {
    pub success: bool,
    pub workspace: String,
    pub timings: Map<String, Value>,
    pub conflict: Option<ConflictInfo>,
    pub conflict_reason: Option<String>,
    pub changed_paths: Vec<String>,
    pub error: Option<Value>,
    pub changed_path_kinds: Map<String, Value>,
    pub mutation_source: String,
    pub status: String,
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
    pub warnings: Vec<String>,
}

/// `glob` response (`SandboxResultBase` + filenames/num_files/truncated).
/// `// PORT backend/src/sandbox/shared/models.py:226-230`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobResult {
    pub success: bool,
    pub workspace: String,
    pub timings: Map<String, Value>,
    pub conflict: Option<ConflictInfo>,
    pub conflict_reason: Option<String>,
    pub changed_paths: Vec<String>,
    pub error: Option<Value>,
    pub filenames: Vec<String>,
    pub num_files: i64,
    pub truncated: bool,
}

/// `grep` response (`SandboxResultBase` + grep counters/content).
/// `// PORT backend/src/sandbox/shared/models.py:246-256`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrepResult {
    pub success: bool,
    pub workspace: String,
    pub timings: Map<String, Value>,
    pub conflict: Option<ConflictInfo>,
    pub conflict_reason: Option<String>,
    pub changed_paths: Vec<String>,
    pub error: Option<Value>,
    pub output_mode: String,
    pub filenames: Vec<String>,
    pub content: String,
    pub num_files: i64,
    pub num_lines: i64,
    pub num_matches: i64,
    pub applied_limit: Option<i64>,
    pub applied_offset: i64,
    pub truncated: bool,
}

/// A single search/replace edit on the wire: `{old_text, new_text, replace_all}`.
/// `// PORT backend/src/sandbox/shared/models.py:181-187 — SearchReplaceEdit`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchReplaceEdit {
    pub old_text: String,
    pub new_text: String,
    #[serde(default)]
    pub replace_all: bool,
}

/// Failure of [`apply_search_replace`]; the message strings are part of the
/// contract. `// PORT backend/src/sandbox/shared/edit_apply.py:21-48`
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SearchReplaceError {
    /// `old_text` was empty.
    #[error("edit anchor old_text must be non-empty")]
    EmptyAnchor,
    /// The anchor was not found in the text.
    #[error("anchor not found")]
    NotFound,
    /// `replace_all=false` but the anchor occurred more than once.
    #[error("anchor occurrence count mismatch")]
    CountMismatch,
}

/// Apply one search/replace edit. Pure; mirrors Python `apply_search_replace`
/// including non-overlapping `str.count` semantics and exact error messages.
/// `// PORT backend/src/sandbox/shared/edit_apply.py:21-48`
pub fn apply_search_replace(
    text: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, SearchReplaceError> {
    if old.is_empty() {
        return Err(SearchReplaceError::EmptyAnchor);
    }
    // Python str.count = number of non-overlapping occurrences.
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
mod tests {
    use super::*;

    #[test]
    fn intent_wire_values() {
        assert_eq!(
            serde_json::to_value(Intent::ReadOnly).unwrap(),
            Value::String("read_only".to_owned())
        );
        assert_eq!(
            serde_json::to_value(Intent::WriteAllowed).unwrap(),
            Value::String("write_allowed".to_owned())
        );
        assert_eq!(
            serde_json::to_value(Intent::Lifecycle).unwrap(),
            Value::String("lifecycle".to_owned())
        );
    }

    #[test]
    fn request_args_wire_shapes() {
        // shell: timeout -> timeout_seconds rename; background omitted when false.
        assert_eq!(
            serde_json::to_value(ShellArgs {
                command: "ls".to_owned(),
                cwd: ".".to_owned(),
                timeout_seconds: None,
                background: false,
            })
            .unwrap(),
            serde_json::json!({"command":"ls","cwd":".","timeout_seconds":null})
        );
        assert_eq!(
            serde_json::to_value(ExecCommandArgs {
                cmd: "printf hi".to_owned(),
                tty: true,
                yield_time_ms: Some(250),
                timeout_seconds: None,
            })
            .unwrap(),
            serde_json::json!({"cmd":"printf hi","tty":true,"yield_time_ms":250})
        );
        assert_eq!(
            serde_json::to_value(ExecCommandResult {
                status: "running".to_owned(),
                exit_code: None,
                output: CommandOutput {
                    stdout: "hi".to_owned(),
                    stderr: String::new(),
                },
                pty_session_id: Some("pty_1".to_owned()),
            })
            .unwrap(),
            serde_json::json!({
                "status": "running",
                "exit_code": null,
                "output": {"stdout": "hi", "stderr": ""},
                "pty_session_id": "pty_1",
            })
        );
        // glob: path omitted when None.
        assert_eq!(
            serde_json::to_value(GlobArgs {
                pattern: "*.rs".to_owned(),
                path: None,
            })
            .unwrap(),
            serde_json::json!({"pattern":"*.rs"})
        );
        // grep: optional path/glob_filter/head_limit omitted when None.
        assert_eq!(
            serde_json::to_value(GrepArgs {
                pattern: "fn".to_owned(),
                output_mode: "content".to_owned(),
                offset: 0,
                case_insensitive: false,
                line_numbers: true,
                multiline: false,
                path: None,
                glob_filter: None,
                head_limit: None,
            })
            .unwrap(),
            serde_json::json!({
                "pattern":"fn","output_mode":"content","offset":0,
                "case_insensitive":false,"line_numbers":true,"multiline":false
            })
        );
        // read/write/edit round-trip cleanly.
        let edit = EditFileArgs {
            path: "f.txt".to_owned(),
            edits: vec![SearchReplaceEdit {
                old_text: "a".to_owned(),
                new_text: "b".to_owned(),
                replace_all: false,
            }],
        };
        let v = serde_json::to_value(&edit).unwrap();
        assert_eq!(serde_json::from_value::<EditFileArgs>(v).unwrap(), edit);
    }

    #[test]
    fn conflict_info_wire_shape() {
        assert_eq!(
            serde_json::to_value(ConflictInfo::overlap("/w/f.txt", "overlap")).unwrap(),
            serde_json::json!({"reason":"aborted_overlap","conflict_file":"/w/f.txt","message":"overlap"})
        );
        assert_eq!(
            serde_json::to_value(ConflictInfo::rejected("rejected", "")).unwrap(),
            serde_json::json!({"reason":"rejected","conflict_file":null,"message":""})
        );
    }

    #[test]
    fn search_replace_semantics() {
        assert_eq!(
            apply_search_replace("a b a", "a", "X", true).unwrap(),
            "X b X"
        );
        assert_eq!(apply_search_replace("a b", "a", "X", false).unwrap(), "X b");
        // empty `old` -> EmptyAnchor regardless of text/replace_all.
        assert_eq!(
            apply_search_replace("anything", "", "X", false),
            Err(SearchReplaceError::EmptyAnchor)
        );
        assert_eq!(
            apply_search_replace("anything", "", "X", true),
            Err(SearchReplaceError::EmptyAnchor)
        );
        assert_eq!(
            apply_search_replace("xyz", "a", "X", false),
            Err(SearchReplaceError::NotFound)
        );
        assert_eq!(
            apply_search_replace("a a", "a", "X", false),
            Err(SearchReplaceError::CountMismatch)
        );
        // non-overlapping count: "aa" in "aaa" occurs once
        assert_eq!(apply_search_replace("aaa", "aa", "X", false).unwrap(), "Xa");
    }

    #[test]
    fn read_file_result_superset_of_fixture() {
        // The read_file_response fixture is the minimal OCC fast-path dispatch
        // dict (dispatch.py:302-318): success/workspace/content/exists/encoding
        // + timings. Our typed model is the FULL dataclass asdict (doc 04 §6),
        // which is a superset. Assert the typed model carries every fixture key
        // (minus timings) with the same value — not the reverse.
        const FIXTURE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/envelopes/read_file_response.json"
        ));
        let fixture: Value = serde_json::from_str(FIXTURE.trim_end()).unwrap();
        let result = ReadFileResult {
            success: true,
            workspace: "ephemeral".to_owned(),
            timings: Map::new(),
            conflict: None,
            conflict_reason: None,
            changed_paths: vec![],
            error: None,
            content: "# README\n".to_owned(),
            exists: true,
            encoding: "utf-8".to_owned(),
        };
        let actual = serde_json::to_value(&result).unwrap();
        for (key, want) in fixture.as_object().unwrap() {
            if key == "timings" {
                continue;
            }
            assert_eq!(actual.get(key), Some(want), "key {key} mismatch");
        }
    }
}
