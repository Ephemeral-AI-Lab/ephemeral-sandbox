# Crate `eos-engine` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-engine/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**31 types across 12 files.**

The `eos-engine` crate owns one ephemeral agent's query loop: the model-turn
loop, post-message tool dispatch, background-task supervision, declarative
notifications, prompt reports, and the event-source seam that decouples the loop
from any concrete provider. Its central types are `QueryContext` (the mutable
per-run state), the `EventSource` trait plus its production `ProviderEventSource`
adapter, the broad `StreamEvent` enum that all turn output flows through,
`AssistantToolDispatchOutcome` from `dispatch_assistant_tools`, the
`BackgroundTaskSupervisor` / `SharedSubagentSupervisor` pair, and `EngineError`.
It depends on the workspace's lower layers — `eos-llm-client` (provider-neutral
`LlmClient`/`Message`/`LlmRequest`), `eos-tools` (registry, dispatch primitives,
and port traits it implements), `eos-types`, `eos-agent-def`, `eos-audit`, and
`eos-state` (tests only) — and is consumed by `eos-runtime`, which wires a real
provider client and helper runners around the loop exposed here.

## Contents

- **`eos-engine/src/error.rs`** — `EngineError`
- **`eos-engine/src/events.rs`** — `AssistantMessageComplete`, `StreamEvent`
- **`eos-engine/src/notifications.rs`** — `SystemNotification`, `NotificationRule`, `NotificationService`, `AdvisorService`
- **`eos-engine/src/prompt_report.rs`** — `PromptReportState`, `PromptReportRecorder`, `BaseEvent`, `LlmRequestEvent`, `AssistantEvent`, `ToolResultsEvent`
- **`eos-engine/src/agent/factory.rs`** — `BuildQueryContextInput`
- **`eos-engine/src/background/supervisor.rs`** — `BackgroundTaskStatus`, `BackgroundTaskKind`, `StopMode`, `BackgroundTaskRecord`, `BackgroundTaskSupervisor`, `SharedSubagentSupervisor`
- **`eos-engine/src/query/context.rs`** — `EngineStream`, `EventSource`, `QueryExitReason`, `QueryContext`
- **`eos-engine/src/query/loop_.rs`** — `QueryStream`
- **`eos-engine/src/query/provider_source.rs`** — `ProviderEventSource`
- **`eos-engine/src/query/request.rs`** — `QueryRunRequest`
- **`eos-engine/src/tool_call/dispatch.rs`** — `ToolUseRequest`, `AssistantToolDispatchOutcome`, `ForegroundCompletion`
- **`eos-engine/src/tool_call/streaming.rs`** — `StreamingToolExecutor`

---

## `eos-engine/src/error.rs`

#### `EngineError`  ·  _enum_  ·  derives: `Debug, thiserror::Error`  ·  #[non_exhaustive]  ·  [L10]

A framework error raised by the engine loop or one of its owned helpers.

**Variants**:
- `Provider(ProviderError)` — `#[from]` provider/client error
- `Tool(ToolError)` — `#[from]` tool-framework error
- `Core(CoreError)` — `#[from]` shared value/store error
- `Io(std::io::Error)` — `#[from]` prompt-report file I/O error
- `Json(serde_json::Error)` — `#[from]` JSON serialization error
- `UnknownTool(String)` — a model requested an unregistered tool
- `MissingEventSource` — query loop run without an event source
- `Internal(String)` — engine invariant broke

**Trait impls**: `Debug (thiserror), Error, Display (thiserror), From<ProviderError>, From<ToolError>, From<CoreError>, From<std::io::Error>, From<serde_json::Error>`

---

## `eos-engine/src/events.rs`

#### `AssistantMessageComplete`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`  ·  [L10]

Payload carried by `StreamEvent::AssistantMessageComplete`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `message` | `Message` | `pub` |
| `usage` | `UsageSnapshot` | `pub` |
| `stop_reason` | `Option<StopReason>` | `pub` |

#### `StreamEvent`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize`  ·  #[serde(tag = "type", rename_all = "snake_case")]  ·  #[non_exhaustive]  ·  [L24]

A broad agent-run stream event: provider deltas plus engine-domain tool, background, and notification events; every variant carries `agent_name` + `agent_run_id` identity fields.

