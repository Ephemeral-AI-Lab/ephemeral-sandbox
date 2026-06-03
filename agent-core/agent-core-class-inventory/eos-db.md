# Crate `eos-db` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-db/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**16 types across 9 files.**

The `eos-db` crate owns the single `SQLite`-backed persistence implementation for
agent-core: it turns the abstract per-entity `Store` traits owned by `eos-state`
into concrete `sqlx` repositories over one local `SQLite` file. It owns the
`SqlitePool` and its PRAGMA discipline (`pool.rs`), the versioned embedded
`migrations/`, the typed `sqlx::FromRow` row structs and their explicit
row↔domain mapping that bridges the legacy column-name gap (`rows.rs`), the
JSON-column codec (`json_col.rs`), the model registry with secret redaction and
env-placeholder resolution (`model_registry.rs`), and the single
composition-root constructor [`Database`] (`composition.rs`). Its central types
are [`Database`] (the composition root that hands out each store as an
`Arc<dyn …Store>` for DIP at the seam), the five `Sql*Store` repositories that
implement the `eos-state` store traits, [`ModelRegistry`] / [`ResolvedModel`],
and the [`DbError`] enum bridging to `eos-state`'s `CoreError`. The crate depends
on `eos-state` (store traits, domain DTOs, id newtypes, `CoreError`),
`eos-config` (`DatabaseConfig`, `DatabaseUrl`), `sqlx`, `serde_json`, and `time`;
it is consumed by the runtime composition layer that wires the stores into the
agent loop.

## Contents

- **`eos-db/src/composition.rs`** — `Database`
- **`eos-db/src/error.rs`** — `DbError`
- **`eos-db/src/model_registry.rs`** — `ResolvedModel`, `ModelRegistrationRow`, `ModelRegistry`
- **`eos-db/src/repositories/agent_run.rs`** — `SqlAgentRunStore`
- **`eos-db/src/repositories/attempt.rs`** — `SqlAttemptStore`
- **`eos-db/src/repositories/iteration.rs`** — `SqlIterationStore`
- **`eos-db/src/repositories/request_task.rs`** — `SqlRequestTaskStore`
- **`eos-db/src/repositories/workflow.rs`** — `SqlWorkflowStore`
- **`eos-db/src/rows.rs`** — `RequestRow`, `TaskRow`, `WorkflowRow`, `IterationRow`, `AttemptRow`, `AgentRunRow`

---

## `eos-db/src/composition.rs`

#### `Database`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L23]

Owns the pool and one instance of each store, handed out as `Arc<dyn …Store>` for DIP at the seam; cloning is cheap (every field is `Arc`-backed).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `pool` | `SqlitePool` |  |
| `request_tasks` | `Arc<SqlRequestTaskStore>` |  |
| `workflows` | `Arc<SqlWorkflowStore>` |  |
| `iterations` | `Arc<SqlIterationStore>` |  |
| `attempts` | `Arc<SqlAttemptStore>` |  |
| `agent_runs` | `Arc<SqlAgentRunStore>` |  |
| `models` | `Arc<ModelRegistry>` |  |

<details><summary>Methods (10)</summary>

`open`, `requests`, `tasks`, `workflows`, `iterations`, `attempts`, `agent_runs`, `models`, `model_registry`, `pool`

</details>

---

## `eos-db/src/error.rs`

#### `DbError`  ·  _enum_  ·  derives: `Debug, thiserror::Error`  ·  #[non_exhaustive]  ·  [L8]

Errors raised by the `SQLite` persistence layer; flattens into `eos-state`'s `CoreError::Store` as a `Display` string.

**Variants**:
- `PostgresRejected` — a non-`SQLite` (network) database url was configured (GC-eos-db-04).
- `Sqlx(sqlx::Error)` — `#[from]` underlying `sqlx` error (connection, query, constraint violation).
- `JsonEncode(serde_json::Error)` — `#[source]` a JSON column failed to encode.
- `JsonDecode(serde_json::Error)` — `#[source]` a JSON column failed to decode.
- `NotFound { table: &'static str, id: String }` — a required row referenced by id was absent.
- `InvalidEnum { field: &'static str, value: String }` — a TEXT column held a value outside the expected enum vocabulary.
- `Migrate(sqlx::migrate::MigrateError)` — `#[from]` a migration failed to apply.
- `Io(std::io::Error)` — `#[from]` a filesystem error creating the database's parent directory.

**Trait impls**: `Error` (via `thiserror`), `From<sqlx::Error>`, `From<sqlx::migrate::MigrateError>`, `From<std::io::Error>`

