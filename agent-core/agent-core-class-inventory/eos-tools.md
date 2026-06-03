# Crate `eos-tools` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-tools/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**92 types across 19 files.**

The `eos-tools` crate owns the tool *model surface*: the typed `ToolName` set, the `ToolIntent` read/write/lifecycle classification, the `ToolError` framework-fault enum, the object-safe `ToolExecutor` seam plus its `RegisteredTool` bundle and the insertion-ordered `ToolRegistry`, the sealed pre-execution `Hook` set, the colocated per-tool `ToolSpec` sources (one Input/Output DTO pair and executor per model-facing tool, under `model_tools/`), the `TerminalTool` descriptor catalog, the inner `execute_tool_once` pipeline (parse → pre-hooks → execute → validate output → stamp-terminal-on-success), and the pure batch-dispatch decision functions (`reject_terminal_batch`, `lifecycle_batch_decision`). It owns the *decisions*; `eos-engine` owns the async query/dispatch *loop*, the background supervisor, stream events, and `ToolResultBlock`. Tools needing downstream state depend on the six narrow, sealed **port traits** in `ports` (`WorkflowControlPort`, `PlanSubmissionPort`, `SubagentSupervisorPort`, `AdvisorPort`, `IsolatedWorkspacePort`, `NotificationSink`), defined here and implemented downstream so `eos-tools` stays upstream of `eos-engine`/`eos-workflow`/`eos-runtime` in the workspace DAG. It depends on `eos-types`, `eos-sandbox-api`, `eos-state`, `eos-skills`, and `eos-llm-client` (for `ToolSpec`).

## Contents

- **`eos-tools/src/lib.rs`** — _(re-export facade; no types declared)_
- **`eos-tools/src/dispatch.rs`** — `DispatchCall`, `BatchRejection`, `LifecycleBatchDecision`
- **`eos-tools/src/error.rs`** — `ToolError`
- **`eos-tools/src/executor.rs`** — `ToolExecutor`, `RegisteredTool`
- **`eos-tools/src/hooks.rs`** — `Hook`, `HookOutcome`, `HookDenial`
- **`eos-tools/src/intent.rs`** — `ToolIntent`
- **`eos-tools/src/metadata.rs`** — `ExecutionMetadata`
- **`eos-tools/src/model_tools/advisor.rs`** — `AskAdvisorInput`, `AskAdvisor`
- **`eos-tools/src/model_tools/isolated.rs`** — `EnterIsolatedWorkspaceInput`, `ExitIsolatedWorkspaceInput`, `EnterIsolatedWorkspace`, `ExitIsolatedWorkspace`
- **`eos-tools/src/model_tools/mod.rs`** — `CallerScope`
- **`eos-tools/src/model_tools/sandbox.rs`** — `ReadFileOutput`, `MutationOutput`, `GrepOutput`, `GlobOutput`, `CommandToolOutput`, `ReadFileInput`, `ReadFile`, `WriteFileInput`, `WriteFile`, `EditFileInput`, `EditFile`, `MultiEditOp`, `MultiEditInput`, `MultiEdit`, `GrepMode`, `GrepInput`, `Grep`, `GlobInput`, `Glob`, `ExecCommandInput`, `ExecCommand`, `WriteStdinInput`, `WriteStdin`
- **`eos-tools/src/model_tools/skills.rs`** — `LoadSkillReferenceInput`, `LoadSkillReference`
- **`eos-tools/src/model_tools/subagent.rs`** — `RunSubagentInput`, `CheckSubagentProgressInput`, `CancelSubagentInput`, `RunSubagent`, `CheckSubagentProgress`, `CancelSubagent`
- **`eos-tools/src/model_tools/submission.rs`** — `SubmissionStatus`, `Verdict`, `SubmitRootOutcomeInput`, `SubmitRootOutcome`, `OutcomeInput`, `SubmitGeneratorOutcome`, `SubmitReducerOutcome`, `PlanTaskInput`, `ReducerInput`, `SubmitPlannerOutcomeInput`, `SubmitPlannerOutcome`, `SubmitAdvisorFeedbackInput`, `SubmitAdvisorFeedback`, `SubmitExplorationResultInput`, `SubmitExplorationResult`
- **`eos-tools/src/model_tools/workflow.rs`** — `DelegateWorkflowInput`, `CheckWorkflowStatusInput`, `CancelWorkflowInput`, `DelegateWorkflow`, `CheckWorkflowStatus`, `CancelWorkflow`
- **`eos-tools/src/name.rs`** — `ToolName`
- **`eos-tools/src/ports.rs`** — `Sealed`, `StartedWorkflow`, `OutstandingWorkflow`, `WorkflowControlPort`, `PlanTask`, `PlanReducer`, `PlannerPlan`, `SubmissionAck`, `PlanSubmissionPort`, `StartedSubagent`, `SubagentSupervisorPort`, `AdvisorApproval`, `AdvisorPort`, `IsolatedWorkspacePort`, `SystemNotification`, `NotificationSink`
- **`eos-tools/src/registry.rs`** — `ToolRegistry`
- **`eos-tools/src/result.rs`** — `ToolResult`, `OutputShape`
- **`eos-tools/src/terminal.rs`** — `TerminalTool`, `TerminalDescriptor`

