//! Pure daemon-envelope helpers: the outbound request-identity payload builder,
//! the hand-written `parse_*_result` decoders, timing normalization, field
//! coercion, and the recoverable-conflict classifier.
//!
//! Ports `sandbox/api/tool/_daemon_response_parsing.py` and
//! `sandbox/api/tool/_conflict_detection.py`. Result decode is **hand-written**,
//! never a blanket `serde_json::from_value` of the envelope into the result
//! struct: the daemon envelope never carries `workspace`, and several fields
//! need defaults / filtering / derivation that raw serde would not apply
//! (invariant 9). Everything here is `pub(crate)`.

use std::collections::BTreeMap;

use eos_types::JsonObject;
use serde_json::Value;

use crate::error::SandboxApiError;
use crate::models::{
    CommandOutput, ConflictInfo, EditFileResult, ExecCommandResult, GlobResult, GrepResult,
    ReadFileResult, SandboxRequestBase, SandboxResultBase, Workspace, WriteFileResult,
};

// ---------------------------------------------------------------------------
// Outbound identity payload
// ---------------------------------------------------------------------------

/// Build the full daemon-envelope identity: a top-level `agent_id`, the nested
/// `caller` block, and (only when present) a top-level `invocation_id`. Mirrors
/// `daemon_request_identity_fields`.
pub(crate) fn daemon_request_identity_fields(base: &SandboxRequestBase) -> JsonObject {
    let mut payload = JsonObject::new();
    payload.insert(
        "agent_id".to_owned(),
        Value::String(base.caller.agent_id.clone()),
    );
    payload.insert(
        "caller".to_owned(),
        Value::Object(base.caller.identity_block()),
    );
    if let Some(invocation_id) = &base.invocation_id {
        payload.insert(
            "invocation_id".to_owned(),
            Value::String(invocation_id.to_string()),
        );
    }
    payload
}

// ---------------------------------------------------------------------------
// Error message + conflict classification
// ---------------------------------------------------------------------------

const DAEMON_INTERNAL_ERROR_PREFIX: &str = "internal_error: ";

/// Strip the daemon `internal_error:` prefix from a raw error message (mirrors
/// `user_visible_error_message`).
pub(crate) fn user_visible_error_message(message: &str) -> &str {
    message
        .strip_prefix(DAEMON_INTERNAL_ERROR_PREFIX)
        .unwrap_or(message)
}

const EDIT_CONFLICT_CODES: [&str; 3] = [
    "aborted_overlap",
    "anchor_not_found",
    "anchor_occurrence_count_mismatch",
];
// Message substrings, already lowercase (ported from conflict_markers.py). Both
// the api side (here) and the audit side (relocated to eos-tools) must keep
// these in sync — see the conflict_markers.py docstring.
const EDIT_CONFLICT_MARKERS: [&str; 3] = [
    "anchor not found",
    "anchor occurrence count mismatch",
    "aborted_overlap",
];
fn matches_conflict(err: &SandboxApiError, codes: &[&str], markers: &[&str]) -> bool {
    if let Some(code) = err.code() {
        let normalized = code.trim().to_lowercase();
        if codes.contains(&normalized.as_str()) {
            return true;
        }
    }
    let lowered = user_visible_error_message(err.message()).to_lowercase();
    markers.iter().any(|marker| lowered.contains(marker))
}

/// Whether a transport error is a recoverable edit conflict (`edit_file` maps it
/// to a successful `Ok(result)` instead of `Err`).
pub(crate) fn is_edit_conflict(err: &SandboxApiError) -> bool {
    matches_conflict(err, &EDIT_CONFLICT_CODES, &EDIT_CONFLICT_MARKERS)
}

// ---------------------------------------------------------------------------
// Field coercion helpers
// ---------------------------------------------------------------------------

/// Python `str(value)` for the values that can appear in a daemon collection.
fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Python `str(value or "")` — the falsy-collapse used by the path/kind filters.
/// Note: a literal numeric `0` collapses to `""` in Python; that path-element
/// edge never occurs for daemon path lists and is treated the same here.
fn py_truthy_str(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(false) => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Number(n) => {
            if n.as_f64() == Some(0.0) {
                String::new()
            } else {
                n.to_string()
            }
        }
        other => other.to_string(),
    }
}

