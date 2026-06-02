# impl-eos-db — SQLite persistence: pool, migrations, typed rows, store-trait repositories

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §3 (`eos-db`).

## 1. Purpose & Responsibility (SRP)

`eos-db` is the **single SQLite-backed persistence implementation** for agent-core.
Its one responsibility is to turn the abstract per-entity `Store` traits (owned by
`eos-state`) into concrete `sqlx` repositories over one local SQLite file: it owns
the `SqlitePool` (with PRAGMA discipline), the versioned `migrations/` SQL files
that replace the live DDL patching in `db/engine.py`, the typed SQL row structs
(`rows.rs`), the explicit row↔domain mapping for the naming gap (anchor §4), and a
single composition-root constructor that hands runtime/engine/workflow every store
they need.

What this crate **must NOT do**: define domain DTOs or `Store` trait signatures
(those are `eos-state`; this crate `impl`s them), own lifecycle/business policy
(no status-transition rules beyond what a store method is asked to write), perform
`class_path` dynamic dispatch (migration data only — anchor §2), support
PostgreSQL or any network DB, or hold an app-level DB mutex (the pool owns
connection concurrency). It is runtime-agnostic: it never builds a Tokio runtime,
only exposes `async fn`s on `&self`.

## 2. Dependencies

- **Upstream crates (depends on):**
  - `eos-types` — newtype IDs (`TaskId`, `WorkflowId`, `IterationId`, `AttemptId`,
    `RequestId`, `AgentRunId`, `SandboxId`), `UtcDateTime`, `JsonObject`, `CoreError`
    (see impl-eos-types.md / anchor §5).
  - `eos-state` — domain DTOs (`Workflow`, `Iteration`, `Attempt`, `Task`,
    request projection), status/stage/reason enums, terminal-submission DTOs, and
    the **per-entity `Store` traits this crate implements** (see impl-eos-state.md
    / anchor §5).
  - `eos-config` — `DatabaseConfig` (URL + flags) for pool construction and the
    Postgres fail-fast check (see impl-eos-config.md / anchor §5).
- **Downstream consumers (used by):** `eos-runtime` only. Runtime calls the
  composition-root constructor and injects the `Arc<dyn Store>` handles into
  engine/workflow (anchor §5 row `eos-db`).

- **External crates** (pinned via `[workspace.dependencies]` inheritance —
  `proj-workspace-deps`; crate name has no `-rs` suffix — `name-crate-no-rs`):

  | Crate | Features | Justification | rust-skills |
  |---|---|---|---|
  | `sqlx` | `runtime-tokio`, `sqlite`, `macros`, `time`, `migrate` — **no `postgres`/`mysql`** | The whole point of the crate: async SQLite pool, `query_as` into typed rows, compile-checked migrations. SQLite-only by feature gate enforces anchor §2. | `async-tokio-runtime` |
  | `thiserror` | — | One `DbError` enum with `#[from] sqlx::Error` (anchor §8). | `err-thiserror-lib`, `err-from-impl` |
  | `async-trait` | — | The `Store` traits use `async fn`; they are stored behind `Arc<dyn Store>` in the composition root, so they need `#[async_trait]` (anchor §6 object-safety note). | `async-tokio-runtime` |
  | `serde` / `serde_json` | `derive` | JSON columns are stored as validated TEXT; map `Vec<TaskId>`/`outcomes`/`kwargs` ↔ `TEXT` via `serde_json::{to_string,from_str}`. | `api-parse-dont-validate` |
  | `time` | `serde` | Timestamps map to `UtcDateTime` (= `eos-types` newtype over `time::OffsetDateTime`); sqlx `time` feature binds them. | — |
  | `uuid` | `v4` | New row ids (`Workflow`/`Iteration`/`Attempt` mint `uuid4` in their `insert`, matching Python `str(uuid.uuid4())`). | — |

  Dev-only: `tempfile` (RAII temp DB file per test — `test-fixture-raii`).
  `eos-state`'s in-memory test stores cover trait substitutability; `eos-db`'s
  own tests run against a real temp SQLite file.

## 3. Scope & Source Mapping