---

## `eos-tools/src/dispatch.rs`

#### `DispatchCall<'a>`  ·  _struct_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq`  ·  [L18]

One call in a model-emitted tool-use batch; `name` is the raw wire string, possibly unknown to the registry.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `tool_use_id` | `&'a str` | `pub` |
| `name` | `&'a str` | `pub` |

#### `BatchRejection`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L27]

A rejection the engine renders back as an errored tool result.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `tool_use_id` | `String` | `pub` |
| `message` | `String` | `pub` |

#### `LifecycleBatchDecision`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L36]

The lifecycle-batch decision: which calls are rejected and which dispatch.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `rejected` | `Vec<BatchRejection>` | `pub` |
| `dispatched` | `Vec<String>` | `pub` |

---

## `eos-tools/src/error.rs`

#### `ToolError`  ·  _enum_  ·  derives: `Debug` (`thiserror::Error`)  ·  #[non_exhaustive]  ·  [L20]

A framework fault during tool execution; tool-domain failures are in-band `ToolResult`s, not variants here.

**Variants**:
- `UnknownTool(String)` — the dispatched tool name is not registered.
- `MissingContext(&'static str)` — a required execution-context value was absent.
- `MissingPort(&'static str)` — a required downstream-state port was not wired.
- `Store(CoreError)` — `#[from]` an upstream `Store` operation failed.
- `Sandbox(SandboxApiError)` — `#[from]` a sandbox transport / daemon RPC failed.
- `Internal(String)` — an internal invariant broke.

---

## `eos-tools/src/executor.rs`

#### `ToolExecutor`  ·  _trait_  ·  bases: `Send + Sync`  ·  async  ·  [L26]

The object-safe execute seam: run a tool body against already-parsed, hook-validated input (stored behind `dyn` in the registry).

**Trait items**:
- `async fn execute(&self, input: &JsonObject, ctx: &ExecutionMetadata) -> Result<ToolResult, ToolError>;`

#### `RegisteredTool`  ·  _struct_  ·  derives: `Clone`  ·  [L38]

An executor bundled with its static registry metadata; built once at composition and stored in the immutable `ToolRegistry`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `name` | `ToolName` | `pub` |
| `intent` | `ToolIntent` | `pub` |
| `is_terminal` | `bool` | `pub` |
| `spec` | `ToolSpec` | `pub` |
| `hooks` | `Vec<Hook>` | `pub` |
| `output` | `OutputShape` | `pub(crate)` |
| `executor` | `Arc<dyn ToolExecutor>` | `pub(crate)` |

**Trait impls**: `Debug`

<details><summary>Methods (4)</summary>

`new`, `with_hooks`, `output`, `executor`

</details>

---

## `eos-tools/src/hooks.rs`

#### `Hook`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq`  ·  #[non_exhaustive]  ·  [L30]

One wired pre-hook; a sealed, closed set matched exhaustively by `Hook::run` rather than an open trait pipeline. Every variant is a struct variant carrying `tool: ToolName` (the protected tool).

**Variants**:
- `RequireNoInflightBackgroundTasks { tool: ToolName }` — refuse a terminal/lifecycle tool while sandbox-bound background work is in flight.
- `AdvisorApproval { tool: ToolName }` — refuse a main-role terminal lacking prior advisor approval.
- `DisallowNestedPlannerDeferral { tool: ToolName }` — refuse a nested-workflow planner that sets a deferred goal.
- `DestructiveGitShell { tool: ToolName }` — refuse git working-tree / metadata mutation shell commands.
- `DestructiveShell { tool: ToolName }` — refuse destructive filesystem shell commands.
- `BlockInIsolatedMode { tool: ToolName }` — refuse a read-only helper (`ask_advisor`) while an isolated workspace is open.

<details><summary>Methods (3)</summary>

`tool`, `hook_name`, `run`

</details>

#### `HookOutcome`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L71]

The outcome of running one hook.

**Variants**: `Pass(JsonObject)`, `Deny(HookDenial)`

<details><summary>Methods (1)</summary>

`pass`

</details>

#### `HookDenial`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L90]

A hook denial: the model-facing message plus the audit/policy metadata the Python `HookResult.fail(...)` carries.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `message` | `String` | `pub` |
| `policy` | `&'static str` | `pub` |
| `reason` | `Option<String>` | `pub` |
| `extra` | `JsonObject` | `pub` |

<details><summary>Methods (3)</summary>

`new`, `with_reason`, `with_count`

</details>

---

## `eos-tools/src/intent.rs`

#### `ToolIntent`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema`  ·  #[serde(rename_all = "snake_case")] · #[non_exhaustive]  ·  [L20]