---

## `eos-db/src/model_registry.rs`

#### `ResolvedModel`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq`  ·  [L29]

The active registration with its kwargs parsed and env-placeholders resolved (Python `get_active_resolved`); returned by `ModelRegistry::active_resolved`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `model_key` | `String` | `pub` |
| `label` | `String` | `pub` |
| `class_path` | `String` | `pub` |
| `kwargs` | `JsonObject` | `pub` |
| `is_active` | `bool` | `pub` |

#### `ModelRegistrationRow`  ·  _struct_  ·  derives: `Debug, Clone, sqlx::FromRow`  ·  private  ·  [L43]

Typed `sqlx` row for the `model_registrations` table; mapped to a `ModelRegistration` DTO by the file-local `row_to_model`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `i64` |  |
| `key` | `String` |  |
| `label` | `String` |  |
| `class_path` | `String` |  |
| `kwargs_json` | `String` |  |
| `is_active` | `bool` |  |
| `created_at` | `OffsetDateTime` |  |
| `updated_at` | `OffsetDateTime` |  |

#### `ModelRegistry`  ·  _struct_  ·  derives: `Debug`  ·  [L56]

`SQLite`-backed model registry (concrete; not a `Store` seam) providing registration CRUD, secret redaction, env-placeholder resolution, and JSON seeding.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `pool` | `SqlitePool` |  |

**Trait impls**: `Sealed`, `ModelStore`

<details><summary>Methods (4)</summary>

`new`, `active_resolved`, `seed_from_json`, `register_inner`

</details>

---

## `eos-db/src/repositories/agent_run.rs`

#### `SqlAgentRunStore`  ·  _struct_  ·  derives: `Debug`  ·  [L18]

`SQLite` repository for agent runs; two-phase (`create_run` sets create-time fields, `finish_run` writes the null-preserving JSON columns).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `pool` | `SqlitePool` |  |

**Trait impls**: `Sealed`, `AgentRunStore`

<details><summary>Methods (1)</summary>

`new`

</details>

---

## `eos-db/src/repositories/attempt.rs`

#### `SqlAttemptStore`  ·  _struct_  ·  derives: `Debug`  ·  [L18]

`SQLite` repository for attempts; returns frozen `Attempt` DTOs.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `pool` | `SqlitePool` |  |

**Trait impls**: `Sealed`, `AttemptStore`

<details><summary>Methods (2)</summary>

`new`, `not_found`

</details>

---

## `eos-db/src/repositories/iteration.rs`

#### `SqlIterationStore`  ·  _struct_  ·  derives: `Debug`  ·  [L17]

`SQLite` repository for iterations; returns frozen `Iteration` DTOs.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `pool` | `SqlitePool` |  |

**Trait impls**: `Sealed`, `IterationStore`

<details><summary>Methods (1)</summary>

`new`

</details>

---

## `eos-db/src/repositories/request_task.rs`

#### `SqlRequestTaskStore`  ·  _struct_  ·  derives: `Debug`  ·  [L25]

`SQLite` repository for requests and tasks (Python `task_store.py`); holds a cheap `SqlitePool` clone and implements both the request and task store seams.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `pool` | `SqlitePool` |  |

**Trait impls**: `Sealed`, `RequestStore`, `TaskStore`

<details><summary>Methods (1)</summary>

`new`

</details>

---

## `eos-db/src/repositories/workflow.rs`

#### `SqlWorkflowStore`  ·  _struct_  ·  derives: `Debug`  ·  [L17]

`SQLite` repository for workflows; returns frozen `Workflow` DTOs.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `pool` | `SqlitePool` |  |

**Trait impls**: `Sealed`, `WorkflowStore`

<details><summary>Methods (1)</summary>

`new`

</details>

---

## `eos-db/src/rows.rs`

#### `RequestRow`  ·  _struct_  ·  derives: `Debug, Clone, sqlx::FromRow`  ·  pub(crate)  ·  [L24]

Typed `sqlx` row for the `requests` table (column names, sqlx-native field types); mapped to a `Request` DTO by `row_to_request`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `String` | `pub` |
| `cwd` | `String` | `pub` |
| `sandbox_id` | `Option<String>` | `pub` |
| `request_prompt` | `String` | `pub` |
| `root_task_id` | `Option<String>` | `pub` |
| `status` | `String` | `pub` |
| `created_at` | `OffsetDateTime` | `pub` |
| `updated_at` | `OffsetDateTime` | `pub` |
| `finished_at` | `Option<OffsetDateTime>` | `pub` |