| Python source | Rust target | Moves / dropped |
|---|---|---|
| `db/base.py` (`DeclarativeBase`) | — | **Dropped.** No ORM base; schema lives in `migrations/`. |
| `db/engine.py` (`initialize_db`, `_rename_columns`, `_add_missing_columns`, `_rebuild_sqlite_table`, `_drop_legacy_tables`, `_DROPPED_COLUMNS`, `_RENAMED_COLUMNS`) | `pool.rs` + `migrations/0001_initial.sql` | Live DDL patching → **one authoritative initial-schema migration** with the final column names. The conditional legacy rename/drop/drop-table logic is **dropped**: it was a runtime upgrade path guarded by `inspect().has_table`/`col in existing`, which a forward-only sqlx migration (applied unconditionally on every fresh DB) cannot reproduce. Postgres pool branch **dropped** (fail fast). |
| `db/models/request.py` `RequestRecord` | `rows.rs::RequestRow` + `repositories/request_task.rs` | All fields kept; `Mapped[...]` → typed columns. |
| `db/models/task.py` `TaskRecord` | `rows.rs::TaskRow` + `repositories/request_task.rs` | Kept; JSON `needs`/`outcomes`/`terminal_tool_result` → TEXT. |
| `db/models/workflow.py` `WorkflowRecord` | `rows.rs::WorkflowRow` + `repositories/workflow.rs` | Kept; DB column `goal` ↔ domain `workflow_goal`. |
| `db/models/iteration.py` `IterationRecord` | `rows.rs::IterationRow` + `repositories/iteration.rs` | Kept; `goal`↔`iteration_goal`, `deferred_goal`↔`deferred_goal_for_next_iteration`; unique `(workflow_id, sequence_no)`. |
| `db/models/attempt.py` `AttemptRecord` | `rows.rs::AttemptRow` + `repositories/attempt.rs` | Kept; unique `(iteration_id, attempt_sequence_no)`. |
| `db/models/agent_run.py` `AgentRunRecord` | `rows.rs::AgentRunRow` + `repositories/agent_run.rs` | Kept; unique `task_id`. |
| `db/models/model_registration.py` `ModelRegistrationRecord` | `rows.rs::ModelRegistrationRow` + `model_registry.rs` | Kept; `class_path` = **migration data only**; `id` autoincrement INTEGER. |
| `db/stores/base.py` `SyncStoreMixin` | — | **Dropped.** Lazy `initialize()`/`_sf` accessor replaced by eager pool injection at construction. |
| `db/stores/task_store.py` `TaskStore` | `repositories/request_task.rs::SqlRequestTaskStore` | Methods → async; dict `SerializedRow` → typed DTOs. |
| `db/stores/workflow_store.py` `WorkflowStore` | `repositories/workflow.rs::SqlWorkflowStore` | `_to_dto` → typed mapping fn. |
| `db/stores/iteration_store.py` `IterationStore` | `repositories/iteration.rs::SqlIterationStore` | incl. `close_succeeded` atomic write. |
| `db/stores/attempt_store.py` `AttemptStore` | `repositories/attempt.rs::SqlAttemptStore` | incl. `close`, sequence lookups. |
| `db/stores/agent_run_store.py` `AgentRunStore` | `repositories/agent_run.rs::SqlAgentRunStore` | Two-phase: `create_run` sets only `id`/`task_id`/`agent_name`/`initial_messages`/`created_at`; `finish_run` writes `message_history`/`terminal_tool_result`/`token_count`/`error`/`finished_at`. The nullable JSON columns stay `None` (not `[]`) until finish. |
| `db/stores/model_store.py` `ModelStore` | `model_registry.rs::ModelRegistry` | `_resolve_env_placeholders` kept (compat migration); `_redact_secrets` kept; seed-from-json kept. |

**In scope:** pool/PRAGMA setup, all migrations, all typed rows, all six store
impls, model registry with env-placeholder resolution, one composition root.
**Out of scope:** PostgreSQL (rejected), ORM relationships/cascades expressed in
code (cascades live in `migrations` `FOREIGN KEY ... ON DELETE CASCADE`), any
`class_path` import/dispatch, business lifecycle rules. **In-place upgrade of a
legacy Python-created SQLite file** (the `engine.py` conditional rename/drop of
`task_summary`/`context_message`/`task_center_run_id`, legacy tables, dropped
columns) is **not** part of the forward migration chain — agent-core is
greenfield, so it would be a separate guarded one-shot importer, not a static
forward migration.

## 4. File & Module Layout

```
eos-db/
├── migrations/
│   └── 0001_initial.sql        # sole authoritative schema: all 7 tables + unique constraints + FK cascades + indexes (final column names)
├── src/
│   ├── lib.rs                  # pub use facade (proj-pub-use-reexport): Database, DbError, store impls
│   ├── error.rs                # DbError (thiserror) — the one crate error enum
│   ├── pool.rs                 # SqlitePool builder: reject Postgres, PRAGMA fk/WAL/busy_timeout, run migrations
│   ├── rows.rs                 # pub(crate) typed FromRow structs + row→DTO mapping (the naming gap §4)
│   ├── json_col.rs             # pub(crate) helpers: serde_json TEXT encode/decode for JSON columns
│   ├── composition.rs          # Database: composition-root holding Arc<...> of every store
│   ├── model_registry.rs       # ModelRegistry: active-model lookup + env-placeholder resolution + seed
│   └── repositories/
│       ├── mod.rs              # pub(crate) re-exports of the six Sql*Store types
│       ├── request_task.rs     # SqlRequestTaskStore (requests + tasks)
│       ├── workflow.rs         # SqlWorkflowStore
│       ├── iteration.rs        # SqlIterationStore
│       ├── attempt.rs          # SqlAttemptStore
│       └── agent_run.rs        # SqlAgentRunStore
└── Cargo.toml
```

`lib.rs` re-exports `Database`, `DbError`, and the store impl types
(`proj-pub-use-reexport`). `rows.rs`, `json_col.rs`, and the `repositories`
internals are `pub(crate)` (`proj-pub-crate-internal`); only the `Sql*Store`
types and `Database` are public.

## 5. Contracts Owned Here

This crate owns **no shared trait** — every `Store` trait it satisfies is owned by
`eos-state` (referenced, not redefined). It owns these concrete, crate-public
types:

