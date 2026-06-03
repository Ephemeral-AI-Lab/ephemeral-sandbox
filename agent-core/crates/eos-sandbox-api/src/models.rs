//! Request/result DTOs, caller identity, and intent for the host-facing sandbox
//! protocol (ported from `sandbox/shared/models.py`).
//!
//! These are wire types: every DTO derives `Serialize`/`Deserialize`/`JsonSchema`
//! (`api-common-traits`). Composition is by embedding `SandboxRequestBase` /
//! `SandboxResultBase` as a flattened field rather than class inheritance. The
//! request structs are never serialized straight to the daemon — the
//! `tool_api` helpers build each daemon payload field-by-field — so the derived
//! serde shape only backs schema snapshots and round-trip tests.
//!
//! Two source-driven removals/relocations from the Python module: `tool_name`
//! is dropped from [`SandboxCaller`] (GC-sandbox-api-01) and `RawExecResult` is
//! dropped (raw provider exec is a host concern, not a daemon op).

use std::collections::BTreeMap;

use eos_types::{
    AgentRunId, AttemptId, InvocationId, JsonObject, RequestId, TaskId, ToolUseId, WorkflowId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// High-level execution intent for a foreground sandbox tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Read-only operation (no mutations).
    ReadOnly,
    /// Operation permitted to mutate the workspace.
    WriteAllowed,
    /// Workspace lifecycle operation (e.g. isolated enter/exit).
    Lifecycle,
}

impl Intent {
    /// The wire string for this intent (the serde `snake_case` form), used when
    /// building a daemon payload by hand.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WriteAllowed => "write_allowed",
            Self::Lifecycle => "lifecycle",
        }
    }
}

/// Which workspace a result was produced against. **Never decoded from a daemon
/// envelope** — the hand-written parsers always leave it at the `Ephemeral`
/// default (invariant 9); the `Deserialize` derive exists only for round-tripping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Workspace {
    /// The shared ephemeral workspace (default).
    #[default]
    Ephemeral,
    /// An agent's private isolated workspace.
    Isolated,
}

/// Caller identity threaded onto every audit-aware request.
///
/// The four required ids (`agent_id`, `run_id`, `agent_run_id`, `task_id`) are
/// always present even when empty; the rest are optional. `tool_id` is the only
/// id stored already-typed (it is `Option`, omitted when unset). The Python
/// `tool_name` field is removed (GC-sandbox-api-01): it was empty in production
/// and the audit fallback uses the operation name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SandboxCaller {
    /// Resolved agent identity. In production it is derived from `agent_run_id`
    /// (`agent_run_id.strip() or agent_name`, eos-tools source), so it frequently
    /// equals `agent_run_id` while staying a distinct field. eos-types owns no
    /// `AgentId` newtype, so this stays a raw `String`.
    pub agent_id: String,
    /// Run id (required-empty compatibility field).
    #[serde(default)]
    pub run_id: String,
    /// Agent-run id as a raw wire field (required-empty). Use [`Self::agent_run`]
    /// for the validated typed form.
    #[serde(default)]
    pub agent_run_id: String,
    /// Task id as a raw wire field (required-empty). Use [`Self::task`].
    #[serde(default)]
    pub task_id: String,
    /// Request id as a raw wire field (optional-empty). Use [`Self::request`].
    #[serde(default)]
    pub request_id: String,
    /// Attempt id as a raw wire field (optional-empty). Use [`Self::attempt`].
    #[serde(default)]
    pub attempt_id: String,
    /// Workflow id as a raw wire field (optional-empty). Use [`Self::workflow`].
    #[serde(default)]
    pub workflow_id: String,
    /// Tool-use id, stored already-typed; omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<ToolUseId>,
}

impl SandboxCaller {
    /// Daemon-facing nested `caller` block (mirrors Python `audit_fields()`), a
    /// **payload-shape** method, not audit logic.
    ///
    /// The four required ids are always present (even empty); optional ids are
    /// omitted when empty. This is only the nested block — the full envelope
    /// identity (top-level `agent_id` + this block + optional `invocation_id`) is
    /// built by `tool_api::parse::daemon_request_identity_fields`.
    pub(crate) fn identity_block(&self) -> JsonObject {
        let mut block = JsonObject::new();
        block.insert("agent_id".to_owned(), Value::String(self.agent_id.clone()));
        block.insert("run_id".to_owned(), Value::String(self.run_id.clone()));
        block.insert(
            "agent_run_id".to_owned(),
            Value::String(self.agent_run_id.clone()),
        );
        block.insert("task_id".to_owned(), Value::String(self.task_id.clone()));
        if !self.request_id.is_empty() {
            block.insert(
                "request_id".to_owned(),
                Value::String(self.request_id.clone()),
            );
        }
        if !self.attempt_id.is_empty() {
            block.insert(
                "attempt_id".to_owned(),
                Value::String(self.attempt_id.clone()),
            );
        }
        if !self.workflow_id.is_empty() {
            block.insert(
                "workflow_id".to_owned(),
                Value::String(self.workflow_id.clone()),
            );
        }
        if let Some(tool_id) = &self.tool_id {
            block.insert("tool_id".to_owned(), Value::String(tool_id.to_string()));
        }
        block
    }