**Variants** (one bullet each; identity fields `agent_name: String`, `agent_run_id: Option<AgentRunId>` omitted):
- `ReasoningDelta { …, text: String }` — incremental reasoning
- `AssistantTextDelta { …, text: String }` — incremental assistant text
- `AssistantMessageComplete { …, payload: Box<AssistantMessageComplete> }` — completed assistant message
- `ToolUseDelta { …, tool_use_id: ToolUseId, name: String, input: JsonObject }` — fully assembled tool call
- `ToolExecutionStarted { …, tool_name: String, tool_input: JsonObject, tool_use_id: ToolUseId }`
- `ToolExecutionCompleted { …, tool_name: String, output: String, is_error: bool, tool_use_id: ToolUseId, metadata: JsonObject, is_terminal: bool }`
- `ToolExecutionProgress { …, tool_use_id: ToolUseId, tool_name: String, output: String }`
- `ToolExecutionCancelled { …, tool_use_id: ToolUseId, tool_name: String, reason: String }`
- `BackgroundTaskStarted { …, task_id: String, tool_name: String, tool_input: JsonObject }`
- `SystemNotification { …, text: String }`

**Trait impls**: `JsonSchema`

---

## `eos-engine/src/notifications.rs`

#### `SystemNotification`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L18]

A stream- and transcript-visible system notification.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `text` | `String` | `pub` |
| `agent_name` | `String` | `pub` |
| `agent_run_id` | `String` | `pub` |

#### `NotificationRule`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L29]

Closed set of engine-owned notification rules.

**Variants**:
- `TerminalCallReminder` — nudge the model to submit a terminal tool
- `ToolCallBudget { label: &'static str, numerator: u32, denominator: u32 }` — tool-call budget threshold

<details><summary>Methods (4)</summary>

`name`, `fire_once`, `trigger`, `body`

</details>

#### `NotificationService`  ·  _struct_  ·  derives: `Debug, Default, Clone`  ·  [L159]

Queue-backed notification sink for tools and hooks.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `queue` | `Arc<Mutex<VecDeque<ToolNotification>>>` |  |

**Trait impls**: `Sealed, NotificationSink`

<details><summary>Methods (2)</summary>

`new`, `drain`

</details>

#### `AdvisorService`  ·  _struct_  ·  derives: `Debug, Default, Clone`  ·  [L189]

Minimal advisor port implementation used until `eos-runtime` wires a helper runner around the engine loop.

_Unit struct — no fields._

**Trait impls**: `Sealed, AdvisorPort`

---

## `eos-engine/src/prompt_report.rs`

#### `PromptReportState`  ·  _struct_  ·  derives: `Debug, Default`  ·  · private  ·  [L14]

Internal mutable recorder state holding the next turn sequence number.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `next_seq` | `u64` |  |

#### `PromptReportRecorder`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L20]

File-backed prompt-report recorder.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `path` | `PathBuf` |  |
| `agent_run_id` | `AgentRunId` |  |
| `agent` | `String` |  |
| `model` | `String` |  |
| `state` | `Arc<Mutex<PromptReportState>>` |  |

<details><summary>Methods (8)</summary>

`new`, `path`, `next_seq`, `base`, `append_json`, `record_llm_request`, `record_assistant`, `record_tool_results`

</details>

#### `BaseEvent<'a>`  ·  _struct_  ·  derives: `Serialize`  ·  · private  ·  [L29]

Shared JSONL prefix (agent run id, agent, model) flattened into each report row.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `agent_run_id` | `&'a AgentRunId` |  |
| `agent` | `&'a str` |  |
| `model` | `&'a str` |  |

#### `LlmRequestEvent<'a>`  ·  _struct_  ·  derives: `Serialize`  ·  · private  ·  [L36]

JSONL row for a model request turn.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `BaseEvent<'a>` | `#[serde(flatten)]` |
| `event` | `&'static str` |  |
| `seq` | `u64` |  |
| `system_prompt` | `&'a str` |  |
| `messages` | `&'a [Message]` |  |
| `tools` | `&'a [ToolSpec]` |  |

#### `AssistantEvent<'a>`  ·  _struct_  ·  derives: `Serialize`  ·  · private  ·  [L47]