How a tool is classified for batch-dispatch policy and sandbox routing; locally owned, converting to/from `eos_sandbox_api::Intent`.

**Variants**: `ReadOnly`, `WriteAllowed`, `Lifecycle`

**Trait impls**: `From<Intent>`

<details><summary>Methods (1)</summary>

`as_str`

</details>

---

## `eos-tools/src/metadata.rs`

#### `ExecutionMetadata`  ·  _struct_  ·  derives: `Clone`  ·  [L38]

The typed bag of runtime context a tool executor reads; built per tool call and owned by the call (ports are cheaply-cloned `Arc<dyn _>`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `sandbox_id` | `Option<SandboxId>` | `pub` |
| `agent_run_id` | `Option<AgentRunId>` | `pub` |
| `agent_name` | `String` | `pub` |
| `cwd` | `String` | `pub` |
| `repo_root` | `String` | `pub` |
| `exec_cwd` | `String` | `pub` |
| `request_id` | `Option<RequestId>` | `pub` |
| `task_id` | `Option<TaskId>` | `pub` |
| `attempt_id` | `Option<AttemptId>` | `pub` |
| `workflow_id` | `Option<WorkflowId>` | `pub` |
| `tool_use_id` | `Option<ToolUseId>` | `pub` |
| `sandbox_invocation_id` | `Option<InvocationId>` | `pub` |
| `caller` | `SandboxCaller` | `pub` |
| `transport` | `Arc<dyn SandboxTransport>` | `pub` |
| `task_store` | `Arc<dyn TaskStore>` | `pub` |
| `request_store` | `Arc<dyn RequestStore>` | `pub` |
| `skill_registry` | `Arc<SkillRegistry>` | `pub` |
| `workflow_control` | `Option<Arc<dyn WorkflowControlPort>>` | `pub` |
| `plan_submission` | `Option<Arc<dyn PlanSubmissionPort>>` | `pub` |
| `subagent_supervisor` | `Option<Arc<dyn SubagentSupervisorPort>>` | `pub` |
| `advisor` | `Option<Arc<dyn AdvisorPort>>` | `pub` |
| `isolated_workspace` | `Option<Arc<dyn IsolatedWorkspacePort>>` | `pub` |
| `notifications` | `Option<Arc<dyn NotificationSink>>` | `pub` |

**Trait impls**: `Debug`

<details><summary>Methods (11)</summary>

`agent_id`, `sandbox_id_str`, `require_sandbox_id`, `require_task_id`, `require_request_id`, `require_attempt_id`, `require_workflow_control`, `require_plan_submission`, `require_subagent_supervisor`, `require_advisor`, `require_isolated_workspace`

</details>

---

## `eos-tools/src/model_tools/advisor.rs`

#### `AskAdvisorInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L23]

Input DTO for `ask_advisor`: the terminal tool the caller intends to call plus its arguments.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `tool_name` | `String` |  |
| `tool_payload` | `JsonObject` | `#[serde(default)]` |

#### `AskAdvisor`  ·  _struct_  ·  · private  ·  [L31]

Unit executor for `ask_advisor`: a blocking read-only advisor audit of a pending terminal submission via `AdvisorPort`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

---

## `eos-tools/src/model_tools/isolated.rs`

#### `EnterIsolatedWorkspaceInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L29]

Input DTO for `enter_isolated_workspace`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `layer_stack_root` | `String` | `#[serde(default)]` |

#### `ExitIsolatedWorkspaceInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L35]

Input DTO for `exit_isolated_workspace`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `grace_s` | `f64` | `#[serde(default = "default_grace_s")]` `#[schemars(default = "default_grace_s")]` |

#### `EnterIsolatedWorkspace`  ·  _struct_  ·  · private  ·  [L41]

Unit executor for `enter_isolated_workspace`: opens this agent's private isolated workspace via `IsolatedWorkspacePort`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `ExitIsolatedWorkspace`  ·  _struct_  ·  · private  ·  [L65]

Unit executor for `exit_isolated_workspace`: closes and discards this agent's isolated workspace via `IsolatedWorkspacePort`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

---

## `eos-tools/src/model_tools/mod.rs`

#### `CallerScope`  ·  _struct_  ·  derives: `Debug, Clone, Default`  ·  [L29]

The per-caller scope a tool registry is built for; carries the caller's dispatchable-subagent allow-list that patches the `run_subagent` input schema's `agent_name` enum.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `dispatchable_subagents` | `Vec<String>` | `pub` |

---

## `eos-tools/src/model_tools/sandbox.rs`

#### `ReadFileOutput`  ·  _struct_  ·  derives: `Debug, Serialize, Deserialize, JsonSchema`  ·  · private  ·  [L95]

Output DTO for `read_file`: the read window of file content plus its bounds.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `cwd` | `String` |  |
| `file_path` | `String` |  |
| `total_lines` | `u32` |  |
| `start_line` | `u32` |  |
| `end_line` | `u32` |  |
| `content` | `String` |  |

#### `MutationOutput`  ·  _struct_  ·  derives: `Debug, Serialize, Deserialize, JsonSchema`  ·  · private  ·  [L105]

Output DTO shared by `write_file`/`edit_file`/`multi_edit`: status, changed-path audit, and tool-specific extras flattened in.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `cwd` | `String` |  |
| `file_path` | `String` |  |
| `status` | `String` |  |
| `changed_paths` | `Vec<String>` |  |
| `changed_path_kinds` | `BTreeMap<String, String>` |  |
| `mutation_source` | `String` |  |
| `conflict_reason` | `Option<String>` |  |
| `error` | `JsonObject` |  |
| `extra` | `BTreeMap<String, Value>` | `#[serde(flatten)]` |

#### `GrepOutput`  ·  _struct_  ·  derives: `Debug, Serialize, Deserialize, JsonSchema`  ·  · private  ·  [L120]

Output DTO for `grep`: match content/filenames plus counts and applied-limit bookkeeping.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `cwd` | `String` |  |
| `pattern` | `String` |  |
| `mode` | `String` |  |
| `filenames` | `Vec<String>` |  |
| `content` | `String` |  |
| `num_files` | `u32` |  |
| `num_lines` | `u32` |  |
| `num_matches` | `u32` |  |
| `applied_limit` | `Option<u32>` |  |
| `applied_offset` | `u32` |  |
| `truncated` | `bool` |  |

#### `GlobOutput`  ·  _struct_  ·  derives: `Debug, Serialize, Deserialize, JsonSchema`  ·  · private  ·  [L135]

Output DTO for `glob`: matched filenames and truncation flag.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `cwd` | `String` |  |
| `pattern` | `String` |  |
| `filenames` | `Vec<String>` |  |
| `num_files` | `u32` |  |
| `truncated` | `bool` |  |

#### `CommandToolOutput`  ·  _struct_  ·  derives: `Debug, Serialize, Deserialize, JsonSchema`  ·  · private  ·  [L145]

Output DTO for `exec_command`/`write_stdin`: command status, streams, and changed-path audit (`command_session_tool.py`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `status` | `String` |  |
| `exit_code` | `Option<i32>` |  |
| `output` | `BTreeMap<String, String>` |  |
| `command_session_id` | `Option<String>` | `#[serde(skip_serializing_if = "Option::is_none")]` |
| `stdout` | `String` |  |
| `stderr` | `String` |  |
| `changed_paths` | `Vec<String>` |  |
| `changed_path_kinds` | `BTreeMap<String, String>` |  |
| `mutation_source` | `String` |  |
| `conflict_reason` | `Option<String>` |  |
| `error` | `Option<JsonObject>` |  |

#### `ReadFileInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L174]

Input DTO for `read_file`: target path and an optional line window.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `file_path` | `String` |  |
| `start_line` | `u32` | `#[serde(default = "default_one")]` `#[schemars(default = "default_one", range(min = 1))]` |
| `end_line` | `u32` | `#[serde(default = "default_max_read_lines")]` `#[schemars(default = "default_max_read_lines", range(min = 1))]` |

#### `ReadFile`  ·  _struct_  ·  · private  ·  [L184]

Unit executor for `read_file` over the `SandboxTransport`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `WriteFileInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L287]

Input DTO for `write_file`: target path and full content.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `file_path` | `String` |  |
| `content` | `String` |  |

#### `WriteFile`  ·  _struct_  ·  · private  ·  [L292]

Unit executor for `write_file` over the `SandboxTransport`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `EditFileInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L353]

Input DTO for `edit_file`: a single search/replace edit with optional `replace_all` and description.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `file_path` | `String` |  |
| `old_text` | `String` | `#[serde(default = "default_empty")]` |
| `new_text` | `String` | `#[serde(default = "default_empty")]` |
| `replace_all` | `bool` | `#[serde(default = "default_false")]` |
| `description` | `String` | `#[serde(default = "default_empty")]` |

#### `EditFile`  ·  _struct_  ·  · private  ·  [L365]

Unit executor for `edit_file` over the `SandboxTransport`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `MultiEditOp`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L420]

