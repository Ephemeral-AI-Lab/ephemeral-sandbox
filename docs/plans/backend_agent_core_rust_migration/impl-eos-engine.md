# impl-eos-engine — agent query loop, tool dispatch, background supervision, notifications, prompt report

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §8.

## 1. Purpose & Responsibility (SRP)

`eos-engine` owns **one ephemeral agent run end-to-end**: it builds the provider
request from the transcript, consumes the model stream through the `EventSource`
seam, executes the assistant's tool batch with **deferred terminal enforcement**,
supervises engine-dispatched background work (subagents, command sessions,
delegated-workflow handles), evaluates declarative notification rules, and writes
the prompt-report JSONL. It assembles a concrete `QueryContext` (registries +
prompts) from an injected `AgentDefinition`.

This crate does **NOT**: resolve models or build API clients from the DB, provision
sandboxes, or run the workflow/Attempt lifecycle (those are `eos-runtime` /
`eos-db` / `eos-workflow`). It does **not** define the neutral `Message`,
`UsageSnapshot`, `LlmStreamEvent`, `ToolSpec`, `LlmClient` (owned by
`eos-llm-client`), the `ToolExecutor`/`ToolRegistry`/`ToolName`/`ToolIntent`/
`ToolResult` (owned by `eos-tools`), `AuditEvent`/`AuditNode`/`AuditSink` (owned
by `eos-audit`), domain `Task`/`Workflow` state (owned by `eos-state`), or
`AgentDefinition` (owned by `eos-agent-def`). It owns the broader **engine
`StreamEvent`** and all query-loop/dispatch/supervisor/notification/prompt-report
types.

## 2. Dependencies

- **Upstream crates (depends on):** `eos-types` (IDs, `UtcDateTime`, `Clock`,
  `CoreError`, `JsonObject`), `eos-llm-client` (`Message`, `MessageRole`,
  `ContentBlock`, `ToolResultBlock`, `UsageSnapshot`, `LlmRequest`,
  `LlmStreamEvent`, `StopReason`, `ToolSpec`, `LlmClient`, `ProviderError`),
  `eos-tools` (`ToolRegistry`, `ToolExecutor`, `ToolName`, `ToolIntent`,
  `ToolResult`, `ExecutionMetadata`/execution-context type, terminal
  descriptors), `eos-audit`
  (`AuditEvent`, `AuditNode`), `eos-agent-def` (`AgentDefinition`, `AgentRole`,
  `AgentType`).
- **Downstream consumers (used by):** `eos-runtime` (composition root: resolves
  the model + `Arc<dyn LlmClient>`, wires the production `EventSource`, runs the
  root agent). `eos-workflow` reaches agent execution only through its own
  `AgentRunner` seam wired by `eos-runtime`; there is no `eos-workflow ->
  eos-engine` edge.
- **Implements (downstream-state ports owned by `eos-tools`, anchor §6b):**
  `SubagentSupervisorPort` (the background supervisor backs `run_subagent` /
  control tools), `AdvisorPort` (the helper-agent runner backs `ask_advisor`), and
  `NotificationSink` (the notification service backs system-notification tools).
  These are `eos-tools`-owned traits (impl-eos-tools.md §5.6); `eos-engine`
  supplies the concrete impls, injected into tool `ExecutionMetadata` at the
  composition root over the existing `eos-engine -> eos-tools` edge. Distinct from
  the `eos-engine`-**owned** `EventSource` seam (anchor §6) and the engine's own
  `NotificationRule` type (§6 below).
- **External crates** (workspace dependency inheritance — `proj-workspace-deps`;
  versions pinned in `[workspace.dependencies]`):

  | Crate | Why | rust-skills |
  |---|---|---|
  | `tokio` (rt, sync, macros, time) | spawn background tool tasks, `select!` racing, `mpsc`/`watch`/`oneshot` channels, timers for heartbeat grace | `async-tokio-runtime`, `async-select-racing` |
  | `tokio-util` (`CancellationToken`) | graceful + parent-exit cancellation of background tasks (child tokens per task) | `async-cancellation-token` |
  | `futures` / `futures-util` | consume the model `Stream<Item = Result<LlmStreamEvent, ProviderError>>`; `Stream` for the loop output | `async-tokio-runtime`, `anti-type-erasure` |
  | `async-stream` | author the loop's `impl Stream<Item = (StreamEvent, Option<UsageSnapshot>)>` generator | — |
  | `async-trait` | `EventSource` is used behind `dyn` at the composition root | (anchor §6) |
  | `serde` / `serde_json` | prompt-report JSONL events and engine DTOs | — |
  | `schemars` | `JsonSchema` on wire/DTO types for parity snapshots | (anchor §11) |
  | `thiserror` | the single `EngineError` enum | `err-thiserror-lib` |
  | `tracing` | structured logging (replaces `logging`) | — |

  Runtime-agnostic: the crate takes `&self`/`async fn` and never creates a Tokio
  runtime; `eos-runtime` owns the multi-thread runtime (anchor §7).

## 3. Scope & Source Mapping

