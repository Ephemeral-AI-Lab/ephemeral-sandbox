//! Pre-execution hooks — a **sealed, closed** set (GC-tools-06), not an open
//! trait pipeline.
//!
//! The Python open `pre_hook` abstraction (not on the anchor §6 seam map) becomes
//! a sealed [`Hook`] enum the pipeline matches exhaustively. The six wired Python
//! hooks map one-to-one; `DestructiveGitShell` and `DestructiveShell` are distinct
//! (different match logic, message, and `policy`). All six are pre-phase (every
//! wired Python hook is a `pre_hook`; the unexercised post-hook stage is dropped),
//! so the enum carries no pre/post discriminator.
//!
//! A `Deny` becomes an in-band [`ToolResult`]`{is_error:true}` carrying the
//! `hook_failure` metadata shape the Python pipeline emits (`hook_pipeline.py`
//! `_build_hook_failure_result`).

use std::sync::LazyLock;

use eos_types::JsonObject;
use regex::Regex;
use serde_json::{json, Value};

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::ToolResult;
use crate::tools::HookServices;

mod advisor_approval;
mod disallow_nested_planner_deferral;
mod require_no_background_sessions;

/// One wired pre-hook. `#[non_exhaustive]`: hooks are added here, never as an
/// open trait (the closed set is matched exhaustively by [`Hook::run`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Hook {
    /// Refuse a terminal / lifecycle tool while background sessions are open.
    RequireNoBackgroundSessions {
        /// The protected tool.
        tool: ToolName,
    },
    /// Refuse a main-role terminal that lacks prior advisor approval (Python
    /// `AdvisorApprovalPreHook`).
    AdvisorApproval {
        /// The protected tool.
        tool: ToolName,
    },
    /// Refuse a planner that sets a deferred goal while its workflow depth
    /// exceeds `max_depth` (Python `DisallowNestedPlannerDeferral`).
    DisallowNestedPlannerDeferral {
        /// The protected tool.
        tool: ToolName,
        /// Deepest workflow depth still allowed to defer; deny when the
        /// submitting workflow's depth exceeds it. Configured per-tool in the
        /// `.eos-agents/tools/<wire>.md` `hooks:` entry.
        max_depth: u32,
    },
    /// Refuse git working-tree / metadata mutation shell commands (Python
    /// `DestructiveGitShellPreHook`).
    DestructiveGitShell {
        /// The protected tool.
        tool: ToolName,
    },
    /// Refuse destructive filesystem shell commands (Python
    /// `DestructiveShellPreHook`).
    DestructiveShell {
        /// The protected tool.
        tool: ToolName,
    },
    /// Refuse a read-only helper (`ask_advisor`) while an isolated workspace is
    /// open (Python `BlockInIsolatedMode`).
    BlockInIsolatedMode {
        /// The protected tool.
        tool: ToolName,
    },
}

/// The outcome of running one hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// The hook passed; execution proceeds. Carries the pass-phase metadata the
    /// Python `HookResult.pass_(value, metadata=...)` records in the `hook_trace`
    /// (empty for most hooks; the daemon-unavailable bailout stamps a `reason`).
    Pass(JsonObject),
    /// The hook denied execution; the pipeline returns an in-band error.
    Deny(HookDenial),
}

impl HookOutcome {
    /// A pass carrying no metadata (the common case).
    pub(crate) fn pass() -> Self {
        HookOutcome::Pass(JsonObject::new())
    }
}

/// A hook denial: the model-facing message plus the audit/policy metadata the
/// Python `HookResult.fail(...)` carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookDenial {
    /// The model-facing block message.
    pub message: String,
    /// The `policy` metadata value (`destructive_git`, `advisor_approval`, …).
    pub policy: &'static str,
    /// The classification `reason` tag, when the hook stamps one.
    pub reason: Option<String>,
    /// Additional metadata (e.g. `count` for in-flight rejections).
    pub extra: JsonObject,
}

impl HookDenial {
    fn new(message: impl Into<String>, policy: &'static str) -> Self {
        Self {
            message: message.into(),
            policy,
            reason: None,
            extra: JsonObject::new(),
        }
    }

    fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    fn with_count(mut self, count: usize) -> Self {
        self.extra.insert("count".to_owned(), json!(count));
        self
    }
}

impl Hook {
    /// The protected tool name.
    #[must_use]
    pub const fn tool(self) -> ToolName {
        match self {
            Hook::RequireNoBackgroundSessions { tool }
            | Hook::AdvisorApproval { tool }
            | Hook::DisallowNestedPlannerDeferral { tool, .. }
            | Hook::DestructiveGitShell { tool }
            | Hook::DestructiveShell { tool }
            | Hook::BlockInIsolatedMode { tool } => tool,
        }
    }

    /// The canonical config token for this hook — the string authored in a
    /// `.eos-agents/tools/*.md` `hooks:` list. The inverse map (token → variant,
    /// filling the `{ tool }` field) lives in `config.rs`; a round-trip test keeps
    /// the two in sync.
    #[must_use]
    pub const fn config_token(self) -> &'static str {
        match self {
            Hook::RequireNoBackgroundSessions { .. } => "no_background_sessions",
            Hook::AdvisorApproval { .. } => "advisor_approval",
            Hook::DisallowNestedPlannerDeferral { .. } => "disallow_nested_planner_deferral",
            Hook::DestructiveGitShell { .. } => "destructive_git_shell",
            Hook::DestructiveShell { .. } => "destructive_shell",
            Hook::BlockInIsolatedMode { .. } => "block_in_isolated_mode",
        }
    }

    /// The Python hook `name` (used in the `hook_failure` trace).
    #[must_use]
    pub fn hook_name(self) -> String {
        let tool = self.tool().as_str();
        match self {
            Hook::RequireNoBackgroundSessions { .. } => format!("no_background_sessions:{tool}"),
            Hook::AdvisorApproval { .. } => format!("advisor_approval:{tool}"),
            Hook::DisallowNestedPlannerDeferral { .. } => {
                format!("no_nested_planner_deferral:{tool}")
            }
            Hook::DestructiveGitShell { .. } => format!("sandbox_shell:destructive_git:{tool}"),
            Hook::DestructiveShell { .. } => format!("sandbox_shell:destructive_shell:{tool}"),
            Hook::BlockInIsolatedMode { .. } => format!("block_in_isolated_mode:{tool}"),
        }
    }

    /// Run this hook against the raw (post-`background`-check) input and context.
    ///
    /// # Errors
    /// Returns [`ToolError`] only on a genuine framework fault while reading
    /// downstream state; hook *denials* are returned as `Ok(HookOutcome::Deny)`.
    pub async fn run(
        self,
        raw_input: &JsonObject,
        ctx: &ExecutionMetadata,
        services: &HookServices,
    ) -> Result<HookOutcome, ToolError> {
        match self {
            Hook::DestructiveGitShell { .. } => Ok(run_destructive_git(raw_input)),
            Hook::DestructiveShell { .. } => Ok(run_destructive_shell(raw_input)),
            Hook::BlockInIsolatedMode { .. } => run_block_in_isolated_mode(ctx).await,
            Hook::RequireNoBackgroundSessions { tool } => {
                require_no_background_sessions::run_require_no_background_sessions(
                    tool, raw_input, ctx, services,
                )
                .await
            }
            Hook::AdvisorApproval { tool } => {
                advisor_approval::run_advisor_approval(tool, ctx).await
            }
            Hook::DisallowNestedPlannerDeferral { max_depth, .. } => {
                disallow_nested_planner_deferral::run_disallow_nested_planner_deferral(
                    max_depth, raw_input, ctx, services,
                )
                .await
            }
        }
    }
}