One search/replace operation within a `multi_edit` batch.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `old_text` | `String` |  |
| `new_text` | `String` | `#[serde(default = "default_empty")]` |
| `replace_all` | `bool` | `#[serde(default = "default_false")]` |

#### `MultiEditInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L429]

Input DTO for `multi_edit`: an ordered list of edits against one file.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `file_path` | `String` |  |
| `edits` | `Vec<MultiEditOp>` |  |
| `description` | `String` | `#[serde(default = "default_empty")]` |

#### `MultiEdit`  ·  _struct_  ·  · private  ·  [L436]

Unit executor for `multi_edit` over the `SandboxTransport`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `GrepMode`  ·  _enum_  ·  derives: `Debug, Clone, Copy, Deserialize, Serialize, JsonSchema`  ·  #[serde(rename_all = "snake_case")]  ·  · private  ·  [L540]

The `output_mode` literal for `grep`.

**Variants**: `Content`, `FilesWithMatches`, `Count`

<details><summary>Methods (1)</summary>

`as_wire`

</details>

#### `GrepInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L567]

Input DTO for `grep`: pattern plus path/glob scoping and output-mode/case/limit flags.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `pattern` | `String` |  |
| `path` | `Option<String>` | `#[serde(default)]` |
| `glob_filter` | `Option<String>` | `#[serde(default)]` |
| `output_mode` | `GrepMode` | `#[serde(default = "default_grep_mode")]` `#[schemars(default = "default_grep_mode")]` |
| `head_limit` | `u32` | `#[serde(default = "default_head_limit")]` `#[schemars(default = "default_head_limit")]` |
| `offset` | `u32` | `#[serde(default = "default_zero")]` |
| `case_insensitive` | `bool` | `#[serde(default = "default_false")]` |
| `line_numbers` | `bool` | `#[serde(default = "default_false")]` |
| `multiline` | `bool` | `#[serde(default = "default_false")]` |

