# Phase 4.6: Mechanical Namespace Execution Unification

## Purpose

Phase 4.6 removes the duplicated active execution model before Phase 5 exposes
daemon and manager query APIs.

The canonical active-work unit is `namespace_execution_id`. Observability must
not expose three parallel active-work lists:

```text
active_executions
active_commands
active_namespace_executions
```

After Phase 4.6, there is one active list:

```text
active_namespace_executions
```

Each active namespace execution has one stable namespace execution id and one
concrete operation name:

```text
operation_name = exec_command
```

Command-specific APIs may still use `command_session_id` for stdin, transcript
reads, and command status. Observability snapshots do not create a command
projection, command attachment, sidecar table, or compatibility alias.
Phase 4.6 also does not introduce a replacement `execution_kind`,
`namespace_execution_kind`, `runner_kind`, or substrate-classification field.

## Why The Split Exists Today

Phase 2 introduced `RuntimeExecutionSnapshot` and `execution_snapshots` before
the runtime had a generic namespace execution ledger. The Phase 2 source of
truth was `CommandProcessStore.active`, so the row shape included command-only
fields such as `command_session_id`, command text, finalization state,
workspace ownership, process group id, and transcript path.

That was intentional at the time. Phase 2 described the DTO as a runtime
execution snapshot rather than a command-only mechanism because `exec_command`
was the only long-running namespace-runner producer.

Phase 4.5 then introduced the generic namespace execution model:

```text
WorkspaceSession
  NamespaceExecutionAttempt
    namespace_execution_id
    operation_name
```

For command execution, the relationship became:

```text
CommandProcessStore
  command_session_id
  namespace_execution_id

NamespaceExecutionStore
  namespace_execution_id
  operation_name = exec_command
```

Phase 4.5 explicitly kept `command_session_id` out of the generic namespace
execution record. Command identity stayed in the command domain, and the
namespace execution record stayed operation-neutral.

The current duplication exists because Phase 4.5 added
`active_namespace_executions` beside the older Phase 2 `active_executions`
instead of deleting the older lane in the same phase. That was useful while
landing the namespace ledger, but it is the wrong shape to expose in Phase 5.

## Architecture Decision

Use a hard mechanical cutover:

- remove `RuntimeExecutionSnapshot`;
- remove `RuntimeObservabilitySnapshot.active_executions`;
- remove `CommandProcessStore::snapshot_active_executions`;
- stop writing `execution_snapshots`;
- remove the production `ExecutionSnapshotRecord` write/read surface;
- keep `namespace_execution_snapshots` as the only active execution snapshot
  table;
- keep the existing namespace execution record shape: `namespace_execution_id`,
  `workspace_session_id`, `operation_name`, lifecycle, timing, terminal status,
  and bounded errors;
- defer any generic kind/substrate field until a second live namespace execution
  producer makes it necessary;
- update Phase 5 DTOs so workspaces contain only
  `active_namespace_executions`.

Do not replace the removed lane with:

- a command projection;
- a nested command attachment;
- a command sidecar table keyed by `namespace_execution_id`;
- an alias where `active_executions` and `active_commands` both point at the
  same vector;
- a compatibility response shape that keeps the three names alive.

The cost is intentional: command-only observability fields disappear from the
active snapshot hierarchy. If a command user needs command text, transcript
content, stdin state, or command-specific finalization details, they should use
the command APIs. Phase 5 manager aggregation should stay summary-first and
namespace-execution-first.

This is an aggressive deletion, not a rename from `execution_kind = "command"`
to `execution_kind = "shell_exec"`. The old classification axis is removed
entirely. `operation_name` is the runtime operation using the namespace
execution ledger.

## Resulting Model

Runtime snapshot:

```rust
pub struct RuntimeObservabilitySnapshot {
    pub workspaces: Vec<RuntimeWorkspaceSnapshot>,
    pub active_namespace_executions: Vec<RuntimeNamespaceExecutionSnapshot>,
    pub completed_namespace_executions: Vec<NamespaceExecutionRecord>,
    pub partial_errors: Vec<String>,
}
```

Namespace execution snapshot:

```rust
pub struct RuntimeNamespaceExecutionSnapshot {
    pub namespace_execution_id: NamespaceExecutionId,
    pub workspace_session_id: WorkspaceSessionId,
    pub operation_name: String,
    pub lifecycle_state: NamespaceExecutionLifecycle,
    pub started_at_unix_ms: i64,
}
```