/// Build the in-band `hook_failure` [`ToolResult`] (Python
/// `_build_hook_failure_result`, pre-phase). `hook_trace` is the
/// already-accumulated trace of the hooks that *passed* before this denial (the
/// denier is recorded in `hook_failure`, not the trace); `raw_input` is the
/// effective tool input that reached the failing hook.
#[must_use]
pub(crate) fn hook_failure_result(
    hook: Hook,
    denial: &HookDenial,
    hook_trace: &[Value],
    raw_input: &JsonObject,
) -> ToolResult {
    let hook_name = hook.hook_name();
    let tool_name = hook.tool().as_str();
    let message = denial.message.clone();

    let output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": message,
        },
        "hookName": hook_name,
        "toolName": tool_name,
        "phase": "pre",
    });

    let mut metadata = JsonObject::new();
    metadata.insert(
        "hook_failure".to_owned(),
        json!({
            "phase": "pre",
            "hook_name": hook_name,
            "tool_name": tool_name,
            "reason": message,
            "hook_event_name": "PreToolUse",
            "permission_decision": "deny",
            "permission_decision_reason": message,
        }),
    );
    metadata.insert("policy".to_owned(), Value::String(denial.policy.to_owned()));
    if let Some(reason) = &denial.reason {
        metadata.insert("reason".to_owned(), Value::String(reason.clone()));
    }
    for (k, v) in &denial.extra {
        metadata.insert(k.clone(), v.clone());
    }
    // The Python `_build_hook_failure_result` shape also carries the accumulated
    // trace of passing hooks and the effective input (`hook_pipeline.py`).
    metadata.insert("hook_trace".to_owned(), Value::Array(hook_trace.to_vec()));
    metadata.insert(
        "effective_tool_input".to_owned(),
        Value::Object(raw_input.clone()),
    );

    ToolResult {
        output: output.to_string(),
        is_error: true,
        metadata,
        is_terminal: false,
    }
}

// ---------------------------------------------------------------------------
// Destructive shell / git command policy (pure; ports destructive_shell.py).
// ---------------------------------------------------------------------------

const DESTRUCTIVE_GIT_MESSAGE: &str = "BLOCKED: exec_command is for runtime commands, tests, and inspection. Destructive git mutation commands are forbidden here. They mutate repository metadata or working-tree files outside the OCC/write-scope audit path. Use edit_file or write_file instead. (Note: shell-substitution forms such as $(...), backticks, bash -c, or eval can bypass this prehook; the sandbox commit/write audit remains the authoritative isolation boundary.)";

const DESTRUCTIVE_SHELL_MESSAGE: &str = "BLOCKED: destructive shell command that targets workspace or system directories (rm -r /testbed, mv /testbed, etc.) is forbidden. These commands destroy the shared workspace and cannot be undone. Use targeted file operations instead.";

static GIT_COMMAND_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[;&|]\s*)(?:command\s+)?git\b([^;&|]*)").expect("valid git pattern")
});

static DESTRUCTIVE_SHELL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(?:^|[;&|]\s*)(?:",
        r"rm\s+(?:-\S*[rR]\S*\s+|--recursive\s+)(?:/(?:testbed|workspace|home|opt|usr|var|etc|tmp)\b|/\s|/\.\.|\.\.)",
        r"|mv\s+/(?:testbed|workspace|home|opt|usr|var|etc)(?:/[^/\s]*)?(?:\s|$)",
        r"|chmod\s+(?:-\S*R\S*\s+|--recursive\s+)\S*\s+/",
        r"|chown\s+(?:-\S*R\S*\s+|--recursive\s+)\S*\s+/",
        r"|rm\s+-\S*[rR]\S*\s+\.\s*$",
        r"|mkfs\b|dd\s+.*of=/",
        r")",
    ))
    .expect("valid destructive-shell pattern")
});