#### `Grep`  ·  _struct_  ·  · private  ·  [L589]

Unit executor for `grep` over the `SandboxTransport`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `GlobInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L650]

Input DTO for `glob`: a pattern with an optional path scope.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `pattern` | `String` |  |
| `path` | `Option<String>` | `#[serde(default)]` |

#### `Glob`  ·  _struct_  ·  · private  ·  [L656]

Unit executor for `glob` over the `SandboxTransport`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `ExecCommandInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L731]

Input DTO for `exec_command`: the command plus yield/timeout/output-token bounds.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `cmd` | `String` |  |
| `yield_time_ms` | `u32` | `#[serde(default = "default_yield_ms")]` `#[schemars(default = "default_yield_ms", range(max = 30000))]` |
| `timeout` | `Option<u32>` | `#[serde(default)]` `#[schemars(range(min = 1))]` |
| `max_output_tokens` | `Option<u32>` | `#[serde(default)]` `#[schemars(range(min = 1))]` |

#### `ExecCommand`  ·  _struct_  ·  · private  ·  [L744]

Unit executor for `exec_command`: runs a command session over the `SandboxTransport`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `WriteStdinInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L799]

Input DTO for `write_stdin`: target command-session id plus chars and yield/output bounds.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `command_session_id` | `CommandSessionId` |  |
| `chars` | `String` | `#[serde(default = "default_chars")]` |
| `yield_time_ms` | `u32` | `#[serde(default = "default_yield_ms")]` `#[schemars(default = "default_yield_ms", range(max = 30000))]` |
| `max_output_tokens` | `Option<u32>` | `#[serde(default)]` `#[schemars(range(min = 1))]` |

#### `WriteStdin`  ·  _struct_  ·  · private  ·  [L811]

Unit executor for `write_stdin`: feeds stdin to a running command session over the `SandboxTransport`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

---

## `eos-tools/src/model_tools/skills.rs`

#### `LoadSkillReferenceInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L26]

Input DTO for `load_skill_reference`: the owning skill and the exact reference document name.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `skill_name` | `String` |  |
| `reference_name` | `String` |  |

#### `LoadSkillReference`  ·  _struct_  ·  · private  ·  [L33]

Unit executor for `load_skill_reference`: serves one named `references/*.md` doc from the per-agent `SkillRegistry`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

---

## `eos-tools/src/model_tools/subagent.rs`

#### `RunSubagentInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L39]

Input DTO for `run_subagent`: caller-scoped dispatchable agent name plus prompt.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `agent_name` | `String` |  |
| `prompt` | `String` |  |

#### `CheckSubagentProgressInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L46]

Input DTO for `check_subagent_progress`: session id and a 1–10 message count.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `subagent_session_id` | `SubagentSessionId` |  |
| `last_n_messages` | `u8` | `#[serde(default = "default_five")]` `#[schemars(default = "default_five", range(min = 1, max = 10))]` |

#### `CancelSubagentInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L55]

Input DTO for `cancel_subagent`: session id and optional cancel reason.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `subagent_session_id` | `SubagentSessionId` |  |
| `reason` | `String` | `#[serde(default)]` |

#### `RunSubagent`  ·  _struct_  ·  · private  ·  [L61]

Unit executor for `run_subagent`: spawns a dispatchable subagent session via `SubagentSupervisorPort`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `CheckSubagentProgress`  ·  _struct_  ·  · private  ·  [L108]

Unit executor for `check_subagent_progress`: renders the latest subagent messages/status via `SubagentSupervisorPort`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `CancelSubagent`  ·  _struct_  ·  · private  ·  [L149]

Unit executor for `cancel_subagent`: cancels a tracked subagent session via `SubagentSupervisorPort`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

