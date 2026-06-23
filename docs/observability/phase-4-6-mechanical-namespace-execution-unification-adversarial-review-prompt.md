# Adversarial Architecture Review Prompt: Phase 4.6 Mechanical Namespace Execution Unification

Use this prompt to run a read-only adversarial review of the aggressive Phase
4.6 mechanical namespace execution unification plan in:

```text
/Users/yifanxu/machine_learning/LoVC/ephemeral-ai/ephemeral-os/docs/observability/phase-4-6-mechanical-namespace-execution-unification.md
```

## Role

You are an adversarial architecture reviewer. Treat the Phase 4.6 plan as
directionally right about deleting the duplicate command-shaped active execution
lane, but potentially still wrong in scope, sequencing, schema strategy, DTO
shape, verification, or future extensibility.

This is a review-only task.

Do not implement code. Do not rewrite the spec unless explicitly asked after the
review. Do not create broad findings from stale docs alone. Treat docs as
proposals and live code as the source of truth.

Lead with findings, ordered by severity, and cite exact file and line
references. After findings, state whether the aggressive removal is still the
right recommendation, or whether a smaller or safer variant is required.

## Review Goal

Review whether Phase 4.6 should remain an aggressive hard cutover:

```text
delete active_executions
delete execution_snapshots
delete RuntimeExecutionSnapshot
delete ExecutionSnapshotRecord
delete command-shaped active observability APIs
keep namespace execution rows unchanged
do not add execution_kind / runner_kind / execution_scope
```

Answer these questions directly:

```text
Is the current Phase 4.6 spec now simple enough?

Is the hard deletion safe, or does it hide a required migration, compatibility,
or DTO transition?

Does the spec still accidentally preserve the old Phase 2 command-shaped
classification lane under another name?

Is deferring execution_kind / NamespaceExecutionKind / runner_kind still correct
after checking live code and future stress cases?

Does operation_name = exec_command carry enough meaning for Phase 5 display and
future non-command shell namespace operations?

Can Phase 4.6 be implemented as a narrow deletion plus doc alignment, without
adding new runtime/store/API concepts?
```

## Repo

```text
/Users/yifanxu/machine_learning/LoVC/ephemeral-ai/ephemeral-os
```

Start by running:

```sh
git status --short
git diff --stat
git diff --name-only
git ls-files --others --exclude-standard
```

If the worktree includes unrelated user changes, keep them out of scope unless
they affect Phase 4.6 observability architecture. Do not revert anything.

## Required Reading

Read the target Phase 4.6 spec first:

```text
docs/observability/phase-4-6-mechanical-namespace-execution-unification.md
```

Then read the prior namespace execution plan and adjacent observability docs:

```text
docs/observability/phase-4-5-namespace-runner-traces.md
docs/observability/phase-2-runtime-snapshots.md
docs/observability/phase-3-request-method-traces.md
docs/observability/phase-5-manager-aggregation.md
docs/observability/sandbox-observability.md
```

Then inspect live code for the real runtime, daemon, and store shapes:

```text
crates/sandbox-runtime/operation/src/observability.rs
crates/sandbox-runtime/operation/src/services.rs
crates/sandbox-runtime/operation/src/namespace_execution.rs
crates/sandbox-runtime/operation/src/command/service/core.rs
crates/sandbox-runtime/operation/src/command/service/process_store.rs
crates/sandbox-runtime/operation/src/command/service/impls/exec_command.rs
crates/sandbox-runtime/operation/src/command/service/finalize.rs
crates/sandbox-runtime/operation/src/lib.rs
crates/sandbox-daemon/src/observability/service.rs
crates/sandbox-daemon/src/observability/namespace_execution.rs
crates/sandbox-observability/src/records.rs
crates/sandbox-observability/src/store.rs
crates/sandbox-observability/src/lib.rs
crates/sandbox-observability/tests/schema.rs
crates/sandbox-daemon/tests/unit/observability.rs
```

Use `rg` for call paths and names. Verify current signatures instead of
assuming the docs are current.

## Adversarial Premise

The current Phase 4.6 plan is intentionally aggressive. It does not replace
`execution_kind = "command"` with `execution_kind = "shell_exec"`. It deletes
the classification axis and keeps namespace execution rows on:

```text
namespace_execution_id
workspace_session_id
operation_name / operation
lifecycle_state
timing/status/error fields
```

Challenge whether that is actually complete.

Do not accept "aggressive removal" as inherently correct. Ask whether the plan:

- deletes too much for Phase 5 to query active command work usefully;
- deletes too little by leaving test helpers, read APIs, DTO fields, docs, or
  indexes that preserve the old lane;
