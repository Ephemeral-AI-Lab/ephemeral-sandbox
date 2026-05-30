# Module `audit` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/audit/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**6 classes across 2 files.**

The audit module provides dependency-light, shared primitives for structured audit telemetry that behavior-owning domain packages (task_center, engine, sandbox, live_e2e) emit and downstream collectors consume. Its core is an immutable event model in base.py: the frozen AuditEvent carrying source, type, payload, and timestamp, plus the AuditNode correlation envelope that threads run/workflow/iteration/attempt/agent/tool identifiers, exposed through the write-only AuditSink Protocol and its NoopAuditSink. Dispatch lives in bus.py via AuditEventBus, a synchronous single-process fanout sink whose AuditDispatchError captures subscriber failures so audit collection can never interrupt the emitting domain path. A companion JSONL helper (append_jsonl_event) offers an append-only persistence path for recorders.

## Contents

- **`audit/base.py`** — `AuditNode`, `AuditEvent`, `AuditSink`, `NoopAuditSink`
- **`audit/bus.py`** — `AuditDispatchError`, `AuditEventBus`

---

## `audit/base.py`

#### `AuditNode`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L15]

Correlation envelope for audit events.

**Fields**

| name | type | default |
|------|------|---------|
| `task_center_run_id` | `str \| None` | `None` |
| `request_id` | `str \| None` | `None` |
| `workflow_id` | `str \| None` | `None` |
| `iteration_id` | `str \| None` | `None` |
| `attempt_id` | `str \| None` | `None` |
| `task_center_task_id` | `str \| None` | `None` |
| `agent_name` | `str \| None` | `None` |
| `agent_run_id` | `str \| None` | `None` |
| `sandbox_id` | `str \| None` | `None` |
| `tool_name` | `str \| None` | `None` |
| `tool_use_id` | `str \| None` | `None` |

#### `AuditEvent`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L36]

Structured audit event emitted by a behavior-owning package.

**Fields**

| name | type | default |
|------|------|---------|
| `source` | `AuditSource` |  |
| `type` | `str` |  |
| `node` | `AuditNode` |  |
| `payload` | `Mapping[str, JsonValue]` | `field(default_factory=dict)` |
| `correlation_id` | `str \| None` | `None` |
| `ts` | `datetime` | `field(default_factory=lambda: datetime.now(UTC))` |

#### `AuditSink`  ·  _protocol_  ·  bases: `Protocol`  ·  [L47]

Write-only audit side channel.

<details><summary>Methods (1)</summary>

`publish`

</details>

#### `NoopAuditSink`  ·  _class_  ·  [L53]

Audit sink used when collection is disabled.

<details><summary>Methods (1)</summary>

`publish`

</details>

---

## `audit/bus.py`

#### `AuditDispatchError`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L15]

Captured subscriber failure.

**Fields**

| name | type | default |
|------|------|---------|
| `event` | `AuditEvent` |  |
| `error` | `BaseException` |  |

#### `AuditEventBus`  ·  _class_  ·  bases: `AuditSink`  ·  [L22]

Single-process synchronous fanout bus.

**Instance attributes**: `_handlers`, `errors`

<details><summary>Methods (3)</summary>

`__init__`, `publish`, `subscribe`

</details>