- **`Database`** (composition root) — `composition.rs`. Holds `Arc` handles to one
  instance of each store and exposes typed accessors. Not a trait.
  ```rust
  #[derive(Clone)]
  pub struct Database {
      pool: SqlitePool,
      request_tasks: Arc<SqlRequestTaskStore>,
      workflows: Arc<SqlWorkflowStore>,
      iterations: Arc<SqlIterationStore>,
      attempts: Arc<SqlAttemptStore>,
      agent_runs: Arc<SqlAgentRunStore>,
      models: Arc<ModelRegistry>,
  }
  impl Database {
      /// Open the SQLite file, reject Postgres, apply PRAGMAs, run migrations,
      /// and construct every store. The single composition-root constructor.
      pub async fn open(config: &DatabaseConfig) -> Result<Self, DbError>;
      #[must_use] pub fn workflows(&self) -> Arc<dyn WorkflowStore> { self.workflows.clone() }
      // ...one accessor per Store trait, returning Arc<dyn …Store> for DIP at the seam.
  }
  ```
- **`Sql{RequestTask,Workflow,Iteration,Attempt,AgentRun}Store`** — each `impl`s
  the matching `eos-state` trait via `#[async_trait]` (object-safe behind
  `Arc<dyn …>` at the composition root — anchor §6). Each holds a `SqlitePool`
  clone (cheap; the pool is `Arc` internally).
- **`ModelRegistry`** — `model_registry.rs`. Concrete (not a seam in anchor §6):
  `register`, `delete`, `get`, `active`, `active_resolved`,
  `seed_from_json`. `class_path` is stored/returned **as data only**.
- **`DbError`** — the crate's one `thiserror` enum (anchor §8).

Object-safety/async note: the `Store` traits use `async fn` and are consumed
behind `Arc<dyn>` in `eos-runtime`, so `eos-state` declares them with
`#[async_trait]` and `eos-db` implements with the matching `#[async_trait]`
(native async-fn-in-trait is not yet `dyn`-safe — anchor §6).

## 6. Types, Fields & Schemas

Row structs are `pub(crate)`, derive `sqlx::FromRow` + `Debug, Clone, PartialEq`
(`api-common-traits`; the `eos-state` DTOs derive `PartialEq` too, enabling the
AC roundtrip equality assertions). Each maps
to/from the `eos-state` DTO via a free `fn` in `rows.rs`. **DB columns keep the
short legacy names; the domain DTOs use the normalized names** (anchor §4) — the
mapping fn is the single explicit bridge. **Enum-backed columns
(`status`/`stage`/`creation_reason`/`fail_reason`) are stored as raw `String`
(TEXT) on the row and parsed into the `eos-state` enum in the mapper via
`<Enum>::from_db(&r.col)?` → `DbError::InvalidEnum`** (not decoded by
`FromRow`), so parse-time failures live in the mapper rather than `FromRow`.

### `RequestRow` → request projection (`eos-state`) — source: `request.py`

| Column | Rust type | Notes / source-of-truth |
|---|---|---|
| `id` | `RequestId` (TEXT, PK, 36) | `RequestRecord.id` |
| `cwd` | `String` (TEXT) | |
| `sandbox_id` | `Option<SandboxId>` (TEXT null) | |
| `request_prompt` | `String` (TEXT) | |
| `root_task_id` | `Option<TaskId>` (TEXT null) | |
| `status` | `String` (TEXT, default `'running'`) | request status is a free string in source; keep as `String` (do not invent an enum eos-state does not own) |
| `created_at`/`updated_at` | `UtcDateTime` | |
| `finished_at` | `Option<UtcDateTime>` | |

### `TaskRow` → `Task` (`eos-state`) — source: `task.py`, serialized in `task_store.py`

| Column | Rust type | Notes |
|---|---|---|
| `id` | `TaskId` (TEXT PK 96) | serialized as `task_id` in DTO |
| `request_id` | `RequestId` (TEXT, FK→requests ON DELETE CASCADE, indexed) | |
| `role` | `String` (TEXT — parsed into the `eos-state` `Task.role` `TaskRole` in the mapper) | `role` column |
| `instruction` | `String` (TEXT) | |
| `status` | task-status enum per eos-state | |
| `workflow_id` | `Option<WorkflowId>` (TEXT null, indexed) | |
| `iteration_id` | `Option<IterationId>` (TEXT null, indexed) | |
| `attempt_id` | `Option<AttemptId>` (TEXT null, indexed) | |
| `agent_name` | `Option<String>` (TEXT null) | |
| `needs` | `Vec<TaskId>` ← **TEXT(JSON array)** | default `[]` |
| `outcomes` | `Vec<JsonObject>` ← **TEXT(JSON array)** | default `[]` |
| `terminal_tool_result` | `Option<JsonObject>` ← **TEXT(JSON object) null** | |
| `created_at`/`updated_at` | `UtcDateTime` | |

### `WorkflowRow` → `Workflow` (`eos-state`) — source: `workflow.py`, `workflow_store.py`