    /// The typed agent-run id, or `None` when the raw field is empty.
    #[must_use]
    pub fn agent_run(&self) -> Option<AgentRunId> {
        self.agent_run_id.parse().ok()
    }

    /// The typed task id, or `None` when the raw field is empty.
    #[must_use]
    pub fn task(&self) -> Option<TaskId> {
        self.task_id.parse().ok()
    }

    /// The typed request id, or `None` when the raw field is empty.
    #[must_use]
    pub fn request(&self) -> Option<RequestId> {
        self.request_id.parse().ok()
    }

    /// The typed attempt id, or `None` when the raw field is empty.
    #[must_use]
    pub fn attempt(&self) -> Option<AttemptId> {
        self.attempt_id.parse().ok()
    }

    /// The typed workflow id, or `None` when the raw field is empty.
    #[must_use]
    pub fn workflow(&self) -> Option<WorkflowId> {
        self.workflow_id.parse().ok()
    }
}

/// Base request shape for audit-aware public sandbox operations. Embedded as a
/// flattened field on each verb request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SandboxRequestBase {
    /// Caller identity for audit and routing.
    pub caller: SandboxCaller,
    /// Human-readable operation description; falls back via [`Self::description_or`].
    #[serde(default)]
    pub description: String,
    /// Optional in-flight correlation id, reused by the transport when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_id: Option<InvocationId>,
}

impl SandboxRequestBase {
    /// `description` if non-empty, else `fallback` (mirrors `default_description`).
    #[must_use]
    pub fn description_or(&self, fallback: &str) -> String {
        if self.description.is_empty() {
            fallback.to_owned()
        } else {
            self.description.clone()
        }
    }
}

/// Base result shape for public sandbox operations. Embedded as a flattened
/// field on each verb result.
///
/// `success` has **no** `Default`/construction shortcut on the parse path — the
/// hand-written parsers set it explicitly with a fail-closed `false` default
/// (invariant 9). `workspace` is never decoded from the envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SandboxResultBase {
    /// Whether the operation succeeded.
    pub success: bool,
    /// Workspace the result was produced against (always `Ephemeral` on parse).
    #[serde(default)]
    pub workspace: Workspace,
    /// Operation timings, keys normalized to plain strings.
    #[serde(default)]
    pub timings: BTreeMap<String, f64>,
    /// Structured conflict details, when the operation conflicted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<ConflictInfo>,
    /// Free-text conflict reason, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_reason: Option<String>,
    /// Paths the operation changed (empty for read-only verbs).
    #[serde(default)]
    pub changed_paths: Vec<String>,
    /// Untyped daemon error payload, when the operation failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonObject>,
}

/// Structured guarded-operation conflict details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConflictInfo {
    /// Conflict reason code (e.g. `aborted_overlap`, `rejected`).
    pub reason: String,
    /// The conflicting file, when the conflict is path-scoped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_file: Option<String>,
    /// User-facing conflict message.
    #[serde(default)]
    pub message: String,
}

impl ConflictInfo {
    /// A rejected-operation conflict (no specific file).
    #[must_use]
    pub fn rejected(reason: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            conflict_file: None,
            message: message.into(),
        }
    }

    /// An overlapping-write conflict scoped to `path`.
    #[must_use]
    pub fn overlap(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            reason: "aborted_overlap".to_owned(),
            conflict_file: Some(path.into()),
            message: message.into(),
        }
    }
}

/// Read one UTF-8 text file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReadFileRequest {
    /// Caller identity / description / invocation id.
    #[serde(flatten)]
    pub base: SandboxRequestBase,
    /// Workspace-relative file path to read.
    pub path: String,
}

