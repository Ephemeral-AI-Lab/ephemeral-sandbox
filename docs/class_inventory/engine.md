# Module `engine` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/engine/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**20 classes across 11 files.**

The engine module owns agent execution: the tool-aware query loop that streams provider events, detects and runs tool calls mid-stream, enforces the terminal-tool exit contract (success on a successful terminal tool, failure at the 1.5x tool-call hard ceiling), and drives the per-agent run lifecycle shared by the chat path and the run_subagent tool. Its query-loop classes (QueryContext, QueryExitReason, the EventSource seam, plus the run request/history builders) carry per-turn loop state and the mock-vs-production stream source. Its tool-call classes handle dispatch coordination and streamed foreground execution (StreamingToolExecutor / StreamingToolRun with defer-to-background predicates) and per-call phase sampling for slow-tail latency flush (phase buffers and per-tool rolling-P95 windows). A background group (BackgroundTaskSupervisor, BackgroundTaskRecord/Status with a precedence-latched terminal-status machine, plus background dispatch and history) supervises async tool and subagent tasks with cancellation, heartbeats, and completed-result collection.

## Contents

- **`engine/agent/factory.py`** — `EphemeralAgent`
- **`engine/agent/lifecycle.py`** — `EphemeralRunResult`
- **`engine/agent/run_tracker.py`** — `AgentRunTracker`
- **`engine/background/history.py`** — `_BackgroundSnapshot`
- **`engine/background/task_supervisor.py`** — `BackgroundTaskStatus`, `BackgroundTaskRecord`, `BackgroundTaskSupervisor`
- **`engine/query/context.py`** — `QueryExitReason`, `QueryContext`
- **`engine/query/loop.py`** — `_ProviderStreamAccumulator`
- **`engine/query/request.py`** — `QueryRunRequest`
- **`engine/tool_call/dispatch.py`** — `AssistantToolDispatchOutcome`
- **`engine/tool_call/phase_buffer.py`** — `_PhaseRecord`, `_PhaseBuffer`, `_RollingWindow`, `_RollingWindowRegistry`, `FinishedPhaseDecision`
- **`engine/tool_call/streaming.py`** — `StreamingToolRunPhase`, `StreamingToolRun`, `StreamingToolExecutor`

---

## `engine/agent/factory.py`

#### `EphemeralAgent`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L51]

A short-lived agent that handles one user request then dies.

**Fields**

| name | type | default |
|------|------|---------|
| `agent_name` | `str` |  |
| `query_context` | `QueryContext` |  |
| `model` | `str` |  |
| `_messages` | `list[Message]` |  |
| `total_usage` | `UsageSnapshot \| None` | `None` |

<details><summary>Methods (3)</summary>

`messages`, `run`, `close`

</details>

---

## `engine/agent/lifecycle.py`

#### `EphemeralRunResult`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L46]

Outcome of one :func:`run_ephemeral_agent` invocation.

**Fields**

| name | type | default |
|------|------|---------|
| `status` | `EphemeralRunStatus` |  |
| `error` | `str \| None` |  |
| `terminal_result` | `ToolResult \| None` |  |
| `agent_name` | `str` |  |
| `tool_call_count` | `int` |  |

---

## `engine/agent/run_tracker.py`

#### `AgentRunTracker`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L51]

Handle wrapping a persisted ``agent_run`` row.

**Fields**

| name | type | default |
|------|------|---------|
| `agent_run_id` | `str \| None` |  |
| `agent_name` | `str` |  |
| `_finished` | `bool` | `field(default=False, init=False)` |

<details><summary>Methods (2)</summary>

`create`, `finish`

</details>

---

## `engine/background/history.py`

#### `_BackgroundSnapshot`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L31]

Parsed view of a background-task status snapshot extracted from a tool-result block's metadata.

**Fields**

| name | type | default |
|------|------|---------|
| `kind` | `str` |  |
| `scope` | `str` |  |
| `statuses` | `list[dict[str, Any]]` |  |
| `elapsed_seconds` | `int \| float \| None` |  |

---

## `engine/background/task_supervisor.py`

#### `BackgroundTaskStatus`  ·  _enum_  ·  bases: `StrEnum`  ·  [L31]

Lifecycle states for a tracked background task.