JSONL row for an assistant completion turn.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `BaseEvent<'a>` | `#[serde(flatten)]` |
| `event` | `&'static str` |  |
| `seq` | `u64` |  |
| `message` | `&'a Message` |  |
| `usage` | `UsageSnapshot` |  |

#### `ToolResultsEvent<'a>`  ·  _struct_  ·  derives: `Serialize`  ·  · private  ·  [L57]

JSONL row for a turn's tool results.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `BaseEvent<'a>` | `#[serde(flatten)]` |
| `event` | `&'static str` |  |
| `seq` | `u64` |  |
| `tool_results` | `&'a [eos_llm_client::ContentBlock]` |  |

---

## `eos-engine/src/agent/factory.rs`

#### `BuildQueryContextInput`  ·  _struct_  ·  [L18]

Inputs for `build_query_context`: an agent definition plus injected runtime seams (client/event source, registry, prompt, cwd, ids, metadata). Has a hand-written `Debug` impl.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `agent` | `AgentDefinition` | `pub` |
| `model` | `String` | `pub` |
| `client` | `Option<Arc<dyn LlmClient>>` | `pub` |
| `event_source` | `Option<Arc<dyn EventSource>>` | `pub` |
| `registry` | `ToolRegistry` | `pub` |
| `base_system_prompt` | `String` | `pub` |
| `max_tokens` | `u32` | `pub` |
| `cwd` | `PathBuf` | `pub` |
| `agent_run_id` | `AgentRunId` | `pub` |
| `task_id` | `Option<TaskId>` | `pub` |
| `tool_metadata` | `ExecutionMetadata` | `pub` |

**Trait impls**: `Debug`

---

## `eos-engine/src/background/supervisor.rs`

#### `BackgroundTaskStatus`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq`  ·  [L14]

Background task status.

**Variants**: `Running`, `Completed`, `Failed`, `Cancelled`, `Delivered`

<details><summary>Methods (2)</summary>

`precedence`, `is_terminal_undelivered`

</details>

#### `BackgroundTaskKind`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq`  ·  [L47]

Background task kind.

**Variants**: `Agent`, `Subagent`, `Workflow`

#### `StopMode`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq`  ·  [L58]

How a task was stopped.

**Variants**: `Cancel`, `EarlyStop`, `ParentExit`

#### `BackgroundTaskRecord`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq`  ·  [L69]

One background task record.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `task_id` | `String` | `pub` |
| `tool_name` | `String` | `pub` |
| `tool_input` | `JsonObject` | `pub` |
| `task_kind` | `BackgroundTaskKind` | `pub` |
| `status` | `BackgroundTaskStatus` | `pub` |
| `cancel_reason` | `Option<String>` | `pub` |
| `stop_mode` | `Option<StopMode>` | `pub` |
| `result` | `Option<ToolResult>` | `pub` |
| `progress_lines` | `Vec<String>` | `pub` |

<details><summary>Methods (2)</summary>

`delivered`, `outstanding`

</details>

#### `BackgroundTaskSupervisor`  ·  _struct_  ·  derives: `Debug, Default`  ·  [L107]

Single-owner background supervisor state.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `counter` | `u64` |  |
| `subagent_counter` | `u64` |  |
| `workflow_counter` | `u64` |  |
| `records` | `HashMap<String, BackgroundTaskRecord>` |  |

<details><summary>Methods (8)</summary>

`new`, `register_running`, `get`, `complete`, `cancel`, `terminate_for_parent_exit`, `inflight_count`, `push_progress`

</details>

#### `SharedSubagentSupervisor`  ·  _struct_  ·  derives: `Debug, Clone, Default`  ·  [L229]

Shared port wrapper for `run_subagent`/progress/cancel tools.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `inner` | `Arc<Mutex<BackgroundTaskSupervisor>>` |  |

**Trait impls**: `Sealed, SubagentSupervisorPort`

<details><summary>Methods (2)</summary>

`new`, `inner`

</details>

---

## `eos-engine/src/query/context.rs`

#### `EngineStream`  ·  _type alias_  ·  = `Pin<Box<dyn Stream<Item = Result<StreamEvent, EngineError>> + Send>>`  ·  [L18]

The engine stream returned by one model turn.

#### `EventSource`  ·  _trait_  ·  bases: `Send + Sync`  ·  async  ·  [L23]