/// Result of [`ReadFileRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReadFileResult {
    /// Common result fields.
    #[serde(flatten)]
    pub base: SandboxResultBase,
    /// File contents (empty when the file does not exist).
    pub content: String,
    /// Whether the file existed (fail-closed `false` on a missing daemon field).
    #[serde(default)]
    pub exists: bool,
    /// Content encoding (defaults to `utf-8`).
    pub encoding: String,
}

/// Write one UTF-8 file through OCC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WriteFileRequest {
    /// Caller identity / description / invocation id.
    #[serde(flatten)]
    pub base: SandboxRequestBase,
    /// Workspace-relative file path to write.
    pub path: String,
    /// New file contents.
    pub content: String,
    /// Whether to overwrite an existing file (defaults to `true`).
    #[serde(default = "default_true")]
    pub overwrite: bool,
}

/// Result of [`WriteFileRequest`] (a guarded mutation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WriteFileResult {
    /// Common result fields.
    #[serde(flatten)]
    pub base: SandboxResultBase,
    /// Per-path mutation kinds reported by the daemon.
    #[serde(default)]
    pub changed_path_kinds: BTreeMap<String, String>,
    /// Source of the mutation (daemon-reported).
    #[serde(default)]
    pub mutation_source: String,
    /// Guarded-operation status string.
    #[serde(default)]
    pub status: String,
}

/// One exact-match replacement applied as part of an [`EditFileRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchReplaceEdit {
    /// Text to find.
    pub old_text: String,
    /// Replacement text.
    pub new_text: String,
    /// Whether to replace all occurrences (defaults to `false`).
    #[serde(default)]
    pub replace_all: bool,
}

/// Apply search/replace edits through OCC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EditFileRequest {
    /// Caller identity / description / invocation id.
    #[serde(flatten)]
    pub base: SandboxRequestBase,
    /// Workspace-relative file path to edit.
    pub path: String,
    /// Ordered list of edits to apply.
    pub edits: Vec<SearchReplaceEdit>,
}

/// Result of [`EditFileRequest`] (a guarded mutation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EditFileResult {
    /// Common result fields.
    #[serde(flatten)]
    pub base: SandboxResultBase,
    /// Per-path mutation kinds reported by the daemon.
    #[serde(default)]
    pub changed_path_kinds: BTreeMap<String, String>,
    /// Source of the mutation (daemon-reported).
    #[serde(default)]
    pub mutation_source: String,
    /// Guarded-operation status string.
    #[serde(default)]
    pub status: String,
    /// Number of edits applied (defaults to `0`).
    #[serde(default)]
    pub applied_edits: u32,
}

/// Stdout/stderr captured from a command session.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct CommandOutput {
    /// Captured stdout.
    #[serde(default)]
    pub stdout: String,
    /// Captured stderr.
    #[serde(default)]
    pub stderr: String,
}

/// Run or start a managed command session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExecCommandRequest {
    /// Caller identity / description / invocation id.
    #[serde(flatten)]
    pub base: SandboxRequestBase,
    /// Command line to run.
    pub cmd: String,
    /// Yield window in milliseconds before returning partial output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yield_time_ms: Option<u32>,
    /// Command timeout in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
    /// Cap on output tokens returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

/// Result of [`ExecCommandRequest`] / command-session writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExecCommandResult {
    /// Common result fields.
    #[serde(flatten)]
    pub base: SandboxResultBase,
    /// Session status (`success` is derived from this; `error`/`timed_out` fail).
    pub status: String,
    /// Process exit code, when the command has finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Captured output.
    pub output: CommandOutput,
    /// The managed command-session id, when one was opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_session_id: Option<String>,
    /// Per-path mutation kinds reported by the daemon.
    #[serde(default)]
    pub changed_path_kinds: BTreeMap<String, String>,
    /// Source of the mutation (daemon-reported).
    #[serde(default)]
    pub mutation_source: String,
}

/// Write characters to an open command session through `api.v1.exec_stdin`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExecStdinRequest {
    /// Caller identity / description / invocation id.
    #[serde(flatten)]
    pub base: SandboxRequestBase,
    /// Target command-session id.
    pub command_session_id: String,
    /// Characters (stdin) to write.
    pub chars: String,
    /// Yield window in milliseconds before returning partial output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yield_time_ms: Option<u32>,
    /// Cap on output tokens returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Tear the session down (SIGTERM→SIGKILL) after writing — the explicit
    /// teardown channel, decoupled from `\x03`/SIGINT (sense-2 D7).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminate: bool,
}