- under-specifies migration behavior for existing SQLite files;
- accidentally depends on a manager or daemon query shape that still expects
  `ExecutionSnapshot`;
- fails to update enough docs to prevent Phase 5 from rebuilding
  `active_commands`;
- hides command lifecycle/finalization facts that should remain observable
  somewhere else;
- defers kind/substrate classification in a way that will cause concrete
  near-term rework;
- uses `workspace_session_id` in storage but `workspace_id` in DTO hierarchy in
  a way that will confuse public API users.

If you argue for reintroducing a second axis such as `execution_kind`,
`namespace_execution_kind`, `runner_kind`, or `execution_scope`, you must prove:

1. there is a second live namespace execution producer now, or an imminent Phase
   5 query that cannot be answered by `operation`;
2. the field belongs in runtime DTOs, SQLite rows, and public query DTOs rather
   than only an internal adapter;
3. the benefit is concrete enough to justify the migration and LOC churn.

## Review Axes

### 1. Concept Count

Count the concepts exposed by the plan:

```text
namespace_execution_id
operation_name / operation
lifecycle_state
workspace_session_id / workspace_id
command_session_id in command domain only
completed namespace execution traces
```

Answer:

- Which concepts are essential for Phase 4.6?
- Which concepts are only Phase 5 display details?
- Does the spec leave any old Phase 2 concept alive under another route?
- Is `operation_name` enough to identify active `exec_command` work?
- Should any public DTO field be renamed for consistency, or is the current
  runtime/storage/API naming split acceptable?

Prefer one stable namespace execution id and one concrete operation name unless
a second classification axis clearly reduces current complexity.

### 2. Ownership Boundaries

Verify live ownership:

- `NamespaceExecutionStore` owns generic namespace execution lifecycle.
- `CommandProcessStore` owns `command_session_id`, command text, transcript
  paths, stdin, command lifecycle, and command finalization.
- Daemon observability maps runtime snapshots to SQLite records.
- `sandbox-observability` owns schema and bounded row validation.
- Phase 5 daemon/manager DTOs should not expose raw store rows.

Challenge:

- Does hard deletion keep command state in the command domain?
- Does any daemon or manager code still need command-specific active fields after
  Phase 4.6?
- Does the spec accidentally make namespace execution own command lifecycle or
  finalization semantics?
- Does any proposed test require reading command text, transcript paths, stdin,
  stdout, stderr, environment, or command output through namespace execution
  snapshots?

### 3. Schema Minimality

Challenge every storage change.

Ask:

- Is the drop-only migration enough?
- If migration SQL is checksum-protected, should old migrations stay immutable
  and Phase 4.6 add a new drop-table migration?
- Should fresh databases still run through the historical `execution_snapshots`
  migration and then drop it, or should the implementation rewrite historical
  migration text?
- Does dropping `execution_snapshots` require removing all production and
  test-support APIs, not just write paths?
- Do final schema tests explicitly reject `execution_snapshots` and
  `idx_execution_snapshots_*`?
- Do namespace execution snapshot and trace tables stay unchanged except for
  deleting the old lane?

Reject raw SQL APIs, manager-side mirrors, observability cache databases,
compatibility tables, child writes to SQLite, and any migration that copies
command-shaped active rows into namespace execution rows.

### 4. DTO Minimality

Review the Phase 5 DTO implied by Phase 4.6:

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