/// `bool(response.get(key, False))`, fail-closed.
fn get_bool(map: &JsonObject, key: &str) -> bool {
    map.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// `str(response.get(key, default))` for the common string-or-default case. A
/// JSON null or absent key yields `default` (Python's `str(None)` quirk on an
/// explicit null is not reproduced — daemon never sends null here).
fn get_string(map: &JsonObject, key: &str, default: &str) -> String {
    match map.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(value) if !value.is_null() => py_str(value),
        _ => default.to_owned(),
    }
}

/// `str(value) if value is not None else None` — present (non-null) keeps the
/// value (even empty), null/absent yields `None`.
fn optional_string(map: &JsonObject, key: &str) -> Option<String> {
    match map.get(key) {
        None | Some(Value::Null) => None,
        Some(value) => Some(py_str(value)),
    }
}

/// `str(response.get(key) or "")` — a falsy value (absent, null, `false`, `0`,
/// empty string) collapses to `""`. Distinct from [`get_string`], whose default
/// applies only on an absent/null key.
fn truthy_or_empty(map: &JsonObject, key: &str) -> String {
    map.get(key).map(py_truthy_str).unwrap_or_default()
}

/// `str(value) if value else None` — a truthy (non-empty) value yields `Some`.
fn truthy_string(map: &JsonObject, key: &str) -> Option<String> {
    match map.get(key) {
        None => None,
        Some(value) if py_truthy_str(value).is_empty() => None,
        Some(value) => Some(py_str(value)),
    }
}

/// `strict_int_from_daemon_field`: absent/null yields `default`, a JSON bool is
/// rejected (no bool-as-int), an integer is returned, anything else is rejected.
fn strict_int(map: &JsonObject, key: &str, default: i64) -> Result<i64, SandboxApiError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(value)) => Err(SandboxApiError::decode(format!(
            "expected integer value, got bool ({value})"
        ))),
        Some(Value::Number(number)) => number.as_i64().ok_or_else(|| {
            SandboxApiError::decode(format!(
                "expected integer value, got non-integer number ({number})"
            ))
        }),
        Some(other) => Err(SandboxApiError::decode(format!(
            "expected integer value, got {}",
            json_type_name(other)
        ))),
    }
}

/// `strict_int(...) if value is not None else None`.
fn optional_strict_int(map: &JsonObject, key: &str) -> Result<Option<i64>, SandboxApiError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        _ => Ok(Some(strict_int(map, key, 0)?)),
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `parse_path_tuple_field`: keep `str(path)` for array entries whose
/// `str(path or "").strip()` is non-empty (blank/whitespace-only entries drop;
/// whitespace-padded values are preserved unstripped). Non-arrays yield empty.
fn parse_path_tuple(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter(|item| !py_truthy_str(item).trim().is_empty())
            .map(py_str)
            .collect(),
        _ => Vec::new(),
    }
}

/// Unfiltered `[str(path) for path in raw]` used by the exec parser only (it
/// does **not** drop blank entries, unlike the guarded parser).
fn parse_path_list_unfiltered(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().map(py_str).collect(),
        _ => Vec::new(),
    }
}

/// `parse_changed_path_kinds_field`: drop pairs whose key or value is blank.
fn parse_changed_path_kinds(value: Option<&Value>) -> BTreeMap<String, String> {
    match value {
        Some(Value::Object(map)) => map
            .iter()
            .filter(|(key, value)| {
                !key.trim().is_empty() && !py_truthy_str(value).trim().is_empty()
            })
            .map(|(key, value)| (key.clone(), py_str(value)))
            .collect(),
        _ => BTreeMap::new(),
    }
}

/// Unfiltered `{str(k): str(v) for k, v in raw.items()}` used by the exec parser.
fn parse_changed_path_kinds_unfiltered(value: Option<&Value>) -> BTreeMap<String, String> {
    match value {
        Some(Value::Object(map)) => map
            .iter()
            .map(|(key, value)| (key.clone(), py_str(value)))
            .collect(),
        _ => BTreeMap::new(),
    }
}