/// Model-facing `write_stdin` request alias for [`ExecStdinRequest`].
pub type CommandSessionWriteRequest = ExecStdinRequest;

/// Cancel an open command session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommandSessionCancelRequest {
    /// Caller identity / description / invocation id.
    #[serde(flatten)]
    pub base: SandboxRequestBase,
    /// Target command-session id.
    pub command_session_id: String,
}

/// Enumerate workspace paths matching a glob pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GlobRequest {
    /// Caller identity / description / invocation id.
    #[serde(flatten)]
    pub base: SandboxRequestBase,
    /// Glob pattern.
    pub pattern: String,
    /// Optional root path to scope the search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Result of [`GlobRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GlobResult {
    /// Common result fields.
    #[serde(flatten)]
    pub base: SandboxResultBase,
    /// Matching file paths.
    #[serde(default)]
    pub filenames: Vec<String>,
    /// Count of matching files.
    #[serde(default)]
    pub num_files: u32,
    /// Whether the result was truncated.
    #[serde(default)]
    pub truncated: bool,
}

/// Regex-scan workspace file contents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GrepRequest {
    /// Caller identity / description / invocation id.
    #[serde(flatten)]
    pub base: SandboxRequestBase,
    /// Regex pattern (`re`-style; the prompt contract is owned by eos-tools).
    pub pattern: String,
    /// Optional root path to scope the search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Optional glob filter applied to candidate files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob_filter: Option<String>,
    /// Output mode (defaults to `files_with_matches`).
    #[serde(default = "default_output_mode")]
    pub output_mode: String,
    /// Cap on returned matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_limit: Option<u32>,
    /// Offset into the match list.
    #[serde(default)]
    pub offset: u32,
    /// Case-insensitive matching.
    #[serde(default)]
    pub case_insensitive: bool,
    /// Emit line numbers.
    #[serde(default)]
    pub line_numbers: bool,
    /// Multiline matching.
    #[serde(default)]
    pub multiline: bool,
}

/// Result of [`GrepRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GrepResult {
    /// Common result fields.
    #[serde(flatten)]
    pub base: SandboxResultBase,
    /// Echoed output mode.
    #[serde(default = "default_output_mode")]
    pub output_mode: String,
    /// Matching file paths.
    #[serde(default)]
    pub filenames: Vec<String>,
    /// Rendered match content (for content output modes).
    #[serde(default)]
    pub content: String,
    /// Count of matching files.
    #[serde(default)]
    pub num_files: u32,
    /// Count of matching lines.
    #[serde(default)]
    pub num_lines: u32,
    /// Count of matches.
    #[serde(default)]
    pub num_matches: u32,
    /// The limit the daemon actually applied, when one was applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_limit: Option<u32>,
    /// The offset the daemon actually applied.
    #[serde(default)]
    pub applied_offset: u32,
    /// Whether the result was truncated.
    #[serde(default)]
    pub truncated: bool,
}

/// Categorical isolated-workspace lifecycle error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LifecycleError {
    /// Error category.
    pub kind: String,
    /// Error message.
    #[serde(default)]
    pub message: String,
    /// Structured detail fields.
    #[serde(default)]
    pub details: BTreeMap<String, String>,
}

/// Base result for isolated-workspace lifecycle operations (distinct from OCC
/// conflicts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LifecycleResultBase {
    /// Whether the lifecycle operation succeeded (defaults to `true`).
    #[serde(default = "default_true")]
    pub success: bool,
    /// Operation timings.
    #[serde(default)]
    pub timings: BTreeMap<String, f64>,
    /// Lifecycle error, when the operation failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<LifecycleError>,
}

/// Enter an isolated workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EnterIsolatedWorkspaceRequest {
    /// Caller identity / description / invocation id.
    #[serde(flatten)]
    pub base: SandboxRequestBase,
    /// `LayerStack` root to base the isolated workspace on.
    pub layer_stack_root: String,
}

/// Result of [`EnterIsolatedWorkspaceRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EnterIsolatedWorkspaceResult {
    /// Common lifecycle result fields.
    #[serde(flatten)]
    pub base: LifecycleResultBase,
    /// Manifest version of the entered workspace.
    #[serde(default)]
    pub manifest_version: String,
    /// Root hash of the entered workspace manifest.
    #[serde(default)]
    pub manifest_root_hash: String,
}