pub struct NamespaceExecutionSnapshot {
    pub namespace_execution_id: String,
    pub workspace_session_id: String,
    pub operation: String,
    pub lifecycle_state: String,
    pub sampled_at_unix_ms: Option<i64>,
    pub partial_errors: Vec<SnapshotPartialError>,
}
```

Challenge:

- Is a separate active command list truly gone from the public DTO?
- Is `workspace_session_id` correct inside `NamespaceExecutionSnapshot`, or
  should the public DTO use `workspace_id` because it is nested under a workspace
  node?
- Is `sampled_at_unix_ms` per execution necessary, or can it inherit from the
  workspace/sandbox snapshot?
- Are per-execution `partial_errors` useful before row-level partial errors
  exist?
- Is `operation` the right public field name, or should API DTOs preserve
  `operation_name` for consistency with runtime naming?

Propose the smallest DTO that still preserves the display hierarchy.

### 5. Runtime LOC and Mechanical Cutover

Challenge the implementation surface.

Ask:

- Can implementation be reduced to deleting `RuntimeExecutionSnapshot`,
  `active_executions`, and `CommandProcessStore::snapshot_active_executions`,
  while leaving `RuntimeNamespaceExecutionSnapshot` unchanged?
- Are `map_lifecycle_state`, `map_finalization_state`, and command-shaped
  snapshot imports actually dead after the deletion?
- Does `SandboxRuntimeOperations::observability_snapshot()` still aggregate
  workspaces, active namespace executions, completed namespace executions, and
  partial errors without touching command process state?
- Does command completion still retain `namespace_execution_id` so terminal
  namespace execution updates work?
- Does the file plan require touching more modules than necessary?

Prefer hard deletion over deletion plus abstraction unless the abstraction
prevents a concrete current bug.

### 6. Phase 5 Compatibility

Inspect the Phase 5 manager aggregation doc and expected query shape.

Ask:

- Does Phase 5 still mention `active_executions`, active command lists,
  `ExecutionSnapshot`, `load_execution_snapshots`, or old indexes?
- Can Phase 5 display active command work as namespace execution rows with
  `operation == "exec_command"`?
- Does Phase 5 need command-session identity in the active tree, or should
  command-specific inspection remain in command APIs?
- Are recent traces still enough for completed request/async/namespace
  execution history?
- Does the daemon query plan read only daemon-local store APIs and never manager
  SQLite files?

### 7. Future Extensibility Stress Cases

Stress the design against future operations:

```text
workspace probe using shell execution
workspace setup validation using shell execution
package install/bootstrap operation using shell execution
remount verification using namespace runner
future tool/plugin execution using shell execution
mount/remount operations that are not shell execution
```

For each case, decide whether the best generic representation is:

```text
namespace_execution_id + operation_name
```

or:

```text
namespace_execution_id + second classification axis + operation_name
```

If the second axis helps only for hypothetical grouping, say so and defer it. If
it prevents concrete future churn, explain exactly which files, queries, or
schema rows would otherwise churn.

### 8. Verification Adequacy

Review the spec's verification commands and tests.

Ask:

- Do grep guards cover runtime, daemon observability, store records, store SQL,
  tests, and Phase 5 DTO docs?
- Should verification include `cargo check -p sandbox-runtime --tests`,
  `cargo check -p sandbox-observability --tests`, and focused daemon/store tests?
- Is `cargo test -p sandbox-runtime observability` the right filter after the
  old runtime execution snapshot tests are deleted or renamed?
- Should schema tests assert final table/index absence, not only API absence?
- Should daemon tests prove command text/transcript/stdin/output do not appear in
  namespace execution snapshots?

## Candidate Designs

After findings, compare these candidate designs:

```text
Candidate A: Spec-as-written aggressive deletion
  Delete active_executions and execution_snapshots.
  Delete RuntimeExecutionSnapshot and ExecutionSnapshotRecord.
  Keep namespace execution rows unchanged.
  No execution_kind / runner_kind / execution_scope.

Candidate B: Aggressive deletion plus DTO tightening
  Same deletion as Candidate A.
  Also simplify or rename Phase 5 NamespaceExecutionSnapshot fields if
  sampled_at_unix_ms, partial_errors, workspace_session_id, or operation naming
  are not justified.

Candidate C: Kind-preserving exception
  Same deletion as Candidate A.
  Add a second classification axis only if live code or Phase 5 queries prove
  operation_name is insufficient now.
```

Compare them on:

- user-facing intuition;
- generic future operations;
- storage churn;
- runtime LOC;
- Phase 5 DTO stability;
- risk of reintroducing command-shaped thinking;
- implementation/test blast radius.

Pick one recommended design.

## Forbidden Recommendations

Do not recommend:

- keeping `active_executions` as an alias;
- keeping active commands as a serialized field;
- adding command projections, command attachments, or command sidecar tables;
- adding `command_session_id` to namespace execution records;
- storing command text, transcript paths, stdin, stdout, stderr, environment, or
  command output in namespace execution snapshots;
- manager-side SQLite reads;
- manager-side observability mirror tables or databases;
- raw SQL query APIs;
- Prometheus, Grafana, Loki, Tempo, OTLP, or log export;
- child writes to `observability.sqlite`;
- public response envelopes such as `{ result, meta }`;
- compatibility fallback APIs.

## Output Format

Use this structure:

```text
Findings

1. [Severity] Finding title
   Evidence: file:line
   Problem:
   Why it matters:
   Smaller design:

2. ...

Open Questions

Candidate Designs

Recommended Design

Minimal File Plan

Deferred Work
```

Severity scale:

```text
P0 blocks implementation correctness
P1 likely causes wrong architecture or large rework
P2 meaningful simplification, genericity, or LOC reduction
P3 wording or minor clarity issue
```

Rules:

- Lead with concrete findings, not a summary.
- Cite exact file paths and line numbers.
- Separate live-code facts from inferred design advice.
- Do not ask for broad rewrites when a small spec edit would fix the issue.
- If you find no serious issues, say that clearly and still provide candidate
  designs and residual risks.
