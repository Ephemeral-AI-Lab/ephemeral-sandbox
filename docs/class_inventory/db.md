# Module `db` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/db/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**18 classes across 16 files.**

The `db` module is the SQLAlchemy persistence layer for EphemeralOS's TaskCenter control plane, owning the relational schema, engine/session bootstrap, and a lightweight in-place migration runner (`engine.py` handles legacy table/column renames, missing-column patching, and SQLite table rebuilds; persistence is optional and disabled when no DB URL is configured). Its first class group is the ORM record models under `db/models/` (all subclassing the shared `Base`): the request/run/task hierarchy in `task_center.py`, the three harness axes `WorkflowRecord`/`IterationRecord`/`AttemptRecord`, plus `AgentRunRecord`, `ContextPacketRecord`, and `ModelRegistrationRecord`. Its second group is the per-entity store classes under `db/stores/` (e.g. `AttemptStore`, `WorkflowStore`, `TaskCenterStore`), all built on `SyncStoreMixin`'s lazy session-factory init, which provide CRUD over those records and return frozen `task_center` DTOs rather than leaking ORM objects.

## Contents

- **`db/base.py`** — `Base`
- **`db/models/agent_run.py`** — `AgentRunRecord`
- **`db/models/attempt.py`** — `AttemptRecord`
- **`db/models/context_packet.py`** — `ContextPacketRecord`
- **`db/models/iteration.py`** — `IterationRecord`
- **`db/models/model_registration.py`** — `ModelRegistrationRecord`
- **`db/models/task_center.py`** — `TaskCenterRequestRecord`, `TaskCenterRunRecord`, `TaskCenterTaskRecord`
- **`db/models/workflow.py`** — `WorkflowRecord`
- **`db/stores/agent_run_store.py`** — `AgentRunStore`
- **`db/stores/attempt_store.py`** — `AttemptStore`
- **`db/stores/base.py`** — `SyncStoreMixin`
- **`db/stores/context_packet_store.py`** — `ContextPacketStore`
- **`db/stores/iteration_store.py`** — `IterationStore`
- **`db/stores/model_store.py`** — `ModelStore`
- **`db/stores/task_center_store.py`** — `TaskCenterStore`
- **`db/stores/workflow_store.py`** — `WorkflowStore`

---

## `db/base.py`

#### `Base`  ·  _class_  ·  bases: `DeclarativeBase`  ·  [L6]

Base class for all ORM models.

---

## `db/models/agent_run.py`

#### `AgentRunRecord`  ·  _class_  ·  bases: `Base`  ·  [L17]

One agent execution for one TaskCenter task.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `Mapped[str]` | `mapped_column(String(36), primary_key=True)` |
| `task_id` | `Mapped[str]` | `mapped_column(String(96), ForeignKey('task_center_tasks.id', ondelete='CASCADE'), unique=True, index=True)` |
| `agent_name` | `Mapped[str]` | `mapped_column(String(128))` |
| `message_history` | `Mapped[list \| None]` | `mapped_column(JSON, nullable=True)` |
| `terminal_tool_result` | `Mapped[dict \| None]` | `mapped_column(JSON, nullable=True)` |
| `token_count` | `Mapped[int]` | `mapped_column(Integer, default=0)` |
| `error` | `Mapped[str \| None]` | `mapped_column(Text, nullable=True)` |
| `created_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC))` |
| `finished_at` | `Mapped[datetime \| None]` | `mapped_column(DateTime(timezone=True), nullable=True)` |
| `task` | `Mapped['TaskCenterTaskRecord']` | `relationship('TaskCenterTaskRecord', back_populates='agent_run')` |

**Class variables**: `__tablename__ = 'agent_runs'`

<details><summary>Methods (1)</summary>

`__repr__`

</details>

---

## `db/models/attempt.py`

#### `AttemptRecord`  ·  _class_  ·  bases: `Base`  ·  [L17]

