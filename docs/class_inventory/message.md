# Module `message` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/message/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**19 classes across 4 files.**

The `message` module owns the query engine's conversation data model and its streaming event protocol, plus the consumers that render and persist that stream. `message.py` defines the Pydantic `Message` and its typed `ContentBlock` union (`TextBlock`, `ThinkingBlock`, `ToolUseBlock`, `ToolResultBlock`, `SystemNotificationBlock`) together with provider-wire serialization (`serialize_content_block`, `to_api_param`) and Anthropic-SDK parsing, including the `is_terminal` marker and `<system-reminder>` flattening. `events.py` declares the frozen `StreamEvent` dataclasses yielded during a run — thinking/text/tool-use deltas, tool-execution start/complete/progress/cancel, assistant-message-complete, and background-task-started — each carrying `(agent_name, run_id)` identity so concurrent agents can be demultiplexed. The two consumer classes both key off those identity fields: `MultiAgentEventPrinter` buffers per-agent lanes and color-codes column-aligned console output, while `AgentMessageJsonlRecorder` (with a module-level registry) appends completed messages to a replayable JSONL transcript.

## Contents

- **`message/agent_message_recorder.py`** — `AgentMessageJsonlRecorder`
- **`message/event_printer.py`** — `_AgentTotals`, `_LaneState`, `MultiAgentEventPrinter`
- **`message/events.py`** — `ThinkingDeltaEvent`, `AssistantTextDeltaEvent`, `AssistantMessageCompleteEvent`, `ToolUseDeltaEvent`, `ToolExecutionStartedEvent`, `ToolExecutionCompletedEvent`, `ToolExecutionProgressEvent`, `ToolExecutionCancelledEvent`, `BackgroundTaskStartedEvent`
- **`message/message.py`** — `TextBlock`, `ToolUseBlock`, `ThinkingBlock`, `ToolResultBlock`, `SystemNotificationBlock`, `Message`

---

## `message/agent_message_recorder.py`

#### `AgentMessageJsonlRecorder`  ·  _class_  ·  [L28]

Record completed conversation messages as append-only JSONL.

**Instance attributes**: `_path`, `_base_event`, `_seq`, `_initial_messages_recorded`, `_thinking`, `_text`

<details><summary>Methods (12)</summary>

`__init__`, `path`, `emit`, `record_initial_messages`, `flush`, `_thinking_for`, `_text_for`, `_flush_lane`, `_flush_thinking`, `_flush_text`, `_record`, `_record_message`

</details>

---

## `message/event_printer.py`

#### `_AgentTotals`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L152]

Tracks an agent's display color and running tool-call count for the multi-agent event printer.

**Fields**

| name | type | default |
|------|------|---------|
| `color` | `str` | `''` |
| `tool_calls` | `int` | `0` |

#### `_LaneState`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L158]

Buffers an agent-run lane's streamed thinking and assistant-text deltas until they are flushed.

**Fields**

| name | type | default |
|------|------|---------|
| `thinking_buf` | `list[str]` | `field(default_factory=list)` |
| `text_buf` | `list[str]` | `field(default_factory=list)` |

#### `MultiAgentEventPrinter`  ·  _class_  ·  [L163]

Format and print ``StreamEvent``s to stdout (or any sink).

**Instance attributes**: `_color`, `_tag_width`, `_sink`, `_timestamps`, `_start`, `_agent_totals`, `_lanes`, `_palette_idx`

<details><summary>Methods (10)</summary>

`__init__`, `emit`, `flush`, `_agent_totals_for`, `_lane_for`, `_flush_lane`, `_flush_buffers`, `_line`, `_agent_tag`, `_c`

</details>

---

## `message/events.py`

#### `ThinkingDeltaEvent`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L23]

Incremental thinking/reasoning content from the model.

**Fields**

| name | type | default |
|------|------|---------|
| `text` | `str` |  |
| `agent_name` | `str` | `''` |
| `run_id` | `str` | `''` |

#### `AssistantTextDeltaEvent`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L32]

Incremental assistant text.

**Fields**

| name | type | default |
|------|------|---------|
| `text` | `str` |  |
| `agent_name` | `str` | `''` |
| `run_id` | `str` | `''` |

#### `AssistantMessageCompleteEvent`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L41]

Completed assistant message.

**Fields**

| name | type | default |
|------|------|---------|
| `message` | `Message` |  |
| `usage` | `UsageSnapshot` |  |
| `stop_reason` | `str \| None` | `None` |
| `agent_name` | `str` | `''` |
| `run_id` | `str` | `''` |