---

## `eos-tools/src/model_tools/submission.rs`

#### `SubmissionStatus`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema`  ·  #[serde(rename_all = "snake_case")]  ·  · private  ·  [L37]

`Literal["success", "failed"]` submission status.

**Variants**: `Success`, `Failed`

<details><summary>Methods (2)</summary>

`as_str`, `outcome_status`

</details>

#### `Verdict`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema`  ·  #[serde(rename_all = "snake_case")]  ·  · private  ·  [L60]

`Literal["approve", "reject"]` advisor verdict.

**Variants**: `Approve`, `Reject`

<details><summary>Methods (1)</summary>

`as_str`

</details>

#### `SubmitRootOutcomeInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L92]

Input DTO for `submit_root_outcome`: terminal status and the user-facing outcome text.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `status` | `SubmissionStatus` |  |
| `outcome` | `String` |  |

#### `SubmitRootOutcome`  ·  _struct_  ·  · private  ·  [L97]

Unit executor for `submit_root_outcome`: the pure `TaskStore`/`RequestStore` terminal that finishes the root request.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `OutcomeInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L194]

Shared input DTO for `submit_generator_outcome`/`submit_reducer_outcome`: status plus outcome text.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `status` | `SubmissionStatus` |  |
| `outcome` | `String` |  |

#### `SubmitGeneratorOutcome`  ·  _struct_  ·  · private  ·  [L199]

Unit executor for `submit_generator_outcome`: records one generator task's terminal outcome via `PlanSubmissionPort`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `SubmitReducerOutcome`  ·  _struct_  ·  · private  ·  [L252]

Unit executor for `submit_reducer_outcome`: applies the reducer (attempt exit gate) via `PlanSubmissionPort`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `PlanTaskInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L317]

Input DTO for one planner generator task: id, bound agent, and `needs` edges.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `String` |  |
| `agent_name` | `String` |  |
| `needs` | `Vec<String>` | `#[serde(default)]` |

#### `ReducerInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L325]

Input DTO for one planner reducer task: id, `needs` edges, and prompt.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `String` |  |
| `needs` | `Vec<String>` | `#[serde(default)]` |
| `prompt` | `String` |  |

#### `SubmitPlannerOutcomeInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L333]

Input DTO for `submit_planner_outcome`: the generator/reducer DAG, task specs, and optional deferred goal.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `tasks` | `Vec<PlanTaskInput>` |  |
| `task_specs` | `BTreeMap<String, String>` |  |
| `reducers` | `Vec<ReducerInput>` |  |
| `deferred_goal_for_next_iteration` | `Option<String>` | `#[serde(default)]` |

#### `SubmitPlannerOutcome`  ·  _struct_  ·  · private  ·  [L341]

Unit executor for `submit_planner_outcome`: structurally validates the DAG then applies it via `PlanSubmissionPort`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `SubmitAdvisorFeedbackInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L506]

Input DTO for `submit_advisor_feedback`: the verdict and a summary.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `verdict` | `Verdict` |  |
| `summary` | `String` |  |

#### `SubmitAdvisorFeedback`  ·  _struct_  ·  · private  ·  [L511]

Unit executor for `submit_advisor_feedback`: the advisor helper terminal stamping verdict metadata.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `SubmitExplorationResultInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L536]

Input DTO for `submit_exploration_result`: summary plus supporting findings and references.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `summary` | `String` |  |
| `findings` | `Vec<String>` | `#[serde(default)]` |
| `references` | `Vec<String>` | `#[serde(default)]` |

#### `SubmitExplorationResult`  ·  _struct_  ·  · private  ·  [L544]

Unit executor for `submit_exploration_result`: the explorer subagent terminal stamping findings/references metadata.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

---

## `eos-tools/src/model_tools/workflow.rs`

#### `DelegateWorkflowInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L29]

Input DTO for `delegate_workflow`: the delegated workflow goal.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `goal` | `String` |  |

#### `CheckWorkflowStatusInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L34]

Input DTO for `check_workflow_status`: workflow id and optional background handle.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `workflow_id` | `WorkflowId` |  |
| `workflow_task_id` | `Option<WorkflowSessionId>` | `#[serde(default)]` |

#### `CancelWorkflowInput`  ·  _struct_  ·  derives: `Debug, Deserialize, Serialize, JsonSchema`  ·  · private  ·  [L41]

Input DTO for `cancel_workflow`: target background handle and optional reason.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `workflow_task_id` | `WorkflowSessionId` |  |
| `reason` | `String` | `#[serde(default)]` |

#### `DelegateWorkflow`  ·  _struct_  ·  · private  ·  [L47]

Unit executor for `delegate_workflow`: launches (or reports an already-outstanding) delegated workflow via `WorkflowControlPort`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `CheckWorkflowStatus`  ·  _struct_  ·  · private  ·  [L108]

Unit executor for `check_workflow_status`: renders delegated-workflow progress/outcomes via `WorkflowControlPort`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