Persisted Attempt (horizontal retry axis).

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `Mapped[str]` | `mapped_column(String(36), primary_key=True)` |
| `iteration_id` | `Mapped[str]` | `mapped_column(String(36), ForeignKey('iterations.id', ondelete='CASCADE'), index=True)` |
| `attempt_sequence_no` | `Mapped[int]` | `mapped_column(Integer)` |
| `stage` | `Mapped[str]` | `mapped_column(String(16))` |
| `status` | `Mapped[str]` | `mapped_column(String(16))` |
| `planner_task_id` | `Mapped[str \| None]` | `mapped_column(String(96), nullable=True)` |
| `task_specification` | `Mapped[str \| None]` | `mapped_column(Text, nullable=True)` |
| `evaluation_criteria` | `Mapped[list[str]]` | `mapped_column(JSON, default=list)` |
| `generator_task_ids` | `Mapped[list[str]]` | `mapped_column(JSON, default=list)` |
| `evaluator_task_id` | `Mapped[str \| None]` | `mapped_column(String(96), nullable=True)` |
| `deferred_goal` | `Mapped[str \| None]` | `mapped_column(Text, nullable=True)` |
| `fail_reason` | `Mapped[str \| None]` | `mapped_column(String(48), nullable=True)` |
| `created_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC))` |
| `updated_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC), onupdate=lambda: datetime.now(UTC))` |
| `closed_at` | `Mapped[datetime \| None]` | `mapped_column(DateTime(timezone=True), nullable=True)` |

**Class variables**: `__tablename__ = 'attempts'`, `__table_args__`

<details><summary>Methods (1)</summary>

`__repr__`

</details>

---

## `db/models/context_packet.py`

#### `ContextPacketRecord`  ·  _class_  ·  bases: `Base`  ·  [L17]

Immutable persisted view of a :class:`ContextPacket`.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `Mapped[str]` | `mapped_column(String(36), primary_key=True)` |
| `target_role` | `Mapped[str]` | `mapped_column(String(32))` |
| `target_id` | `Mapped[str \| None]` | `mapped_column(String(255), nullable=True)` |
| `canonical_refs` | `Mapped[dict]` | `mapped_column(JSON)` |
| `blocks` | `Mapped[list]` | `mapped_column(JSON, default=list)` |
| `metadata_payload` | `Mapped[dict]` | `mapped_column('metadata', JSON, default=dict)` |
| `source_ids` | `Mapped[list]` | `mapped_column(JSON, default=list)` |
| `created_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC))` |

**Class variables**: `__tablename__ = 'context_packets'`

<details><summary>Methods (1)</summary>

`__repr__`

</details>

---

## `db/models/iteration.py`

#### `IterationRecord`  ·  _class_  ·  bases: `Base`  ·  [L17]

Persisted Iteration (vertical continuation axis).

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `Mapped[str]` | `mapped_column(String(36), primary_key=True)` |
| `workflow_id` | `Mapped[str]` | `mapped_column(String(36), ForeignKey('workflows.id', ondelete='CASCADE'), index=True)` |
| `sequence_no` | `Mapped[int]` | `mapped_column(Integer)` |
| `creation_reason` | `Mapped[str]` | `mapped_column(String(32))` |
| `goal` | `Mapped[str]` | `mapped_column(Text)` |
| `attempt_budget` | `Mapped[int]` | `mapped_column(Integer)` |
| `status` | `Mapped[str]` | `mapped_column(String(16))` |
| `attempt_ids` | `Mapped[list[str]]` | `mapped_column(JSON, default=list)` |
| `deferred_goal` | `Mapped[str \| None]` | `mapped_column(Text, nullable=True)` |
| `created_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC))` |
| `updated_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC), onupdate=lambda: datetime.now(UTC))` |
| `closed_at` | `Mapped[datetime \| None]` | `mapped_column(DateTime(timezone=True), nullable=True)` |
| `task_specification` | `Mapped[str \| None]` | `mapped_column(Text, nullable=True)` |
| `task_summary` | `Mapped[str \| None]` | `mapped_column(Text, nullable=True)` |

**Class variables**: `__tablename__ = 'iterations'`, `__table_args__`

<details><summary>Methods (1)</summary>

`__repr__`

</details>

---

## `db/models/model_registration.py`

#### `ModelRegistrationRecord`  ·  _class_  ·  bases: `Base`  ·  [L13]

A registered LLM model with its configuration and API credentials.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `Mapped[int]` | `mapped_column(Integer, primary_key=True, autoincrement=True)` |
| `key` | `Mapped[str]` | `mapped_column(String(128), unique=True, nullable=False)` |
| `label` | `Mapped[str]` | `mapped_column(String(256), nullable=False)` |
| `class_path` | `Mapped[str]` | `mapped_column(String(512), nullable=False)` |
| `kwargs_json` | `Mapped[str]` | `mapped_column(Text, nullable=False, default='{}')` |
| `is_active` | `Mapped[bool]` | `mapped_column(Boolean, default=False)` |
| `created_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC))` |
| `updated_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC), onupdate=lambda: datetime.now(UTC))` |

**Class variables**: `__tablename__ = 'model_registrations'`