#### `ToolUseDeltaEvent`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L52]

A tool_use content block arrived mid-stream (pre-execution).

**Fields**

| name | type | default |
|------|------|---------|
| `tool_use_id` | `str` |  |
| `name` | `str` |  |
| `input` | `dict[str, Any]` |  |
| `agent_name` | `str` | `''` |
| `run_id` | `str` | `''` |

#### `ToolExecutionStartedEvent`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L63]

The engine is about to execute a tool.

**Fields**

| name | type | default |
|------|------|---------|
| `tool_name` | `str` |  |
| `tool_input` | `dict[str, Any]` |  |
| `tool_use_id` | `str` | `''` |
| `agent_name` | `str` | `''` |
| `run_id` | `str` | `''` |

#### `ToolExecutionCompletedEvent`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L74]

A tool has finished executing.

**Fields**

| name | type | default |
|------|------|---------|
| `tool_name` | `str` |  |
| `output` | `str` |  |
| `is_error` | `bool` | `False` |
| `tool_use_id` | `str` | `''` |
| `metadata` | `dict[str, Any]` | `field(default_factory=dict)` |
| `is_terminal` | `bool` | `False` |
| `agent_name` | `str` | `''` |
| `run_id` | `str` | `''` |

#### `ToolExecutionProgressEvent`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L88]

Progress update from a running tool.

**Fields**

| name | type | default |
|------|------|---------|
| `tool_use_id` | `str` |  |
| `tool_name` | `str` |  |
| `output` | `str` |  |
| `agent_name` | `str` | `''` |
| `run_id` | `str` | `''` |

#### `ToolExecutionCancelledEvent`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L104]

A tool was cancelled by LLM abort signal.

**Fields**

| name | type | default |
|------|------|---------|
| `tool_use_id` | `str` |  |
| `tool_name` | `str` |  |
| `reason` | `str` |  |
| `agent_name` | `str` | `''` |
| `run_id` | `str` | `''` |

#### `BackgroundTaskStartedEvent`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L115]

A tool has been launched as a background task.

**Fields**

| name | type | default |
|------|------|---------|
| `task_id` | `str` |  |
| `tool_name` | `str` |  |
| `tool_input` | `dict[str, Any]` |  |
| `agent_name` | `str` | `''` |
| `run_id` | `str` | `''` |

---

## `message/message.py`

#### `TextBlock`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L14]

Plain text content.

**Fields**

| name | type | default |
|------|------|---------|
| `type` | `Literal['text']` | `'text'` |
| `text` | `str` |  |

#### `ToolUseBlock`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L21]

A request from the model to execute a named tool.

**Fields**

| name | type | default |
|------|------|---------|
| `type` | `Literal['tool_use']` | `'tool_use'` |
| `tool_use_id` | `str` | `Field(default_factory=lambda: f'toolu_{uuid4().hex}')` |
| `name` | `str` |  |
| `input` | `dict[str, Any]` | `Field(default_factory=dict)` |

#### `ThinkingBlock`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L30]

Model reasoning / chain-of-thought content.

**Fields**

| name | type | default |
|------|------|---------|
| `type` | `Literal['thinking']` | `'thinking'` |
| `text` | `str` |  |

#### `ToolResultBlock`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L37]

Tool result content sent back to the model.

**Fields**

| name | type | default |
|------|------|---------|
| `type` | `Literal['tool_result']` | `'tool_result'` |
| `tool_use_id` | `str` |  |
| `content` | `str` |  |
| `is_error` | `bool` | `False` |
| `metadata` | `dict[str, Any]` | `Field(default_factory=dict)` |
| `is_terminal` | `bool` | `False` |

#### `SystemNotificationBlock`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L51]

Engine-generated reminder for the model wrapped in tags.

**Fields**

| name | type | default |
|------|------|---------|
| `type` | `Literal['system_notification']` | `'system_notification'` |
| `text` | `str` |  |

#### `Message`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L87]

A single assistant or user message.

**Fields**

| name | type | default |
|------|------|---------|
| `role` | `Literal['user', 'assistant']` |  |
| `content` | `list[ContentBlock]` | `Field(default_factory=list)` |

<details><summary>Methods (7)</summary>

`from_user_text`, `assistant_text`, `system_notifications`, `system_notification_text`, `thinking`, `tool_uses`, `to_api_param`

</details>