/// Exit an isolated workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExitIsolatedWorkspaceRequest {
    /// Caller identity / description / invocation id.
    #[serde(flatten)]
    pub base: SandboxRequestBase,
    /// Grace period in seconds before forcing teardown (defaults to `5.0`).
    #[serde(default = "default_grace_s")]
    pub grace_s: f64,
}

/// Result of [`ExitIsolatedWorkspaceRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExitIsolatedWorkspaceResult {
    /// Common lifecycle result fields.
    #[serde(flatten)]
    pub base: LifecycleResultBase,
    /// Bytes evicted from the upperdir on teardown.
    #[serde(default)]
    pub evicted_upperdir_bytes: u64,
    /// Total lifetime of the isolated workspace, seconds.
    #[serde(default)]
    pub lifetime_s: f64,
    /// Per-phase teardown timings, milliseconds.
    #[serde(default)]
    pub phases_ms: BTreeMap<String, f64>,
}

/// One tool invocation routed through a workspace pipeline.
///
/// `invocation_id` is the typed [`InvocationId`]; [`Self::from_payload`] parses
/// it at the boundary and is fallible (a spec-sanctioned tightening of the
/// Python path, which tolerated an empty id string).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolCallRequest {
    /// Correlation id for this invocation.
    pub invocation_id: InvocationId,
    /// Calling agent id.
    pub agent_id: String,
    /// Tool verb (e.g. `read_file`).
    pub verb: String,
    /// Execution intent.
    pub intent: Intent,
    /// Untyped tool arguments.
    pub args: JsonObject,
    /// Whether the invocation runs in the background.
    #[serde(default)]
    pub background: bool,
}

impl ToolCallRequest {
    /// Build the daemon payload (mirrors `to_payload`).
    #[must_use]
    pub fn to_payload(&self) -> JsonObject {
        let mut payload = JsonObject::new();
        payload.insert(
            "invocation_id".to_owned(),
            Value::String(self.invocation_id.to_string()),
        );
        payload.insert("agent_id".to_owned(), Value::String(self.agent_id.clone()));
        payload.insert("verb".to_owned(), Value::String(self.verb.clone()));
        payload.insert(
            "intent".to_owned(),
            Value::String(self.intent.as_wire().to_owned()),
        );
        payload.insert("args".to_owned(), Value::Object(self.args.clone()));
        payload.insert("background".to_owned(), Value::Bool(self.background));
        payload
    }