const GIT_OPTIONS_WITH_VALUES: &[&str] = &[
    "-C",
    "-c",
    "--config-env",
    "--exec-path",
    "--git-dir",
    "--namespace",
    "--super-prefix",
    "--work-tree",
];
const GIT_FLAG_OPTIONS: &[&str] = &[
    "--bare",
    "--glob-pathspecs",
    "--icase-pathspecs",
    "--literal-pathspecs",
    "--no-pager",
    "--no-replace-objects",
    "--noglob-pathspecs",
    "--paginate",
    "-P",
    "-p",
];
const BLOCKED_GIT_SUBCOMMANDS: &[&str] = &[
    "add",
    "am",
    "branch",
    "checkout",
    "checkout-index",
    "cherry-pick",
    "commit",
    "merge",
    "mv",
    "notes",
    "prune",
    "read-tree",
    "rebase",
    "replace",
    "reset",
    "restore",
    "revert",
    "rm",
    "stash",
    "submodule",
    "switch",
    "tag",
    "update-index",
    "update-ref",
    "worktree",
];
/// git clean short flags that may legitimately appear alongside `-n` (Python
/// `_GIT_CLEAN_SHORT_FLAGS = frozenset("ndfxXqi")`).
const GIT_CLEAN_SHORT_FLAGS: &[char] = &['n', 'd', 'f', 'x', 'X', 'q', 'i'];

/// `args.command` then `args.cmd`; `None` if missing or blank.
fn shell_command(raw_input: &JsonObject) -> Option<&str> {
    let command = raw_input
        .get("command")
        .or_else(|| raw_input.get("cmd"))
        .and_then(Value::as_str)?;
    if command.trim().is_empty() {
        None
    } else {
        Some(command)
    }
}

/// `args.deferred_goal_for_next_iteration`, trimmed; `None` if missing or blank.
/// One source of the wire key and the nonblank rule for every hook that reads
/// it (the bailout discriminator and the nested-planner depth gate).
fn deferred_goal(raw_input: &JsonObject) -> Option<&str> {
    raw_input
        .get("deferred_goal_for_next_iteration")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|goal| !goal.is_empty())
}

/// Faithful port of `_split_git_args`. Python prefers `shlex.split` (falling
/// back to `str.split`); this uses whitespace splitting (the fallback path) —
/// quote handling is best-effort and the prehook is explicitly not the
/// authoritative isolation boundary (see [`DESTRUCTIVE_GIT_MESSAGE`]).
fn split_git_args(raw_args: &str) -> Vec<&str> {
    raw_args.split_whitespace().collect()
}

/// Port of `_git_subcommand`: skip global options, stop at `--`, return the
/// first bare token (lowercased) plus the remaining args.
fn git_subcommand<'a>(args: &[&'a str]) -> Option<(String, Vec<&'a str>)> {
    let mut idx = 0;
    while idx < args.len() {
        let arg = args[idx];
        if arg == "--" {
            return None;
        }
        if GIT_OPTIONS_WITH_VALUES.contains(&arg) {
            idx += 2;
            continue;
        }
        if GIT_OPTIONS_WITH_VALUES
            .iter()
            .any(|opt| arg.starts_with(&format!("{opt}=")))
        {
            idx += 1;
            continue;
        }
        if GIT_FLAG_OPTIONS.contains(&arg) {
            idx += 1;
            continue;
        }
        if arg.starts_with('-') {
            idx += 1;
            continue;
        }
        return Some((arg.to_lowercase(), args[idx + 1..].to_vec()));
    }
    None
}

/// Port of `_clean_args_are_dry_run`.
fn clean_args_are_dry_run(args: &[&str]) -> bool {
    for arg in args {
        if *arg == "--" {
            break;
        }
        if *arg == "--dry-run" {
            return true;
        }
        if arg.starts_with("--") {
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            let chars: Vec<char> = arg[1..].chars().collect();
            let has_n = chars.contains(&'n');
            let all_known = chars.iter().all(|c| GIT_CLEAN_SHORT_FLAGS.contains(c));
            if has_n && all_known {
                return true;
            }
        }
    }
    false
}

/// Port of `_git_apply_is_read_only`.
fn git_apply_is_read_only(args: &[&str]) -> bool {
    args.contains(&"--check") && !args.contains(&"--cached") && !args.contains(&"--index")
}

/// Port of `_has_git_mutation_command`.
fn has_git_mutation_command(command: &str) -> bool {
    for caps in GIT_COMMAND_PATTERN.captures_iter(command) {
        let raw_args = caps.get(1).map_or("", |m| m.as_str());
        let Some((subcommand, args)) = git_subcommand(&split_git_args(raw_args)) else {
            continue;
        };
        match subcommand.as_str() {
            "clean" => {
                if !clean_args_are_dry_run(&args) {
                    return true;
                }
            }
            "apply" => {
                if !git_apply_is_read_only(&args) {
                    return true;
                }
            }
            other if BLOCKED_GIT_SUBCOMMANDS.contains(&other) => return true,
            _ => {}
        }
    }
    false
}

