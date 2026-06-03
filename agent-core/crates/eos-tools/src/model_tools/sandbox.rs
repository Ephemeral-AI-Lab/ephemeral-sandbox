//! Sandbox tools: `read_file`, `write_file`, `edit_file`, `multi_edit`, `grep`,
//! `glob`, `exec_command`, `write_stdin`. Each builds a typed request, calls the
//! `eos-sandbox-api` `tool_api` helper over the [`SandboxTransport`], and projects
//! the typed result into a serialized output DTO.
//!
//! Command-session **registration** with the background supervisor and the
//! `recover-from-supervisor` / `mark-reported` steps are engine-dispatch concerns
//! (anchor §3, "background execution is an engine dispatch mode"), relocated to
//! `eos-engine`; the tool body surfaces `command_session_id` and issues the
//! Ctrl-C cancel.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use eos_sandbox_api::{
    CommandSessionWriteRequest, EditFileRequest, ExecCommandRequest, ExecCommandResult,
    GlobRequest, GrepRequest, ReadFileRequest, SandboxRequestBase, SearchReplaceEdit,
    WriteFileRequest,
};
use eos_types::{CommandSessionId, InvocationId, JsonObject};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::ToolError;
use crate::execution::parse_input;
use crate::executor::ToolExecutor;
use crate::metadata::ExecutionMetadata;
use crate::name::ToolName;
use crate::registry::ToolRegistry;
use crate::result::{OutputShape, ToolResult};
use crate::spec::json_spec;

const MAX_READ_FILE_LINES: u32 = 200;
const MAX_YIELD_TIME_MS: u32 = 30_000;

// ---------------------------------------------------------------------------
// Shared helpers (ported from sandbox/_lib/tool_context.py).
// ---------------------------------------------------------------------------

fn request_base(ctx: &ExecutionMetadata, description: &str) -> SandboxRequestBase {
    SandboxRequestBase {
        caller: ctx.caller.clone(),
        description: description.to_owned(),
        invocation_id: ctx.sandbox_invocation_id.clone(),
    }
}

/// `resolve_tool_sandbox_path`: absolute paths pass through; otherwise join under
/// `repo_root`.
fn resolve_path(ctx: &ExecutionMetadata, path: &str) -> String {
    if path.starts_with('/') {
        return path.to_owned();
    }
    let repo_root = ctx.repo_root.trim();
    if repo_root.is_empty() {
        path.to_owned()
    } else {
        format!("{}/{path}", repo_root.trim_end_matches('/'))
    }
}

fn cwd(ctx: &ExecutionMetadata) -> String {
    ctx.repo_root.trim().to_owned()
}

fn serialize<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("tool output DTO serializes")
}

fn invalid_input(tool: ToolName, message: impl std::fmt::Display) -> ToolResult {
    ToolResult::error(format!(
        "Invalid input for {}: {message}. Please retry the tool call with valid arguments.",
        tool.as_str()
    ))
}

/// `_failure_status(conflict_reason)`.
fn failure_status(conflict_reason: Option<&str>) -> String {
    match conflict_reason {
        Some("base_mismatch" | "version_conflict" | "drift") => "aborted_version",
        Some("lock_conflict" | "locked") => "aborted_lock",
        Some("not_found" | "missing") => "not_found",
        _ => "failed",
    }
    .to_owned()
}