    /// Parse a daemon payload (mirrors `from_payload`). Fails when `args` is
    /// present but not an object, or when `invocation_id` is missing/empty.
    pub fn from_payload(payload: &JsonObject) -> Result<Self, crate::error::SandboxApiError> {
        let args = match payload.get("args") {
            None | Some(Value::Null) => JsonObject::new(),
            Some(Value::Object(map)) => map.clone(),
            Some(_) => {
                return Err(crate::error::SandboxApiError::decode(
                    "tool-call payload args must be an object",
                ));
            }
        };
        let invocation_raw = payload
            .get("invocation_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let invocation_id = invocation_raw.parse::<InvocationId>().map_err(|_| {
            crate::error::SandboxApiError::decode("tool-call payload missing invocation_id")
        })?;
        let intent = match payload.get("intent").and_then(Value::as_str) {
            None | Some("") => Intent::ReadOnly,
            Some("read_only") => Intent::ReadOnly,
            Some("write_allowed") => Intent::WriteAllowed,
            Some("lifecycle") => Intent::Lifecycle,
            Some(other) => {
                return Err(crate::error::SandboxApiError::decode(format!(
                    "unknown tool-call intent: {other}"
                )));
            }
        };
        Ok(Self {
            invocation_id,
            agent_id: payload
                .get("agent_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            verb: payload
                .get("verb")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            intent,
            args,
            background: payload
                .get("background")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

fn default_true() -> bool {
    true
}

fn default_output_mode() -> String {
    "files_with_matches".to_owned()
}

fn default_grace_s() -> f64 {
    5.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(agent_id: &str) -> SandboxCaller {
        SandboxCaller {
            agent_id: agent_id.to_owned(),
            run_id: String::new(),
            agent_run_id: String::new(),
            task_id: String::new(),
            request_id: String::new(),
            attempt_id: String::new(),
            workflow_id: String::new(),
            tool_id: None,
        }
    }

    // AC-sandbox-api-02: a serialized caller never emits `tool_name`; an unset
    // `tool_id` is omitted and a set one round-trips.
    #[test]
    fn caller_omits_tool_name_and_optional_tool_id() {
        let value = serde_json::to_value(caller("agent-1")).expect("serialize caller");
        let object = value.as_object().expect("caller is an object");
        assert!(
            !object.contains_key("tool_name"),
            "tool_name was removed (GC-01)"
        );
        assert!(
            !object.contains_key("tool_id"),
            "unset tool_id is omitted from the wire"
        );

        let mut with_tool = caller("agent-1");
        with_tool.tool_id = Some("tool-9".parse().expect("non-empty tool id"));
        let value = serde_json::to_value(&with_tool).expect("serialize caller");
        assert_eq!(value["tool_id"], serde_json::json!("tool-9"));
        let back: SandboxCaller = serde_json::from_value(value).expect("roundtrip caller");
        assert_eq!(back, with_tool);
    }

    // AC-sandbox-api-04 (identity-block portion): the nested `caller` block
    // always carries the four required ids (even empty) and omits empty optional
    // ids. The fixture uses agent_id == agent_run_id (production shape) to catch
    // accidental newtype coupling while keeping the fields distinct.
    #[test]
    fn identity_block_required_empty_and_optional_omitted() {
        let mut c = caller("agent-run-7");
        c.agent_run_id = "agent-run-7".to_owned(); // equal to agent_id, distinct field
        let block = c.identity_block();

        for required in ["agent_id", "run_id", "agent_run_id", "task_id"] {
            assert!(block.contains_key(required), "required key {required}");
        }
        assert_eq!(block["agent_id"], serde_json::json!("agent-run-7"));
        assert_eq!(block["agent_run_id"], serde_json::json!("agent-run-7"));
        assert_eq!(block["run_id"], serde_json::json!(""));
        assert_eq!(block["task_id"], serde_json::json!(""));

        for optional in ["request_id", "attempt_id", "workflow_id", "tool_id"] {
            assert!(
                !block.contains_key(optional),
                "empty optional key {optional} must be omitted"
            );
        }

        // Populated optionals appear; tool_id uses its inner string.
        c.request_id = "req-1".to_owned();
        c.tool_id = Some("tool-2".parse().expect("non-empty tool id"));
        let block = c.identity_block();
        assert_eq!(block["request_id"], serde_json::json!("req-1"));
        assert_eq!(block["tool_id"], serde_json::json!("tool-2"));
        assert!(!block.contains_key("attempt_id"));
    }

    #[test]
    fn typed_accessors_validate_non_empty() {
        let mut c = caller("agent-1");
        assert_eq!(c.agent_run(), None);
        assert_eq!(c.task(), None);
        c.agent_run_id = "ar-1".to_owned();
        c.task_id = "task-1".to_owned();
        assert_eq!(c.agent_run().expect("typed").as_str(), "ar-1");
        assert_eq!(c.task().expect("typed").as_str(), "task-1");
    }

    #[test]
    fn description_or_falls_back_when_empty() {
        let base = SandboxRequestBase {
            caller: caller("a"),
            description: String::new(),
            invocation_id: None,
        };
        assert_eq!(base.description_or("write x"), "write x");
        let base = SandboxRequestBase {
            caller: caller("a"),
            description: "custom".to_owned(),
            invocation_id: None,
        };
        assert_eq!(base.description_or("write x"), "custom");
    }

    #[test]
    fn tool_call_request_payload_roundtrip() {
        let mut args = JsonObject::new();
        args.insert("path".to_owned(), Value::String("a.txt".to_owned()));
        let request = ToolCallRequest {
            invocation_id: "inv-1".parse().expect("non-empty"),
            agent_id: "agent-1".to_owned(),
            verb: "read_file".to_owned(),
            intent: Intent::WriteAllowed,
            args,
            background: true,
        };
        let payload = request.to_payload();
        assert_eq!(payload["intent"], serde_json::json!("write_allowed"));
        assert_eq!(payload["background"], serde_json::json!(true));
        let back = ToolCallRequest::from_payload(&payload).expect("parse payload");
        assert_eq!(back, request);
    }

    #[test]
    fn tool_call_request_rejects_non_object_args_and_empty_invocation() {
        let mut bad_args = JsonObject::new();
        bad_args.insert(
            "invocation_id".to_owned(),
            Value::String("inv-1".to_owned()),
        );
        bad_args.insert("args".to_owned(), Value::String("not-an-object".to_owned()));
        assert!(ToolCallRequest::from_payload(&bad_args).is_err());

        let empty_inv = JsonObject::new();
        assert!(ToolCallRequest::from_payload(&empty_inv).is_err());
    }
}