**Enum members**: `RUNNING = 'running'`, `COMPLETED = 'completed'`, `FAILED = 'failed'`, `CANCELLED = 'cancelled'`, `DELIVERED = 'delivered'`

#### `BackgroundTaskRecord`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L76]

In-memory record for one engine-owned background task.

**Fields**

| name | type | default |
|------|------|---------|
| `task_id` | `str` |  |
| `tool_name` | `str` |  |
| `tool_input` | `dict[str, Any]` |  |
| `asyncio_task` | `asyncio.Task[ToolResult]` |  |
| `task_type` | `str` | `DEFAULT_BACKGROUND_TASK_TYPE` |
| `agent_id` | `str \| None` | `None` |
| `uses_sandbox` | `bool` | `False` |
| `sandbox_id` | `str \| None` | `None` |
| `sandbox_invocation_id` | `str \| None` | `None` |
| `status` | `BackgroundTaskStatus` | `BackgroundTaskStatus.RUNNING` |
| `cancel_reason` | `str \| None` | `None` |
| `stop_mode` | `str \| None` | `None` |
| `completion_mode` | `str \| None` | `None` |
| `result` | `ToolResult \| None` | `None` |
| `started_at` | `float` | `field(default_factory=time.monotonic)` |
| `progress_lines` | `list[str]` | `field(default_factory=list)` |
| `progress_provider` | `Callable[[int], str] \| None` | `None` |
| `_terminal_lock` | `threading.Lock` | `field(default_factory=threading.Lock)` |

#### `BackgroundTaskSupervisor`  ·  _class_  ·  [L185]

Supervise async background tasks launched by the query loop.

**Instance attributes**: `_tasks`, `_alias_counter`, `_heartbeat_task`

<details><summary>Methods (22)</summary>

`__init__`, `next_alias`, `launch`, `collect_completed`, `iter_all`, `iter_running`, `has_pending`, `count_by_agent`, `append_progress`, `set_progress_provider`, `make_progress_callback`, `cancel`, `cancel_by_agent`, `get_task`, `cancel_all`, `_mark_cancelled`, `_apply_terminal_status_transition`, `_cancel_sandbox_invocation_if_bound`, `_ensure_heartbeat_task`, `_stop_heartbeat_if_idle`, `_heartbeat_loop`, `_running_sandbox_invocation_ids`

</details>

---

## `engine/query/context.py`

#### `QueryExitReason`  ·  _enum_  ·  bases: `StrEnum`  ·  [L31]

Why the query loop exited.

**Enum members**: `TOOL_STOP = 'tool_stop'`, `TERMINAL_NOT_SUBMITTED = 'terminal_not_submitted'`

#### `QueryContext`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L39]

Mutable per-agent state and configuration threaded through the query loop's turns.

**Fields**

| name | type | default |
|------|------|---------|
| `api_client` | `SupportsStreamingMessages` |  |
| `tool_registry` | `ToolRegistry` |  |
| `cwd` | `Path` |  |
| `model` | `str` |  |
| `system_prompt` | `str` |  |
| `max_tokens` | `int` |  |
| `tool_call_limit` | `int` |  |
| `agent_name` | `str` | `''` |
| `run_id` | `str` | `''` |
| `task_center_task_id` | `str` | `''` |
| `tool_calls_used` | `int` | `0` |
| `text_only_no_terminal_turns` | `int` | `0` |
| `tool_metadata` | `ExecutionMetadata \| None` | `None` |
| `enable_background_tasks` | `bool` | `False` |
| `terminal_tools` | `set[str]` | `field(default_factory=set)` |
| `exit_reason` | `QueryExitReason \| None` | `None` |
| `terminal_result` | `ToolResult \| None` | `None` |
| `event_source` | `EventSource \| None` | `None` |
| `prompt_report_recorder` | `PromptReportRecorder \| None` | `None` |
| `notification_rules` | `list[NotificationRule]` | `field(default_factory=list)` |
| `notification_fired` | `set[str]` | `field(default_factory=set)` |
| `notification_state` | `dict[str, Any]` | `field(default_factory=dict)` |

---

## `engine/query/loop.py`

#### `_ProviderStreamAccumulator`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L87]

Mutable accumulator for one provider stream.

**Fields**

| name | type | default |
|------|------|---------|
| `final_message` | `Message \| None` | `None` |
| `usage` | `UsageSnapshot` | `field(default_factory=UsageSnapshot)` |
| `streamed_tool_use_ids` | `set[str]` | `field(default_factory=set)` |

