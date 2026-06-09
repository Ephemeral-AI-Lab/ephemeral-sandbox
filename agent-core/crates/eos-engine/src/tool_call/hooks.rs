//! Engine-owned hook execution policy.

use std::collections::HashSet;
use std::sync::LazyLock;

use eos_tool::{ExecutionMetadata, Hook, ToolError, ToolResult};
use eos_types::{BackgroundSessionCounts, JsonObject, WorkflowId};
use regex::Regex;
use serde_json::{json, Value};

use crate::agent_loop::ToolCallHookStores;
use crate::background::BackgroundManagers;

mod advisor_approval;
mod disallow_nested_planner_deferral;
mod require_no_background_sessions;

/// The outcome of running one hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HookOutcome {
    /// The hook passed; execution proceeds.
    Pass(JsonObject),
    /// The hook denied execution; the pipeline returns an in-band error.
    Deny(HookDenial),
}

impl HookOutcome {
    fn pass() -> Self {
        HookOutcome::Pass(JsonObject::new())
    }
}

/// A hook denial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HookDenial {
    pub(super) message: String,
    pub(super) policy: &'static str,
    pub(super) reason: Option<String>,
    pub(super) extra: JsonObject,
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

/// Engine-owned state used by tool-call pre-hooks.
#[derive(Clone, Debug)]
pub(crate) struct ToolCallHooks {
    background: BackgroundManagers,
    dependencies: ToolCallHookStores,
}

impl ToolCallHooks {
    pub(crate) fn new(background: &BackgroundManagers, dependencies: ToolCallHookStores) -> Self {
        Self {
            background: background.clone(),
            dependencies,
        }
    }

    pub(super) async fn background_counts(&self) -> Result<BackgroundSessionCounts, ToolError> {
        Ok(self.background.count().await)
    }

    pub(super) async fn cancel_all_subagents(&self, reason: &str) -> Result<(), ToolError> {
        self.background.cancel_all_subagents(reason).await;
        Ok(())
    }

    pub(super) async fn workflow_depth_for_call(
        &self,
        ctx: &ExecutionMetadata,
    ) -> Result<Option<u32>, ToolError> {
        let Some(workflow_id) = self.workflow_id_for_call(ctx).await? else {
            return Ok(None);
        };
        self.workflow_depth(&workflow_id).await.map(Some)
    }

    async fn workflow_id_for_call(
        &self,
        ctx: &ExecutionMetadata,
    ) -> Result<Option<WorkflowId>, ToolError> {
        if let Some(workflow_id) = &ctx.workflow_id {
            return Ok(Some(workflow_id.clone()));
        }
        let Some(agent_run_id) = &ctx.agent_run_id else {
            return Ok(None);
        };
        let Some(run) = self.dependencies.agent_run_store.get(agent_run_id).await? else {
            return Ok(None);
        };
        let Some(task_id) = run.task_id else {
            return Ok(None);
        };
        let Some(task) = self.dependencies.task_store.get(&task_id).await? else {
            return Ok(None);
        };
        Ok(task.workflow_id)
    }

    async fn workflow_depth(&self, workflow_id: &WorkflowId) -> Result<u32, ToolError> {
        let mut depth = 1;
        let mut current = workflow_id.clone();
        let mut seen = HashSet::new();
        while seen.insert(current.clone()) {
            let Some(workflow) = self.dependencies.workflow_store.get(&current).await? else {
                break;
            };
            let Some(parent) = self
                .dependencies
                .task_store
                .get(&workflow.parent_task_id)
                .await?
            else {
                break;
            };
            let Some(parent_workflow_id) = parent.workflow_id else {
                break;
            };
            depth += 1;
            current = parent_workflow_id;
        }
        Ok(depth)
    }
}

pub(super) async fn run_hook(
    hook: Hook,
    raw_input: &JsonObject,
    ctx: &ExecutionMetadata,
    hooks: Option<&ToolCallHooks>,
) -> Result<HookOutcome, ToolError> {
    match hook {
        Hook::DestructiveGitShell { .. } => Ok(run_destructive_git(raw_input)),
        Hook::DestructiveShell { .. } => Ok(run_destructive_shell(raw_input)),
        Hook::BlockInIsolatedMode { .. } => run_block_in_isolated_mode(ctx).await,
        Hook::RequireNoBackgroundSessions { tool } => {
            let hooks = hooks.ok_or_else(|| {
                ToolError::Internal("tool-call hook dependencies not initialized".to_owned())
            })?;
            require_no_background_sessions::run_require_no_background_sessions(tool, hooks).await
        }
        Hook::AdvisorApproval { tool } => advisor_approval::run_advisor_approval(tool, ctx).await,
        Hook::DisallowNestedPlannerDeferral { max_depth, .. } => {
            let hooks = hooks.ok_or_else(|| {
                ToolError::Internal("tool-call hook dependencies not initialized".to_owned())
            })?;
            disallow_nested_planner_deferral::run_disallow_nested_planner_deferral(
                max_depth, raw_input, ctx, hooks,
            )
            .await
        }
        _ => Err(ToolError::Internal("unsupported hook variant".to_owned())),
    }
}

#[must_use]
pub(super) fn hook_failure_result(
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
    for (key, value) in &denial.extra {
        metadata.insert(key.clone(), value.clone());
    }
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
const GIT_CLEAN_SHORT_FLAGS: &[char] = &['n', 'd', 'f', 'x', 'X', 'q', 'i'];

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

fn deferred_goal(raw_input: &JsonObject) -> Option<&str> {
    raw_input
        .get("deferred_goal_for_next_iteration")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|goal| !goal.is_empty())
}

fn split_git_args(raw_args: &str) -> Vec<&str> {
    raw_args.split_whitespace().collect()
}

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

fn git_apply_is_read_only(args: &[&str]) -> bool {
    args.contains(&"--check") && !args.contains(&"--cached") && !args.contains(&"--index")
}

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

const BLOCK_IN_ISOLATED_MESSAGE: &str = "BLOCKED: ask_advisor is unavailable inside an isolated workspace; call exit_isolated_workspace first, then ask_advisor and submit your terminal.";

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