| Column | Rust type | Domain field (anchor §4) |
|---|---|---|
| `id` | `WorkflowId` (TEXT PK 36) | `id` |
| `request_id` | `RequestId` (FK→requests CASCADE, indexed) | `request_id` |
| `parent_task_id` | `TaskId` (TEXT NOT NULL, indexed) | `parent_task_id` |
| **`goal`** | `String` (TEXT) | **`workflow_goal`** ← explicit rename in `row_to_workflow` |
| `status` | `String` (TEXT — parsed into the `eos-state` `WorkflowStatus` enum `open`/`succeeded`/`failed`/`cancelled` in the mapper) | `status` |
| `iteration_ids` | `Vec<IterationId>` ← TEXT(JSON array, default `[]`) | `iteration_ids` |
| `outcomes` | `Option<String>` (TEXT null — raw `json.dumps` string, **not** decoded) | `outcomes` |
| `created_at`/`updated_at` | `UtcDateTime` | |
| `closed_at` | `Option<UtcDateTime>` | |

### `IterationRow` → `Iteration` (`eos-state`) — source: `iteration.py`, `iteration_store.py`

| Column | Rust type | Domain field |
|---|---|---|
| `id` | `IterationId` (TEXT PK 36) | `id` |
| `workflow_id` | `WorkflowId` (FK→workflows CASCADE, indexed) | `workflow_id` |
| `sequence_no` | `i64` (INTEGER) | `sequence_no` |
| `creation_reason` | `String` (TEXT — parsed into the `eos-state` `IterationCreationReason` enum `initial`/`deferred_goal_continuation` in the mapper) | `creation_reason` |
| **`goal`** | `String` (TEXT) | **`iteration_goal`** |
| `attempt_budget` | `i64` (INTEGER) | `attempt_budget` |
| `status` | `String` (TEXT — parsed into the `eos-state` `IterationStatus` enum in the mapper) | `status` |
| `attempt_ids` | `Vec<AttemptId>` ← TEXT(JSON array, default `[]`) | `attempt_ids` |
| **`deferred_goal`** | `Option<String>` (TEXT null) | **`deferred_goal_for_next_iteration`** |
| `created_at`/`updated_at`/`closed_at` | `UtcDateTime` / `Option<UtcDateTime>` | |
| `outcomes` | `Option<String>` (TEXT null — raw projection string) | `outcomes` |

Unique: `UNIQUE(workflow_id, sequence_no)` (`uq_iteration_workflow_sequence`).

### `AttemptRow` → `Attempt` (`eos-state`) — source: `attempt.py`, `attempt_store.py`

| Column | Rust type | Domain field |
|---|---|---|
| `id` | `AttemptId` (TEXT PK 36) | `id` |
| `iteration_id` | `IterationId` (FK→iterations CASCADE, indexed) | `iteration_id` |
| `workflow_id` | `WorkflowId` (TEXT, indexed) | `workflow_id` |
| `attempt_sequence_no` | `i64` | `attempt_sequence_no` |
| `stage` | `String` (TEXT — parsed into the `eos-state` `AttemptStage` enum `plan`/`run`/`closed` in the mapper) | `stage` |
| `status` | `String` (TEXT — parsed into the `eos-state` `AttemptStatus` enum `running`/`passed`/`failed` in the mapper) | `status` |
| `planner_task_id` | `Option<TaskId>` (TEXT null) | `planner_task_id` |
| `generator_task_ids` | `Vec<TaskId>` ← TEXT(JSON, default `[]`) | `generator_task_ids` |
| `reducer_task_ids` | `Vec<TaskId>` ← TEXT(JSON, default `[]`) | `reducer_task_ids` |
| `outcomes` | `Vec<JsonObject>` ← TEXT(JSON, default `[]`) → `parse_outcomes_record` (eos-state) | `outcomes` |
| **`deferred_goal`** | `Option<String>` (TEXT null) | **`deferred_goal_for_next_iteration`** |
| `fail_reason` | `Option<String>` (TEXT null — parsed into the `eos-state` `AttemptFailReason` enum `task_failed`/`startup_failed` in the mapper) | `fail_reason` |
| `created_at`/`updated_at`/`closed_at` | `UtcDateTime` / `Option<UtcDateTime>` | |

Unique: `UNIQUE(iteration_id, attempt_sequence_no)` (`uq_attempt_iteration_sequence`).

### `AgentRunRow` → agent-run DTO (`eos-state`) — source: `agent_run.py`, `agent_run_store.py`

| Column | Rust type | Notes |
|---|---|---|
| `id` | `AgentRunId` (TEXT PK 36) | |
| `task_id` | `TaskId` (TEXT, FK→tasks CASCADE, **UNIQUE**, indexed) | one run per task |
| `initial_messages` | `Option<Vec<JsonObject>>` ← TEXT(JSON null) | null-preserving (`decode_opt`); set at `create_run` |
| `agent_name` | `String` (TEXT) | |
| `message_history` | `Option<Vec<JsonObject>>` ← TEXT(JSON null) | null-preserving (`decode_opt`); written at `finish_run`, `None` until then |
| `terminal_tool_result` | `Option<JsonObject>` ← TEXT null | null-preserving (`decode_opt`); written at `finish_run` |
| `token_count` | `i64` (INTEGER, default 0) | |
| `error` | `Option<String>` (TEXT null) | |
| `created_at` | `UtcDateTime` | |
| `finished_at` | `Option<UtcDateTime>` | |

### `ModelRegistrationRow` — source: `model_registration.py`, `model_store.py`