A per-agent stream source; production adapts an `LlmClient`, tests can replay scripted engine events while still exercising the real loop.

**Trait items**:
- `async fn stream(&self, request: &LlmRequest) -> Result<EngineStream, EngineError>;`

#### `QueryExitReason`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`  ·  #[serde(rename_all = "snake_case")]  ·  [L34]

Why the query loop exited.

**Variants**: `ToolStop`, `TerminalNotSubmitted`

#### `QueryContext`  ·  _struct_  ·  derives: `Clone`  ·  [L43]

Mutable state for one agent query loop. Has a hand-written `Debug` impl with `finish_non_exhaustive`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `tool_registry` | `Arc<ToolRegistry>` | `pub` |
| `cwd` | `PathBuf` | `pub` |
| `model` | `String` | `pub` |
| `system_prompt` | `String` | `pub` |
| `max_tokens` | `u32` | `pub` |
| `tool_call_limit` | `u32` | `pub` |
| `agent_name` | `String` | `pub` |
| `agent_run_id` | `AgentRunId` | `pub` |
| `task_id` | `Option<TaskId>` | `pub` |
| `tool_calls_used` | `u32` | `pub` |
| `text_only_no_terminal_turns` | `u32` | `pub` |
| `tool_metadata` | `ExecutionMetadata` | `pub` |
| `enable_background_tasks` | `bool` | `pub` |
| `terminal_tools` | `BTreeSet<ToolName>` | `pub` |
| `exit_reason` | `Option<QueryExitReason>` | `pub` |
| `terminal_result` | `Option<ToolResult>` | `pub` |
| `event_source` | `Option<Arc<dyn EventSource>>` | `pub` |
| `prompt_report` | `Option<PromptReportRecorder>` | `pub` |
| `notification_rules` | `Vec<NotificationRule>` | `pub` |
| `notification_fired` | `BTreeSet<String>` | `pub` |
| `notification_state` | `JsonObject` | `pub` |

**Trait impls**: `Debug`

---

## `eos-engine/src/query/loop_.rs`

#### `QueryStream<'a>`  ·  _type alias_  ·  = `Pin<Box<dyn Stream<Item = Result<(StreamEvent, Option<UsageSnapshot>), EngineError>> + Send + 'a>>`  ·  [L18]

Query-loop output stream pairing each engine event with optional turn usage.

---

## `eos-engine/src/query/provider_source.rs`

#### `ProviderEventSource`  ·  _struct_  ·  derives: `Clone`  ·  [L15]

Provider-backed event source that adapts `LlmClient` stream events into engine `StreamEvent`s. Has a hand-written `Debug` impl.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `client` | `Arc<dyn LlmClient>` |  |

**Trait impls**: `Debug, EventSource`

<details><summary>Methods (1)</summary>

`new`

</details>

---

## `eos-engine/src/query/request.rs`

#### `QueryRunRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L9]

Built provider request plus the prompt-report sequence for the turn.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `request` | `LlmRequest` | `pub` |
| `prompt_report_seq` | `u64` | `pub` |

---

## `eos-engine/src/tool_call/dispatch.rs`

#### `ToolUseRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L20]

One model-emitted tool request.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `tool_use_id` | `ToolUseId` | `pub` |
| `name` | `String` | `pub` |
| `input` | `JsonObject` | `pub` |

#### `AssistantToolDispatchOutcome`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq`  ·  [L31]

Result of dispatching one assistant tool batch.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `tool_results` | `Vec<ContentBlock>` | `pub` |
| `terminal_result` | `Option<ToolResult>` | `pub` |
| `events` | `Vec<StreamEvent>` | `pub` |

#### `ForegroundCompletion`  ·  _struct_  ·  derives: `Debug`  ·  · private  ·  [L104]

Pairs a dispatched foreground tool call with its produced result during fan-in.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `call` | `ToolUseRequest` |  |
| `result` | `ToolResult` |  |

---

## `eos-engine/src/tool_call/streaming.rs`

#### `StreamingToolExecutor`  ·  _struct_  ·  derives: `Debug, Clone, Default`  ·  [L9]

Tracks whether mid-stream tool execution is enabled for a run.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `deferred` | `Vec<ToolName>` |  |

<details><summary>Methods (3)</summary>

`new`, `defer`, `deferred`

</details>