Phase 4.6 intentionally keeps this shape unchanged from Phase 4.5. A
single-value kind such as `ShellExec` would add public shape without adding
runtime meaning.

Storage snapshot:

```rust
pub struct NamespaceExecutionSnapshotRecord {
    pub sandbox_id: String,
    pub namespace_execution_id: String,
    pub workspace_session_id: String,
    pub operation: String,
    pub lifecycle_state: String,
    pub sampled_at_unix_ms: i64,
    pub error_message: Option<String>,
}
```

Phase 5 workspace DTO:

```rust
pub struct WorkspaceSnapshot {
    pub workspace_id: String,
    pub state: String,
    pub remount_state: Option<String>,
    pub profile: Option<String>,
    pub sampled_at_unix_ms: Option<i64>,
    pub resources: Option<ResourceSnapshot>,
    pub active_namespace_executions: Vec<NamespaceExecutionSnapshot>,
    pub recent_traces: Vec<TraceSummary>,
    pub partial_errors: Vec<SnapshotPartialError>,
}
```

Phase 5 namespace execution DTO:

```rust
pub struct NamespaceExecutionSnapshot {
    pub namespace_execution_id: String,
    pub operation: String,
    pub lifecycle_state: String,
}
```

There is no `ExecutionSnapshot` DTO in the public Phase 5 query API.
`active_commands` is not a serialized field. A display client may identify
command operations with `operation == "exec_command"`, but the wire/API shape
remains namespace execution only.

The public Phase 5 DTO intentionally omits fields that are storage mechanics or
not backed by row-level behavior:

- `workspace_session_id` is used by storage/query code to group rows, but the
  public active namespace execution entry is nested under a `WorkspaceSnapshot`;
- `sampled_at_unix_ms` is inherited from the containing workspace/sandbox
  snapshot unless a future API proves per-row freshness is needed;
- per-execution `partial_errors` are deferred until namespace execution rows can
  actually carry row-level partial errors.

## Operation Semantics

`operation_name` is the concrete runtime operation:

```text
exec_command
```

For the current command path:

```text
namespace_execution_id = namespace_execution_1
operation_name = exec_command
```

Do not add or use these fields in Phase 4.6:

```text
execution_kind
namespace_execution_kind
runner_kind
execution_scope
```

Command is a domain/API concept owned by `CommandProcessStore` and command
operations. `shell_exec` is an internal runner substrate today, not a public
namespace execution category. Future non-command shell operations should use
distinct `operation_name` values such as `workspace_probe` or
`bootstrap_package_install` if they are modeled as namespace executions. Add a
second classification axis only when there is both a second live producer and a
concrete query need proving that `operation_name` is not enough.

## Lifecycle Semantics

The canonical namespace lifecycle remains generic:

```text
starting
running
terminal
```

Phase 4.6 does not copy command lifecycle states into namespace execution
snapshots:

```text
quiesced_for_remount
finalizing
cancelled
not_started
in_progress
complete
failed
```

Cancellation and failures are represented at completion through
`NamespaceExecutionTerminalStatus` and completed namespace execution traces.
While a command is active, observability only promises the generic namespace
execution lifecycle.

If a future non-command namespace operation needs another generic active state,
add it to `NamespaceExecutionLifecycle` only when the state is meaningful for
all namespace execution producers. Do not use command-only lifecycle terms as
generic namespace states.

## Storage Migration

Add a new schema migration after the current Phase 4.5 migration. This should
be a V5 migration, for example
`phase_4_6_mechanical_namespace_execution_unification`:

```sql
DROP INDEX IF EXISTS idx_execution_snapshots_workspace;
DROP INDEX IF EXISTS idx_execution_snapshots_command;
DROP TABLE IF EXISTS execution_snapshots;
```

This is a hard cutover. Do not migrate rows from `execution_snapshots` into
`namespace_execution_snapshots`; the old table represented active current state,
not durable history. Existing completed namespace execution traces already live
in `namespace_execution_traces`.

Do not rewrite historical migration SQL. The current store records checksums in
`schema_migrations`, so changing the Phase 2 migration text would break existing
databases that have already applied it. Fresh databases should still apply the
historical Phase 2 migration and then apply the new V5 drop migration. The final
database schema after all migrations must not contain `execution_snapshots` or
`idx_execution_snapshots_*`, even though historical migration text may still
mention them.

## File Plan