#### `TaskRow`  ·  _struct_  ·  derives: `Debug, Clone, sqlx::FromRow`  ·  pub(crate)  ·  [L37]

Typed `sqlx` row for the `tasks` table; mapped to a `Task` DTO by `row_to_task` (extra `created_at`/`updated_at` columns are ignored by `FromRow`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `String` | `pub` |
| `request_id` | `String` | `pub` |
| `role` | `String` | `pub` |
| `instruction` | `String` | `pub` |
| `status` | `String` | `pub` |
| `workflow_id` | `Option<String>` | `pub` |
| `iteration_id` | `Option<String>` | `pub` |
| `attempt_id` | `Option<String>` | `pub` |
| `agent_name` | `Option<String>` | `pub` |
| `needs` | `String` | `pub` |
| `outcomes` | `String` | `pub` |
| `terminal_tool_result` | `Option<String>` | `pub` |

#### `WorkflowRow`  ·  _struct_  ·  derives: `Debug, Clone, sqlx::FromRow`  ·  pub(crate)  ·  [L55]

Typed `sqlx` row for the `workflows` table; mapped to a `Workflow` DTO by `row_to_workflow` (legacy column `goal` → domain `workflow_goal`, §4).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `String` | `pub` |
| `request_id` | `String` | `pub` |
| `parent_task_id` | `String` | `pub` |
| `goal` | `String` | `pub` |
| `status` | `String` | `pub` |
| `iteration_ids` | `String` | `pub` |
| `outcomes` | `Option<String>` | `pub` |
| `created_at` | `OffsetDateTime` | `pub` |
| `updated_at` | `OffsetDateTime` | `pub` |
| `closed_at` | `Option<OffsetDateTime>` | `pub` |

#### `IterationRow`  ·  _struct_  ·  derives: `Debug, Clone, sqlx::FromRow`  ·  pub(crate)  ·  [L69]

Typed `sqlx` row for the `iterations` table; mapped to an `Iteration` DTO by `row_to_iteration` (legacy `goal`/`deferred_goal` renames, §4).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `String` | `pub` |
| `workflow_id` | `String` | `pub` |
| `sequence_no` | `i64` | `pub` |
| `creation_reason` | `String` | `pub` |
| `goal` | `String` | `pub` |
| `attempt_budget` | `i64` | `pub` |
| `status` | `String` | `pub` |
| `attempt_ids` | `String` | `pub` |
| `deferred_goal` | `Option<String>` | `pub` |
| `created_at` | `OffsetDateTime` | `pub` |
| `updated_at` | `OffsetDateTime` | `pub` |
| `closed_at` | `Option<OffsetDateTime>` | `pub` |
| `outcomes` | `Option<String>` | `pub` |

#### `AttemptRow`  ·  _struct_  ·  derives: `Debug, Clone, sqlx::FromRow`  ·  pub(crate)  ·  [L86]

Typed `sqlx` row for the `attempts` table; mapped to an `Attempt` DTO by `row_to_attempt`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `String` | `pub` |
| `iteration_id` | `String` | `pub` |
| `workflow_id` | `String` | `pub` |
| `attempt_sequence_no` | `i64` | `pub` |
| `stage` | `String` | `pub` |
| `status` | `String` | `pub` |
| `planner_task_id` | `Option<String>` | `pub` |
| `generator_task_ids` | `String` | `pub` |
| `reducer_task_ids` | `String` | `pub` |
| `outcomes` | `String` | `pub` |
| `deferred_goal` | `Option<String>` | `pub` |
| `fail_reason` | `Option<String>` | `pub` |
| `created_at` | `OffsetDateTime` | `pub` |
| `updated_at` | `OffsetDateTime` | `pub` |
| `closed_at` | `Option<OffsetDateTime>` | `pub` |

#### `AgentRunRow`  ·  _struct_  ·  derives: `Debug, Clone, sqlx::FromRow`  ·  pub(crate)  ·  [L105]

Typed `sqlx` row for the `agent_runs` table; mapped to an `AgentRun` DTO by `row_to_agent_run`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `id` | `String` | `pub` |
| `task_id` | `String` | `pub` |
| `initial_messages` | `Option<String>` | `pub` |
| `agent_name` | `String` | `pub` |
| `message_history` | `Option<String>` | `pub` |
| `terminal_tool_result` | `Option<String>` | `pub` |
| `token_count` | `i64` | `pub` |
| `error` | `Option<String>` | `pub` |
| `created_at` | `OffsetDateTime` | `pub` |
| `finished_at` | `Option<OffsetDateTime>` | `pub` |