fn run_destructive_git(raw_input: &JsonObject) -> HookOutcome {
    match shell_command(raw_input) {
        Some(command) if has_git_mutation_command(command) => {
            HookOutcome::Deny(HookDenial::new(DESTRUCTIVE_GIT_MESSAGE, "destructive_git"))
        }
        _ => HookOutcome::pass(),
    }
}

fn run_destructive_shell(raw_input: &JsonObject) -> HookOutcome {
    match shell_command(raw_input) {
        Some(command) if DESTRUCTIVE_SHELL_PATTERN.is_match(command) => HookOutcome::Deny(
            HookDenial::new(DESTRUCTIVE_SHELL_MESSAGE, "destructive_shell"),
        ),
        _ => HookOutcome::pass(),
    }
}

// ---------------------------------------------------------------------------
// State-dependent hooks (read downstream state via ports / the transport).
// ---------------------------------------------------------------------------

const BLOCK_IN_ISOLATED_MESSAGE: &str = "BLOCKED: ask_advisor is unavailable inside an isolated workspace; call exit_isolated_workspace first, then ask_advisor and submit your terminal.";

/// `BlockInIsolatedMode`: fail-OPEN on any daemon RPC error (Python parity).
async fn run_block_in_isolated_mode(ctx: &ExecutionMetadata) -> Result<HookOutcome, ToolError> {
    if ctx.is_isolated_workspace_mode {
        Ok(HookOutcome::Deny(
            HookDenial::new(BLOCK_IN_ISOLATED_MESSAGE, "block_in_isolated_mode")
                .with_reason("isolated_workspace_open"),
        ))
    } else {
        Ok(HookOutcome::pass())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd_input(command: &str) -> JsonObject {
        let mut obj = JsonObject::new();
        obj.insert("cmd".to_owned(), Value::String(command.to_owned()));
        obj
    }

    #[test]
    fn destructive_git_blocks_mutations_and_passes_reads() {
        for blocked in [
            "git reset --hard",
            "git commit -m x",
            "ls; git checkout main",
            "git -c user.name=x rebase main",
            "git clean -fd",
            "git apply --cached patch",
        ] {
            assert!(has_git_mutation_command(blocked), "should block: {blocked}");
        }
        for allowed in [
            "git status",
            "git log --oneline",
            "git diff",
            "git clean -nfd",
            "git clean --dry-run",
            "git apply --check patch",
            "echo git reset", // 'git' is mid-token after `echo`, not at a `;&|`/start boundary
        ] {
            assert!(
                !has_git_mutation_command(allowed),
                "should allow: {allowed}"
            );
        }
    }

    #[test]
    fn destructive_shell_blocks_and_passes() {
        assert!(DESTRUCTIVE_SHELL_PATTERN.is_match("rm -rf /testbed"));
        assert!(DESTRUCTIVE_SHELL_PATTERN.is_match("mv /workspace /tmp"));
        assert!(DESTRUCTIVE_SHELL_PATTERN.is_match("mkfs.ext4 /dev/sda"));
        assert!(!DESTRUCTIVE_SHELL_PATTERN.is_match("rm -rf ./build"));
        assert!(!DESTRUCTIVE_SHELL_PATTERN.is_match("ls -la /testbed"));
    }

    #[test]
    fn shell_command_prefers_command_then_cmd() {
        let mut obj = JsonObject::new();
        obj.insert("command".to_owned(), Value::String("echo hi".to_owned()));
        obj.insert("cmd".to_owned(), Value::String("rm -rf /".to_owned()));
        assert_eq!(shell_command(&obj), Some("echo hi"));
        assert_eq!(shell_command(&cmd_input("  ")), None);
        assert_eq!(shell_command(&JsonObject::new()), None);
    }
}
