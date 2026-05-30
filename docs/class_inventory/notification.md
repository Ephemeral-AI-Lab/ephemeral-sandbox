# Module `notification` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/notification/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**3 classes across 2 files.**

The `notification` module owns engine-generated `<system-reminder>` content surfaced to both the user and the agent during a run. Its runtime layer centers on `SystemNotification` (a frozen notification value) and the run-scoped `SystemNotificationService`, which buffers notifications, emits them as stream events for live agent runs, and drains transcript-bound blocks for standalone tool execution. Its `rules` layer defines the declarative `NotificationRule` (a named `trigger`/`body` pair with fire-once dedup), evaluated in list order each model turn by `dispatch_rules`; concrete reminders—such as terminal-tool-call enforcement and tool-call budget-tier warnings—are produced by the rule factories. A small `metadata` helper serializes notification blocks into tool-result metadata for backwards compatibility.

## Contents

- **`notification/rules/model.py`** — `NotificationRule`
- **`notification/runtime.py`** — `SystemNotification`, `SystemNotificationService`

---

## `notification/rules/model.py`

#### `NotificationRule`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L28]

Declarative rule for emitting a `<system-reminder>` block.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `body` | `RuleBody` |  |
| `trigger` | `RuleTrigger` |  |
| `fire_once` | `bool` | `True` |

---

## `notification/runtime.py`

#### `SystemNotification`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L14]

Engine-generated notification visible to the user and the agent.

**Fields**

| name | type | default |
|------|------|---------|
| `text` | `str` |  |
| `agent_name` | `str` | `''` |
| `run_id` | `str` | `''` |

#### `SystemNotificationService`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L23]

Run-scoped notification sink for hooks, tools, and runtime code.

**Fields**

| name | type | default |
|------|------|---------|
| `emit` | `Callable[[SystemNotification], Awaitable[None]] \| None` | `None` |
| `_registered_agent_run` | `bool` | `field(default=False, init=False, repr=False)` |
| `_notifications` | `list[SystemNotificationBlock]` | `field(default_factory=list, repr=False)` |
| `_events` | `list[SystemNotification]` | `field(default_factory=list, init=False, repr=False)` |

<details><summary>Methods (5)</summary>

`has_registered_agent_run`, `register_agent_run`, `notify_system`, `flush_events`, `pop_pending_notifications`

</details>

