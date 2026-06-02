# impl-eos-tools — typed tool names, specs, registry, execution pipeline & dispatch policy

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §7.

## 1. Purpose & Responsibility (SRP)

`eos-tools` owns the **tool model surface**: the typed `ToolName` set for every
public tool, the `ToolIntent` classification, the `ToolError` framework-fault
enum, the `ToolExecutor` trait, the `ToolRegistry`, the colocated `ToolSpec`
sources (one per model-facing tool), the terminal-descriptor catalog, the inner
**execution pipeline** (parse → pre-hooks → execute → validate output →
stamp-terminal-on-success), and the **pure batch-dispatch decision functions**
(terminal-batch rejection, lifecycle-batch policy).

This crate must **NOT**:
- Depend on `eos-engine` or `eos-workflow` (it is upstream of both per the
  Ownership Map). Where a tool needs downstream state (orchestrator submission,
  background supervisor, advisor approval, notifications), it depends on a
  **narrow port trait defined here** and implemented downstream, injected at the
  composition root.
- Own the async query/dispatch loop, the `QueryContext` wrapper, the background
  task supervisor, `StreamEvent` emission, phase buffers, or `ToolResultBlock`
  (those are `eos-engine`). It owns the *decisions*; the engine owns the *loop*.
- Own `ToolSpec` itself (that is `eos-llm-client`, anchor §5a) or the orchestrator
  submission state machine (that is `eos-workflow`).
- Re-emit engine stream events inside the pipeline. `execute_tool_once` returns a
  `ToolResult`; the engine emits start/complete events around it.

## 2. Dependencies

- **Upstream (depends on):** `eos-types` (IDs, `UtcDateTime`, `JsonObject`,
  `CoreError`), `eos-llm-client` (`ToolSpec`; the §5a edge), `eos-state`
  (`TaskStore` + per-entity store traits + submission/terminal DTOs),
  `eos-sandbox-api` (`SandboxCaller`, request/result DTOs, `SandboxTransport`,
  `Intent` → `ToolIntent` boundary conversion — see §6.2), `eos-skills` (`SkillRegistry` for
  `load_skill_reference`), `eos-audit` (`AuditSink` for tool-call audit wrapper).
- **Downstream (used by):** `eos-engine` (registry, pipeline, batch predicates,
  `ToolExecutor`), `eos-workflow` (registers orchestrator-coupled submission
  executors against the ports defined here).
- **External crates** (pinned via `[workspace.dependencies]` inheritance,
  `proj-workspace-deps`):

| Crate | Justification | rust-skills |
|---|---|---|
| `serde` (derive) | `Serialize`/`Deserialize` on every tool Input/Output DTO | anchor §9 |
| `schemars` | `JsonSchema` derive → `ToolSpec.input_schema`/`output_schema` (anchor §10; no hand-written schemas) | anchor §10 |
| `serde_json` | render tool-output payloads, parse raw model input `JsonObject` | anchor §10 |
| `thiserror` | the single `ToolError` enum | `err-thiserror-lib`, `err-custom-type` |
| `async-trait` | `ToolExecutor` + the port traits are used behind `dyn` in the registry | anchor §6, `async-tokio-runtime` |
| `futures` | only if a tool needs stream combinators; otherwise omit (YAGNI) | anchor §7 |

No `tokio` dependency in the crate body: executors are `async fn (&self, …)`;
the runtime is provided by `eos-runtime` (anchor §7, runtime-agnostic lower
crates).

## 3. Scope & Source Mapping