| Column | Rust type | Notes |
|---|---|---|
| `id` | `i64` (INTEGER PK **AUTOINCREMENT**) | SQLite autoincrement requires INTEGER PK |
| `key` | `String` (TEXT **UNIQUE** NOT NULL) | |
| `label` | `String` (TEXT NOT NULL) | |
| `class_path` | `String` (TEXT NOT NULL) | **migration data only** — never dispatched on |
| `kwargs_json` | `String` (TEXT NOT NULL default `'{}'`) | parsed to `JsonObject` on read |
| `is_active` | `bool` (INTEGER 0/1, default 0) | at most one active; `register(activate=true)` deactivates all first |
| `created_at`/`updated_at` | `UtcDateTime` | |

**Representative snippet — JSON column helpers (`json_col.rs`)** — JSON fields are
TEXT-of-validated-JSON (anchor §2; `api-parse-dont-validate`). There are **two
decode paths** because the Python stores disagree on NULL handling (see §8):
`decode_default` mirrors the `task_store` `record.x or []` coercion; `decode_opt`
mirrors the `agent_run_store` nullable columns that must preserve `None`:

```rust
pub(crate) fn encode<T: Serialize>(value: &T) -> Result<String, DbError> {
    serde_json::to_string(value).map_err(DbError::JsonEncode)
}
/// Default-to-empty path for the `x or []` columns (needs, task.outcomes,
/// iteration_ids/attempt_ids, generator/reducer_task_ids, attempt.outcomes).
pub(crate) fn decode_default<T: DeserializeOwned + Default>(text: Option<&str>) -> Result<T, DbError> {
    match text {
        None | Some("") => Ok(T::default()),          // mirror task_store `record.x or []`
        Some(s) => serde_json::from_str(s).map_err(DbError::JsonDecode),
    }
}
/// Null-preserving path for the nullable agent_run columns
/// (initial_messages, message_history, terminal_tool_result): NULL/empty stays
/// `None` — agent_run_store does NOT coerce to `[]`.
pub(crate) fn decode_opt<T: DeserializeOwned>(text: Option<&str>) -> Result<Option<T>, DbError> {
    match text {
        None | Some("") => Ok(None),
        Some(s) => serde_json::from_str(s).map(Some).map_err(DbError::JsonDecode),
    }
}
```

**Representative snippet — explicit naming-gap mapping (`rows.rs`)**:

```rust
pub(crate) fn row_to_workflow(r: WorkflowRow) -> Result<Workflow, DbError> {
    Ok(Workflow {
        id: r.id,
        request_id: r.request_id,
        workflow_goal: r.goal,                         // §4 column `goal` → domain `workflow_goal`
        status: WorkflowStatus::from_db(&r.status)?,   // parse, don't pass raw string
        iteration_ids: json_col::decode_default(r.iteration_ids.as_deref())?,
        parent_task_id: r.parent_task_id,
        outcomes: r.outcomes,                          // raw projection string, not decoded
        created_at: r.created_at,
        updated_at: r.updated_at,
        closed_at: r.closed_at,
    })
}
```