---

## `engine/query/request.py`

#### `QueryRunRequest`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L20]

Bundles a built provider request with the prompt-report recorder and its sequence number for one turn.

**Fields**

| name | type | default |
|------|------|---------|
| `request` | `MessageRequest` |  |
| `prompt_report` | `PromptReportRecorder` |  |
| `prompt_report_seq` | `int` |  |

---

## `engine/tool_call/dispatch.py`

#### `AssistantToolDispatchOutcome`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L120]

Result of dispatching an assistant turn's tool calls: tool results, any terminal result, and emitted events.

**Fields**

| name | type | default |
|------|------|---------|
| `tool_results` | `list[ToolResultBlock]` |  |
| `terminal_result` | `ToolResult \| None` | `None` |
| `events` | `list[StreamEvent]` | `field(default_factory=list)` |

---

## `engine/tool_call/phase_buffer.py`

#### `_PhaseRecord`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L51]

One recorded phase name paired with its duration for a single tool call.

**Fields**

| name | type | default |
|------|------|---------|
| `phase` | `str` |  |
| `duration_ms` | `float` |  |

#### `_PhaseBuffer`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L57]

Per-call buffer accumulating phase records for one in-progress foreground tool dispatch.

**Fields**

| name | type | default |
|------|------|---------|
| `tool_use_id` | `str` |  |
| `tool_name` | `str` |  |
| `entries` | `deque[_PhaseRecord]` | `field(default_factory=lambda: deque(maxlen=_PHASE_BUFFER_MAX))` |

#### `_RollingWindow`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L71]

Lock-protected sliding window of recent tool durations that computes P95 for the slow-tail flush decision.

**Fields**

| name | type | default |
|------|------|---------|
| `samples` | `deque[float]` | `field(default_factory=lambda: deque(maxlen=_ROLLING_WINDOW_SIZE))` |
| `lock` | `threading.Lock` | `field(default_factory=threading.Lock)` |

<details><summary>Methods (1)</summary>

`append_and_p95`

</details>

#### `_RollingWindowRegistry`  ·  _class_  ·  [L101]

LRU-capped registry mapping ``tool_name`` -> :class:`_RollingWindow`.

**Instance attributes**: `_cap`, `_windows`, `_lock`

<details><summary>Methods (3)</summary>

`__init__`, `get`, `clear`

</details>

#### `FinishedPhaseDecision`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L160]

Decision returned when finishing a phase buffer: whether to flush phase events plus the totals rollup.

**Fields**

| name | type | default |
|------|------|---------|
| `flush` | `bool` |  |
| `cold_window` | `bool` |  |
| `phases` | `tuple[_PhaseRecord, ...]` |  |
| `rollup` | `dict[str, float]` |  |

---

## `engine/tool_call/streaming.py`

#### `StreamingToolRunPhase`  ·  _enum_  ·  bases: `StrEnum`  ·  [L32]

Internal lifecycle for a streamed foreground tool call.

**Enum members**: `QUEUED = 'queued'`, `EXECUTING = 'executing'`, `COMPLETED = 'completed'`, `YIELDED = 'yielded'`

#### `StreamingToolRun`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L42]

Tracks the lifecycle and result of one tool started mid-stream by the streaming executor.

**Fields**

| name | type | default |
|------|------|---------|
| `tool_use_id` | `str` |  |
| `name` | `str` |  |
| `input` | `dict[str, Any]` |  |
| `phase` | `StreamingToolRunPhase` | `StreamingToolRunPhase.QUEUED` |
| `task` | `asyncio.Task[None] \| None` | `None` |
| `progress_lines` | `list[str]` | `field(default_factory=list)` |
| `result` | `ToolResult \| None` | `None` |
| `cancelled` | `bool` | `False` |
| `cancel_reason` | `str` | `''` |

#### `StreamingToolExecutor`  ·  _class_  ·  [L78]

Executes tools as they arrive mid-stream with progress support.

**Instance attributes**: `_tool_registry`, `_context`, `_should_defer`, `_tools`, `_events`

<details><summary>Methods (9)</summary>

`__init__`, `add_tool`, `get_events`, `get_progress`, `get_remaining`, `_start_tool`, `_execute_tool`, `cancel_all`, `_emit_event`

</details>