| Python source | Rust target | What moves / what is dropped |
|---|---|---|
| `_framework/core/base.py` (`BaseTool`) | `executor.rs` (`trait ToolExecutor`), `spec.rs` (`ToolSpec` author site) | `BaseTool`'s data fields split: model-facing (`name`/`description`/schemas) → colocated `ToolSpec`; behavior (`execute`) → `ToolExecutor`; classification (`intent`/`is_terminal_tool`/`task_type`) → per-tool registry metadata. `to_api_schema` → `eos-llm-client` `ToolSpec` (already owned there). `context_requirements`/`ToolContextRequirement` (plan §7 core schema) is an **engine-dispatch concern** — it drives `context_preparers` attachment during runtime assembly, so it is **relocated to `eos-engine`** with `context_preparers` (§6.4) and is **not modeled here**. |
| `_framework/core/decorator.py` (`@tool`) | **dropped** | No runtime decorator. Each tool is a concrete `ToolExecutor` impl + a `const`/`include_str!` description; schemas are derived. `intent`-required-at-import becomes intent on the registry entry (compile-time, not import-time). |
| `_framework/core/registry.py` (`ToolRegistry`) | `registry.rs` | `register`/`register_many`/`get`/`list_tools`/`remove_tools`/`restrict_to_tools` → `register`/`get`/`list`/`remove`/`restrict`; `to_api_schema` → `specs() -> Vec<ToolSpec>`. |
| `_framework/core/results.py` (`ToolResult`, `TextToolOutput`, `ToolInputParseResult`) | `result.rs` | `ToolResult{output,is_error,metadata,is_terminal}`; `TextToolOutput` → a marker for plain-text output tools; `ToolInputParseResult` → `Result<ParsedInput, ToolResult>` (parse-don't-validate, `api-parse-dont-validate`). |
| `_framework/core/runtime.py` (`ExecutionMetadata`) | `metadata.rs` | Typed struct only. **Drop** the `Mapping`-emulation shim (`get`/`__getitem__`/`__iter__`/`extras`/`_TYPED_FIELDS`) — migration scaffolding. IDs become newtypes; downstream services become injected port-trait objects. |
| `_framework/core/validation.py` | `execution.rs` | `parse_tool_input`, `execute_tool_body`, `validate_tool_output` → pipeline stages. The `background`-not-an-arg special case → kept as a parse-time error. |
| `_framework/execution/tool_call.py` (`execute_tool_once`) | `execution.rs` | Inner pipeline only (parse→hooks→exec→validate→stamp). `execute_tool_call(_streaming)`, budget counting, `ToolResultBlock`, `StreamEvent` emission, phase buffer, trace → **`eos-engine`** (reference). |
| `_framework/execution/hook_pipeline.py` | `hooks.rs` | A **sealed, closed** hook set (`api-sealed-trait`) — not an open trait. See §6.5 + GC-tools-06. |
| `_framework/factory.py` | `registry.rs` builder + `eos-runtime` wiring | Process-global `_factories`/`_builtins_registered` → explicit `ToolRegistry` construction at the composition root. `override`/`has_tool`/`list_available_tools` collapse into registry methods. |
| `_names.py` | `name.rs` (`ToolName`) | The literals; **plus** the names `_names.py` omits (see §6.1, GC-tools-04). |
| `_terminals/registry.py` (`TerminalToolDescriptor`, `TERMINAL_DESCRIPTORS`, `render_terminal_catalog`) | `terminal.rs` | Descriptors keyed by an exhaustive `TerminalTool` enum (`type-enum-states`); totality is compile-time. |
| `sandbox/*` tools | `model_tools/sandbox/` | Input/Output DTOs + `ToolExecutor` impls over `SandboxTransport` (eos-sandbox-api). |
| `submission/*` tools | `model_tools/submission/` (DTOs/specs here) + executors split (see §8/GC-tools-01) | Input/Output DTOs, `ToolName`, intent, terminal flag, descriptors owned here. Orchestrator-coupled executors register against a port (eos-workflow implements). `submit_root_outcome` is the clean case (pure `TaskStore`). |
| `workflow/*` tools | `model_tools/workflow/` | `delegate_workflow`/`check_workflow_status`/`cancel_workflow` DTOs here; executors call a `WorkflowControlPort`. |
| `subagent/*` tools | `model_tools/subagent/` | `run_subagent`/`check_subagent_progress`/`cancel_subagent` DTOs + `SubagentSupervisorPort`. |
| `ask_helper/ask_advisor` | `model_tools/ask_advisor/` | `AskAdvisorInput` DTO + `AdvisorPort`. |
| `skills/load_skill_reference.py` | `model_tools/skills/` | DTO + executor over `SkillRegistry` (eos-skills). |
| `isolated_workspace/*` | `model_tools/isolated_workspace/` | `enter_isolated_workspace`/`exit_isolated_workspace` DTOs + lifecycle port. |

**Out of scope / dropped:** the unregistered granular submission dirs
(`submit_plan_closes_goal`, `submit_plan_defers_goal`, `submit_reduction_success`,
`submit_reduction_failure`) — `make_submission_tools()` does not return them; they
are treated as dead code, no `ToolName`/spec is minted (GC-tools-04 note). Plugin
tool discovery → `eos-plugin-catalog`. The async dispatch loop, background
dispatch, phase buffers, trace → `eos-engine`.

## 4. File & Module Layout

```
eos-tools/
  src/
    lib.rs            // pub use re-exports (proj-pub-use-reexport)
    name.rs           // ToolName enum (every public tool) + TerminalTool subset
    intent.rs         // ToolIntent enum
    error.rs          // ToolError (thiserror)
    result.rs         // ToolResult, ParsedInput, output-shape markers
    metadata.rs       // ExecutionMetadata (typed struct, port-trait fields)
    spec.rs           // colocated-spec helpers; each tool authors its ToolSpec via a free `fn spec() -> ToolSpec` (§6.7)
    executor.rs       // ToolExecutor trait (+ object-safety note), RegisteredTool
    registry.rs       // ToolRegistry: register/get/list/remove/restrict/specs
    execution.rs      // execute_tool_once pipeline (parse→hooks→exec→validate→stamp)
    hooks.rs          // sealed Hook set + HookOutcome
    dispatch.rs       // pure predicates: terminal-batch + lifecycle-batch policy
    terminal.rs       // TerminalTool enum, TerminalDescriptor, render_terminal_catalog
    ports.rs          // narrow seam traits implemented downstream (see §5.6)
    model_tools/
      mod.rs
      sandbox/ ...     // read/write/edit/multi_edit/exec_command/write_stdin/grep/glob
      submission/ ...  // root/generator/reducer/planner/advisor/explorer DTOs+specs
      workflow/ ...    // delegate/check/cancel
      subagent/ ...    // run/check/cancel
      ask_advisor/ ...
      skills/ ...      // load_skill_reference
      isolated_workspace/ ... // enter/exit
    descriptions/      // *.md include_str! for long model-facing text
```

`lib.rs` re-exports `ToolName`, `ToolIntent`, `ToolError`, `ToolResult`,
`ToolExecutor`, `ToolRegistry`, `ExecutionMetadata`, `TerminalTool`, the port
traits, and the pipeline/predicate entry points. Tool internals are
`pub(crate)`; only the registry-building free functions are public
(`proj-pub-crate-internal`).

## 5. Contracts Owned Here

Per anchor §5, this crate owns `ToolName`, `ToolIntent`, `ToolError`,
`ToolExecutor`, `ToolRegistry`, terminal descriptors, and the execution/dispatch
policy. `ToolSpec` is **referenced** from `eos-llm-client` (anchor §5a); IDs,
`UtcDateTime`, `JsonObject`, `CoreError` from `eos-types`; `Intent`/`SandboxCaller`
DTOs from `eos-sandbox-api`; submission/terminal DTOs + `TaskStore` from `eos-state`.

### 5.1 `ToolName` — see §6.1 (typed enum over every public tool).
### 5.2 `ToolIntent` — see §6.2.
### 5.3 `ToolError` — see §6.4 / §8.

### 5.4 `ToolExecutor` (object-safe, `#[async_trait]`)

```rust
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Execute against already-parsed, hook-validated input.
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError>;
}
```

Used behind `dyn` in the registry (heterogeneous tool storage) → `#[async_trait]`
(native async-fn-in-trait is not yet `dyn`-safe, anchor §6). A `RegisteredTool`
bundles the executor with its static metadata:

```rust
pub struct RegisteredTool {
    pub name: ToolName,
    pub intent: ToolIntent,
    pub is_terminal: bool,
    pub spec: ToolSpec,                 // from eos-llm-client (§5a)
    pub hooks: Vec<Hook>,               // sealed set (§6.5)
    executor: Arc<dyn ToolExecutor>,
}
```

### 5.5 `ToolRegistry`

```rust
impl ToolRegistry {
    pub fn register(&mut self, tool: RegisteredTool);
    pub fn get(&self, name: ToolName) -> Option<&RegisteredTool>;
    pub fn list(&self) -> impl Iterator<Item = &RegisteredTool>;
    pub fn remove(&mut self, names: &[ToolName]);
    pub fn restrict(&mut self, names: &[ToolName]);
    pub fn specs(&self) -> Vec<ToolSpec>;   // replaces to_api_schema
}
```

Keyed by `ToolName` (not `String`) — `type-newtype-validated`, OCP via
registration (anchor §6, no `match` in dispatch). Insertion order preserved (an
`IndexMap` or `Vec` + lookup map) to keep `specs()` deterministic for the Phase-0
schema-parity snapshot.

### 5.6 Port traits (the seam to downstream state) — `ports.rs`

These satisfy DIP for tools that need engine/workflow state without a backward
DAG edge. They are **owned by `eos-tools`** and implemented downstream; they are
recorded as sanctioned seams on the anchor §6 SOLID Seam Map and as
eos-tools-owned port edges in `overview.md` §4 (the same §5a amendment mechanism
used for the `eos-tools → eos-llm-client` edge), so they are allowed abstractions
rather than self-justified ones. Each has exactly one wired implementor (ISP
ports, not speculation). All `#[async_trait]`, sealed (`api-sealed-trait`) so only
agent-core crates implement them:

| Port | Methods (sketch) | Implementor (edge to eos-tools) | Replaces (Python) |
|---|---|---|---|
| `WorkflowControlPort` | `start(parent_task_id: TaskId, agent_id: String, goal: String) -> StartedWorkflow { workflow_id, workflow_task_id }`, `status(WorkflowId, Option<WorkflowSessionId>)`, `cancel(WorkflowSessionId, reason)`, `find_outstanding(parent_task_id, agent_id)` | eos-workflow + eos-engine workflow-handle adapter | `delegate`/`check`/`cancel_workflow` runtime |
| `PlanSubmissionPort` | `apply_plan(PlannerSubmission)`, `apply_reducer(ReducerSubmission)`, `submit_generator(status, outcome)` | eos-workflow `AttemptOrchestrator` | submission `orchestrator.apply_*` |
| `SubagentSupervisorPort` | `spawn(agent, prompt) -> SubagentSessionId`, `progress(SubagentSessionId, n)`, `cancel(SubagentSessionId, reason)`, `set_progress_provider(...)` | eos-engine background supervisor | `run_subagent`/control |
| `AdvisorPort` | `review(tool_name, payload) -> Verdict` | eos-engine helper-agent runner | `ask_advisor` / `AdvisorApprovalPreHook` |
| `IsolatedWorkspacePort` | `enter(agent_id, sandbox_id, req)`, `exit(agent_id, sandbox_id, req)`; adapter enforces no in-flight ephemeral jobs/command sessions before enter and cancels/drains per-agent background work before exit | eos-runtime adapter over eos-sandbox-host lifecycle + eos-engine background state | enter/exit_isolated_workspace |
| `NotificationSink` | `notify_system(event)` | eos-engine notification service | `SystemNotificationService` |

`TaskStore` (used by `submit_root_outcome`) is **not** a new port — it is the
existing per-entity store trait owned by `eos-state` (referenced).

## 6. Types, Fields & Schemas

### 6.1 `ToolName` (name.rs) — `#[non_exhaustive]`, `type-no-stringly`

Authoritative set derived from the six registration sites + the two subagent
control tools (NOT from `_names.py`, which omits four — GC-tools-04). Variants
map to wire strings via `serde(rename)` / a `Display`/`FromStr` table:

| Variant | Wire string | Source |
|---|---|---|
| `ReadFile` | `read_file` | sandbox |
| `WriteFile` | `write_file` | sandbox |
| `EditFile` | `edit_file` | sandbox |
| `MultiEdit` | `multi_edit` | sandbox |
| `ExecCommand` | `exec_command` | sandbox |
| `WriteStdin` | `write_stdin` | sandbox (omitted from `_names.py`) |
| `Grep` | `grep` | sandbox |
| `Glob` | `glob` | sandbox |
| `EnterIsolatedWorkspace` | `enter_isolated_workspace` | isolated_workspace (omitted) |
| `ExitIsolatedWorkspace` | `exit_isolated_workspace` | isolated_workspace (omitted) |
| `RunSubagent` | `run_subagent` | subagent |
| `CheckSubagentProgress` | `check_subagent_progress` | subagent control |
| `CancelSubagent` | `cancel_subagent` | subagent control |
| `AskAdvisor` | `ask_advisor` | ask_helper |
| `DelegateWorkflow` | `delegate_workflow` | workflow |
| `CheckWorkflowStatus` | `check_workflow_status` | workflow |
| `CancelWorkflow` | `cancel_workflow` | workflow |
| `LoadSkillReference` | `load_skill_reference` | skills (omitted) |
| `SubmitRootOutcome` | `submit_root_outcome` | submission (terminal) |
| `SubmitGeneratorOutcome` | `submit_generator_outcome` | submission (terminal) |
| `SubmitReducerOutcome` | `submit_reducer_outcome` | submission (terminal) |
| `SubmitPlannerOutcome` | `submit_planner_outcome` | submission (terminal) |
| `SubmitAdvisorFeedback` | `submit_advisor_feedback` | submission (terminal) |
| `SubmitExplorationResult` | `submit_exploration_result` | submission (terminal) |

### 6.2 `ToolIntent` (intent.rs) — `#[non_exhaustive]`, `type-enum-states`

| Variant | Wire | Python source |
|---|---|---|
| `ReadOnly` | `read_only` | `Intent.READ_ONLY` |
| `WriteAllowed` | `write_allowed` | `Intent.WRITE_ALLOWED` |
| `Lifecycle` | `lifecycle` | `Intent.LIFECYCLE` |

`Intent` is owned by `eos-sandbox-api` (the foreground sandbox-call intent). The
*tool-classification* `ToolIntent` here happens to share the same three values.
**Decision:** `ToolIntent` is an eos-tools-owned enum (anchor §5 assigns the name
to this crate) defined in `intent.rs` with `ReadOnly|WriteAllowed|Lifecycle`; the
sandbox boundary converts via `From<Intent>`/`Into<Intent>` rather than aliasing
another crate's type — keeping the owned contract local and avoiding an unrecorded
cross-crate ownership inversion. Documented so the lifecycle-batch predicate
(§6.6) and sandbox routing agree. Per-tool intent:
read/grep/glob/check_workflow_status/ask_advisor/load_skill_reference/all
`submit_*` = `ReadOnly`; write/edit/multi_edit/exec_command/write_stdin/run_subagent
= `WriteAllowed`; delegate_workflow/cancel_workflow/enter+exit_isolated_workspace =
`Lifecycle`. (Confirmed against each `@tool(intent=...)`.)

### 6.3 `ToolResult` (result.rs) — derive `Debug, Clone, PartialEq`

| Field | Rust type | serde/schemars | Source |
|---|---|---|---|
| `output` | `String` | — | `ToolResult.output` |
| `is_error` | `bool` | default `false` | `ToolResult.is_error` |
| `metadata` | `JsonObject` | default empty | `ToolResult.metadata` |
| `is_terminal` | `bool` | default `false`; set only by the stamp stage | `ToolResult.is_terminal` |

`metadata` stays `JsonObject` (transitional, anchor §4) because hooks/audit stamp
heterogeneous keys (`submission_kind`, `hook_trace`, `command_session_id`).

### 6.4 `ExecutionMetadata` (metadata.rs) — typed struct, no Mapping shim

Drops the Python `extras`/`_TYPED_FIELDS`/`get`/`__getitem__` emulation. IDs are
newtypes from `eos-types`; downstream services are port-trait objects (§5.6):

| Field | Rust type | Source (Python) |
|---|---|---|
| `sandbox_id` | `Option<SandboxId>` | `sandbox_id: str` |
| `agent_run_id` | `Option<AgentRunId>` | `agent_run_id` |
| `agent_name` | `String` | `agent_name` |
| `cwd` / `repo_root` / `exec_cwd` | `String` | same |
| `request_id` | `Option<RequestId>` | `request_id` |
| `task_id` | `Option<TaskId>` | `task_id` |
| `attempt_id` | `Option<AttemptId>` | `attempt_id` |
| `workflow_id` | `Option<WorkflowId>` | `workflow_id` |
| `tool_use_id` | `Option<ToolUseId>` | `tool_use_id` |
| `sandbox_invocation_id` | `Option<InvocationId>` | `sandbox_invocation_id` |
| `caller` | `SandboxCaller` | derived (`sandbox_caller_from_tool_context`) |
| `transport` | `Arc<dyn SandboxTransport>` | derived (the `sandbox_api.*` call surface) |
| `task_store` | `Arc<dyn TaskStore>` | Python extras key (promoted to typed field; read via `context.get("task_store")`) |
| `workflow_control` | `Option<Arc<dyn WorkflowControlPort>>` | workflow runtime |
| `plan_submission` | `Option<Arc<dyn PlanSubmissionPort>>` | `orchestrator` |
| `subagent_supervisor` | `Option<Arc<dyn SubagentSupervisorPort>>` | `background_task_manager` |
| `advisor` | `Option<Arc<dyn AdvisorPort>>` | helper composer |
| `isolated_workspace` | `Option<Arc<dyn IsolatedWorkspacePort>>` | lifecycle |
| `notifications` | `Option<Arc<dyn NotificationSink>>` | `system_notification_service` |
| `skill_registry` | `Arc<SkillRegistry>` | factory-closure dependency injected per agent (`make_load_skill_reference(*, skill_registry, …)`), not an extras key; pairs with the `IsolatedWorkspacePort`/`SkillRegistry` injection story |

`runtime_config`/`composer`/`attempt_runtime`/`conversation_messages`/
`context_preparers`/`on_progress_line`/`background_task_id` are replaced by the
typed ports or moved to `eos-engine` dispatch context (they are engine plumbing,
not tool-facing). The Python `tool_registry` field is **intentionally dropped**:
it exists only for skills that introspect/call sibling tools, which is out of
scope (no in-scope Rust tool needs the broader tool surface).

### 6.5 Hooks (hooks.rs) — sealed closed set, NOT an open trait

The Python hook pipeline is an open extension abstraction not on the seam map
(anchor §6). Resolution (GC-tools-06): a **sealed enum** of the known hooks, each
carrying the data it needs; the pipeline matches exhaustively. The six wired
Python hook classes map **one-to-one** to six variants —
`DestructiveGitShellPreHook` and `DestructiveShellPreHook` are distinct (different
match logic, message, and `policy` metadata: `destructive_git` vs
`destructive_shell`), so each is its own variant rather than folded. All six are
**pre-phase** (every wired Python hook is a `pre_hook`; no post_hook variant
exists), so the enum carries no pre/post discriminator and the pipeline runs them
only before execute (§8.1 drops the unexercised post-hook stage).

```rust
#[non_exhaustive]
pub enum Hook {
    RequireNoInflightBackgroundTasks { tool: ToolName },
    AdvisorApproval { tool: ToolName },
    DisallowNestedPlannerDeferral { tool: ToolName },
    DestructiveGitShell { tool: ToolName },     // git working-tree/metadata mutations
    DestructiveShell { tool: ToolName },        // destructive filesystem commands
    BlockInIsolatedMode { tool: ToolName },     // reject while caller is in isolated workspace
}

pub enum HookOutcome { Pass(JsonObject), Deny { reason: String } }
```

Hooks needing downstream state (`AdvisorApproval` → `AdvisorPort`,
`RequireNoInflight*` → `SubagentSupervisorPort`, `BlockInIsolatedMode` →
`IsolatedWorkspacePort`, wired today as `ask_advisor`'s pre-hook) read it from
`ExecutionMetadata`.
A `Deny` becomes an in-band `ToolResult{is_error:true}` carrying the
`hook_failure` metadata shape the Python pipeline emits.

### 6.6 Representative DTOs (model_tools)

All derive `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`; schemas
feed `ToolSpec.input_schema`. Real field names from source:

- `ExecCommandInput { cmd: String(min 1), yield_time_ms: u32 (0..=30_000, default 1000), timeout: Option<u32>(>=1), max_output_tokens: Option<u32>(>=1) }`
- `WriteStdinInput { command_session_id: CommandSessionId, chars: String(default ""), yield_time_ms: u32, max_output_tokens: Option<u32> }`
- `ReadFileInput { file_path: String, start_line: u32 (>=1, default 1), end_line: u32 (>=1, default 200=MAX_READ_FILE_LINES) }` — when `end_line` is omitted a before-validator fills it as `start_line + 199` (the auto-window); after-validator enforces the ≤200-line invariant. Both defaults + the auto-window must be in the schemars schema for the crate-owned Phase-4 parity snapshot (AC-tools-08).
- `WriteFileInput { file_path: String, content: String }`
- `EditFileInput { file_path, old_text, new_text, replace_all: bool, description }`
- `MultiEditInput { file_path, edits: Vec<MultiEditOp{old_text,new_text,replace_all}>, description }`
- `GrepInput { pattern, path: Option, glob_filter: Option, output_mode: GrepMode(Content|FilesWithMatches|Count), head_limit, offset, case_insensitive, line_numbers, multiline }`
- `GlobInput { pattern, path: Option<String> }`
- `CommandToolOutput { status, exit_code: Option<i32>, output: BTreeMap<String,String>, command_session_id: Option<String>, stdout, stderr, changed_paths: Vec<String>, changed_path_kinds: BTreeMap<String,String>, mutation_source, conflict_reason: Option<String>, error: Option<JsonObject> }`
- Submission inputs:
  - `SubmitRootOutcomeInput { status: SubmissionStatus(Success|Failed), outcome: String(min 1, nonblank) }`
  - `SubmitGeneratorOutcomeInput`, `SubmitReducerOutcomeInput` — identical `{status, outcome}`.
  - `SubmitPlannerOutcomeInput { tasks: Vec<PlanTaskInput{id, agent_name, needs: Vec<String>}>(min 1), task_specs: BTreeMap<String,String>(min 1), reducers: Vec<ReducerInput{id, needs, prompt}>(min 1), deferred_goal_for_next_iteration: Option<String>(nonblank when present) }`
  - `SubmitAdvisorFeedbackInput { verdict: Verdict(Approve|Reject), summary: String(min 1) }`
  - `SubmitExplorationResultInput { summary: String(min 1), findings: Vec<String>, references: Vec<String> }`
- Workflow inputs:
  - `DelegateWorkflowInput { goal: String(min 1, nonblank) }`
  - `CheckWorkflowStatusInput { workflow_id: WorkflowId, workflow_task_id: Option<WorkflowSessionId> }`
  - `CancelWorkflowInput { workflow_task_id: WorkflowSessionId, reason: String(default "") }`
- `RunSubagentInput { agent_name: String, prompt: String(min 1) }` — the registered tool is the **restricted** variant (`RestrictedRunSubagentTool`): `agent_name` carries a runtime-scoped `enum` of the caller's dispatchable subagents (`json_schema_extra={"enum": allowed_list}`), so the emitted spec's input schema is **patched per caller at spec-build time** with that enum (and validated against it). The crate-owned Phase-4 parity snapshot (AC-tools-08) must reflect the patched-enum schema, not a bare `agent_name: String`.
- `CheckSubagentProgressInput { subagent_session_id: SubagentSessionId, last_n_messages: u8 (1..=10, default 5) }`
- `CancelSubagentInput { subagent_session_id: SubagentSessionId, reason: String }`
- `AskAdvisorInput { tool_name: String, tool_payload: JsonObject }`
- `LoadSkillReferenceInput { skill_name: String, reference_name: String }`
- `EnterIsolatedWorkspaceInput { layer_stack_root: String(default "") }`, `ExitIsolatedWorkspaceInput { … }`

### 6.7 Colocated spec source (anchor §10)

Each model-facing tool exposes **one** description source — `const DESCRIPTION:
&str` or `include_str!("../descriptions/<tool>.md")` for long text — plus the
schemars-derived input/output schemas. No docstring fallback, no separate
prompt-file + inline mix (GC-tools-02):

```rust
// model_tools/sandbox/exec_command.rs
const DESCRIPTION: &str = "Run a managed sandbox command session.";
fn spec() -> ToolSpec {
    ToolSpec::new(ToolName::ExecCommand.as_str(), DESCRIPTION)
        .with_input_schema(schema_for!(ExecCommandInput))
        .with_output_schema(schema_for!(CommandToolOutput))
}
```

### 6.8 `TerminalTool` + descriptors (terminal.rs)

```rust
#[non_exhaustive]
pub enum TerminalTool { Root, Generator, Reducer, Planner, AdvisorFeedback, ExplorationResult }

pub struct TerminalDescriptor { pub name: ToolName, pub selection_guidance: &'static str, pub advisor_review_focus: &'static str }

pub fn descriptor(t: TerminalTool) -> TerminalDescriptor { match t { /* exhaustive */ } }
```

Totality is a compile-time exhaustive `match` over `TerminalTool` (the §1
"flexibility only at seams" win via `type-enum-states`). The Python registry has
only 4 of 6 (advisor + exploration descriptors are missing and rely on the
`render_terminal_catalog` fallback). Resolution (GC-tools-03): the Rust domain is
**all six** `is_terminal_tool=True` tools; advisor + exploration descriptors are
authored so the fallback branch disappears.

## 7. Concurrency & State Ownership

- **Runtime-agnostic.** No `tokio` runtime created here; executors are `async
  fn(&self, …)` driven by `eos-runtime`'s multi-thread runtime (anchor §7).
- **`ToolRegistry`** is built once at composition and shared **immutable** as
  `Arc<ToolRegistry>` (`own-arc-shared`); `restrict`/`remove` happen during
  per-agent construction *before* sharing, so no interior mutability is needed at
  dispatch time. The engine clones the `Arc` per query.
- **Ports** are `Arc<dyn _Port>` in `ExecutionMetadata`, cloned cheaply; the
  metadata struct is built per tool call and owned by the call (no shared mutation).
- **No locks held across `.await`** (`async-no-lock-await`): the pipeline takes
  `&self`/owned input and awaits the executor without holding any guard. Ports
  that wrap shared mutable downstream state own their own synchronization; tools
  call them and await.
- **Dispatch predicates** (`dispatch.rs`) are **pure synchronous functions** over
  `&[ToolCall]` + registry lookups — no async, no shared state. The async loop
  that consumes their decisions lives in `eos-engine`.
- **`spawn_blocking`**: none needed here (no CPU-bound work; redaction/hashing
  live in `eos-audit`).

## 8. Behavior & Invariants

1. **Execution pipeline order** (port of `execute_tool_once`):
   parse input → run pre-hooks (first `Deny`/error short-circuits to in-band
   error) → execute → validate output against the output shape →
   **stamp `is_terminal = true` iff the tool is terminal AND `!is_error`**. This
   stamping is the single source of the loop's `TOOL_STOP` signal (anchor §3,
   plan §7). The Python pipeline has an open post-hook stage, but every wired hook
   is a pre_hook (no post_hook variant exists), so the post-hook stage is dropped
   (YAGNI); all six `Hook` enum members (§6.5) are pre-phase.
2. **`ToolError` vs in-band failure** (advisor #checklist): tool-domain failures
   (bad arguments, hook deny, tool said "no") are **in-band** `ToolResult{is_error:
   true}` returned as `Ok`. `ToolError` (`Result::Err`) is reserved for framework
   faults — unknown tool, internal validation crash, missing required port. Do not
   collapse the two; the engine renders in-band errors back to the model and
   surfaces `Err` to triage. **This Err/in-band boundary is a deliberate change
   from Python**, where `execute_tool_body` and the internal-validation branch
   (validation.py) return those failures *in-band* as `ToolResult(is_error=True)`
   rather than raising; the ported ACs (AC-tools-02..04) encode the new Rust
   boundary, not the Python one.
3. **`background` is not a tool argument**: parse rejects any input carrying a
   `background` key when the tool schema lacks it, with the
   "use typed subagent or command-session controls" message (ported verbatim).
4. **Terminal-batch rejection** (pure predicate, `dispatch::reject_terminal_batch`):
   if `len(calls) > 1` and any call is a terminal tool → reject **every** call in
   the batch with the "must be called alone … No tool in this batch executed"
   message; nothing executes. (Engine consumes this; the *test* of the loop is
   `eos-engine`'s `test_tool_batch.py`, but the predicate + its unit test live here.)
5. **Lifecycle-batch policy** (`dispatch::lifecycle_batch_decision`):
   `>1` lifecycle in batch → reject all lifecycle calls, **siblings still
   dispatch**; `=1` lifecycle + ≥1 sibling → reject siblings, the lifecycle call
   **executes solo**. Divergence from the terminal precedent is deliberate (plan
   §7 / isolated-workspace invariant: later calls observe new routing state).
6. **exec_command / command-session naming** (GC-tools-07): the tool is
   `exec_command`; its output carries `command_session_id`; the follow-up tool is
   `write_stdin` over a command session. Rust keeps these model-facing names.
   The daemon wire labels are owned by `eos-sandbox-api` and are
   `api.v1.exec_command` / `api.v1.exec_stdin`; the deleted `shell` op is not a
   tool-name concern here.
7. **submit_root_outcome** validates: task exists, belongs to request,
   `workflow_id is None`, `role == root`; sets task status + finishes request via
   `TaskStore`. Pure store path → no orchestrator port needed.

## 9. SOLID & Principles Applied

- **DIP:** tools depend on `ToolSpec` (neutral, eos-llm-client §5a) and on the
  §5.6 port traits, never on `eos-engine`/`eos-workflow` concretes. The
  composition root injects implementors.
- **OCP:** new tools are *registered* `RegisteredTool`s; the registry and
  pipeline never grow a `match` on tool name. The only exhaustive matches are on
  the closed `TerminalTool`/`Hook` enums (intentional totality).
- **ISP:** the six ports are small and single-purpose (no god "engine" trait);
  `ToolExecutor` is one method. Each port has exactly **one** wired implementor.
  They are sanctioned abstractions: added to the anchor §6 SOLID Seam Map and
  recorded as eos-tools-owned port edges in `overview.md` §4 (the §5a amendment
  mechanism), so they are the named cross-crate seam under anchor §1, not
  speculative flexibility.
- **LSP:** all executors are substitutable behind `dyn ToolExecutor`; mocks
  implement the same ports for tests (`test-mock-traits`).
- **SRP:** this crate models tools and decides batch policy; it does not run the
  loop, mutate workflow state, or supervise background tasks.
- **KISS/YAGNI/DRY:** drop the decorator, the Mapping shim, the open hook trait,
  and the dead granular submission dirs; `ToolIntent` stays an eos-tools-owned
  enum (anchor §5) with a `From`/`Into` conversion to `eos-sandbox-api::Intent`
  rather than inverting ownership by aliasing the sandbox type.
- **Non-goals respected:** no tool-visibility enum (visibility = presence in the
  request `Vec<ToolSpec>`); no deferred/lazy tool loading (specs built at spawn);
  no global orchestrator (workflow control is a port to per-Attempt machinery).

## 10. Gap Closeouts (tracked requirements)

- **GC-tools-01** — *Orchestrator-coupled submission executors must not create a
  backward DAG edge.* Resolution: `eos-tools` owns the submission **Input/Output
  DTOs, `ToolName`, intent, terminal flag, and descriptors**; planner/reducer/
  generator executors call `PlanSubmissionPort` (`ports.rs`), implemented by
  `eos-workflow`. `submit_root_outcome` uses the `eos-state` `TaskStore` directly.
- **GC-tools-02** — *One colocated spec source per model-facing tool.* Resolution:
  each tool module defines `const DESCRIPTION`/`include_str!` + schemars schemas;
  no docstring/prompt-file/inline mix. A test asserts every `RegisteredTool.spec`
  description is non-empty and not derived from a doc comment.
- **GC-tools-03** — *Compile/test coverage that every terminal tool has a
  descriptor.* Resolution: descriptors are an exhaustive `match` over the
  `TerminalTool` enum (compile-time totality); a test asserts each terminal
  `ToolName` maps to a `TerminalTool` and that both descriptor fields are
  non-empty. Adds the missing advisor + exploration descriptors.
- **GC-tools-04** — *Typed constant for every public tool name incl. omitted
  ones.* Resolution: `ToolName` enumerates all 24 tools (§6.1), explicitly adding
  `write_stdin`, `enter_isolated_workspace`, `exit_isolated_workspace`,
  `load_skill_reference` that `_names.py` lacks. The unregistered granular
  submission dirs get **no** constant (dead code).
- **GC-tools-05** — *Wrapper/synthesized controls carry intent.* Resolution:
  `RegisteredTool.intent` is mandatory (non-`Option`); `check_subagent_progress`/
  `cancel_subagent` (Python `BaseTool` subclasses lacking an explicit `@tool`
  intent) get explicit intents (`ReadOnly`/`WriteAllowed`); a test asserts every
  registered tool has an intent.
- **GC-tools-06** — *Hooks are not an open abstraction.* Resolution: the `Hook`
  closed enum (§6.5) replaces the open pre/post-hook trait pipeline; the wired
  hooks are exhaustive variants matched at execution time:
  `RequireNoInflightBackgroundTasks`, `AdvisorApproval`,
  `DisallowNestedPlannerDeferral`, `DestructiveGitShell`, `DestructiveShell`, and
  `BlockInIsolatedMode` (the live `ask_advisor` pre-hook).
- **GC-tools-07** — *Consistent `exec_command`/command-session naming.*
  Resolution: tool names `exec_command` + `write_stdin`; output field
  `command_session_id`; daemon wire labels `api.v1.exec_command` and
  `api.v1.exec_stdin` stay in `eos-sandbox-api`.
- **GC-tools-08** — *Update stale subagent prompt referencing retired wait/check
  controls.* Resolution: `subagent/run_subagent/prompt.py:71` ("so `check`/`wait`
  can …") references a retired generic `wait` control; the Rust
  `descriptions/run_subagent.md` drops the `wait` mention and names only the live
  typed controls `check_subagent_progress` / `cancel_subagent`.
- **GC-tools-09** — *Docstrings do not become model-facing text.* Resolution: Rust
  `///` doc comments are developer-only; model-facing text is the explicit
  `DESCRIPTION` const (anchor §10).

## 11. Acceptance Criteria

Write each test first (TDD, anchor §11); confirm it fails before implementing.

- **AC-tools-01** — *Terminal stamping.* `execute_tool_once` over a terminal tool
  with a successful result stamps `is_terminal=true`; on `is_error` it does not.
  Test: `execution::tests::stamps_terminal_on_success`. Ports
  `test_tool_execution.py`.
- **AC-tools-02** — *Pipeline order + hook short-circuit.* A pre-hook `Deny`
  yields an in-band `ToolResult{is_error:true}` with the `hook_failure` shape and
  the executor never runs. Test: `execution::tests::pre_hook_deny_short_circuits`.
- **AC-tools-03** — *Parse rejects stray `background`.* Test:
  `execution::tests::rejects_background_arg`. Ports the validation special case.
- **AC-tools-04** — *Output validation.* A plain-text tool returning valid output
  passes; a structured-output tool returning non-matching JSON yields an in-band
  error with `output_validation_error` metadata. Test:
  `execution::tests::validates_output_shape`.
- **AC-tools-05** — *Terminal-batch rejection.* `reject_terminal_batch` returns
  rejections for all calls when a batch mixes a terminal with siblings; returns
  `None` for a solo terminal call. Test: `dispatch::tests::terminal_batch_rejected`.
  Pairs with eos-engine `test_tool_batch.py`.
- **AC-tools-06** — *Lifecycle-batch policy.* `>1` lifecycle rejects all lifecycle,
  keeps siblings; `1` lifecycle + siblings rejects siblings, keeps the lifecycle
  call. Test: `dispatch::tests::lifecycle_batch_decision`. Pairs with eos-engine
  `test_tool_call_dispatch_lifecycle.py`.
- **AC-tools-07** — *Terminal-descriptor totality.* Every `TerminalTool` has
  non-empty `selection_guidance` + `advisor_review_focus`; every terminal
  `ToolName` maps to a `TerminalTool`. Test: `terminal::tests::descriptors_total`.
  Ports `test_descriptor_registry.py`.
- **AC-tools-08** — *Schema summary / spec parity.* `registry.specs()` produces a
  stable, ordered `Vec<ToolSpec>` matching the crate-owned Phase-4 schema snapshot for the
  full default tool set. The `run_subagent` spec reproduces the restricted schema:
  `agent_name` carries the caller-scoped enum of dispatchable subagents (schema
  patched per caller at spec-build time), so the snapshot fixture is built for a
  fixed caller allow-list. Test: `registry::tests::specs_snapshot`. Ports
  `test_schema_summary.py`.
- **AC-tools-09** — *Every tool has a ToolName + intent.* Test:
  `registry::tests::all_tools_named_and_intented` (no `String` keys, intent
  mandatory). Covers GC-tools-04/05.
- **AC-tools-10** — *Submission terminals by role.* root/generator/reducer reject
  blank `outcome`; root rejects non-root/foreign-request/workflow-bound tasks.
  Test: `model_tools::submission::tests::main_role_terminals`. Ports
  `test_submission_main_role_terminals.py`.
- **AC-tools-11** — *exec_command / write_stdin.* `exec_command` registers a
  command session on `command_session_id`; `write_stdin` with `\x03` while running
  triggers a cancel. Tests: `model_tools::sandbox::tests::{exec_command_session,
  write_stdin_ctrl_c}`. Ports `test_exec_command.py`, `test_write_stdin.py`.
- **AC-tools-12** — *Planner DAG submission shape.* `SubmitPlannerOutcomeInput`
  validates duplicate ids, missing/extra `task_specs`, and `deferred_goal`
  nonblank-when-present; the executor calls `PlanSubmissionPort` (mock asserts the
  ordered generator/reducer ids). Test: `model_tools::submission::tests::planner_dag`.

## 12. Implementation Checklist

1. `name.rs` `ToolName` (+ wire map) and `intent.rs` `ToolIntent` → AC-tools-09.
2. `result.rs` (`ToolResult`, `ParsedInput`) and `error.rs` (`ToolError`).
3. `metadata.rs` typed `ExecutionMetadata` (IDs + port-trait fields).
4. `ports.rs` six sealed `#[async_trait]` port traits → unblock executors.
5. `executor.rs` (`ToolExecutor`, `RegisteredTool`) + `registry.rs`
   (`register/get/list/remove/restrict/specs`) → AC-tools-08/09.
6. `hooks.rs` sealed `Hook` set → GC-tools-06.
7. `execution.rs` pipeline (parse→hooks→exec→validate→stamp) → AC-tools-01..04.
8. `dispatch.rs` pure predicates → AC-tools-05/06.
9. `terminal.rs` enum + exhaustive descriptors → AC-tools-03/07.
10. `model_tools/sandbox/*` over `SandboxTransport` → AC-tools-11.
11. `model_tools/submission/*` DTOs/specs + executors (root via `TaskStore`,
    others via `PlanSubmissionPort`) → AC-tools-10/12.
12. `model_tools/{workflow,subagent,ask_advisor,skills,isolated_workspace}/*`.
13. `descriptions/*.md` incl. corrected `run_subagent.md` → GC-tools-08.
14. Crate-owned Phase-4 schema snapshot fixture + spec-parity test.

---
**On completion:** update the Progress Tracker in `./overview.md` for row
`eos-tools` per spec-conventions.md §13. Do not edit other crates' rows.
