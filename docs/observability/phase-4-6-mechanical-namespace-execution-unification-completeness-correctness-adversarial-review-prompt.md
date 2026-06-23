# Adversarial Completeness and Correctness Review Prompt: Phase 4.6 Mechanical Namespace Execution Unification

Use this prompt to run a read-only adversarial review of the implemented Phase
4.6 mechanical namespace execution unification in:

```text
/Users/yifanxu/machine_learning/LoVC/ephemeral-ai/ephemeral-os
```

This is a post-implementation review prompt. The task is not to re-litigate the
Phase 4.6 direction in the abstract. The task is to verify whether the current
checkout actually completed the hard cutover correctly and narrowly.

## Role

You are an adversarial implementation reviewer. Assume the intended Phase 4.6
policy is correct unless live code proves it was implemented incompletely or
incorrectly.

This is a review-only task.

Do not edit files. Do not stage files. Do not commit. Do not run destructive
commands. Do not rewrite docs or code unless explicitly asked after the review.

Lead with findings ordered by severity. Cite exact file and line references for
every finding. If you find no issues, say that clearly and list residual risks
or test gaps only after the no-findings statement.

## Review Objective

Review completeness and correctness of the Phase 4.6 hard deletion:

```text
remove RuntimeExecutionSnapshot
remove RuntimeObservabilitySnapshot.active_executions
remove CommandProcessStore::snapshot_active_executions
remove ExecutionSnapshotRecord and execution snapshot store APIs
stop daemon writes/prunes for execution_snapshots
add a V5 migration that drops execution_snapshots and old indexes
keep active command work visible only through active_namespace_executions
keep command-specific data in command APIs only
do not add a replacement kind/scope axis
```

The expected final active observability lane is exactly:

```text
RuntimeObservabilitySnapshot.active_namespace_executions
NamespaceExecutionSnapshotRecord
Phase 5 WorkspaceSnapshot.active_namespace_executions
```

There must be no compatibility alias, sidecar table, command projection,
`active_commands`, or replacement classification field.

## Start Here

Run these first and keep unrelated user changes out of scope:

```sh
git status --short
git diff --stat
git diff --name-only
git ls-files --others --exclude-standard
```

Then read:

```text
docs/observability/phase-4-6-mechanical-namespace-execution-unification.md
docs/observability/phase-4-5-namespace-runner-traces.md
docs/observability/phase-5-manager-aggregation.md
docs/observability/sandbox-observability.md
```

Treat docs as intent. Treat live Rust code, tests, migrations, and current diff
as the source of truth.

## Required Code Review Surface

Inspect these files directly:

```text
crates/sandbox-runtime/operation/src/observability.rs
crates/sandbox-runtime/operation/src/services.rs
crates/sandbox-runtime/operation/src/namespace_execution.rs
crates/sandbox-runtime/operation/src/command/service/process_store.rs
crates/sandbox-runtime/operation/src/command/service/impls/exec_command.rs
crates/sandbox-runtime/operation/src/command/service/finalize.rs
crates/sandbox-runtime/operation/src/lib.rs
crates/sandbox-runtime/operation/tests/observability_snapshot.rs

crates/sandbox-daemon/src/observability/service.rs
crates/sandbox-daemon/src/observability/namespace_execution.rs
crates/sandbox-daemon/tests/unit/observability.rs

crates/sandbox-observability/src/records.rs
crates/sandbox-observability/src/store.rs
crates/sandbox-observability/src/lib.rs
crates/sandbox-observability/tests/schema.rs
```

Use `rg` to verify call paths and deleted names, but do not treat broad grep
hits as proof without reading surrounding code. In particular,
`NamespaceExecutionSnapshotRecord` contains the substring
`ExecutionSnapshotRecord`; use word-boundary searches when checking whether the
old type is truly gone.

## Core Correctness Questions

Answer these directly:

1. Does `SandboxRuntimeOperations::observability_snapshot()` aggregate only
   workspace snapshots, active namespace execution snapshots, completed
   namespace execution records, and partial errors?
2. Is `CommandProcessStore` no longer read by runtime observability snapshot
   aggregation?
3. Does command execution still begin a namespace execution with
   `operation_name = "exec_command"`?
4. Does command completion still use `ActiveCommandProcess.namespace_execution_id`
   to complete namespace execution records?
5. Are command identity, command lifecycle, stdin, stdout, stderr, transcript
   paths/content, command text, and environment absent from namespace execution
   snapshots?
6. Does daemon observability write/prune only `namespace_execution_snapshots`
   for active execution state?
7. Are completed namespace execution traces still projected and acknowledged
   only after successful store writes?
8. Is final SQLite schema free of `execution_snapshots`,
   `idx_execution_snapshots_workspace`, and `idx_execution_snapshots_command`?
9. Did the implementation add only a new drop migration instead of rewriting
   historical migration SQL?
10. Are Phase 5 DTO docs aligned to expose only
    `active_namespace_executions: Vec<NamespaceExecutionSnapshot>`?

## Deletion Completeness Checks

Run and interpret these checks:

```sh
rg -n '\bRuntimeExecutionSnapshot\b|\bExecutionSnapshotRecord\b|\bupsert_execution_snapshots\b|\bprune_execution_snapshots\b|\bexecution_snapshots_for_test\b' \
  crates/sandbox-runtime/operation/src \
  crates/sandbox-daemon/src/observability \
  crates/sandbox-observability/src \
  crates/sandbox-observability/tests \
  crates/sandbox-daemon/tests

rg -n 'active_executions|active_commands' \
  crates/sandbox-runtime/operation/src/observability.rs \
  crates/sandbox-runtime/operation/src/services.rs \
  crates/sandbox-daemon/src/observability/service.rs \
  docs/observability/phase-5-manager-aggregation.md

rg -n 'NamespaceExecutionKind|namespace_execution_kind|runner_kind|execution_scope' \
  crates/sandbox-runtime/operation/src/namespace_execution.rs \
  crates/sandbox-daemon/src/observability/namespace_execution.rs \
  crates/sandbox-observability/src/records.rs \
  crates/sandbox-observability/src/store.rs
```