### Runtime

`crates/sandbox-runtime/operation/src/observability.rs`

- Remove `RuntimeExecutionSnapshot`.
- Remove `RuntimeObservabilitySnapshot.active_executions`.
- Remove the now-unused `CommandSessionId` import.

`crates/sandbox-runtime/operation/src/namespace_execution.rs`

- Do not add `NamespaceExecutionKind` or any replacement kind/scope enum.
- Keep `BeginNamespaceExecution`, `NamespaceExecutionRecord`, and
  `RuntimeNamespaceExecutionSnapshot` on the existing operation-name model.
- Keep `command_session_id` out of namespace execution records.

`crates/sandbox-runtime/operation/src/services.rs`

- Stop calling `self.command.process_store().snapshot_active_executions()`.
- Build `RuntimeObservabilitySnapshot` from workspace snapshots,
  `NamespaceExecutionStore::snapshot_active_namespace_executions()`,
  drained completed namespace executions, and partial errors.
- Keep command process state out of the observability aggregate.

`crates/sandbox-runtime/operation/src/command/service/process_store.rs`

- Remove `snapshot_active_executions`.
- Remove `map_lifecycle_state` and `map_finalization_state` if they become
  unused.
- Keep `ActiveCommandProcess.namespace_execution_id`; command APIs and
  completion still need it for correlation and terminal namespace execution
  updates.

`crates/sandbox-runtime/operation/src/command/service/impls/exec_command.rs`

- Continue beginning namespace execution with `operation_name = "exec_command"`.
- Do not pass runner substrate or command identity into
  `BeginNamespaceExecution`.

`crates/sandbox-runtime/operation/src/lib.rs`

- Stop re-exporting `RuntimeExecutionSnapshot`.

### Daemon Observability

`crates/sandbox-daemon/src/observability/service.rs`

- Remove `ExecutionSnapshotRecord` and `RuntimeExecutionSnapshot` imports.
- Remove the loop that maps `active_executions` into execution records.
- Remove `execution_record`.
- Remove calls to `upsert_execution_snapshots` and
  `prune_execution_snapshots`.
- Continue writing `namespace_execution_snapshots` from
  `active_namespace_executions`.

`crates/sandbox-daemon/src/observability/namespace_execution.rs`

- Keep this as the only active execution snapshot mapper.
- Map `operation_name` to the stable storage/API `operation` string.
- Do not add command fields.

### Observability Store

`crates/sandbox-observability/src/records.rs`

- Remove `ExecutionSnapshotRecord` and its validation implementation.
- Keep `NamespaceExecutionSnapshotRecord`.
- Do not add `execution_kind` validation to namespace execution snapshots or
  trace records.

`crates/sandbox-observability/src/store.rs`

- Add the schema migration that drops `execution_snapshots` and its indexes.
- Remove `upsert_execution_snapshots`.
- Remove `prune_execution_snapshots`.
- Remove `execution_snapshots_for_test`.
- Remove production imports and SQL for `ExecutionSnapshotRecord`.
- Keep `upsert_namespace_execution_snapshots`,
  `prune_namespace_execution_snapshots`, and the namespace trace APIs.

`crates/sandbox-observability/src/lib.rs`

- Stop re-exporting `ExecutionSnapshotRecord`.

### Docs

`docs/observability/sandbox-observability.md`

- Mark the Phase 2 `execution_snapshots` model as superseded by Phase 4.6.
- Replace hierarchy examples that list `execution_snapshots` with
  `namespace_execution_snapshots`.
- Align the namespace execution hierarchy with `operation_name = exec_command`
  and no kind/substrate axis.

`docs/observability/phase-4-5-namespace-runner-traces.md`

- Keep the "Execution Scope" decision: Phase 4.6 deletes the old
  command-shaped lane without adding a single-value kind field.

`docs/observability/phase-2-runtime-snapshots.md`

- Add a short superseded note near the Phase 2 execution snapshot sections.
- Do not rewrite the historical Phase 2 plan as if it had made the Phase 4.6
  decision originally.

`docs/observability/phase-3-request-method-traces.md`

- Update references that say command trace correlation enriches
  `RuntimeExecutionSnapshot`; the target after Phase 4.6 is namespace execution
  traces/snapshots.

`docs/observability/phase-5-manager-aggregation.md`