<details><summary>Methods (1)</summary>

`__repr__`

</details>

---

## `db/models/task_center.py`

#### `TaskCenterRequestRecord`  ·  _class_  ·  bases: `Base`  ·  [L21]

ORM record persisting a top-level TaskCenter request with its working directory, sandbox, and prompt.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `Mapped[str]` | `mapped_column(String(36), primary_key=True)` |
| `cwd` | `Mapped[str]` | `mapped_column(String(1024))` |
| `sandbox_id` | `Mapped[str \| None]` | `mapped_column(String(128), nullable=True)` |
| `request_prompt` | `Mapped[str]` | `mapped_column(Text)` |
| `created_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC))` |
| `updated_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC), onupdate=lambda: datetime.now(UTC))` |
| `runs` | `Mapped[list['TaskCenterRunRecord']]` | `relationship('TaskCenterRunRecord', back_populates='request', cascade='all, delete-orphan')` |

**Class variables**: `__tablename__ = 'task_center_requests'`

<details><summary>Methods (1)</summary>

`__repr__`

</details>

#### `TaskCenterRunRecord`  ·  _class_  ·  bases: `Base`  ·  [L47]

ORM record persisting one execution run of a TaskCenter request, tracking status and timing.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `Mapped[str]` | `mapped_column(String(36), primary_key=True)` |
| `request_id` | `Mapped[str]` | `mapped_column(String(36), ForeignKey('task_center_requests.id', ondelete='CASCADE'), index=True)` |
| `status` | `Mapped[str]` | `mapped_column(String(32), default='running')` |
| `started_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC))` |
| `finished_at` | `Mapped[datetime \| None]` | `mapped_column(DateTime(timezone=True), nullable=True)` |
| `request` | `Mapped[TaskCenterRequestRecord]` | `relationship(back_populates='runs')` |
| `tasks` | `Mapped[list['TaskCenterTaskRecord']]` | `relationship('TaskCenterTaskRecord', back_populates='run', cascade='all, delete-orphan')` |

**Class variables**: `__tablename__ = 'task_center_runs'`

<details><summary>Methods (1)</summary>

`__repr__`

</details>

#### `TaskCenterTaskRecord`  ·  _class_  ·  bases: `Base`  ·  [L73]

ORM record persisting a single role-scoped task within a run, including status, context, and recovery wiring.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `Mapped[str]` | `mapped_column(String(96), primary_key=True)` |
| `task_center_run_id` | `Mapped[str]` | `mapped_column(String(36), ForeignKey('task_center_runs.id', ondelete='CASCADE'), index=True)` |
| `role` | `Mapped[str]` | `mapped_column(String(32))` |
| `agent_name` | `Mapped[str \| None]` | `mapped_column(String(128), nullable=True)` |
| `context_message` | `Mapped[str]` | `mapped_column(Text)` |
| `status` | `Mapped[str]` | `mapped_column(String(32))` |
| `summaries` | `Mapped[list[dict]]` | `mapped_column(JSON, default=list)` |
| `needs` | `Mapped[list[str]]` | `mapped_column(JSON, default=list)` |
| `task_center_attempt_id` | `Mapped[str \| None]` | `mapped_column(String(96), nullable=True)` |
| `context_packet_id` | `Mapped[str \| None]` | `mapped_column(String(36), nullable=True)` |
| `fix_target_id` | `Mapped[str \| None]` | `mapped_column(String(96), nullable=True)` |
| `spawn_reason` | `Mapped[str \| None]` | `mapped_column(String(64), nullable=True)` |
| `created_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC))` |
| `updated_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC), onupdate=lambda: datetime.now(UTC))` |
| `run` | `Mapped[TaskCenterRunRecord]` | `relationship(back_populates='tasks')` |
| `agent_run` | `Mapped['AgentRunRecord \| None']` | `relationship('AgentRunRecord', back_populates='task', uselist=False)` |

**Class variables**: `__tablename__ = 'task_center_tasks'`

<details><summary>Methods (1)</summary>

`__repr__`

</details>

---

## `db/models/workflow.py`

#### `WorkflowRecord`  ·  _class_  ·  bases: `Base`  ·  [L18]