For `execution_kind`, distinguish between forbidden active API/schema additions
and immutable historical V2 migration SQL. Historical V2 SQL may still mention
the old column; active APIs, records, tests, and final schema expectations must
not preserve the lane.

## Schema and Migration Review

Adversarially check:

- `MIGRATIONS` includes a V5 migration after Phase 4.5.
- V5 drops exactly the old execution snapshot indexes and table.
- Phase 1-4 migration SQL text is not rewritten.
- `schema_migrations` count tests increased.
- final schema tests reject the old table and indexes.
- no production read/write helper can still query `execution_snapshots`.
- namespace execution snapshot and trace tables remain unchanged except for
  removing old-lane coupling.

Look for subtle migration bugs:

- fresh database applies V2, V4, then V5 and ends without old objects;
- existing database that already has V2/V4 applies V5 without checksum drift;
- dropping a table does not break later tests or query helpers;
- no tests still call a removed helper or rely on querying a dropped table.

## Runtime Boundary Review

Verify that the runtime boundary is generic:

```rust
pub struct RuntimeObservabilitySnapshot {
    pub workspaces: Vec<RuntimeWorkspaceSnapshot>,
    pub active_namespace_executions: Vec<RuntimeNamespaceExecutionSnapshot>,
    pub completed_namespace_executions: Vec<NamespaceExecutionRecord>,
    pub partial_errors: Vec<String>,
}

pub struct RuntimeNamespaceExecutionSnapshot {
    pub namespace_execution_id: NamespaceExecutionId,
    pub workspace_session_id: WorkspaceSessionId,
    pub operation_name: String,
    pub lifecycle_state: NamespaceExecutionLifecycle,
    pub started_at_unix_ms: i64,
}
```

Challenge anything that adds:

```text
command_session_id
command text
transcript path/content
stdin/stdout/stderr
environment
execution_kind
runner_kind
execution_scope
command lifecycle/finalization state
workspace ownership
process group id
```

to namespace execution snapshots.

## Daemon Mapping Review

Verify that daemon observability:

- destructures `RuntimeObservabilitySnapshot` without `active_executions`;
- maps `active_namespace_executions` through
  `observability/namespace_execution.rs`;
- writes active state only through `upsert_namespace_execution_snapshots`;
- prunes active state only through `prune_namespace_execution_snapshots`;
- no longer builds `ExecutionSnapshotRecord`;
- still records workspace snapshots, resource samples, request traces, async
  command finalization traces, and completed namespace execution traces.

Check that removing the old lane did not accidentally remove command
finalization trace data. Async command finalization traces may still carry
`command_session_id`; that belongs to trace metadata, not active namespace
execution snapshots.

## Test Adequacy Review

Review tests for both proof and gaps:

- runtime test proves active command work appears as an active namespace
  execution with `operation_name = "exec_command"`;
- daemon test proves active command work persists only as a namespace execution
  snapshot;
- daemon test proves command payload strings do not appear in namespace
  execution snapshot rows;
- schema test proves final migrated schema excludes `execution_snapshots` and
  old indexes;
- schema test proves namespace execution snapshot columns do not include command
  payload fields or kind fields;
- store tests still cover namespace execution snapshot upsert/prune and
  completed namespace execution trace insert.

Do not accept a test that merely asserts "no old rows were written" if the old
table was dropped and no longer queryable. Prefer final schema and API-surface
checks.

## Verification Commands

Run the smallest relevant subset first if time is limited, then broaden if a
failure or suspicious gap appears:

```sh
cargo fmt --check
cargo check -p sandbox-runtime --tests
cargo test -p sandbox-runtime observability
cargo check -p sandbox-observability --tests
cargo test -p sandbox-observability
cargo test -p sandbox-daemon observability
cargo clippy -p sandbox-runtime --all-targets --no-deps -- -D warnings
cargo clippy -p sandbox-daemon --all-targets --no-deps -- -D warnings
git diff --check
```

If you do not run a command, say so and explain why. If a command has expected
false-positive grep hits, identify the exact false positives and explain why
they are not active compatibility surfaces.

## Findings to Prefer

Prioritize findings that would cause:

- a public API or DTO to expose the removed lane;
- an existing database to fail migration;
- final schema to retain dropped objects;
- daemon collection to lose completed namespace execution traces;
- active command work to disappear entirely from observability;
- command payload data to leak through namespace execution snapshots;
- tests to pass while the old production API remains usable;
- docs to direct Phase 5 to rebuild `active_commands` or `ExecutionSnapshot`;
- a new kind/scope axis to appear in active code without a second live producer.

Do not raise findings for:

- old terms preserved in explicitly historical Phase 2 docs when a superseded
  note is present;
- old terms inside immutable historical migration SQL when V5 drops the final
  objects;
- `command_session_id` in command APIs or async command finalization traces;
- command lifecycle/finalization details remaining inside command-domain code.

## Output Format

Use this structure:

```text
Findings
- [P0/P1/P2/P3] file:line - concise title
  Evidence:
  Impact:
  Required fix:

Open Questions
- ...

Verification
- command: pass/fail/not run

Residual Risk
- ...

Overall Verdict
- Complete / incomplete / correct with caveats / incorrect
```

If there are no findings, start with:

```text
Findings
No correctness or completeness issues found.
```

Then still include verification and residual risk.