| Python source | Rust target | What moves / what is dropped |
|---|---|---|
| `engine/query/context.py` (`QueryContext`, `QueryExitReason`, `EventSource`) | `query/context.rs` | Full move. `EventSource` becomes a trait (anchor §6 seam). |
| `engine/query/request.py` (`QueryRunRequest`, `build_query_run_request`, `_record_initial_messages_once`) | `query/request.rs` | `QueryRunRequest` + request build move. `_record_initial_messages_once` is **dropped** here: the on-disk `agent_message_recorder` transcript is message-domain (out of scope per §3). |
| `engine/query/loop.py` (`run_query`, `_run_query_loop`, `terminal_submission_failed`, `_stamp`) | `query/loop.rs` | Full move. `_count_tool_dispatch` budget hook re-expressed inline. |
| `engine/tool_call/dispatch.py` (`dispatch_assistant_tools`, batch/lifecycle rejection, foreground fan-out, `AssistantToolDispatchOutcome`) | `tool_call/dispatch.rs` | Full move. `phase_buffer` + daemon `audit_schema` (`ToolCallSection`/`build_tool_call_event`/`safe_emit`) telemetry is **dropped** (sandbox-daemon-internal); replaced by `audit/stream.rs` projection. |
| `engine/tool_call/streaming.py` (`StreamingToolExecutor`, `StreamingToolRun`, `defer_background_dispatch`) | `tool_call/streaming.rs` | Full move; latent-fallback path (see §8 defer-all). |
| `engine/background/task_supervisor.py` (`BackgroundTaskSupervisor`, `BackgroundTaskRecord`, `CommandSessionRecord`, `WorkflowBackgroundRecord`, `BackgroundTaskStatus`) | `background/supervisor.rs` | Full move; `_terminal_lock` dropped (single-owner reap, GC-engine-04). Heartbeat→`watch`-driven task. `sandbox.api` calls go through an injected handle. |
| `engine/background/dispatch.py` (`launch_background_tool`, `dispatch_background_tool_call`, `validate_background_tool_input`) | `background/dispatch.rs` | Full move. |
| `engine/background/policy.py` (`is_engine_background_tool`, `needs_background_manager`, tool-name sets) | `background/policy.rs` | Full move; name sets become `ToolName` constants. |
| `engine/background/history.py` (`reduce_background_task_history`) | — | **Dropped**: it is an identity passthrough; no Rust target. |
| `engine/agent/factory.py` (`spawn_agent`, registry/prompt finalization) | `agent/factory.rs` | **Assembly only** moves (terminal-tool derivation, termination-prompt append, default-rule attach). Model/client resolution + sandbox provisioning stay in `eos-runtime` (GC-engine-06). |
| `engine/audit/stream.py` (`audit_events_from_stream_event`) | `audit/stream.rs` | Full move; emits `eos-audit` `AuditEvent`/`AuditNode`. |
| `notification/` (`SystemNotification`, `SystemNotificationService`, `NotificationRule`, rules, `dispatch_rules`) | `notifications.rs` | Full move; closures→enum rules (GC-engine-03 / §6). |
| `prompt/prompt_report_recorder.py` (`PromptReportRecorder`) | `prompt_report.rs` | Full move; reuses `eos-llm-client` `Message`/`UsageSnapshot` (GC-engine-02). |
| `prompt/runtime_prompt.py` (`build_runtime_system_prompt`, `build_termination_condition_prompt`) | `prompt/runtime_prompt.rs` | Termination-condition builder moves (factory needs it); base-prompt assembly is thin. |

**In scope:** query loop, request build, `EventSource` seam + production adapter,
streaming executor, dispatch + terminal/lifecycle enforcement, background
supervisor + dispatch, notification rules, prompt-report JSONL, agent assembly
factory, stream→audit projection.
**Out of scope:** model/client resolution, sandbox provisioning, daemon telemetry
rings, the message-domain on-disk transcript recorder, workflow lifecycle.

## 4. File & Module Layout

```
eos-engine/src/
  lib.rs                  // pub use re-exports (proj-pub-use-reexport)
  error.rs                // EngineError (thiserror)
  events.rs               // engine StreamEvent enum + identity-stamp helper
  query/
    mod.rs
    context.rs            // QueryContext, QueryExitReason, EventSource trait
    request.rs            // QueryRunRequest, build_query_run_request
    loop.rs               // run_query, _run_query_loop, hard-ceiling gate
    provider_source.rs    // production EventSource: LlmClient -> StreamEvent adapter
  tool_call/
    mod.rs
    streaming.rs          // StreamingToolExecutor, StreamingToolRun, defer predicate
    dispatch.rs           // dispatch_assistant_tools, batch/lifecycle rejection, fan-out
  background/
    mod.rs
    supervisor.rs         // BackgroundTaskSupervisor + records + BackgroundTaskStatus
    dispatch.rs           // launch_background_tool, dispatch_background_tool_call
    policy.rs             // is_engine_background_tool / needs_background_manager
  notifications.rs        // SystemNotification(Service), NotificationRule enum, dispatch
  prompt_report.rs        // PromptReportRecorder (JSONL writer)
  prompt/
    mod.rs
    runtime_prompt.rs     // build_termination_condition_prompt
  agent/
    mod.rs
    factory.rs            // build_query_context from injected AgentDefinition deps
  audit/
    mod.rs
    stream.rs             // audit_events_from_stream_event
```

`pub use` surfaces `QueryContext`, `QueryExitReason`, `EventSource`, `StreamEvent`,
`run_query`, `build_query_context`, `BackgroundTaskSupervisor`, `NotificationRule`,
`EngineError`. Everything else is `pub(crate)` (`proj-pub-crate-internal`).

## 5. Contracts Owned Here