Persisted Workflow (origin axis).

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `Mapped[str]` | `mapped_column(String(36), primary_key=True)` |
| `task_center_run_id` | `Mapped[str]` | `mapped_column(String(36), ForeignKey('task_center_runs.id', ondelete='CASCADE'), index=True)` |
| `origin_kind` | `Mapped[str \| None]` | `mapped_column(String(16), nullable=True)` |
| `requested_by_task_id` | `Mapped[str \| None]` | `mapped_column(String(96), nullable=True, index=True)` |
| `goal` | `Mapped[str]` | `mapped_column(Text)` |
| `status` | `Mapped[str]` | `mapped_column(String(16))` |
| `iteration_ids` | `Mapped[list[str]]` | `mapped_column(JSON, default=list)` |
| `final_outcome` | `Mapped[dict \| None]` | `mapped_column(JSON, nullable=True)` |
| `created_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC))` |
| `updated_at` | `Mapped[datetime]` | `mapped_column(DateTime(timezone=True), default=lambda: datetime.now(UTC), onupdate=lambda: datetime.now(UTC))` |
| `closed_at` | `Mapped[datetime \| None]` | `mapped_column(DateTime(timezone=True), nullable=True)` |

**Class variables**: `__tablename__ = 'workflows'`

<details><summary>Methods (1)</summary>

`__repr__`

</details>

---

## `db/stores/agent_run_store.py`

#### `AgentRunStore`  ·  _class_  ·  bases: `SyncStoreMixin`  ·  [L12]

CRUD operations for agent run records.

<details><summary>Methods (3)</summary>

`create_run`, `finish_run`, `get_run`

</details>

---

## `db/stores/attempt_store.py`

#### `AttemptStore`  ·  _class_  ·  bases: `SyncStoreMixin`  ·  [L18]

CRUD for Attempt. Returns frozen Attempt DTOs.

<details><summary>Methods (11)</summary>

`insert`, `get`, `set_planner_task_id`, `set_plan_contract`, `set_generator_task_ids`, `set_evaluator_task_id`, `set_stage`, `close`, `list_for_iteration`, `get_by_sequence`, `_to_dto`

</details>

---

## `db/stores/base.py`

#### `SyncStoreMixin`  ·  _class_  ·  [L18]

Lazy-init pattern for synchronous SQLAlchemy stores.

**Fields**

| name | type | default |
|------|------|---------|
| `_store_label` | `ClassVar[str]` | `''` |

**Class variables**: `is_ready = initialized`

**Instance attributes**: `_session_factory`

<details><summary>Methods (4)</summary>

`__init__`, `initialize`, `initialized`, `_sf`

</details>

---

## `db/stores/context_packet_store.py`

#### `ContextPacketStore`  ·  _class_  ·  bases: `SyncStoreMixin`  ·  [L14]

CRUD for :class:`ContextPacket`. Returns frozen pydantic instances.

<details><summary>Methods (3)</summary>

`insert`, `get`, `_to_dto`

</details>

---

## `db/stores/iteration_store.py`

#### `IterationStore`  ·  _class_  ·  bases: `SyncStoreMixin`  ·  [L17]

CRUD for Iteration. Returns frozen Iteration DTOs.

<details><summary>Methods (9)</summary>

`insert`, `get`, `append_attempt_id`, `set_deferred_goal_for_next_iteration`, `set_status`, `list_for_workflow`, `get_by_sequence`, `close_succeeded`, `_to_dto`

</details>

---

## `db/stores/model_store.py`

#### `ModelStore`  ·  _class_  ·  bases: `SyncStoreMixin`  ·  [L91]

CRUD operations for model registrations.

**Class variables**: `_store_label = 'ModelStore'`

<details><summary>Methods (9)</summary>

`register`, `select_active`, `delete`, `list_all`, `get`, `get_active`, `get_active_resolved`, `seed_from_json`, `_deactivate_all`

</details>

---

## `db/stores/task_center_store.py`

#### `TaskCenterStore`  ·  _class_  ·  bases: `SyncStoreMixin`  ·  [L63]

CRUD operations for TaskCenter persistence.

<details><summary>Methods (15)</summary>

`create_request`, `get_request`, `list_requests`, `create_run`, `finish_run`, `get_run`, `list_runs_for_request`, `upsert_task`, `get_task`, `list_tasks_for_run`, `list_tasks_for_attempt`, `list_generator_tasks_for_attempt`, `set_task_status`, `set_task_context_packet_id`, `set_task_status_if_current`

</details>

---

## `db/stores/workflow_store.py`

#### `WorkflowStore`  ·  _class_  ·  bases: `SyncStoreMixin`  ·  [L19]

CRUD for Workflow. Returns frozen Workflow DTOs.

<details><summary>Methods (7)</summary>

`insert`, `get`, `append_iteration_id`, `set_status`, `list_for_parent_task`, `list_for_run`, `_to_dto`

</details>

