//! Shared verb request/response wire models + the search/replace primitive.
//!
//! Invariant: these model the WIRE shape (the daemon's recursively-serialized
//! response object), NOT the Rust DTO front door. Field order/name/type follow
//! the §6 serialization in `docs/contract/04-shared-models.md`. Two
//! representations of the same types, kept canonically-equal by fixtures.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// Read cap shared by `read_file` (over it raises `ValueError`).
pub const MAX_READ_BYTES: usize = 16 * 1024 * 1024;
/// Per-file write/edit cap.
pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

/// The single enum in the verb model; serialized as its `.value` string.
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
// Verb request-args models (the WIRE `args` shape, NOT the Rust DTO).
//
// These carry only the verb-specific keys the daemon primitive reads out of
// `args`; the identity envelope (`caller_id`/`caller`/`invocation_id`) and the
// standard members (`layer_stack_root`, protocol version) are injected at the
// envelope layer (see `docs/contract/04-shared-models.md` §0/§1, the
// three-layer trap). Optional keys use `skip_serializing_if` so the wire form
// matches "sent only when non-None" exactly.
// ---------------------------------------------------------------------------

/// `read_file` request args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFileArgs {
    pub path: String,
}

/// `write_file` request args. `overwrite` defaults `true` at the primitive, so
/// an omitted wire key deserializes to `true` to match the documented contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
    #[serde(default = "write_overwrite_default")]
    pub overwrite: bool,
}

fn write_overwrite_default() -> bool {
    true
}

/// `edit_file` request args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditFileArgs {
    pub path: String,
    pub edits: Vec<SearchReplaceEdit>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yield_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
}

/// Public `exec_command` / command-session result payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecCommandResult {
    pub status: String,
    pub exit_code: Option<i64>,
    pub output: CommandOutput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_session_id: Option<String>,
}

/// `write_stdin` request args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSessionWriteArgs {
    pub command_session_id: String,
    pub chars: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yield_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
}

/// Internal `api.v1.command.cancel` request args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSessionCancelArgs {
    pub command_session_id: String,
}

/// `read_file` response (`SandboxResultBase` + content/exists/encoding).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// A single search/replace edit on the wire: `{old_text, new_text, replace_all}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchReplaceEdit {
    pub old_text: String,
    pub new_text: String,
    #[serde(default)]
    pub replace_all: bool,
}

/// Failure of [`apply_search_replace`]; the message strings are part of the
/// contract.
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

/// Apply one search/replace edit.
///
/// Pure; mirrors Rust `apply_search_replace` including non-overlapping
/// `str.count` semantics and exact error messages.
///
/// # Errors
///
/// Returns [`SearchReplaceError::EmptyAnchor`] for an empty anchor,
/// [`SearchReplaceError::NotFound`] when the anchor is absent, or
/// [`SearchReplaceError::CountMismatch`] when `replace_all=false` and the
/// anchor appears more than once.
pub fn apply_search_replace(
    text: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, SearchReplaceError> {
    if old.is_empty() {
        return Err(SearchReplaceError::EmptyAnchor);
    }
    // Number of non-overlapping occurrences of the anchor.
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

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn intent_wire_values() -> TestResult {
        assert_eq!(
            serde_json::to_value(Intent::ReadOnly)?,
            Value::String("read_only".to_owned())
        );
        assert_eq!(
            serde_json::to_value(Intent::WriteAllowed)?,
            Value::String("write_allowed".to_owned())
        );
        assert_eq!(
            serde_json::to_value(Intent::Lifecycle)?,
            Value::String("lifecycle".to_owned())
        );
        Ok(())
    }

    #[test]
    fn request_args_wire_shapes() -> TestResult {
        assert_eq!(
            serde_json::to_value(ExecCommandArgs {
                cmd: "printf hi".to_owned(),
                yield_time_ms: Some(250),
                timeout_seconds: None,
                max_output_tokens: None,
            })?,
            serde_json::json!({"cmd":"printf hi","yield_time_ms":250})
        );
        assert_eq!(
            serde_json::to_value(ExecCommandResult {
                status: "running".to_owned(),
                exit_code: None,
                output: CommandOutput {
                    stdout: "hi".to_owned(),
                    stderr: String::new(),
                },
                command_session_id: Some("cmd_1".to_owned()),
            })?,
            serde_json::json!({
                "status": "running",
                "exit_code": null,
                "output": {"stdout": "hi", "stderr": ""},
                "command_session_id": "cmd_1",
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
        let v = serde_json::to_value(&edit)?;
        assert_eq!(serde_json::from_value::<EditFileArgs>(v)?, edit);
        Ok(())
    }

    #[test]
    fn conflict_info_wire_shape() -> TestResult {
        assert_eq!(
            serde_json::to_value(ConflictInfo::overlap("/w/f.txt", "overlap"))?,
            serde_json::json!({"reason":"aborted_overlap","conflict_file":"/w/f.txt","message":"overlap"})
        );
        assert_eq!(
            serde_json::to_value(ConflictInfo::rejected("rejected", ""))?,
            serde_json::json!({"reason":"rejected","conflict_file":null,"message":""})
        );
        Ok(())
    }

    #[test]
    fn search_replace_semantics() -> TestResult {
        assert_eq!(apply_search_replace("a b a", "a", "X", true)?, "X b X");
        assert_eq!(apply_search_replace("a b", "a", "X", false)?, "X b");
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
        assert_eq!(apply_search_replace("aaa", "aa", "X", false)?, "Xa");
        Ok(())
    }

    #[test]
    fn read_file_result_superset_of_fixture() -> TestResult {
        // The read_file_response fixture is the minimal OCC fast-path dispatch
        // dict (dispatch.py:302-318): success/workspace/content/exists/encoding
        // + timings. Our typed model is the FULL DTO asdict (doc 04 §6),
        // which is a superset. Assert the typed model carries every fixture key
        // (minus timings) with the same value — not the reverse.
        const FIXTURE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/envelopes/read_file_response.json"
        ));
        let fixture: Value = serde_json::from_str(FIXTURE.trim_end())?;
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
        let actual = serde_json::to_value(&result)?;
        let fixture = fixture
            .as_object()
            .ok_or_else(|| std::io::Error::other("read_file fixture must be an object"))?;
        for (key, want) in fixture {
            if key == "timings" {
                continue;
            }
            assert_eq!(actual.get(key), Some(want), "key {key} mismatch");
        }
        Ok(())
    }
}