#### `CancelWorkflow`  ·  _struct_  ·  · private  ·  [L154]

Unit executor for `cancel_workflow`: cancels an outstanding delegated workflow via `WorkflowControlPort`.

_Unit struct — no fields._

**Trait impls**: `ToolExecutor`

---

## `eos-tools/src/name.rs`

#### `ToolName`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema`  ·  #[serde(rename_all = "snake_case")] · #[non_exhaustive]  ·  [L24]

The typed name of every public model-facing tool (`type-no-stringly`); each variant maps to its `snake_case` wire string. The authoritative 24-tool set is the union of the six registration sites.

**Variants**: `ReadFile`, `WriteFile`, `EditFile`, `MultiEdit`, `ExecCommand`, `WriteStdin`, `Grep`, `Glob`, `EnterIsolatedWorkspace`, `ExitIsolatedWorkspace`, `RunSubagent`, `CheckSubagentProgress`, `CancelSubagent`, `AskAdvisor`, `DelegateWorkflow`, `CheckWorkflowStatus`, `CancelWorkflow`, `LoadSkillReference`, `SubmitRootOutcome`, `SubmitGeneratorOutcome`, `SubmitReducerOutcome`, `SubmitPlannerOutcome`, `SubmitAdvisorFeedback`, `SubmitExplorationResult`

**Trait impls**: `Display, FromStr`

<details><summary>Methods (2)</summary>

`as_str`, `from_wire`

</details>

---

## `eos-tools/src/ports.rs`

#### `Sealed`  ·  _trait_  ·  · #[doc(hidden)]  ·  [L32]

Friend-seal marker (`api-sealed-trait`) so only agent-core crates implement the port traits; mirrors `eos_state::Sealed`.

**Trait items**: _(empty marker trait)_

#### `StartedWorkflow`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L40]

A started delegated workflow handle returned by `WorkflowControlPort::start`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `workflow_id` | `WorkflowId` | `pub` |
| `workflow_task_id` | `WorkflowSessionId` | `pub` |

#### `OutstandingWorkflow`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L49]