- Remove `ExecutionSnapshot` from the DTO section.
- Remove `active_executions` and `active_commands` from `WorkspaceSnapshot`.
- Remove store read APIs for `load_execution_snapshots`.
- Keep `load_namespace_execution_snapshots`.
- State that command work is represented as namespace executions with
  `operation == "exec_command"`.

## Test Plan

`crates/sandbox-runtime/operation` tests:

- Runtime observability snapshot contains active namespace executions for
  running commands.
- Active command namespace executions have `operation_name = exec_command`.
- Runtime observability snapshot no longer exposes `active_executions`.
- `CommandProcessStore` still carries `namespace_execution_id` for command
  completion.

`crates/sandbox-daemon/tests/unit/observability.rs`:

- Daemon snapshot collection writes `namespace_execution_snapshots`.
- Daemon snapshot collection does not write or query `execution_snapshots`.
- Command transcript path, command text, stdin, and command output do not appear
  in namespace execution snapshots.

`crates/sandbox-observability/tests/schema.rs`:

- Final migrated schema does not include `execution_snapshots`.
- Final migrated schema does not include `idx_execution_snapshots_workspace` or
  `idx_execution_snapshots_command`.
- Schema migration count increases to include the Phase 4.6 V5 drop migration.
- Namespace execution snapshot upsert and prune tests remain.
- Namespace execution snapshot and trace rows do not add `execution_kind`.
- Any test helper named `execution_snapshots_for_test` is removed.

Phase 5 tests, when implemented:

- `WorkspaceSnapshot` has one active namespace execution list.
- No serialized `active_executions`, `active_commands`, or `ExecutionSnapshot`
  fields exist.
- Manager aggregation identifies command operations by `operation`, not by
  command session id.

## Verification Commands

```sh
cargo fmt --check
cargo check -p sandbox-runtime --tests
cargo test -p sandbox-runtime observability
cargo check -p sandbox-observability --tests
cargo test -p sandbox-observability
cargo test -p sandbox-daemon observability
cargo clippy -p sandbox-runtime --all-targets --no-deps -- -D warnings
cargo clippy -p sandbox-daemon --all-targets --no-deps -- -D warnings
rg -n "RuntimeExecutionSnapshot|ExecutionSnapshotRecord|upsert_execution_snapshots|prune_execution_snapshots|execution_snapshots_for_test" crates/sandbox-runtime/operation/src crates/sandbox-daemon/src/observability crates/sandbox-observability/src crates/sandbox-observability/tests crates/sandbox-daemon/tests
rg -n "active_executions|active_commands" crates/sandbox-runtime/operation/src/observability.rs crates/sandbox-runtime/operation/src/services.rs crates/sandbox-daemon/src/observability/service.rs docs/observability/phase-5-manager-aggregation.md
rg -n "NamespaceExecutionKind|execution_kind|namespace_execution_kind|runner_kind|execution_scope" crates/sandbox-runtime/operation/src/namespace_execution.rs crates/sandbox-daemon/src/observability/namespace_execution.rs crates/sandbox-observability/src/records.rs crates/sandbox-observability/src/store.rs
git diff --check
```

The `rg` commands are expected to return no production hits after
implementation. Historical docs and immutable historical migration SQL may still
mention superseded Phase 2 names, and the V5 migration must mention the dropped
table/index names. The active runtime, daemon, store APIs/tests, and Phase 5 DTO
plan must not keep the old lane or add a replacement kind axis.

## Completion Checklist

- [x] `RuntimeExecutionSnapshot` is removed.
- [x] `RuntimeObservabilitySnapshot.active_executions` is removed.
- [x] `CommandProcessStore::snapshot_active_executions` is removed.
- [x] Daemon observability writes only namespace execution snapshots for active
      execution state.
- [x] Namespace execution snapshots carry `operation = exec_command` for
      active command work.
- [x] No active observability path adds `execution_kind`, `runner_kind`,
      `execution_scope`, or a replacement single-value kind field.
- [x] `execution_snapshots` and its indexes are dropped from the final SQLite
      schema.
- [x] `ExecutionSnapshotRecord` and its store APIs are removed.
- [x] Phase 5 DTOs expose `active_namespace_executions` only.
- [x] `active_commands` is not serialized; command work is identified by
      `operation == "exec_command"` when a display layer needs that label.
- [x] No command transcript content, command output, stdin, or environment data
      is added to namespace execution snapshots.
- [x] No projection, attachment, sidecar table, or compatibility alias is added
      for command-shaped observability snapshots.