// ---------------------------------------------------------------------------
// Output DTOs.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ReadFileOutput {
    cwd: String,
    file_path: String,
    total_lines: u32,
    start_line: u32,
    end_line: u32,
    content: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct MutationOutput {
    cwd: String,
    file_path: String,
    status: String,
    changed_paths: Vec<String>,
    changed_path_kinds: BTreeMap<String, String>,
    mutation_source: String,
    conflict_reason: Option<String>,
    error: JsonObject,
    /// `bytes_written` for `write_file`, `applied_edits` for the edit tools.
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct GrepOutput {
    cwd: String,
    pattern: String,
    mode: String,
    filenames: Vec<String>,
    content: String,
    num_files: u32,
    num_lines: u32,
    num_matches: u32,
    applied_limit: Option<u32>,
    applied_offset: u32,
    truncated: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct GlobOutput {
    cwd: String,
    pattern: String,
    filenames: Vec<String>,
    num_files: u32,
    truncated: bool,
}

/// `CommandToolOutput` (`command_session_tool.py`).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CommandToolOutput {
    status: String,
    exit_code: Option<i32>,
    output: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_session_id: Option<String>,
    stdout: String,
    stderr: String,
    changed_paths: Vec<String>,
    changed_path_kinds: BTreeMap<String, String>,
    mutation_source: String,
    conflict_reason: Option<String>,
    error: Option<JsonObject>,
}

// ---------------------------------------------------------------------------
// read_file
// ---------------------------------------------------------------------------

const READ_FILE_DESCRIPTION: &str = include_str!("descriptions/read_file.md");

fn default_one() -> u32 {
    1
}
fn default_max_read_lines() -> u32 {
    MAX_READ_FILE_LINES
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct ReadFileInput {
    file_path: String,
    #[serde(default = "default_one")]
    #[schemars(default = "default_one", range(min = 1))]
    start_line: u32,
    #[serde(default = "default_max_read_lines")]
    #[schemars(default = "default_max_read_lines", range(min = 1))]
    end_line: u32,
}

struct ReadFile;

#[async_trait]
impl ToolExecutor for ReadFile {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: ReadFileInput = match parse_input(ToolName::ReadFile, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        // `default_end_line_to_window`: auto-window only when end_line is omitted.
        let end_line = if input.contains_key("end_line") {
            parsed.end_line
        } else {
            parsed
                .start_line
                .saturating_add(MAX_READ_FILE_LINES.saturating_sub(1))
        };
        if parsed.start_line == 0 {
            return Ok(invalid_input(ToolName::ReadFile, "start_line must be >= 1"));
        }
        if end_line == 0 {
            return Ok(invalid_input(ToolName::ReadFile, "end_line must be >= 1"));
        }
        if end_line < parsed.start_line {
            return Ok(invalid_input(
                ToolName::ReadFile,
                "end_line cannot be smaller than start_line",
            ));
        }
        if end_line - parsed.start_line + 1 > MAX_READ_FILE_LINES {
            return Ok(invalid_input(
                ToolName::ReadFile,
                format!("read_file can return at most {MAX_READ_FILE_LINES} lines"),
            ));
        }

        let path = resolve_path(ctx, &parsed.file_path);
        let sandbox_id = ctx.require_sandbox_id()?;
        let request = ReadFileRequest {
            base: request_base(ctx, &format!("read {path}")),
            path: path.clone(),
        };
        let result = match eos_sandbox_api::read_file(&*ctx.transport, sandbox_id, &request).await {
            Ok(result) => result,
            Err(err) => return Ok(ToolResult::error(err.to_string())),
        };
        if !result.base.success {
            return Ok(ToolResult::error(format!("Failed to read file: {path}")));
        }
        if !result.exists {
            return Ok(ToolResult::error(format!("Path does not exist: {path}")));
        }

        let output =
            build_read_file_output(ctx, &path, &result.content, parsed.start_line, end_line);
        Ok(ToolResult::ok(serialize(&output)))
    }
}

/// `build_read_file_result`: window + `cat -n`-style line numbering done in the
/// tool over the daemon's full-file content.
fn build_read_file_output(
    ctx: &ExecutionMetadata,
    file_path: &str,
    content: &str,
    start_line: u32,
    end_line: u32,
) -> ReadFileOutput {
    let lines: Vec<&str> = if content.is_empty() {
        Vec::new()
    } else {
        content.split('\n').collect()
    };
    let total = lines.len() as u32;
    let start = start_line.max(1);
    let end = end_line.min(total);
    let mut rendered = Vec::new();
    if total > 0 && start <= end {
        for n in start..=end {
            rendered.push(format!("{n:4}: {}", lines[(n - 1) as usize]));
        }
    }
    ReadFileOutput {
        cwd: cwd(ctx),
        file_path: file_path.to_owned(),
        total_lines: total,
        start_line: start,
        end_line: end,
        content: rendered.join("\n"),
    }
}

// ---------------------------------------------------------------------------
// write_file
// ---------------------------------------------------------------------------

const WRITE_FILE_DESCRIPTION: &str = include_str!("descriptions/write_file.md");

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct WriteFileInput {
    file_path: String,
    content: String,
}

struct WriteFile;

#[async_trait]
impl ToolExecutor for WriteFile {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: WriteFileInput = match parse_input(ToolName::WriteFile, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        let path = resolve_path(ctx, &parsed.file_path);
        let sandbox_id = ctx.require_sandbox_id()?;
        let request = WriteFileRequest {
            base: request_base(ctx, &format!("write {path}")),
            path: path.clone(),
            content: parsed.content.clone(),
            overwrite: true,
        };
        let result = match eos_sandbox_api::write_file(&*ctx.transport, sandbox_id, &request).await
        {
            Ok(result) => result,
            Err(err) => return Ok(ToolResult::error(err.to_string())),
        };
        let bytes = parsed.content.len() as u64;
        let output = MutationOutput {
            cwd: cwd(ctx),
            file_path: path,
            status: if result.base.success {
                "written".to_owned()
            } else {
                failure_status(result.base.conflict_reason.as_deref())
            },
            changed_paths: result.base.changed_paths,
            changed_path_kinds: result.changed_path_kinds,
            mutation_source: result.mutation_source,
            conflict_reason: result.base.conflict_reason,
            error: result.base.error.unwrap_or_default(),
            extra: BTreeMap::from([("bytes_written".to_owned(), json!(bytes))]),
        };
        Ok(mutation_result(result.base.success, output))
    }
}

// ---------------------------------------------------------------------------
// edit_file + multi_edit
// ---------------------------------------------------------------------------

const EDIT_FILE_DESCRIPTION: &str = include_str!("descriptions/edit_file.md");
const MULTI_EDIT_DESCRIPTION: &str = include_str!("descriptions/multi_edit.md");

fn default_false() -> bool {
    false
}
fn default_empty() -> String {
    String::new()
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct EditFileInput {
    file_path: String,
    #[serde(default = "default_empty")]
    old_text: String,
    #[serde(default = "default_empty")]
    new_text: String,
    #[serde(default = "default_false")]
    replace_all: bool,
    #[serde(default = "default_empty")]
    description: String,
}

struct EditFile;

#[async_trait]
impl ToolExecutor for EditFile {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: EditFileInput = match parse_input(ToolName::EditFile, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.old_text.is_empty() {
            return Ok(ToolResult::error(
                "Provide `old_text` (text to find) and `new_text` (replacement).",
            ));
        }
        let path = resolve_path(ctx, &parsed.file_path);
        let sandbox_id = ctx.require_sandbox_id()?;
        let description = if parsed.description.is_empty() {
            format!("edit {path}")
        } else {
            parsed.description.clone()
        };
        let request = EditFileRequest {
            base: request_base(ctx, &description),
            path: path.clone(),
            edits: vec![SearchReplaceEdit {
                old_text: parsed.old_text,
                new_text: parsed.new_text,
                replace_all: parsed.replace_all,
            }],
        };
        let result = match eos_sandbox_api::edit_file(&*ctx.transport, sandbox_id, &request).await {
            Ok(result) => result,
            Err(err) => return Ok(ToolResult::error(err.to_string())),
        };
        let applied = if result.base.success {
            u64::from(result.applied_edits)
        } else {
            0
        };
        Ok(edit_output(
            ctx,
            path,
            &result.base,
            result.changed_path_kinds,
            result.mutation_source,
            applied,
        ))
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct MultiEditOp {
    old_text: String,
    #[serde(default = "default_empty")]
    new_text: String,
    #[serde(default = "default_false")]
    replace_all: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct MultiEditInput {
    file_path: String,
    edits: Vec<MultiEditOp>,
    #[serde(default = "default_empty")]
    description: String,
}

struct MultiEdit;

#[async_trait]
impl ToolExecutor for MultiEdit {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: MultiEditInput = match parse_input(ToolName::MultiEdit, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.edits.is_empty() {
            return Ok(ToolResult::error("Provide at least one edit in `edits`."));
        }
        let path = resolve_path(ctx, &parsed.file_path);
        let sandbox_id = ctx.require_sandbox_id()?;
        let count = parsed.edits.len() as u64;
        let description = if parsed.description.is_empty() {
            format!("multi-edit {path}")
        } else {
            parsed.description.clone()
        };
        let request = EditFileRequest {
            base: request_base(ctx, &description),
            path: path.clone(),
            edits: parsed
                .edits
                .into_iter()
                .map(|op| SearchReplaceEdit {
                    old_text: op.old_text,
                    new_text: op.new_text,
                    replace_all: op.replace_all,
                })
                .collect(),
        };
        let result = match eos_sandbox_api::edit_file(&*ctx.transport, sandbox_id, &request).await {
            Ok(result) => result,
            Err(err) => return Ok(ToolResult::error(err.to_string())),
        };
        // multi_edit reports the count of edits submitted (not result.applied_edits).
        let applied = if result.base.success { count } else { 0 };
        Ok(edit_output(
            ctx,
            path,
            &result.base,
            result.changed_path_kinds,
            result.mutation_source,
            applied,
        ))
    }
}

fn edit_output(
    ctx: &ExecutionMetadata,
    file_path: String,
    base: &eos_sandbox_api::SandboxResultBase,
    changed_path_kinds: BTreeMap<String, String>,
    mutation_source: String,
    applied_edits: u64,
) -> ToolResult {
    let output = MutationOutput {
        cwd: cwd(ctx),
        file_path,
        status: if base.success {
            "edited".to_owned()
        } else {
            failure_status(base.conflict_reason.as_deref())
        },
        changed_paths: base.changed_paths.clone(),
        changed_path_kinds,
        mutation_source,
        conflict_reason: base.conflict_reason.clone(),
        error: base.error.clone().unwrap_or_default(),
        extra: BTreeMap::from([("applied_edits".to_owned(), json!(applied_edits))]),
    };
    mutation_result(base.success, output)
}

fn mutation_result(success: bool, output: MutationOutput) -> ToolResult {
    let serialized = serialize(&output);
    let mut result = if success {
        ToolResult::ok(serialized)
    } else {
        ToolResult::error(serialized)
    };
    // Moves `output.status` out, consuming `output` (so it is not a
    // pass-by-reference-only argument).
    result
        .metadata
        .insert("status".to_owned(), Value::String(output.status));
    result
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

const GREP_DESCRIPTION: &str = include_str!("descriptions/grep.md");

/// `output_mode` literal.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum GrepMode {
    Content,
    FilesWithMatches,
    Count,
}

impl GrepMode {
    fn as_wire(self) -> &'static str {
        match self {
            GrepMode::Content => "content",
            GrepMode::FilesWithMatches => "files_with_matches",
            GrepMode::Count => "count",
        }
    }
}

fn default_grep_mode() -> GrepMode {
    GrepMode::FilesWithMatches
}
fn default_head_limit() -> u32 {
    250
}
fn default_zero() -> u32 {
    0
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct GrepInput {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    glob_filter: Option<String>,
    #[serde(default = "default_grep_mode")]
    #[schemars(default = "default_grep_mode")]
    output_mode: GrepMode,
    #[serde(default = "default_head_limit")]
    #[schemars(default = "default_head_limit")]
    head_limit: u32,
    #[serde(default = "default_zero")]
    offset: u32,
    #[serde(default = "default_false")]
    case_insensitive: bool,
    #[serde(default = "default_false")]
    line_numbers: bool,
    #[serde(default = "default_false")]
    multiline: bool,
}

struct Grep;

#[async_trait]
impl ToolExecutor for Grep {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: GrepInput = match parse_input(ToolName::Grep, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        let resolved = parsed.path.as_deref().map(|p| resolve_path(ctx, p));
        let sandbox_id = ctx.require_sandbox_id()?;
        let request = GrepRequest {
            base: request_base(ctx, "grep"),
            pattern: parsed.pattern.clone(),
            path: resolved,
            glob_filter: parsed.glob_filter,
            output_mode: parsed.output_mode.as_wire().to_owned(),
            head_limit: Some(parsed.head_limit),
            offset: parsed.offset,
            case_insensitive: parsed.case_insensitive,
            line_numbers: parsed.line_numbers,
            multiline: parsed.multiline,
        };
        let result = match eos_sandbox_api::grep(&*ctx.transport, sandbox_id, &request).await {
            Ok(result) => result,
            Err(err) => return Ok(ToolResult::error(err.to_string())),
        };
        if !result.base.success {
            return Ok(ToolResult::error(format!(
                "grep failed for pattern: {}",
                parsed.pattern
            )));
        }
        let output = GrepOutput {
            cwd: cwd(ctx),
            pattern: parsed.pattern,
            mode: result.output_mode,
            filenames: result.filenames,
            content: result.content,
            num_files: result.num_files,
            num_lines: result.num_lines,
            num_matches: result.num_matches,
            applied_limit: result.applied_limit,
            applied_offset: result.applied_offset,
            truncated: result.truncated,
        };
        Ok(ToolResult::ok(serialize(&output)))
    }
}

// ---------------------------------------------------------------------------
// glob
// ---------------------------------------------------------------------------

const GLOB_DESCRIPTION: &str = include_str!("descriptions/glob.md");

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct GlobInput {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

struct Glob;

#[async_trait]
impl ToolExecutor for Glob {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: GlobInput = match parse_input(ToolName::Glob, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        let resolved = parsed.path.as_deref().map(|p| resolve_path(ctx, p));
        let sandbox_id = ctx.require_sandbox_id()?;
        let request = GlobRequest {
            base: request_base(ctx, "glob"),
            pattern: parsed.pattern.clone(),
            path: resolved,
        };
        let result = match eos_sandbox_api::glob(&*ctx.transport, sandbox_id, &request).await {
            Ok(result) => result,
            Err(err) => return Ok(ToolResult::error(err.to_string())),
        };
        if !result.base.success {
            return Ok(ToolResult::error(format!(
                "glob failed for pattern: {}",
                parsed.pattern
            )));
        }
        let output = GlobOutput {
            cwd: cwd(ctx),
            pattern: parsed.pattern,
            filenames: result.filenames,
            num_files: result.num_files,
            truncated: result.truncated,
        };
        Ok(ToolResult::ok(serialize(&output)))
    }
}

// ---------------------------------------------------------------------------
// exec_command + write_stdin (command sessions).
// ---------------------------------------------------------------------------

const EXEC_COMMAND_DESCRIPTION: &str = "Run a command in a managed PTY session inside the sandbox. \
If the command finishes within `yield_time_ms` you get the final result; otherwise the session \
keeps running in the background and you get `status: running` with a `command_session_id` — use \
`write_stdin` to feed input, poll for more output, or tear it down. Set `timeout` (seconds) to bound \
the run and `max_output_tokens` to cap returned output. Output is a merged PTY stream: everything \
(including the program's stderr) arrives in `stdout`, and the `stderr` field is always empty.";
const WRITE_STDIN_DESCRIPTION: &str = "Interact with a running command session by `command_session_id`. \
Write literal text to its stdin (e.g. `\"y\\n\"`), or poll for more output with empty `chars`. A `\\x03` \
(Ctrl-C) character only interrupts the foreground program (SIGINT); to end the session entirely set \
`terminate: true` (SIGTERM→SIGKILL). Returns the final result once the command exits, otherwise \
`status: running` with output so far. Output is the merged PTY stream in `stdout`; `stderr` is always empty.";

fn default_yield_ms() -> u32 {
    1000
}

fn validate_command_timing(
    tool: ToolName,
    yield_time_ms: u32,
    timeout: Option<u32>,
    max_output_tokens: Option<u32>,
) -> Option<ToolResult> {
    if yield_time_ms > MAX_YIELD_TIME_MS {
        return Some(invalid_input(
            tool,
            format!("yield_time_ms must be <= {MAX_YIELD_TIME_MS}"),
        ));
    }
    if timeout == Some(0) {
        return Some(invalid_input(tool, "timeout must be >= 1"));
    }
    if max_output_tokens == Some(0) {
        return Some(invalid_input(tool, "max_output_tokens must be >= 1"));
    }
    None
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct ExecCommandInput {
    cmd: String,
    #[serde(default = "default_yield_ms")]
    #[schemars(default = "default_yield_ms", range(max = 30000))]
    yield_time_ms: u32,
    #[serde(default)]
    #[schemars(range(min = 1))]
    timeout: Option<u32>,
    #[serde(default)]
    #[schemars(range(min = 1))]
    max_output_tokens: Option<u32>,
}

struct ExecCommand;

#[async_trait]
impl ToolExecutor for ExecCommand {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: ExecCommandInput = match parse_input(ToolName::ExecCommand, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if let Some(err) = validate_command_timing(
            ToolName::ExecCommand,
            parsed.yield_time_ms,
            parsed.timeout,
            parsed.max_output_tokens,
        ) {
            return Ok(err);
        }
        if parsed.cmd.is_empty() {
            return Ok(invalid_input(
                ToolName::ExecCommand,
                "cmd must be non-empty",
            ));
        }
        let sandbox_id = ctx.require_sandbox_id()?;
        let invocation_id = ctx
            .sandbox_invocation_id
            .clone()
            .unwrap_or_else(InvocationId::new_v4);
        let mut base = request_base(ctx, "exec_command");
        base.invocation_id = Some(invocation_id);
        let command = parsed.cmd.clone();
        let request = ExecCommandRequest {
            base,
            cmd: parsed.cmd,
            yield_time_ms: Some(parsed.yield_time_ms),
            timeout: parsed.timeout,
            max_output_tokens: parsed.max_output_tokens,
        };
        let result =
            match eos_sandbox_api::exec_command(&*ctx.transport, sandbox_id, &request).await {
                Ok(result) => result,
                Err(err) => return Ok(ToolResult::error(err.to_string())),
            };
        // Register a backgrounded session with the supervisor so the heartbeat
        // pulls its completion. The daemon scopes the session under the RPC's
        // top-level `agent_id` (== `caller.agent_id`), so register under the same
        // id or the heartbeat's `collect_completed` filter would never match.
        if let (Some(port), Some(session_id)) =
            (&ctx.command_session_supervisor, &result.command_session_id)
        {
            if result.status == "running" {
                port.register(
                    session_id,
                    sandbox_id.as_str(),
                    &ctx.caller.agent_id,
                    &command,
                )
                .await;
            }
        }
        Ok(command_tool_result(&result))
    }
}

fn default_chars() -> String {
    String::new()
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct WriteStdinInput {
    command_session_id: CommandSessionId,
    #[serde(default = "default_chars")]
    chars: String,
    #[serde(default = "default_yield_ms")]
    #[schemars(default = "default_yield_ms", range(max = 30000))]
    yield_time_ms: u32,
    #[serde(default)]
    #[schemars(range(min = 1))]
    max_output_tokens: Option<u32>,
    /// Tear the session down after writing. A `\x03` char only interrupts
    /// (SIGINT); set this to end the session (SIGTERM→SIGKILL).
    #[serde(default = "default_false")]
    terminate: bool,
}

struct WriteStdin;

#[async_trait]
impl ToolExecutor for WriteStdin {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: WriteStdinInput = match parse_input(ToolName::WriteStdin, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if let Some(err) = validate_command_timing(
            ToolName::WriteStdin,
            parsed.yield_time_ms,
            None,
            parsed.max_output_tokens,
        ) {
            return Ok(err);
        }
        if parsed.command_session_id.as_str().is_empty() {
            return Ok(invalid_input(
                ToolName::WriteStdin,
                "command_session_id must be non-empty",
            ));
        }
        let command_session_id = parsed.command_session_id.into_inner();
        let sandbox_id = ctx.require_sandbox_id()?;
        // Ctrl-C decoupling (sense-2 D7): `\x03` rides through as ordinary stdin
        // and the daemon raises SIGINT; teardown is the explicit `terminate`
        // flag (SIGTERM→SIGKILL), so the tool no longer escalates to a cancel RPC.
        let write_request = CommandSessionWriteRequest {
            base: request_base(ctx, "write_stdin"),
            command_session_id: command_session_id.clone(),
            chars: parsed.chars.clone(),
            yield_time_ms: Some(parsed.yield_time_ms),
            max_output_tokens: parsed.max_output_tokens,
            terminate: parsed.terminate,
        };
        let result =
            match eos_sandbox_api::write_stdin(&*ctx.transport, sandbox_id, &write_request).await {
                Ok(result) => result,
                Err(err) => return Ok(ToolResult::error(err.to_string())),
            };
        // Recover race + exactly-once latch (anchor §8). If the daemon already
        // lost the live session, surface the supervisor's stored terminal;
        // otherwise, once a terminal status is observed inline, latch it
        // `Delivered` so the heartbeat never re-notifies the same completion.
        if let Some(port) = &ctx.command_session_supervisor {
            if is_command_session_not_found(&result) {
                if let Some(stored) = port.command_session_result(&command_session_id).await {
                    port.mark_command_session_reported(&command_session_id, stored.clone())
                        .await;
                    return Ok(command_tool_result_from_value(&stored));
                }
            } else if result.status != "running" {
                port.mark_command_session_reported(
                    &command_session_id,
                    command_result_value(&result),
                )
                .await;
            }
        }
        Ok(command_tool_result(&result))
    }
}

/// Whether a `write_stdin` result is the daemon's "live session is gone" signal
/// (`command_session_not_found`), so the supervisor's stored terminal can be
/// recovered.
fn is_command_session_not_found(result: &ExecCommandResult) -> bool {
    result.status == "error" && result.output.stderr.contains("command_session_not_found")
}

/// Project an [`ExecCommandResult`] into the daemon completion `result` shape the
/// supervisor stores (status / `exit_code` / `output`).
fn command_result_value(result: &ExecCommandResult) -> Value {
    json!({
        "status": result.status,
        "exit_code": result.exit_code,
        "output": {
            "stdout": result.output.stdout,
            "stderr": result.output.stderr,
        },
    })
}

/// Render a supervisor-stored terminal `result` value into the tool output DTO
/// (the recover-race return path).
fn command_tool_result_from_value(result: &Value) -> ToolResult {
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("ok")
        .to_owned();
    let exit_code = result
        .get("exit_code")
        .and_then(Value::as_i64)
        .map(|code| code as i32);
    let stdout = result
        .get("output")
        .and_then(|output| output.get("stdout"))
        .and_then(Value::as_str)
        .or_else(|| result.get("stdout").and_then(Value::as_str))
        .unwrap_or("")
        .to_owned();
    let stderr = result
        .get("output")
        .and_then(|output| output.get("stderr"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let is_error = matches!(status.as_str(), "error" | "timed_out");
    let mut output_map = BTreeMap::new();
    output_map.insert("stdout".to_owned(), stdout.clone());
    output_map.insert("stderr".to_owned(), stderr.clone());
    let payload = CommandToolOutput {
        status: status.clone(),
        exit_code,
        output: output_map,
        command_session_id: None,
        stdout,
        stderr,
        changed_paths: Vec::new(),
        changed_path_kinds: BTreeMap::new(),
        mutation_source: String::new(),
        conflict_reason: None,
        error: None,
    };
    let mut metadata = JsonObject::new();
    metadata.insert("status".to_owned(), json!(status));
    ToolResult {
        output: serialize(&payload),
        is_error,
        metadata,
        is_terminal: false,
    }
}

/// `command_tool_result`.
fn command_tool_result(result: &ExecCommandResult) -> ToolResult {
    let is_error = matches!(result.status.as_str(), "error" | "timed_out");
    let mut output_map = BTreeMap::new();
    output_map.insert("stdout".to_owned(), result.output.stdout.clone());
    output_map.insert("stderr".to_owned(), result.output.stderr.clone());
    let payload = CommandToolOutput {
        status: result.status.clone(),
        exit_code: result.exit_code,
        output: output_map,
        command_session_id: result.command_session_id.clone(),
        stdout: result.output.stdout.clone(),
        stderr: result.output.stderr.clone(),
        changed_paths: result.base.changed_paths.clone(),
        changed_path_kinds: result.changed_path_kinds.clone(),
        mutation_source: result.mutation_source.clone(),
        conflict_reason: result.base.conflict_reason.clone(),
        error: result.base.error.clone(),
    };
    let mut metadata = JsonObject::new();
    metadata.insert("status".to_owned(), json!(result.status));
    if let Some(id) = &result.command_session_id {
        metadata.insert("command_session_id".to_owned(), json!(id));
    }
    ToolResult {
        output: serialize(&payload),
        is_error,
        metadata,
        is_terminal: false,
    }
}

// ---------------------------------------------------------------------------
// Registration.
// ---------------------------------------------------------------------------

pub(crate) fn register(registry: &mut ToolRegistry) {
    super::register_tool(
        registry,
        ToolName::ReadFile,
        json_spec(
            ToolName::ReadFile,
            READ_FILE_DESCRIPTION,
            schema_for!(ReadFileInput),
            schema_for!(ReadFileOutput),
        ),
        OutputShape::json::<ReadFileOutput>("ReadFileOutput"),
        Arc::new(ReadFile),
    );
    super::register_tool(
        registry,
        ToolName::WriteFile,
        json_spec(
            ToolName::WriteFile,
            WRITE_FILE_DESCRIPTION,
            schema_for!(WriteFileInput),
            schema_for!(MutationOutput),
        ),
        OutputShape::json::<MutationOutput>("WriteFileOutput"),
        Arc::new(WriteFile),
    );
    super::register_tool(
        registry,
        ToolName::EditFile,
        json_spec(
            ToolName::EditFile,
            EDIT_FILE_DESCRIPTION,
            schema_for!(EditFileInput),
            schema_for!(MutationOutput),
        ),
        OutputShape::json::<MutationOutput>("EditFileOutput"),
        Arc::new(EditFile),
    );
    super::register_tool(
        registry,
        ToolName::MultiEdit,
        json_spec(
            ToolName::MultiEdit,
            MULTI_EDIT_DESCRIPTION,
            schema_for!(MultiEditInput),
            schema_for!(MutationOutput),
        ),
        OutputShape::json::<MutationOutput>("MultiEditOutput"),
        Arc::new(MultiEdit),
    );
    super::register_tool(
        registry,
        ToolName::ExecCommand,
        json_spec(
            ToolName::ExecCommand,
            EXEC_COMMAND_DESCRIPTION,
            schema_for!(ExecCommandInput),
            schema_for!(CommandToolOutput),
        ),
        OutputShape::json::<CommandToolOutput>("CommandToolOutput"),
        Arc::new(ExecCommand),
    );
    super::register_tool(
        registry,
        ToolName::WriteStdin,
        json_spec(
            ToolName::WriteStdin,
            WRITE_STDIN_DESCRIPTION,
            schema_for!(WriteStdinInput),
            schema_for!(CommandToolOutput),
        ),
        OutputShape::json::<CommandToolOutput>("CommandToolOutput"),
        Arc::new(WriteStdin),
    );
    super::register_tool(
        registry,
        ToolName::Glob,
        json_spec(
            ToolName::Glob,
            GLOB_DESCRIPTION,
            schema_for!(GlobInput),
            schema_for!(GlobOutput),
        ),
        OutputShape::json::<GlobOutput>("GlobOutput"),
        Arc::new(Glob),
    );
    super::register_tool(
        registry,
        ToolName::Grep,
        json_spec(
            ToolName::Grep,
            GREP_DESCRIPTION,
            schema_for!(GrepInput),
            schema_for!(GrepOutput),
        ),
        OutputShape::json::<GrepOutput>("GrepOutput"),
        Arc::new(Grep),
    );
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use eos_sandbox_api::{DaemonOp, SandboxApiError};

    use super::*;
    use crate::testsupport::{caller, FakeRequestStore, FakeTaskStore, FakeTransport};
    use eos_skills::SkillRegistry;

    fn metadata_with(transport: Arc<dyn eos_sandbox_api::SandboxTransport>) -> ExecutionMetadata {
        ExecutionMetadata {
            sandbox_id: Some("sandbox-1".parse().expect("id")),
            agent_run_id: None,
            agent_name: "tester".to_owned(),
            cwd: String::new(),
            repo_root: "/repo".to_owned(),
            exec_cwd: String::new(),
            request_id: None,
            task_id: None,
            attempt_id: None,
            workflow_id: None,
            tool_use_id: None,
            sandbox_invocation_id: Some("inv-1".parse().expect("id")),
            caller: caller(),
            transport,
            task_store: Arc::new(FakeTaskStore::new()),
            request_store: Arc::new(FakeRequestStore::new()),
            skill_registry: Arc::new(SkillRegistry::new()),
            workflow_control: None,
            plan_submission: None,
            subagent_supervisor: None,
            command_session_supervisor: None,
            isolated_workspace: None,
            notifications: None,
            conversation: Arc::from(Vec::new()),
        }
    }

    fn obj(pairs: &[(&str, Value)]) -> JsonObject {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    // AC-tools-11 (exec_command half): exec_command surfaces command_session_id
    // from the daemon response.
    #[tokio::test]
    async fn exec_command_session() {
        let transport = Arc::new(FakeTransport::new(|op, _| {
            assert_eq!(op, DaemonOp::ExecCommand);
            Ok(obj(&[
                ("status", json!("running")),
                ("command_session_id", json!("cs-7")),
                ("output", json!({"stdout": "", "stderr": ""})),
            ]))
        }));
        let ctx = metadata_with(transport);
        let input = obj(&[("cmd", json!("sleep 5"))]);
        let res = ExecCommand.execute(&input, &ctx).await.expect("ok");
        assert!(!res.is_error);
        assert_eq!(res.metadata["command_session_id"], json!("cs-7"));
        let payload: serde_json::Value = serde_json::from_str(&res.output).expect("json");
        assert_eq!(payload["command_session_id"], json!("cs-7"));
    }

    #[tokio::test]
    async fn exec_command_rejects_invalid_numeric_bounds() {
        let ctx = metadata_with(Arc::new(FakeTransport::inert()));
        for input in [
            obj(&[("cmd", json!("true")), ("yield_time_ms", json!(30_001))]),
            obj(&[("cmd", json!("true")), ("timeout", json!(0))]),
            obj(&[("cmd", json!("true")), ("max_output_tokens", json!(0))]),
        ] {
            let res = ExecCommand.execute(&input, &ctx).await.expect("ok");
            assert!(res.is_error, "{}", res.output);
            assert!(res.output.contains("Invalid input for exec_command"));
        }
    }

    // sense-2 D7: `\x03` is SIGINT-only and rides through as ordinary stdin — the
    // tool no longer escalates to a cancel RPC (the daemon raises SIGINT itself).
    #[tokio::test]
    async fn write_stdin_ctrl_c_does_not_escalate_to_cancel() {
        let cancels = Arc::new(AtomicUsize::new(0));
        let cancels_seen = cancels.clone();
        let transport = Arc::new(FakeTransport::new(move |op, _| match op {
            DaemonOp::ExecStdin => Ok(obj(&[
                ("status", json!("running")),
                ("output", json!({"stdout": "", "stderr": ""})),
            ])),
            DaemonOp::CommandCancel => {
                cancels_seen.fetch_add(1, Ordering::SeqCst);
                Ok(obj(&[("status", json!("cancelled"))]))
            }
            other => Err(SandboxApiError::decode(format!("unexpected op {other:?}"))),
        }));
        let ctx = metadata_with(transport);
        let input = obj(&[
            ("command_session_id", json!("cs-7")),
            ("chars", json!("\u{3}")),
        ]);
        let res = WriteStdin.execute(&input, &ctx).await.expect("ok");
        assert_eq!(
            cancels.load(Ordering::SeqCst),
            0,
            "ctrl-c must NOT issue a cancel RPC (D7: SIGINT only)"
        );
        let payload: serde_json::Value = serde_json::from_str(&res.output).expect("json");
        assert_eq!(payload["status"], json!("running"));
    }

    // sense-2 D7: `terminate: true` is forwarded on the write RPC so the daemon
    // tears the session down; no separate cancel RPC is issued by the tool.
    #[tokio::test]
    async fn write_stdin_terminate_forwards_flag() {
        let terminate_seen = Arc::new(AtomicUsize::new(0));
        let seen = terminate_seen.clone();
        let transport = Arc::new(FakeTransport::new(move |op, payload| match op {
            DaemonOp::ExecStdin => {
                if payload.get("terminate").and_then(Value::as_bool) == Some(true) {
                    seen.fetch_add(1, Ordering::SeqCst);
                }
                Ok(obj(&[
                    ("status", json!("cancelled")),
                    ("exit_code", json!(130)),
                    ("output", json!({"stdout": "", "stderr": ""})),
                ]))
            }
            other => Err(SandboxApiError::decode(format!("unexpected op {other:?}"))),
        }));
        let ctx = metadata_with(transport);
        let input = obj(&[
            ("command_session_id", json!("cs-7")),
            ("terminate", json!(true)),
        ]);
        let res = WriteStdin.execute(&input, &ctx).await.expect("ok");
        assert_eq!(
            terminate_seen.load(Ordering::SeqCst),
            1,
            "the terminate flag must be forwarded on the write RPC"
        );
        let payload: serde_json::Value = serde_json::from_str(&res.output).expect("json");
        assert_eq!(payload["status"], json!("cancelled"));
    }

    // A non-ctrl-c write does not cancel.
    #[tokio::test]
    async fn write_stdin_plain_does_not_cancel() {
        let transport = Arc::new(FakeTransport::new(|op, _| match op {
            DaemonOp::ExecStdin => Ok(obj(&[
                ("status", json!("running")),
                ("output", json!({"stdout": "ok", "stderr": ""})),
            ])),
            other => Err(SandboxApiError::decode(format!("unexpected op {other:?}"))),
        }));
        let ctx = metadata_with(transport);
        let input = obj(&[
            ("command_session_id", json!("cs-7")),
            ("chars", json!("y\n")),
        ]);
        let res = WriteStdin.execute(&input, &ctx).await.expect("ok");
        let payload: serde_json::Value = serde_json::from_str(&res.output).expect("json");
        assert_eq!(payload["status"], json!("running"));
    }

    #[tokio::test]
    async fn write_stdin_rejects_invalid_numeric_bounds() {
        let ctx = metadata_with(Arc::new(FakeTransport::inert()));
        for input in [
            obj(&[
                ("command_session_id", json!("cs-7")),
                ("yield_time_ms", json!(30_001)),
            ]),
            obj(&[
                ("command_session_id", json!("cs-7")),
                ("max_output_tokens", json!(0)),
            ]),
            obj(&[("command_session_id", json!(""))]),
        ] {
            let res = WriteStdin.execute(&input, &ctx).await.expect("ok");
            assert!(res.is_error, "{}", res.output);
            assert!(res.output.contains("Invalid input for write_stdin"));
        }
    }

    #[tokio::test]
    async fn read_file_rejects_zero_line_numbers() {
        let ctx = metadata_with(Arc::new(FakeTransport::inert()));
        for input in [
            obj(&[("file_path", json!("src/lib.rs")), ("start_line", json!(0))]),
            obj(&[
                ("file_path", json!("src/lib.rs")),
                ("start_line", json!(1)),
                ("end_line", json!(0)),
            ]),
        ] {
            let res = ReadFile.execute(&input, &ctx).await.expect("ok");
            assert!(res.is_error, "{}", res.output);
            assert!(res.output.contains("Invalid input for read_file"));
        }
    }
}