One outstanding workflow launched by a parent task (for `find_outstanding`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `workflow_id` | `WorkflowId` | `pub` |
| `workflow_task_id` | `WorkflowSessionId` | `pub` |
| `workflow_goal` | `String` | `pub` |

#### `WorkflowControlPort`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L63]

Per-Attempt workflow control for the `delegate`/`check`/`cancel_workflow` tools; live state lives downstream, so `status`/`cancel` return rendered text.

**Trait items**:
- `async fn start(&self, parent_task_id: &TaskId, agent_id: &str, workflow_goal: &str) -> Result<StartedWorkflow, ToolError>;`
- `async fn status(&self, workflow_id: &WorkflowId, workflow_task_id: Option<&WorkflowSessionId>) -> Result<String, ToolError>;`
- `async fn cancel(&self, workflow_task_id: &WorkflowSessionId, reason: &str) -> Result<String, ToolError>;`
- `async fn find_outstanding(&self, parent_task_id: &TaskId, agent_id: &str) -> Result<Vec<OutstandingWorkflow>, ToolError>;`
- `async fn is_nested_workflow(&self, workflow_id: &WorkflowId) -> Result<bool, ToolError>;`

#### `PlanTask`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L106]

One planner-authored generator task: id, bound agent, and `needs` edges.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `String` | `pub` |
| `agent_name` | `String` | `pub` |
| `needs` | `Vec<String>` | `pub` |

#### `PlanReducer`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L117]

One planner-authored reducer task: id, `needs` edges, and prompt.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `String` | `pub` |
| `needs` | `Vec<String>` | `pub` |
| `prompt` | `String` | `pub` |

#### `PlannerPlan`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L136]

A validated planner DAG submission; richer than `eos_state::PlannerSubmission` so the orchestrator can create the not-yet-existent task rows.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `attempt_id` | `eos_types::AttemptId` | `pub` |
| `planner_task_id` | `TaskId` | `pub` |
| `kind` | `PlannerKind` | `pub` |
| `deferred_goal_for_next_iteration` | `Option<String>` | `pub` |
| `tasks` | `Vec<PlanTask>` | `pub` |
| `task_specs` | `BTreeMap<String, String>` | `pub` |
| `reducers` | `Vec<PlanReducer>` | `pub` |

#### `SubmissionAck`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L158]

The result of applying a terminal submission: accepted, or rejected with a model-facing message.

**Variants**: `Accepted`, `Rejected(String)`

#### `PlanSubmissionPort`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L168]

Per-Attempt submission application for the planner/generator/reducer terminal tools; implemented by the `eos-workflow` `AttemptOrchestrator`.

**Trait items**:
- `async fn apply_plan(&self, plan: PlannerPlan) -> Result<SubmissionAck, ToolError>;`
- `async fn submit_generator(&self, submission: GeneratorSubmission) -> Result<SubmissionAck, ToolError>;`
- `async fn apply_reducer(&self, submission: ReducerSubmission) -> Result<SubmissionAck, ToolError>;`

#### `StartedSubagent`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L193]

A started subagent handle returned by `SubagentSupervisorPort::spawn`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `subagent_session_id` | `SubagentSessionId` | `pub` |

#### `SubagentSupervisorPort`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L201]

The engine background supervisor for the subagent tools and the no-inflight-background-tasks hook; implemented by `eos-engine`.

**Trait items**:
- `async fn spawn(&self, agent_name: &str, prompt: &str) -> Result<StartedSubagent, ToolError>;`
- `async fn progress(&self, subagent_session_id: &SubagentSessionId, last_n_messages: u8) -> Result<String, ToolError>;`
- `async fn cancel(&self, subagent_session_id: &SubagentSessionId, reason: &str) -> Result<String, ToolError>;`
- `async fn background_inflight_count(&self, agent_id: &str) -> usize;`

#### `AdvisorApproval`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L235]

The result of checking prior advisor approval for a terminal (the `AdvisorApproval` pre-hook).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `approved` | `bool` | `pub` |
| `reason` | `Option<String>` | `pub` |

#### `AdvisorPort`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L244]

The advisor helper-agent runner; implemented by `eos-engine`.

**Trait items**:
- `async fn review(&self, tool_name: &str, tool_payload: &JsonObject) -> Result<String, ToolError>;`
- `async fn approval_status(&self, target_tool: &str) -> Result<AdvisorApproval, ToolError>;`

#### `IsolatedWorkspacePort`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L266]

The `eos-runtime` adapter over the `eos-sandbox-host` isolated-workspace lifecycle (enter/exit).

**Trait items**:
- `async fn enter(&self, agent_id: &str, sandbox_id: &SandboxId, layer_stack_root: &str) -> Result<String, ToolError>;`
- `async fn exit(&self, agent_id: &str, sandbox_id: &SandboxId, grace_s: f64) -> Result<String, ToolError>;`

#### `SystemNotification`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L291]

A system notification a tool/hook asks the engine to surface.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `event` | `String` | `pub` |
| `message` | `String` | `pub` |

#### `NotificationSink`  ·  _trait_  ·  bases: `Sealed + Send + Sync`  ·  async  ·  [L300]

The engine notification service; implemented by `eos-engine`.

**Trait items**:
- `async fn notify_system(&self, notification: SystemNotification) -> Result<(), ToolError>;`

---

## `eos-tools/src/registry.rs`

#### `ToolRegistry`  ·  _struct_  ·  derives: `Debug, Default`  ·  [L19]

An insertion-ordered, `ToolName`-keyed store of `RegisteredTool`s; built once at composition and shared immutably as `Arc<ToolRegistry>`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `tools` | `Vec<RegisteredTool>` |  |
| `index` | `HashMap<ToolName, usize>` |  |

<details><summary>Methods (11)</summary>

`new`, `register`, `register_many`, `get`, `list`, `remove`, `restrict`, `specs`, `len`, `is_empty`, `reindex`

</details>

---

## `eos-tools/src/result.rs`

#### `ToolResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L15]

The normalized in-band tool result; both success and tool-domain failure are values of this type (only framework faults are `Err(ToolError)`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `output` | `String` | `pub` |
| `is_error` | `bool` | `pub` |
| `metadata` | `JsonObject` | `pub` |
| `is_terminal` | `bool` | `pub` |

<details><summary>Methods (4)</summary>

`ok`, `error`, `with_metadata`, `meta`

</details>

#### `OutputShape`  ·  _enum_  ·  derives: `Clone`  ·  [L72]

The declared shape of a tool's successful output (Python `output_model`); carried on each `RegisteredTool` so the pipeline validates output without a per-tool match.

**Variants**:
- `Text` — plain text; any non-error output is valid.
- `Json { model_name: &'static str, validate: fn(&str) -> Result<(), String> }` — structured JSON that must deserialize into the named model.

**Trait impls**: `Debug`

<details><summary>Methods (1)</summary>

`json`

</details>

---

## `eos-tools/src/terminal.rs`

#### `TerminalTool`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord`  ·  #[non_exhaustive]  ·  [L17]

The closed set of terminal tools; every variant has a descriptor (compile-time totality).

**Variants**: `Root`, `Generator`, `Reducer`, `Planner`, `AdvisorFeedback`, `ExplorationResult`

<details><summary>Methods (2)</summary>

`tool_name`, `from_tool_name`

</details>

#### `TerminalDescriptor`  ·  _struct_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq`  ·  [L74]

A terminal tool's catalog entry (Python `TerminalToolDescriptor`): submitting name, selection guidance, and advisor review focus.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `name` | `ToolName` | `pub` |
| `selection_guidance` | `&'static str` | `pub` |
| `advisor_review_focus` | `&'static str` | `pub` |