/// `parse_conflict_info_field`: a non-object yields `None`.
fn parse_conflict_info(value: Option<&Value>) -> Option<ConflictInfo> {
    let map = value?.as_object()?;
    let conflict_file = match map.get("conflict_file") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    Some(ConflictInfo {
        reason: get_string(map, "reason", ""),
        conflict_file,
        message: get_string(map, "message", ""),
    })
}

/// `dict(error) if isinstance(error, dict) else None`.
fn error_object(value: Option<&Value>) -> Option<JsonObject> {
    match value {
        Some(Value::Object(map)) => Some(map.clone()),
        _ => None,
    }
}

/// `normalize_timing_map`: object keys are kept verbatim and values coerced to
/// `f64`. Keys are already plain strings over JSON — `TimingKey` is a
/// `str`-subclass enum, so its members serialize as their value, never the
/// `TimingKey.*` repr; the prefix branch in Python's `_timing_key_text` is dead
/// at this boundary and is deliberately not ported (it would require coupling to
/// the daemon-internal enum). Non-numeric values are skipped defensively.
fn parse_timing_map(value: Option<&Value>) -> BTreeMap<String, f64> {
    match value {
        Some(Value::Object(map)) => map
            .iter()
            .filter_map(|(key, value)| value.as_f64().map(|seconds| (key.clone(), seconds)))
            .collect(),
        _ => BTreeMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Per-verb result parsers
// ---------------------------------------------------------------------------

/// The result base for the read-only verbs (read/glob/grep): only `success` and
/// `timings` come from the envelope; conflict/changed-path/error fields stay at
/// their empty defaults and `workspace` is always `Ephemeral` (invariant 9).
fn simple_result_base(response: &JsonObject) -> SandboxResultBase {
    SandboxResultBase {
        success: get_bool(response, "success"),
        workspace: Workspace::Ephemeral,
        timings: parse_timing_map(response.get("timings")),
        conflict: None,
        conflict_reason: None,
        changed_paths: Vec::new(),
        error: None,
    }
}

pub(crate) fn parse_read_file_result(
    response: &JsonObject,
) -> Result<ReadFileResult, SandboxApiError> {
    Ok(ReadFileResult {
        base: simple_result_base(response),
        content: get_string(response, "content", ""),
        exists: get_bool(response, "exists"),
        encoding: get_string(response, "encoding", "utf-8"),
    })
}

pub(crate) fn parse_glob_result(response: &JsonObject) -> Result<GlobResult, SandboxApiError> {
    Ok(GlobResult {
        base: simple_result_base(response),
        filenames: parse_path_tuple(response.get("filenames")),
        num_files: strict_int(response, "num_files", 0)? as u32,
        truncated: get_bool(response, "truncated"),
    })
}

pub(crate) fn parse_grep_result(response: &JsonObject) -> Result<GrepResult, SandboxApiError> {
    let applied_limit = optional_strict_int(response, "applied_limit")?.map(|value| value as u32);
    Ok(GrepResult {
        base: simple_result_base(response),
        output_mode: get_string(response, "output_mode", "files_with_matches"),
        filenames: parse_path_tuple(response.get("filenames")),
        content: get_string(response, "content", ""),
        num_files: strict_int(response, "num_files", 0)? as u32,
        num_lines: strict_int(response, "num_lines", 0)? as u32,
        num_matches: strict_int(response, "num_matches", 0)? as u32,
        applied_limit,
        applied_offset: strict_int(response, "applied_offset", 0)? as u32,
        truncated: get_bool(response, "truncated"),
    })
}

/// The common guarded-mutation fields shared by write/edit/shell results.
struct GuardedCommon {
    base: SandboxResultBase,
    changed_path_kinds: BTreeMap<String, String>,
    mutation_source: String,
    status: String,
}

fn parse_guarded_common(response: &JsonObject) -> GuardedCommon {
    GuardedCommon {
        base: SandboxResultBase {
            success: get_bool(response, "success"),
            workspace: Workspace::Ephemeral,
            timings: parse_timing_map(response.get("timings")),
            conflict: parse_conflict_info(response.get("conflict")),
            conflict_reason: optional_string(response, "conflict_reason"),
            changed_paths: parse_path_tuple(response.get("changed_paths")),
            error: error_object(response.get("error")),
        },
        changed_path_kinds: parse_changed_path_kinds(response.get("changed_path_kinds")),
        // Guarded `mutation_source` collapses falsy values (`str(x or "")`),
        // unlike `status` whose default applies only on an absent key.
        mutation_source: truthy_or_empty(response, "mutation_source"),
        status: get_string(response, "status", ""),
    }
}

pub(crate) fn parse_write_file_result(
    response: &JsonObject,
) -> Result<WriteFileResult, SandboxApiError> {
    let common = parse_guarded_common(response);
    Ok(WriteFileResult {
        base: common.base,
        changed_path_kinds: common.changed_path_kinds,
        mutation_source: common.mutation_source,
        status: common.status,
    })
}

pub(crate) fn parse_edit_file_result(
    response: &JsonObject,
) -> Result<EditFileResult, SandboxApiError> {
    let common = parse_guarded_common(response);
    Ok(EditFileResult {
        base: common.base,
        changed_path_kinds: common.changed_path_kinds,
        mutation_source: common.mutation_source,
        status: common.status,
        applied_edits: strict_int(response, "applied_edits", 0)? as u32,
    })
}

pub(crate) fn parse_exec_command_result(
    response: &JsonObject,
) -> Result<ExecCommandResult, SandboxApiError> {
    // `success = status not in {"error","timed_out"}`, using a falsy status as
    // "" (which IS a success); the `status` field falls back to "error".
    let raw_status = response
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| !status.is_empty());
    let success = !matches!(raw_status.unwrap_or(""), "error" | "timed_out");
    let status = raw_status.unwrap_or("error").to_owned();

    let output_map = response.get("output").and_then(Value::as_object);
    let output = output_map.map_or_else(CommandOutput::default, |map| CommandOutput {
        stdout: get_string(map, "stdout", ""),
        stderr: get_string(map, "stderr", ""),
    });

    // `int(exit_code) if isinstance(exit_code, int) else None` — Python's
    // isinstance(bool, int) is true, so a JSON bool maps to 0/1.
    let exit_code = match response.get("exit_code") {
        Some(Value::Number(number)) => number.as_i64().map(|value| value as i32),
        Some(Value::Bool(value)) => Some(i32::from(*value)),
        _ => None,
    };

    Ok(ExecCommandResult {
        base: SandboxResultBase {
            success,
            workspace: Workspace::Ephemeral,
            timings: parse_timing_map(response.get("timings")),
            conflict: None,
            conflict_reason: optional_string(response, "conflict_reason"),
            changed_paths: parse_path_list_unfiltered(response.get("changed_paths")),
            error: error_object(response.get("error")),
        },
        status,
        exit_code,
        output,
        command_session_id: truthy_string(response, "command_session_id"),
        changed_path_kinds: parse_changed_path_kinds_unfiltered(response.get("changed_path_kinds")),
        mutation_source: optional_string(response, "mutation_source").unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SandboxCaller;

    fn obj(value: Value) -> JsonObject {
        match value {
            Value::Object(map) => map,
            _ => panic!("test json is an object"),
        }
    }

    fn caller() -> SandboxCaller {
        SandboxCaller {
            agent_id: "agent-1".to_owned(),
            run_id: String::new(),
            agent_run_id: "agent-1".to_owned(),
            task_id: String::new(),
            request_id: String::new(),
            attempt_id: String::new(),
            workflow_id: String::new(),
            tool_id: None,
        }
    }

    // AC-sandbox-api-04 (envelope portion): the full identity emits a top-level
    // agent_id, the nested caller block, and a top-level invocation_id only when
    // present. The fixture uses agent_id == agent_run_id.
    #[test]
    fn identity_envelope_has_top_level_agent_and_optional_invocation() {
        let base = SandboxRequestBase {
            caller: caller(),
            description: String::new(),
            invocation_id: None,
        };
        let payload = daemon_request_identity_fields(&base);
        assert_eq!(payload["agent_id"], serde_json::json!("agent-1"));
        assert!(payload["caller"].is_object());
        assert_eq!(
            payload["caller"]["agent_run_id"],
            serde_json::json!("agent-1")
        );
        assert!(!payload.contains_key("invocation_id"));

        let base = SandboxRequestBase {
            caller: caller(),
            description: String::new(),
            invocation_id: Some("inv-9".parse().expect("non-empty")),
        };
        let payload = daemon_request_identity_fields(&base);
        assert_eq!(payload["invocation_id"], serde_json::json!("inv-9"));
    }

    // AC-sandbox-api-03: read decodes content/exists/encoding and timings.
    #[test]
    fn parse_read_file_decodes_fields_and_timings() {
        let response = obj(serde_json::json!({
            "success": true,
            "exists": true,
            "content": "hello",
            "encoding": "utf-8",
            "timings": {"api.read.total_s": 0.5},
        }));
        let result = parse_read_file_result(&response).expect("parse");
        assert!(result.base.success);
        assert!(result.exists);
        assert_eq!(result.content, "hello");
        assert_eq!(result.encoding, "utf-8");
        assert_eq!(result.base.timings.get("api.read.total_s"), Some(&0.5));
    }

    #[test]
    fn parse_glob_decodes_and_strict_ints() {
        let response = obj(serde_json::json!({
            "success": true,
            "filenames": ["a.txt", "b.txt"],
            "num_files": 2,
            "truncated": false,
            "timings": {"api.glob.total_s": 0.1},
        }));
        let result = parse_glob_result(&response).expect("parse");
        assert_eq!(result.filenames, vec!["a.txt", "b.txt"]);
        assert_eq!(result.num_files, 2);
        assert_eq!(result.base.timings.get("api.glob.total_s"), Some(&0.1));
    }

    #[test]
    fn parse_grep_decodes_optional_applied_limit_and_timings() {
        let with_limit = obj(serde_json::json!({
            "success": true,
            "output_mode": "content",
            "filenames": ["a.txt"],
            "content": "match",
            "num_files": 1,
            "num_lines": 3,
            "num_matches": 3,
            "applied_limit": 10,
            "applied_offset": 2,
            "truncated": true,
            "timings": {"api.grep.total_s": 0.2},
        }));
        let result = parse_grep_result(&with_limit).expect("parse");
        assert_eq!(result.applied_limit, Some(10));
        assert_eq!(result.applied_offset, 2);
        assert_eq!(result.num_matches, 3);
        assert_eq!(result.base.timings.get("api.grep.total_s"), Some(&0.2));

        let without_limit = obj(serde_json::json!({"success": true}));
        let result = parse_grep_result(&without_limit).expect("parse");
        assert_eq!(result.applied_limit, None);
        assert_eq!(result.output_mode, "files_with_matches");
    }

    // AC-sandbox-api-03: missing `success`/`exists` decode to false (fail-closed).
    #[test]
    fn parse_missing_success_and_exists_are_false() {
        let response = obj(serde_json::json!({"content": "x"}));
        let result = parse_read_file_result(&response).expect("parse");
        assert!(!result.base.success, "missing success is false");
        assert!(!result.exists, "missing exists is false");

        let guarded = parse_write_file_result(&obj(serde_json::json!({}))).expect("parse");
        assert!(
            !guarded.base.success,
            "missing success is false for guarded"
        );
    }

    // AC-sandbox-api-03: blank/whitespace path entries and blank kind pairs are
    // filtered by the guarded parser, replicating the Python filters.
    #[test]
    fn parse_drops_blank_paths_and_kinds() {
        let response = obj(serde_json::json!({
            "success": false,
            "changed_paths": ["real.txt", "  ", ""],
            "changed_path_kinds": {"real.txt": "modified", "": "x", "blank": "  "},
            "status": "ok",
        }));
        let result = parse_write_file_result(&response).expect("parse");
        assert_eq!(result.base.changed_paths, vec!["real.txt"]);
        assert_eq!(result.changed_path_kinds.len(), 1);
        assert_eq!(
            result.changed_path_kinds.get("real.txt"),
            Some(&"modified".to_owned())
        );

    }

    // AC-sandbox-api-03: ExecCommandResult.success is derived from status.
    #[test]
    fn parse_exec_derives_success_from_status() {
        let ok = parse_exec_command_result(&obj(serde_json::json!({
            "status": "completed",
            "exit_code": 0,
            "output": {"stdout": "hi", "stderr": ""},
        })))
        .expect("parse");
        assert!(ok.base.success);
        assert_eq!(ok.status, "completed");
        assert_eq!(ok.exit_code, Some(0));
        assert_eq!(ok.output.stdout, "hi");

        for failing in ["error", "timed_out"] {
            let result = parse_exec_command_result(&obj(serde_json::json!({"status": failing})))
                .expect("parse");
            assert!(!result.base.success, "status {failing} is not success");
        }

        // Missing status: success (empty status is not in the failure set), but
        // the status field falls back to "error".
        let missing = parse_exec_command_result(&obj(serde_json::json!({}))).expect("parse");
        assert!(missing.base.success);
        assert_eq!(missing.status, "error");
        assert_eq!(missing.exit_code, None);
    }

    #[test]
    fn parse_exec_does_not_filter_changed_paths() {
        // Exec uses the unfiltered list/map, unlike the guarded parser.
        let result = parse_exec_command_result(&obj(serde_json::json!({
            "status": "completed",
            "changed_paths": ["a", ""],
            "changed_path_kinds": {"a": "m", "": "x"},
        })))
        .expect("parse");
        assert_eq!(result.base.changed_paths, vec!["a", ""]);
        assert_eq!(result.changed_path_kinds.len(), 2);
    }

    // strict_int rejects bool-as-int; raw serde would silently coerce.
    #[test]
    fn strict_int_rejects_bool() {
        let response = obj(serde_json::json!({"success": true, "num_files": true}));
        assert!(parse_glob_result(&response).is_err());
    }

    // invariant 9: workspace is never decoded from the envelope.
    #[test]
    fn parse_ignores_workspace_field() {
        let response = obj(serde_json::json!({"success": true, "workspace": "isolated"}));
        let result = parse_write_file_result(&response).expect("parse");
        assert_eq!(result.base.workspace, Workspace::Ephemeral);
    }

    #[test]
    fn guarded_parses_conflict_and_changed_paths() {
        let response = obj(serde_json::json!({
            "success": false,
            "status": "aborted_overlap",
            "conflict": {"reason": "aborted_overlap", "conflict_file": "a.txt", "message": "overlap"},
            "conflict_reason": "overlap",
            "changed_paths": ["a.txt"],
            "error": {"code": "x"},
        }));
        let result = parse_edit_file_result(&response).expect("parse");
        assert_eq!(result.status, "aborted_overlap");
        let conflict = result.base.conflict.expect("conflict");
        assert_eq!(conflict.reason, "aborted_overlap");
        assert_eq!(conflict.conflict_file.as_deref(), Some("a.txt"));
        assert_eq!(result.base.conflict_reason.as_deref(), Some("overlap"));
        assert!(result.base.error.is_some());
    }

    #[test]
    fn guarded_mutation_source_collapses_falsy() {
        // Python `str(response.get("mutation_source") or "")`: a falsy value
        // collapses to "" (not "False"/"0"); a truthy string is kept.
        for falsy in [
            serde_json::json!(false),
            serde_json::json!(0),
            serde_json::json!(""),
            serde_json::Value::Null,
        ] {
            let response = obj(serde_json::json!({"success": true, "mutation_source": falsy}));
            let result = parse_write_file_result(&response).expect("parse");
            assert_eq!(result.mutation_source, "", "falsy mutation_source");
        }
        let kept = parse_write_file_result(&obj(
            serde_json::json!({"success": true, "mutation_source": "overlay"}),
        ))
        .expect("parse");
        assert_eq!(kept.mutation_source, "overlay");
    }

    #[test]
    fn user_visible_strips_internal_error_prefix() {
        assert_eq!(user_visible_error_message("internal_error: boom"), "boom");
        assert_eq!(user_visible_error_message("plain"), "plain");
    }

    #[test]
    fn conflict_classifier_matches_code_or_marker() {
        // Code match.
        assert!(is_edit_conflict(&SandboxApiError::transport(
            Some("aborted_overlap".to_owned()),
            "anything",
        )));
        // Marker match (case-insensitive, prefix-stripped).
        assert!(is_edit_conflict(&SandboxApiError::transport(
            None,
            "internal_error: Anchor Not Found here",
        )));
        // Non-conflict.
        assert!(!is_edit_conflict(&SandboxApiError::transport(
            Some("boom".to_owned()),
            "random failure",
        )));
        // A decode error is never a conflict.
        assert!(!is_edit_conflict(&SandboxApiError::decode("bad number")));
    }
}