### `DbError` (owned, `error.rs`)

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DbError {
    #[error("postgresql is not supported in agent-core; configure a sqlite url")]
    PostgresRejected,                              // GC-eos-db-04 fail-fast
    #[error("database error")]
    Sqlx(#[from] sqlx::Error),
    #[error("failed to encode json column")]
    JsonEncode(#[source] serde_json::Error),
    #[error("failed to decode json column")]
    JsonDecode(#[source] serde_json::Error),
    #[error("row {id} not found in {table}")]
    NotFound { table: &'static str, id: String },  // mirror Python LookupError sites
    #[error("invalid enum value {value:?} for {field}")]
    InvalidEnum { field: &'static str, value: String },
    #[error("migration failed")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}
```

Lowercase, no trailing punctuation (`err-lowercase-msg`); `#[from]` for upstream
conversion (`err-from-impl`); `#[source]` to chain serde failures
(`err-source-chain`); `#[non_exhaustive]` (`api-non-exhaustive`). No
`Box<dyn Error>` in signatures (anchor §8).

## 7. Concurrency & State Ownership

- **Runtime:** runtime-agnostic. Every store method is `async fn(&self, …)`; the
  Tokio multi-thread runtime is created in `eos-runtime` (anchor §7). `eos-db`
  never spawns a runtime.
- **Connection concurrency:** the `SqlitePool` owns it. SQLite's single-writer
  constraint is handled at the engine via `PRAGMA busy_timeout` + `journal_mode=WAL`,
  **not** an app-level mutex (anchor §7 "No app-level DB mutex"). Each `Sql*Store`
  holds a `SqlitePool` clone (the pool is internally `Arc`; cloning is cheap —
  `own-arc-shared`).
- **Pool pressure and transactions:** the pool builder sets explicit
  `max_connections`, `acquire_timeout`, `busy_timeout`, and WAL/foreign-key
  pragmas from `DatabaseConfig`. Write methods open the shortest possible
  transaction (`BEGIN IMMEDIATE` where write-lock acquisition must be explicit)
  and **never hold a transaction across LLM, sandbox, tool, or agent-run awaits**;
  all such awaits happen before entering the transaction or after commit/rollback.
- **Shared immutable state:** the `Database` composition root holds each store in an
  `Arc<…>` so runtime hands out `Arc<dyn …Store>` cheaply (`own-arc-shared`).
- **Lock discipline:** there are **no `Mutex`/`RwLock` guards** held across `.await`
  because there are no app-level locks; all serialization is the pool's
  (`async-no-lock-await`, `anti-lock-across-await` trivially satisfied).
- **Atomicity:** multi-write methods (`close_succeeded` writing status+outcomes;
  `ModelRegistry::register(activate=true)` deactivating all then activating one) run
  inside a single `sqlx` transaction (`pool.begin().await` → `tx.commit()`), so a
  mid-write failure leaves the rows untouched — preserving the Python
  `db.commit()`-once guarantee.
- **CPU-bound:** none. JSON encode/decode of small columns stays inline (no
  `spawn_blocking`).

## 8. Behavior & Invariants

- **One local SQLite file, fail fast on Postgres.** `Database::open` parses the
  configured URL; if the driver is not `sqlite`/`sqlite:` it returns
  `DbError::PostgresRejected` **before** opening any connection (anchor §2; the plan
  gap "fail fast with a migration error instead of silently starting a different
  backend"). The Python default URL `sqlite:///./.ephemeralos/ephemeralos.db` is the
  baseline; the parent dir is created for non-`:memory:` SQLite files (mirrors
  `engine.py` `mkdir(parents=True, exist_ok=True)`).
- **PRAGMA discipline (pool.rs):** every connection sets `PRAGMA foreign_keys = ON`,
  `journal_mode = WAL`, and a `busy_timeout` (via `SqliteConnectOptions` so it
  applies to every pooled connection). FK cascades in the schema are then actually
  enforced (Python relied on ORM `cascade="all, delete-orphan"` + FK
  `ondelete="CASCADE"`; in SQLite, FKs are inert without the PRAGMA).
- **Migrations replace live DDL patching.** `0001_initial.sql` is the **sole
  authoritative schema**: it declares all seven tables with the **final** column
  names (`iterations.outcomes`, `tasks.instruction`, `tasks.request_id`), the
  unique constraints, FK cascades, and indexes from the models. Migrations are
  forward-only and applied in order on `open` via `sqlx::migrate!`. The
  `engine.py` runtime rename/drop/drop-table logic is **not** reproduced as a
  forward migration: `_rename_columns`/`_add_missing_columns`/`_drop_legacy_tables`
  were conditional on `inspect().has_table`/`col in existing`, but `sqlx::migrate!`
  applies every file unconditionally on every DB including fresh ones, and SQLite
  `RENAME COLUMN`/`DROP COLUMN` have no `IF EXISTS` guard — so a forward migration
  renaming `task_summary→outcomes` on a fresh DB (where 0001 already created
  `outcomes`) would error. Legacy in-place upgrade is therefore out of scope (§3),
  not a migration step.
- **Unique constraints are authoritative:** `agent_runs.task_id UNIQUE`,
  `iterations(workflow_id, sequence_no) UNIQUE`,
  `attempts(iteration_id, attempt_sequence_no) UNIQUE`. A duplicate insert surfaces
  as `DbError::Sqlx` (constraint violation), not a silent overwrite.
- **JSON columns are validated JSON TEXT** (anchor §2). `needs`, `outcomes`,
  `iteration_ids`/`attempt_ids`, `generator_task_ids`/`reducer_task_ids`,
  `initial_messages`/`message_history`, `terminal_tool_result`, `kwargs_json` are
  serialized with `serde_json` and round-trip exactly. `outcomes` on Workflow/
  Iteration is stored as the raw projection **string** (not decoded — it is a
  `json.dumps` blob the planner reads later).
- **NULL handling differs by source store — two decode paths (§6).**
  *Default-to-empty* columns mirror `task_store`'s `record.x or []` and decode a
  NULL/empty cell to `T::default()` (`[]`): `tasks.needs`, `tasks.outcomes`,
  `workflows.iteration_ids`, `iterations.attempt_ids`,
  `attempts.generator_task_ids`/`reducer_task_ids`, `attempts.outcomes`.
  *Null-preserving* columns mirror `agent_run_store`, which does **not** coerce:
  `agent_runs.initial_messages`, `agent_runs.message_history`, and
  `agent_runs.terminal_tool_result` decode NULL → `None` (mapped to
  `Option<Vec<JsonObject>>`/`Option<JsonObject>`), never `Some([])`. Use
  `decode_default` for the first set and `decode_opt` for the second.
- **`finish_request` is idempotent on terminal status:** if the request status is
  already in `{done, failed}` it returns the existing projection unchanged (no
  re-write), mirroring `task_store.py` lines 97-98. The remaining request/task
  trait methods (`set_root_task_id`, `finish_request`, `set_task_status`,
  `get`, `list_tasks_for_request`) are owned by the `eos-state`
  request/task `Store` trait and covered by its trait-level tests; AC-eos-db-01
  proves the `finish_request`/`set_root_task_id` round-trip plus this terminal
  no-op against the sqlx backend.
- **`upsert_task` semantics preserved:** insert when absent, full-field update when
  present, `updated_at` bumped — matching `TaskStore.upsert_task`.
- **CAS update preserved:** `set_task_status_if_current` updates only when the
  current status equals `expected_status`, else returns `None` (optimistic
  concurrency the workflow layer relies on).
- **`close_succeeded` atomic projection write** (Iteration): status→`succeeded` and
  `outcomes` written in one transaction so a crash leaves the row untouched
  (`iteration_store.py` docstring invariant). Continuation-iteration spawn is the
  caller's job after this returns.
- **Model registry invariants:** `key` is unique (upsert on conflict); at most one
  `is_active`; deleting the active row promotes the oldest remaining
  (`created_at, id` ascending); `active_resolved` resolves `env:`/`${VAR}`/`$VAR`
  placeholders in `kwargs` (compat-migration only); `get`/`active` redact secret
  markers by default. `class_path` is carried verbatim and **never used to import or
  dispatch** (anchor §2; GC-eos-db-01).
- **Subtle risk (from plan):** the naming gap is the highest-risk drift point — the
  row mappers in `rows.rs` are the *only* place `goal`/`deferred_goal` ↔ normalized
  names is bridged; a missing rename would silently mis-map. Covered by AC-eos-db-02.

## 9. SOLID & Principles Applied

- **DIP:** `eos-db` depends on the `eos-state` `Store` trait abstractions and
  implements them; `eos-runtime` (composition root) wires the concretes. `eos-db`
  has zero knowledge of engine/workflow callers (anchor §6 seam: per-entity `Store`
  traits, `test-mock-traits`/`api-sealed-trait`).
- **ISP:** one `Sql*Store` per entity — no god-store. Each implements only the
  narrow trait its callers need (request/task, workflow, iteration, attempt,
  agent-run).
- **LSP:** `Sql*Store` is substitutable with `eos-state`'s in-memory test store
  behind the same trait; AC tests run trait-level so either backend satisfies them.
- **SRP:** the crate only persists; lifecycle/policy stays in `eos-workflow`/engine
  (it does not decide *when* to close, only how to write a close).
- **OCP:** the only intended extension is *new migrations appended* to
  `migrations/`; schema evolves by adding ordered files, never by editing applied
  ones or patching DDL at runtime.
- **KISS/YAGNI/DRY:** no generic repository abstraction, no query builder, no
  multi-backend trait — SQLite is the only target (anchor §2). One JSON-column
  helper pair (`encode`/`decode`) is the single serialization path (DRY). No
  config knobs beyond `DatabaseConfig`.
- **Non-goals respected:** no PostgreSQL; no `class_path` dispatch; no app-level DB
  mutex; no ORM.

## 10. Gap Closeouts (tracked requirements)

- **GC-eos-db-01 — `class_path` is migration data only.** `ModelRegistrationRow`
  stores and `ModelRegistry` returns `class_path` verbatim; no code path imports,
  instantiates, or dispatches on it. Final provider dispatch is typed elsewhere by
  `llm_provider` + `model_key` (anchor §2/§4). Proven by AC-eos-db-07.
- **GC-eos-db-02 — typed DTO mapping, not dict rows.** Every read returns the
  `eos-state` DTO (`Workflow`/`Iteration`/`Attempt`/`Task`/agent-run) via `rows.rs`
  mappers; no `dict[str, Any]`/`SerializedRow` equivalent crosses the boundary.
  Proven by AC-eos-db-01..04.
- **GC-eos-db-03 — one composition-root constructor.** `Database::open(&DatabaseConfig)`
  builds the pool, runs migrations, and constructs every store runtime/engine/
  workflow needs, exposed as `Arc<dyn …Store>` accessors. Proven by AC-eos-db-08.
- **GC-eos-db-04 — reject PostgreSQL, fail fast.** Non-SQLite URL → immediate
  `DbError::PostgresRejected` before any connection; the Python Postgres pool branch
  is removed. Proven by AC-eos-db-06.
- **GC-eos-db-05 — versioned migrations replace live DDL patching.** All schema
  lives in `migrations/*.sql` (authoritative `0001_initial.sql` with final column
  names), applied by `sqlx::migrate!`; no runtime DDL patching. The `engine.py`
  conditional legacy rename/drop is dropped (not expressible as a forward
  migration — §8). Proven by AC-eos-db-05.
- **GC-eos-db-06 — explicit naming-gap mapping.** DB columns `goal`/`goal`/
  `deferred_goal` map to domain `workflow_goal`/`iteration_goal`/
  `deferred_goal_for_next_iteration` in `rows.rs` (anchor §4). Proven by AC-eos-db-02/03.

## 11. Acceptance Criteria

TDD: write each test first against the `eos-state` trait, confirm it fails, then
implement. Maps to anchor §11 "eos-db: store roundtrips for request/task/workflow/
iteration/attempt/agent_run".

- **AC-eos-db-01 — request+task roundtrip & upsert/CAS.** `create_request` →
  `RequestStore::get`; `set_root_task_id` persists and reloads; `finish_request` sets a
  terminal status, and a second `finish_request` on an already-`done`/`failed`
  request returns the existing projection unchanged (terminal no-op);
  `upsert_task` insert-then-update bumps fields/`updated_at`;
  `set_task_status_if_current` returns the DTO on match and `None` on mismatch;
  `list_tasks_for_attempt` ordered by `created_at`. Test `request_task_roundtrip`
  (ports `test_task_store_helpers.py`).
- **AC-eos-db-02 — workflow roundtrip + naming gap.** Insert with `workflow_goal`;
  reload asserts `workflow.workflow_goal == input` while the raw `goal` column holds
  it; `append_iteration_id`/`set_status` persist; `list_for_request`/
  `list_for_parent_task` ordered by `created_at`. Test `workflow_roundtrip_goal_mapping`
  (ports `test_workflow_store.py`).
- **AC-eos-db-03 — iteration roundtrip + `close_succeeded` atomicity + naming gap.**
  `insert`→`get`; `set_deferred_goal_for_next_iteration` writes the `deferred_goal`
  column and reads back as `deferred_goal_for_next_iteration`; `close_succeeded`
  sets `succeeded`+`outcomes` in one transaction; `get_by_sequence` honors the unique
  `(workflow_id, sequence_no)`. Tests `iteration_roundtrip`, `close_succeeded_atomic`
  (port `test_iteration_store.py`, `test_close_succeeded.py`).
- **AC-eos-db-04 — attempt roundtrip + outcomes parse.** `insert` defaults
  (`stage=plan`, `status=running`); `set_{planner,generator,reducer}_task_ids`,
  `set_stage`, `close` with `fail_reason`; `outcomes` round-trips through
  `parse_outcomes_record`; unique `(iteration_id, attempt_sequence_no)` enforced.
  Test `attempt_roundtrip` (ports `test_attempt_store.py`).
- **AC-eos-db-04b — agent-run roundtrip + unique task_id.** `create_run`→`finish_run`
  persists `message_history`/`terminal_tool_result`/`token_count`/`error`; a second
  `create_run` for the same `task_id` errors on the UNIQUE constraint. Test
  `agent_run_roundtrip`.
- **AC-eos-db-05 — migrations build the full schema.** Opening a fresh temp DB
  runs `0001_initial.sql`; all seven tables exist with the **final** column names
  (`iterations.outcomes`, `tasks.instruction`, `tasks.request_id`), the three
  unique indexes (`agent_runs.task_id`, `iterations(workflow_id, sequence_no)`,
  `attempts(iteration_id, attempt_sequence_no)`), and the FK cascades. Test
  `migrations_create_schema` (ports `test_db_engine.py`).
- **AC-eos-db-06 — Postgres URL rejected fast.** `Database::open` with a
  `postgresql://…` URL returns `Err(DbError::PostgresRejected)` and opens no
  connection. Test `postgres_url_rejected`.
- **AC-eos-db-07 — model registry: active lookup, env resolution, class_path is data.**
  `register`/`active`/`active_resolved` resolve `env:`/`${VAR}` placeholders;
  activating a second key deactivates the first; deleting the active promotes the
  oldest; `class_path` is returned verbatim and no import is attempted. Test
  `model_registry_active_and_resolve` (ports `test_model_store.py`).
- **AC-eos-db-08 — single composition root + FK cascade.** `Database::open` yields
  every `Arc<dyn …Store>`; deleting a `requests` row cascades to its `tasks`/
  `workflows` (FK PRAGMA on). Test `composition_root_and_cascade`.

Tests use a `tempfile`-backed SQLite file per test, dropped via RAII
(`test-fixture-raii`); async via `#[tokio::test]`; `#[cfg(test)] mod tests` +
`use super::*` (`test-cfg-test-module`, `test-use-super`); cross-store cascade test
lives in `tests/` integration dir.

## 12. Implementation Checklist

1. `error.rs`: `DbError` enum (incl. `PostgresRejected`, `#[from] sqlx::Error`,
   `#[from] MigrateError`) → verify it compiles. (`err-thiserror-lib`)
2. `migrations/0001_initial.sql`: seven tables (final column names), unique
   constraints, FK cascades, indexes → write `migrations_create_schema` test
   first, make it pass.
3. `pool.rs`: `open` with URL parse + `PostgresRejected` (test
   `postgres_url_rejected` first), PRAGMA fk/WAL/busy_timeout via
   `SqliteConnectOptions`, dir creation, `sqlx::migrate!()`.
4. `json_col.rs` + `rows.rs`: row structs + DTO mappers incl. naming gap → unit-test
   the mappers directly.
5. `repositories/request_task.rs`: `SqlRequestTaskStore` → AC-eos-db-01.
6. `repositories/workflow.rs`: `SqlWorkflowStore` → AC-eos-db-02.
7. `repositories/iteration.rs`: `SqlIterationStore` incl. transactional
   `close_succeeded` → AC-eos-db-03.
8. `repositories/attempt.rs`: `SqlAttemptStore` → AC-eos-db-04.
9. `repositories/agent_run.rs`: `SqlAgentRunStore` → AC-eos-db-04b.
10. `model_registry.rs`: `ModelRegistry` (register/delete/get/active/
    active_resolved/seed; env resolution; redaction; class_path-as-data) →
    AC-eos-db-07.
11. `composition.rs`: `Database::open` + `Arc<dyn …Store>` accessors → AC-eos-db-08.
12. `lib.rs`: `pub use` facade; `cargo fmt --check` + `clippy -D warnings`
    (`lint-rustfmt-check`).

(`small-incremental-changes`: each step is independently testable.)

---
**On completion:** update the Progress Tracker in `./overview.md` for row `eos-db`
per spec-conventions.md §13. Do not edit other crates' rows.