Per anchor §5, `eos-engine` owns: `QueryContext`, the query loop, dispatch/streaming,
the background supervisor, notifications, the prompt report, the agent factory,
and the **`EventSource` trait**. It additionally owns the engine **`StreamEvent`**
enum (the llm-client doc explicitly drops the non-model variants to "engine-domain
`EventSource` events owned by `eos-engine`"). Fully specified in §6.

### `EventSource` trait (anchor §6 seam — DIP, deterministic tests)

A per-agent source called once per loop turn with the built request; yields the
**engine `StreamEvent`** shape. Production wraps `LlmClient::stream_message`; the
mock yields scripted engine events so a mock agent runs the *real* loop. Used
behind `dyn` at the composition root, so `#[async_trait]` (native async-fn-in-trait
is not yet `dyn`-safe — anchor §6).

```rust
pub type EngineStream =
    Pin<Box<dyn Stream<Item = Result<StreamEvent, EngineError>> + Send>>;

#[async_trait::async_trait]
pub trait EventSource: Send + Sync {
    /// One model turn. The loop passes the just-built request; the source
    /// yields engine `StreamEvent`s (deltas + the `AssistantMessageComplete`
    /// terminus). Errors are surfaced as `EngineError`.
    async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError>;
}
```

The production `ProviderEventSource { client: Arc<dyn LlmClient> }` adapts the
`LlmStreamEvent` stream into `StreamEvent` (1:1 for the four model variants;
`AssistantMessageComplete` re-wrapped with `agent_name`/`agent_run_id`). Contracts
merely **used** (`LlmClient`, `LlmRequest`, `LlmStreamEvent`, `Message`,
`ToolResultBlock`, `UsageSnapshot`, `ToolSpec`, `ToolRegistry`, `ToolResult`,
`ToolIntent`, `AuditEvent`, `AgentDefinition`) are referenced, never
re-specified — see
impl-eos-llm-client.md, the forthcoming impl-eos-tools.md, impl-eos-audit.md,
impl-eos-agent-def.md.

## 6. Types, Fields & Schemas

All derive `Debug, Clone` where the inner data allows (`api-common-traits`); wire
DTOs add `Serialize, Deserialize, JsonSchema`; public enums likely to grow are
`#[non_exhaustive]` (`api-non-exhaustive`).

### `QueryExitReason` (enum, `type-enum-states`)

Source: `context.py::QueryExitReason` (`StrEnum`). serde `rename_all = "snake_case"`.

| Variant | meaning | source |
|---|---|---|
| `ToolStop` | success: a terminal tool was submitted | `tool_stop` |
| `TerminalNotSubmitted` | failure: hard ceiling crossed without terminal submission | `terminal_not_submitted` |

### `QueryContext` (owned; mutated by the loop, single-owner — §7)

Source: `context.py::QueryContext`. Holds run-scoped configuration **and** the
mutable turn counters / dedup state. Production **always** sets
`event_source = Some(ProviderEventSource { client })` (the factory/runtime builds
it from the injected `Arc<dyn LlmClient>`); `None` is reserved for the
scripted-mock path (or simply absent in tests). There is no loop-built default
source — `api_client` is not carried on the context (client resolution lives in
`eos-runtime`, GC-engine-06).

| Field | Rust type | source | notes |
|---|---|---|---|
| `tool_registry` | `Arc<ToolRegistry>` | `tool_registry` | shared immutable (`own-arc-shared`); eos-tools |
| `cwd` | `PathBuf` | `cwd` | |
| `model` | `String` | `model` | resolved upstream; opaque |
| `system_prompt` | `String` | `system_prompt` | request field, never a `Message` (GC-engine-02) |
| `max_tokens` | `u32` | `max_tokens` | |
| `tool_call_limit` | `u32` | `tool_call_limit` | hard-ceiling basis |
| `agent_name` | `String` | `agent_name` | identity stamp |
| `agent_run_id` | `AgentRunId` | `agent_run_id` | eos-types newtype |
| `task_id` | `Option<TaskId>` | `task_id` | eos-types newtype |
| `tool_calls_used` | `u32` | `tool_calls_used` | bumped once per `ToolUseDelta` |
| `text_only_no_terminal_turns` | `u32` | `text_only_no_terminal_turns` | bumped on a text-only turn |
| `tool_metadata` | `ExecutionMetadata` | `tool_metadata` | eos-tools; carries supervisor handle |
| `enable_background_tasks` | `bool` | `enable_background_tasks` | |
| `terminal_tools` | `BTreeSet<ToolName>` | `terminal_tools` | derived from registry if empty |
| `exit_reason` | `Option<QueryExitReason>` | `exit_reason` | set at loop exit |
| `terminal_result` | `Option<ToolResult>` | `terminal_result` | eos-tools; the persisted submission |
| `event_source` | `Option<Arc<dyn EventSource>>` | `event_source` | seam; `Some(ProviderEventSource)` in production, `None` only for scripted mocks |
| `prompt_report` | `Option<PromptReportRecorder>` | `prompt_report_recorder` | |
| `notification_rules` | `Vec<NotificationRule>` | `notification_rules` | enum rules |
| `notification_fired` | `BTreeSet<String>` | `notification_fired` | fire-once dedup by name |
| `notification_state` | `JsonObject` | `notification_state` | per-rule scratchpad |

### Engine `StreamEvent` (enum, `type-enum-states`, `#[non_exhaustive]`)

Source: `message/events.py::StreamEvent` union. The **broad** agent-facing event:
the four model deltas (re-wrapped from `LlmStreamEvent` with identity) plus the
engine-domain tool/background/notification events. Every variant carries
`agent_name: String` and `agent_run_id: AgentRunId` identity (defaulted empty;
stamped by the loop — see stamp helper). Boxed large variant (`mem-box-large-variant`)
for `AssistantMessageComplete` which embeds a `Message`.

| Variant | Fields (beyond identity) | source event |
|---|---|---|
| `ReasoningDelta` | `text: String` | `ThinkingDeltaEvent` (renamed; cf. eos-llm-client GC-llm-client-01) |
| `AssistantTextDelta` | `text: String` | `AssistantTextDeltaEvent` |
| `AssistantMessageComplete` | `Box<{ message: Message, usage: UsageSnapshot, stop_reason: Option<StopReason> }>` | `AssistantMessageCompleteEvent` |
| `ToolUseDelta` | `tool_use_id: ToolUseId, name: String, input: JsonObject` | `ToolUseDeltaEvent` |
| `ToolExecutionStarted` | `tool_name: String, tool_input: JsonObject, tool_use_id: ToolUseId` | `ToolExecutionStartedEvent` |
| `ToolExecutionCompleted` | `tool_name: String, output: String, is_error: bool, tool_use_id: ToolUseId, metadata: JsonObject, is_terminal: bool` | `ToolExecutionCompletedEvent` |
| `ToolExecutionProgress` | `tool_use_id: ToolUseId, tool_name: String, output: String` | `ToolExecutionProgressEvent` |
| `ToolExecutionCancelled` | `tool_use_id: ToolUseId, tool_name: String, reason: String` | `ToolExecutionCancelledEvent` |
| `BackgroundTaskStarted` | `task_id: String, tool_name: String, tool_input: JsonObject` | `BackgroundTaskStartedEvent` |
| `SystemNotification` | `text: String` | `SystemNotification` |

`stamp_identity(event, agent_name, &agent_run_id)` fills empty identity fields
(the `_stamp` logic in `loop.py`); engine-owned behavior, kept.

### `QueryRunRequest` (owned)

Source: `request.py::QueryRunRequest`. Built once per turn by
`build_query_run_request`. `prompt_report_seq` is reused across the turn's three
JSONL events (§8 ordering invariant).

| Field | Rust type | source |
|---|---|---|
| `request` | `LlmRequest` | `request` (eos-llm-client) |
| `prompt_report_seq` | `u64` | `prompt_report_seq` |

(The Python field `prompt_report` is the recorder reference; in Rust the recorder
lives on `QueryContext`, so the request only carries the per-turn `seq`.)

### `AssistantToolDispatchOutcome` (owned)

Source: `dispatch.py::AssistantToolDispatchOutcome`.

| Field | Rust type | source |
|---|---|---|
| `tool_results` | `Vec<ToolResultBlock>` | `tool_results` (eos-llm-client) |
| `terminal_result` | `Option<ToolResult>` | `terminal_result` |
| `events` | `Vec<StreamEvent>` | `events` |

### `BackgroundTaskStatus` (enum, `type-enum-states`)

Source: `task_supervisor.py::BackgroundTaskStatus`. `RUNNING -> {COMPLETED,
FAILED, CANCELLED} -> DELIVERED`. Precedence (CAS in reap): `Completed(3) >
Failed(2) > Cancelled(1) > Running(0)`; `Delivered(4)` is the sink.

| Variant | source |
|---|---|
| `Running` | `running` |
| `Completed` | `completed` |
| `Failed` | `failed` |
| `Cancelled` | `cancelled` |
| `Delivered` | `delivered` |

### `BackgroundTaskRecord` / `CommandSessionRecord` / `WorkflowBackgroundRecord` (owned)

Sources: `task_supervisor.py`. Selected fields (all owned by the loop task, no
interior mutex — GC-engine-04):

`BackgroundTaskRecord`: `task_id: String` (background-supervisor-local id),
`tool_name: String`, `tool_input:
JsonObject`, `task_kind: BackgroundTaskKind` (`Agent|Subagent|Workflow` —
replaces stringly `task_type`, `type-no-stringly`), `subagent_session_id:
Option<SubagentSessionId>`, `agent_id: Option<String>`, `uses_sandbox: bool`, `sandbox_id:
Option<SandboxId>`, `sandbox_invocation_id: Option<InvocationId>`,
`heartbeat_enabled: bool`, `status: BackgroundTaskStatus`, `cancel_reason:
Option<String>`, `stop_mode: Option<StopMode>` (`Cancel|EarlyStop|ParentExit`),
`completion_mode: Option<CompletionMode>`, `result: Option<ToolResult>`,
`started_at: Instant` (`tokio::time::Instant`; source `time.monotonic()`, backs
duration/uptime math — not a wall-clock timestamp), `progress_lines:
Vec<String>`. The `asyncio.Task`
handle becomes an `AbortHandle` + child `CancellationToken` held in the JoinSet
keyed by `task_id` (§7).

`CommandSessionRecord`: `command_session_id: CommandSessionId`, `sandbox_id`,
`agent_id`, `command: String`, `status: BackgroundTaskStatus`, `result: Option<JsonObject>`,
`started_at: Instant` (`tokio::time::Instant`; source `time.monotonic()`).

`WorkflowBackgroundRecord`: `workflow_task_id: WorkflowTaskId`, `workflow_id: WorkflowId`,
`parent_task_id: TaskId`, `parent_attempt_id: Option<AttemptId>`, `request_id:
RequestId`, `agent_id: String`, `goal: String`, `status`, `final_status:
Option<String>`, `final_outcomes: Vec<JsonObject>`, `last_seen_at: Instant`
(`tokio::time::Instant`; source `time.monotonic()`, mutated by
`refresh_workflow_status`/`mark_*` for staleness tracking), plus the four
delivery flags
(`terminal_reported_by_status_tool`, `terminal_reported_by_notification`,
`cancelled_by_cancel_tool`, `parent_cancelled`). `delivered()` / `outstanding()`
are the same derived predicates as the source.

### `SystemNotification` (owned)

Source: `runtime.py::SystemNotification`. `{ text: String, agent_name: String,
agent_run_id: String }`. Stream- and transcript-visible per engine policy (plan
§"Prompt reporting and notifications").

### `NotificationRule` (owned — enum, NOT a trait seam; GC-engine-03)

Source: `notification/rules`. The Python `NotificationRule{name, body, trigger,
fire_once}` carries closures; closures are not on the §6 seam map, so this is a
**closed enum** (`type-enum-states`) — the smallest concrete shape:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum NotificationRule {
    /// Nudge to submit a terminal tool (fire_once = false).
    TerminalCallReminder,
    /// Budget warning at a tier; fire_once = true, deduped by name.
    ToolCallBudget { label: &'static str, numerator: u32, denominator: u32 },
}

impl NotificationRule {
    pub fn name(&self) -> String { /* "terminal_call_reminder" | "tool_call_budget_<label>_percent" */ }
    pub fn fire_once(&self) -> bool { matches!(self, Self::ToolCallBudget { .. }) }
    pub fn trigger(&self, messages: &[Message], ctx: &QueryContext) -> bool { /* per source */ }
    pub fn body(&self, ctx: &QueryContext) -> String { /* per source text */ }
}
```

Tier table (`terminal_tool_call_count_reminder.py`): `("75%",3,4)`, `("100%",1,1)`,
`("125%",5,4)`. `make_default_notification_rules()` returns the three budget tiers
+ `TerminalCallReminder`, deduped by `name()` (factory).

### `PromptReportRecorder` (owned) + JSONL event schema

Source: `prompt_report_recorder.py`. Reuses `eos-llm-client` `Message`/
`UsageSnapshot`/`ToolResultBlock` (`m.model_dump` → `serde_json::to_value`);
GC-engine-02. Three events, all sharing the turn's `seq`:

| Event tag | Fields | source method |
|---|---|---|
| `llm_request` | `seq: u64`, `system_prompt: String`, `messages: Vec<Message>`, `tools: Vec<ToolSpec>` | `record_llm_request` |
| `assistant` | `seq: u64`, `message: Message`, `usage: UsageSnapshot` | `record_assistant` |
| `tool_results` | `seq: u64`, `tool_results: Vec<ToolResultBlock>` | `record_tool_results` |

Each row is the base event (`{agent_run_id, agent, model}`) merged with the event
body, appended through the file-backed audit writer contract. Production wiring
uses the buffered JSONL sink so prompt-report writes do not block Tokio worker
threads. **No `role="system"` `Message` is ever synthesized** (GC-engine-02):
`system_prompt` is a top-level
string field; `MessageRole` has only `User`/`Assistant`.

## 7. Concurrency & State Ownership

Runtime-agnostic crate; `eos-runtime` owns the single multi-thread Tokio runtime
(anchor §7). This is the highest-risk crate; the model below is deliberately
**single-owner** to avoid shared-mutable supervisor state.

- **Loop ownership.** `_run_query_loop` is an `async-stream` generator running as
  one task (the agent-run owner). `QueryContext` is owned/`&mut` by that task;
  there is no `Arc<Mutex<QueryContext>>`. Counters (`tool_calls_used`,
  `text_only_no_terminal_turns`), `notification_fired`, `terminal_result`, and
  `exit_reason` are mutated only here. `tool_registry` is `Arc<ToolRegistry>`
  shared immutable (`own-arc-shared`).
- **Loop output.** `run_query` returns `impl Stream<Item = (StreamEvent,
  Option<UsageSnapshot>)>`; the `stamp_identity` wrapper maps the inner stream
  (the `_stamp`/`_stamped` logic), borrowing the immutable identity strings.
- **Provider stream consumption.** `EventSource::stream` returns
  `Pin<Box<dyn Stream<...>>>`; the loop `while let Some(event) = stream.next().await`
  drives it, accumulating `final_message`/`usage`/`streamed_tool_use_ids` into a
  local `ProviderStreamAccumulator`. On any error/early break the in-flight
  streaming executor tasks are aborted (`executor.cancel_all()`).
- **Background supervisor — single owner, JoinSet + per-task CancellationToken.**
  All mutations (`launch`, `cancel`, `cancel_all`, reap, `register_*`,
  `mark_*`) run inline on the loop-owner task — dispatch is `.await`ed inline, and
  `cancel_subagent`/`cancel_workflow`/`check_*` are *tools* dispatched within the
  same task. So `BackgroundTaskSupervisor` is plain owned state:
  `HashMap<String, BackgroundTaskRecord>` + the command-session / workflow maps,
  with a `JoinSet<(String, BackgroundOutcome)>` for spawned tool tasks
  (`async-joinset-structured`). Each `launch` spawns into the JoinSet with a child
  `CancellationToken` derived from a run-scoped root token
  (`async-cancellation-token`); `cancel`/`cancel_all`/`terminate_for_parent_exit`
  call `token.cancel()` (subagents additionally yield once to salvage a partial
  result, matching `_request_subagent_early_stop`). Completed tasks are reaped via
  `join_next()` at the existing drain points (top-of-turn drain, TOOL_STOP exit,
  ceiling exit, `finally`).
- **Terminal-status precedence without a lock (GC-engine-04).** The Python
  `_terminal_lock` + `_TERMINAL_PRECEDENCE` CAS exists only because the
  `done_callback` and `cancel()` race across asyncio callbacks. With single-owner
  reap there is no race: `cancel()` records the requested status, and the reap
  step applies precedence to the `join_next()` outcome (a real `Completed` result
  overrides a previously-requested `Cancelled`, resolving the cancel-vs-finish
  race to `COMPLETED` exactly as the source comment requires). Drop the lock —
  net-negative simplification.
- **Heartbeat via `watch` (anchor §7 / `async-watch-latest`).** The optional
  heartbeat is one spawned task that only needs a *read snapshot* of
  `{SandboxId: Vec<InvocationId>}` and the running command sessions. The owner
  publishes the snapshot through a `watch::Sender` whenever the running set
  changes; the heartbeat task `borrow_and_update().clone()`s it each interval and
  issues the sandbox heartbeat/poll via the injected sandbox handle — keeping all
  supervisor-map mutation on the owner task. The interval is `select!`ed against
  the run-scoped `CancellationToken` (`async-select-racing`).
- **Foreground multi-tool fan-in (`async-mpsc-queue`, `async-bounded-channel`).**
  `_dispatch_many_foreground_tools`' `asyncio.Queue` becomes a **bounded** `mpsc`:
  capacity is `max(2 * tool_batch_len, 16)` for final-result headroom. Each tool
  task `select!`s sends against the run `CancellationToken`. Final
  `(ToolUseBlock, ToolResultBlock)` sends are mandatory before task completion;
  progress sends are best-effort and may be coalesced/dropped with a dropped-count
  note when the channel is full. The owner drains in completion order, preserving
  progress-before-result interleaving for delivered events, closes the receiver on
  terminal/ceiling/parent-exit, and aborts unfinished foreground tasks. Single-tool
  dispatch stays a direct inline `await`.
- **Foreground ownership and supervisor controls.** Spawned foreground tool tasks
  must own cloned `ToolUseBlock` input and an owned/cloned `ExecutionMetadata`;
  they may borrow those values only inside the spawned async block so the future is
  `Send + 'static`. Background-control/lifecycle tools that mutate supervisor
  state (`check_*`, `cancel_*`, `write_stdin`, workflow control) are not launched as
  parallel foreground siblings unless the mutation is routed through the loop-owner
  task. This preserves the single-owner supervisor model while retaining parallel
  fan-in for ordinary independent foreground tools.
- **Lock discipline.** No lock is held across `.await` anywhere
  (`async-no-lock-await`, `anti-lock-across-await`); the design has no app-level
  mutex on the hot path. If a future change forces a supervisor mutation off the
  owner task, fall back to `Arc<parking_lot::Mutex<Supervisor>>` with
  clone-before-await (`async-clone-before-await`, anchor §7) — or
  `tokio::sync::Mutex` only if a mutation must hold the guard across an `.await`.
  The current design needs neither.
- **Supervisor ↔ tools back-reference.** Tools (`run_subagent`, `exec_command`,
  `write_stdin`, workflow tools) reach the supervisor through the eos-tools
  `ExecutionMetadata`/execution context (the `background_task_manager` /
  `on_progress_line` slots). Direction: the engine **populates** the supervisor
  handle into the eos-tools execution context; eos-tools never depends on
  eos-engine.

## 8. Behavior & Invariants

Cite the plan §8 gap closeouts and core rules (anchor §3).

1. **Defer-all terminal enforcement (plan §8 GC-1).** `make_stream_dispatch_deferrer`
   returns `true` for **every** tool whenever `context.terminal_tools` is
   non-empty (it is non-empty for every production agent — the factory asserts ≥1
   terminal tool). Therefore in production **all** tool execution is deferred to
   the post-message `dispatch_assistant_tools`, so terminal-tool exclusivity is
   validated against the **complete** assistant message before any sibling body
   runs. The mid-stream `StreamingToolExecutor` body path is a **latent fallback**
   exercised only when `terminal_tools` is empty (test fixtures); do not model it
   as the primary path. Background tools defer regardless (their dispatch path).
2. **Terminal tools called alone.** `validate_tool_batch`: if a batch (`len > 1`)
   contains any terminal tool, **no** tool executes; every call gets an error
   `ToolResultBlock` ("must be called alone… resubmit"). The first terminal result
   is projected as `terminal_result`. (Lifecycle-batch rejection —
   `Intent::Lifecycle`, the source `_record_lifecycle_batch_rejection` — is the
   engine-side analogue, kept; not a separately tracked gap-closeout.)
3. **Single budget count.** The counter increments once per observed `ToolUseDelta`
   (`_count_tool_dispatch`), and deferred dispatch passes `consume_budget =
   tool_use_id ∉ streamed_tool_use_ids` (false for already-streamed ids), so each
   tool is counted exactly once at stream time.
4. **Hard ceiling (plan §8 GC-2).** `terminal_submission_failed` ⇔
   `tool_calls_used + text_only_no_terminal_turns >= ceil(1.5 * tool_call_limit)`.
   The ceiling is **derived from `tool_call_limit`** and compared against the
   **sum** of tool calls and text-only turns. The reminder-rule message text's
   `turns_remaining` uses only `tool_calls_used` — a deliberate approximation in
   the *text*; it must not be "unified" with the real gate.
5. **Exit reasons & drains.** On `terminal_result` set → `exit_reason =
   ToolStop`; the loop calls `terminate_for_parent_exit` (subagents get a
   parent-exit result, `stop_mode = ParentExit`, status `Cancelled`) then flushes
   notification events, then breaks. On ceiling crossed → `cancel_all`, emit a
   single `ToolExecutionCompleted{tool_name:"", is_error:true}` carrying the
   `_terminal_not_submitted_message`, flush, set `exit_reason =
   TerminalNotSubmitted`, break. The `finally` cancels any still-pending tasks.
6. **Notification ordering.** `dispatch_rules` evaluates rules in list order at the
   **top of each turn** (after draining background-completion notifications), before
   building the next request, so newly-fired reminders reach the model that turn.
   An earlier rule's emission lands in the pool but is **not** visible to a later
   rule's `trigger` the same turn (the pool is drained after all rules run). The
   drained `SystemNotificationBlock`s are appended as one `user` message.
7. **Prompt-report seq stability.** One `next_seq()` per turn; the same `seq`
   labels `llm_request`, then `assistant`, then `tool_results`. The golden test
   depends on this ordering and on stable JSON field ordering.
8. **Background = engine dispatch mode (plan §8 GC-3).** Background execution is an
   engine dispatch path (JoinSet + CancellationToken), **not** provider/session
   state. Cancellation and parent-exit semantics (early-stop salvage, parent-exit
   result, precedence CAS) are preserved.
9. **Identity stamping.** Events missing `agent_name`/`agent_run_id` are stamped
   by the loop from the context (the `_stamp` helper). Provider-source model events
   already carry empty identity and get stamped.

## 9. SOLID & Principles Applied

- **DIP.** The loop depends on the `EventSource` trait, not on a concrete
  provider; production injects `ProviderEventSource(Arc<dyn LlmClient>)`, tests
  inject a scripted source — the mock runs the *real* loop. Tool execution depends
  on the eos-tools `ToolExecutor`/`ToolRegistry` abstraction; audit on
  `eos-audit` types. (anchor §6 seam.)
- **OCP.** New tools are added by registering in the `ToolRegistry` and (if
  background) listed via `ToolName` constants in `policy.rs`; the dispatch path
  branches on `intent`/policy, not on hard-coded tool names sprinkled through the
  loop. Notification behavior extends by adding a `NotificationRule` variant.
- **ISP.** `EventSource` has exactly one method. `QueryContext` carries only what
  one run needs; no god-context reaching across crate boundaries.
- **LSP.** Provider-neutral `Message`/`LlmStreamEvent` (eos-llm-client) make
  Anthropic/OpenAI/mock substitutable behind `EventSource`; exhaustive enums
  (`QueryExitReason`, `BackgroundTaskStatus`, `StreamEvent`) keep states total.
- **SRP.** Each module owns one concern; the factory **assembles** but does not
  resolve models or provision sandboxes (kept in `eos-runtime`).
- **KISS/YAGNI/DRY.** `NotificationRule` is a closed enum (no trait seam — not on
  §6 list); the `_terminal_lock` is dropped (single-owner); `history.py` (identity
  passthrough) is dropped entirely. Daemon telemetry rings are out of scope.
- **Non-goals respected:** background is a dispatch mode not a session; no global
  orchestrator (per-run only); no provider `class_path`; no tool-visibility enum
  (visibility = presence in the request's `Vec<ToolSpec>`).

## 10. Gap Closeouts (tracked requirements)

| ID | Requirement | Resolution |
|---|---|---|
| GC-engine-01 | Keep terminal-execution deferral inside the loop/streaming executor so terminal exclusivity is checked after the full assistant message. | Defer-all predicate (§8.1) + `validate_tool_batch` over the complete `final_message.tool_uses` (§8.2). Proven by AC-engine-01, AC-engine-02. |
| GC-engine-02 | Prompt report reuses provider-neutral `Message`/`UsageSnapshot` from eos-llm-client **and** fixes the system-role transcript mismatch. | `prompt_report.rs` serializes the imported `Message`/`UsageSnapshot`; `llm_request` records `system_prompt: String` as a top-level field and `messages` holds only user/assistant rows. `MessageRole` has no `System` variant (cf. eos-llm-client GC-llm-client-03); no `role="system"` Message is ever built. Proven by AC-engine-04, AC-engine-06. |
| GC-engine-03 | Notification rules stay declarative and engine-owned. | `NotificationRule` closed enum + `dispatch_rules` evaluated top-of-turn in list order with fire-once dedup; rules attached by the factory. Not a trait seam. Proven by AC-engine-05. |
| GC-engine-04 | Hard ceiling for terminal non-submission remains derived from `tool_call_limit`. | `terminal_submission_failed` = `ceil(1.5 * tool_call_limit)` vs the sum `tool_calls_used + text_only_no_terminal_turns` (§8.4). Single-owner reap drops the `_terminal_lock` while preserving precedence. Proven by AC-engine-03. |
| GC-engine-05 | Background execution is an engine dispatch mode, not provider state; preserve cancellation + parent-exit. | `BackgroundTaskSupervisor` = JoinSet + per-task `CancellationToken`; `terminate_for_parent_exit` + `cancel_all` on the two exit paths; early-stop salvage retained (§7, §8.5/8.8). Proven by AC-engine-07. |
| GC-engine-06 | Factory builds concrete registries + prompts from `AgentDefinition` only (no DB/client resolution). | `build_query_context` takes *injected* `(model, Arc<dyn LlmClient>, ToolRegistry, base_system_prompt, tool_call_limit, AgentDefinition)`; derives terminal tools, appends the termination-condition prompt, attaches default notification rules. Model/`make_api_client`/sandbox provisioning stay in eos-runtime. Proven by AC-engine-08. |
| GC-engine-07 | `EventSource` is the DIP seam for mock vs real stream. | Trait in `query/context.rs` (`#[async_trait]`, `dyn`-safe); production `ProviderEventSource` adapts `LlmClient`; mock yields engine events. Proven by AC-engine-01/05 (mock-driven loop). |

## 11. Acceptance Criteria

TDD: write each test first, confirm it fails for the right reason, then implement.
Maps to anchor §11 "Tests to Port First" for eos-engine (`test_tool_batch.py`,
`test_tool_call_dispatch_lifecycle.py`, prompt-report golden, notification rules).

| ID | Assertion | Proving test | Ports |
|---|---|---|---|
| AC-engine-01 | A scripted `EventSource` emitting an assistant message that batches a terminal tool with a sibling yields error `ToolResultBlock`s for **both**, no tool body runs, and `terminal_result` is the first terminal block. | `tool_call/dispatch.rs` `#[tokio::test] fn terminal_batched_with_sibling_rejects_all` | `test_tool_batch.py` |
| AC-engine-02 | With `terminal_tools` non-empty, all tools are deferred (the streaming executor tracks none); execution happens in `dispatch_assistant_tools` after the complete message. | `tool_call/streaming.rs` `#[tokio::test] fn defer_all_when_terminal_present` | `test_tool_call_dispatch_lifecycle.py` |
| AC-engine-03 | A run that never submits a terminal tool exits with `TerminalNotSubmitted` exactly when `tool_calls_used + text_only_no_terminal_turns == ceil(1.5*limit)`, emitting the failure `ToolExecutionCompleted`. | `query/loop.rs` `#[tokio::test] fn hard_ceiling_exit_terminal_not_submitted` | — |
| AC-engine-04 | Replaying a fixed transcript through a scripted source produces a prompt-report JSONL byte-identical to the golden file: per turn `llm_request`→`assistant`→`tool_results` share one `seq`; `llm_request.system_prompt` is a top-level string and `messages` contain no system row. | `prompt_report.rs` `#[tokio::test] fn prompt_report_matches_golden` (golden under `tests/fixtures/prompt_report/`) | prompt-report golden |
| AC-engine-05 | `TerminalCallReminder` fires every turn until a terminal is submitted (fire_once=false); each `ToolCallBudget` tier fires once at its threshold; an earlier rule's emission is not visible to a later rule's trigger the same turn. | `notifications.rs` `#[tokio::test] fn notification_rules_fire_in_order_with_dedup` | notification rules |
| AC-engine-06 | `PromptReportRecorder` never serializes a system-role transcript row: every recorded `Message` has role `user`/`assistant`, and `system_prompt` is the top-level `llm_request` string field. (The enum having no `System` variant is owned/proven by eos-llm-client; see impl-eos-llm-client.md GC-llm-client-03.) | `prompt_report.rs` `#[test] fn no_system_role_in_transcript` | — |
| AC-engine-07 | On TOOL_STOP a running subagent is terminated via parent-exit (`stop_mode=ParentExit`, status `Cancelled`, parent-exit result); a `cancel()` racing a natural completion resolves to `Completed` via reap precedence. | `background/supervisor.rs` `#[tokio::test] fn parent_exit_and_cancel_complete_race` | `test_tool_call_dispatch_lifecycle.py` |
| AC-engine-08 | `build_query_context` for an `AgentDefinition` with one terminal tool derives `terminal_tools`, appends the termination-condition prompt, and attaches the three budget tiers + `TerminalCallReminder` (deduped by name). | `agent/factory.rs` `#[test] fn factory_assembles_terminals_prompt_and_rules` | — |
| AC-engine-09 | A progress-heavy foreground multi-tool batch cannot deadlock the owner, may drop/coalesce progress with a counter, and always delivers every final result unless cancellation wins. | `tool_call/dispatch.rs` `#[tokio::test] fn foreground_fan_in_backpressure_preserves_final_results` | — |

## 12. Implementation Checklist

Ordered, small, verifiable steps (`small-incremental-changes`):

1. `error.rs`: `EngineError` (`thiserror`, `#[from]` for `ProviderError`,
   `ToolError`, `CoreError`).
2. `events.rs`: engine `StreamEvent` enum + `stamp_identity`; unit-test the stamp.
3. `query/context.rs`: `QueryContext`, `QueryExitReason`, `EventSource` trait.
4. `prompt_report.rs`: `PromptReportRecorder` + 3 JSONL events; **AC-engine-04,
   AC-engine-06 first**.
5. `notifications.rs`: `SystemNotification(Service)`, `NotificationRule` enum,
   `dispatch_rules`, default-rule factory; **AC-engine-05 first**.
6. `tool_call/streaming.rs`: `StreamingToolExecutor` + defer predicate;
   **AC-engine-02 first**.
7. `tool_call/dispatch.rs`: batch/lifecycle rejection, foreground fan-out,
   `AssistantToolDispatchOutcome`; **AC-engine-01 first**.
8. `background/policy.rs` + `background/supervisor.rs` (JoinSet + tokens, watch
   heartbeat, reap precedence) + `background/dispatch.rs`; **AC-engine-07 first**.
9. `query/provider_source.rs`: `ProviderEventSource` adapter.
10. `query/request.rs` + `query/loop.rs`: the full loop wiring all of the above;
    **AC-engine-03 first**.
11. `prompt/runtime_prompt.rs` + `agent/factory.rs`: assembly; **AC-engine-08 first**.
12. `audit/stream.rs`: `audit_events_from_stream_event` → eos-audit events.
13. `lib.rs` re-exports; `cargo fmt --check`, `clippy -D warnings`, full test run.

---
**On completion:** update the Progress Tracker in `./overview.md` for row
`eos-engine` per spec-conventions.md §13. Do not edit other crates' rows.
