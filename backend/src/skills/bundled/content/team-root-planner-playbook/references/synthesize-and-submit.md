# Root Planner Synthesize and Submit Reference

Load this reference in Stage 3 before drafting any `submit_plan(...)` payload. It holds the synthesis rules, terminal tool contract, task-spec examples, and dependency DAG examples for root planner submissions.

This reference is a one-way Stage 3 transition. If a newly revealed production owner slice still needs scouting, the reference was loaded too early; after loading it, preserve that slice as uncertainty and route it to child `team_planner` or scoped diagnostic work instead of launching scouts or CI/workspace/symbol exploration. After the payload is ready, the final assistant action is the `submit_plan(...)` tool call.

## Synthesis Rules

Start from the Stage 1 owner ledger plus Stage 2 scout notes and uncertainty. Produce a same-payload DAG with task ids, lane names, `deps`, `scope_paths`, and validator coverage. Every named failing cluster must have a repair/decomposition owner or be explicitly handed to a child `team_planner`; a terminal validator is never an owner for otherwise unassigned failures.

Scout evidence gate: trigger -> the coverage ledger has a benchmark/fail-to-pass/migration/compatibility family that Stage 1 marked `scout_required`; required action -> use its scout note, or preserve explicit uncertainty and route the family to `team_planner`; failure signal -> a current-layer `developer` is called atomic using only first-pass owner labels.

### Clustering Guidance

Treat benchmark, fail-to-pass, migration, compatibility, and broad upgrade requests as clustering jobs when they contain many failing tests, several production families, or a multi-engine, multi-dtype, multi-format, or multi-API matrix.

- Clear owner names do not override a clustering signal; a named file can still belong to a broader family that should be decomposed below a child planner.
- When clustering triggers, include at least one child `team_planner` in the root payload.
- A clustering root payload with four or more independent `developer` lanes and no child `team_planner` is invalid, even when scouts named plausible owners or files.
- Use child `team_planner` for broad decomposition. Keep root `developer` lanes for small leaf fixes with a single narrow production surface and coherent verification command.

### Atomic vs. Expandable Decision

Clustering Guidance above is a payload-level signal. The test below runs per owner slice: atomic slices become root `developer` lanes, expandable slices become child `team_planner` lanes. A slice is atomic only when **every** atomic test holds; **any** expandable signal routes it to `team_planner`.

Name-field lock: after classifying a slice, write the `name` field from that classification before writing the rest of the task. If the slice is expandable, clustered, broad, multi-family, matrix-shaped, unresolved, mixed, or not atomic, the only valid `name` is `"team_planner"`. Do not create a `developer` task whose own `Goal`, `Task Details`, notes, or checklist rationale calls the same slice expandable.

Atomic tests — all must hold:

1. **Single production owner.** The fix lives in one file, symbol, or tight production surface that live scout evidence pinned. Not a shortlist, not a guess, not "start here and see what else breaks".
2. **Single coherent verification.** One focused pytest file or a tight cluster of ids under one suite exercises the change. The acceptance command is one line, not a matrix.
3. **No cross-module spread.** Edits stay inside one module boundary. Multiple files inside the same module are fine when they share one coherent change.
4. **Bounded blast radius.** The slice touches one invariant, one API boundary, or one behavior — a reviewer can hold the whole repair in their head.
5. **Ownership settled.** Scout notes do not leave ownership as "could also be X", "between A and B", or "depends on Y". The owner is identified, not shortlisted.
6. **One failure mechanism.** Every named failing test in the slice traces to the same root cause. Multiple independent root causes on one file is still expandable.

Atomic grouping gate: trigger -> two or more atomic slices have different owner files, symbols, or verification commands; required action -> submit separate root `developer` lanes or route the group to `team_planner`; failure signal -> one `developer` task spec lists multiple independent fixes across unrelated owners because each item looked atomic alone. Example: ✓ one focused `developer` for a single engine-selection fix and one `team_planner` for a broad read/write benchmark family; ✗ one `developer` that bundles the engine fix with dozens of read/write/glob/path failures because both live under the same package.

Mechanism contradiction gate: trigger -> a drafted `developer` spec names two or more independent failure mechanisms, says "fix each mechanism", or assigns separate production boundaries inside one lane; required action -> split into one root `developer` per mechanism or route the whole slice to `team_planner`; failure signal -> one `developer` task details names multiple mechanisms and justifies the bundle with shared scope, nearby files, or one verification suite. Example: ✓ a child `team_planner` for three method families in one module; ✗ one `developer` lane that says it will fix each mechanism independently across those methods.

Same-file catch-all gate: trigger -> a drafted `developer` spec says "all failing tests" for one file, lists several behaviors, entry points, modes, or scenarios, or uses "root cause(s)" because the mechanism is not yet known; required action -> route the slice to `team_planner` or split into root `developer` lanes by known behavior/mechanism; failure signal -> one `developer` goal says to repair all failures in a file while `Task Details` enumerates many operations under that file.

Expandable signals — any one routes to `team_planner`:

- **Multi-family failure span.** Failing clusters cross production families, layers, or modules.
- **Matrix shape.** The request or scout notes name multi-engine, multi-dtype, multi-format, multi-API, multi-backend, or multi-version coverage.
- **Four-plus leaf fixes.** The slice requires four or more independent edits, even when each edit is narrow.
- **Unresolved ownership.** Scout left ownership as a shortlist or gated it on further investigation.
- **Broad surface request.** Benchmark, migration, compatibility, or framework upgrade work; the request itself is clustering, regardless of how many owners scouts named.
- **Catch-all drafting.** The draft `2. Task Details:` would have to say "repair everything in module X" or list more than one independent production surface.
- **Cross-cutting invariant.** The fix must be enforced at multiple independent call sites that each need their own verification.
- **Mixed intent.** A single slice bundles a bugfix with a refactor, a migration with a feature, or policy with plumbing.

Multi-API family gate: trigger -> the request or scout notes for one family list multiple public APIs, backend implementations, or helper surfaces such as read, write, metadata, wrappers, adapters, or engines; required action -> classify the family as expandable and route it to `team_planner`; failure signal -> a `developer` spec calls the family coherent while its `Task Details` lists those APIs or surfaces.

Shared-cause proof gate: trigger -> you want to call a multi-API or all-failures slice atomic because one shared dependency, library, version, or file likely changed; required action -> name the single internal helper, invariant, or adapter boundary proven by scout evidence, else route to `team_planner`; failure signal -> a `developer` spec cites one broad cause while its `Task Details` lists read/write, load/save, import/export, or multiple public entrypoints.

Self-consistency gate: trigger -> your synthesis notes call any slice expandable or say no slice passed the atomic tests; required action -> every named expandable slice is submitted with `name: "team_planner"`; failure signal -> notes say "expandable", "team_planner required", or "no slice passes atomic tests" but the final payload gives that slice `name: "developer"`. If that mismatch appears in your draft, change the `name` to `"team_planner"`; do not rewrite the rationale to make the developer assignment look acceptable.

Borderline cases:

- One named file with three independent failures that touch different APIs inside it → **expandable**; the file is a scope coincidence, not a coherent fix.
- Three files in one module that all consume one shared contract change → **atomic**; sibling files are incidental scope, not independent owners.
- Benchmark-suite request where live scout evidence *disproves* the clustering signal at the slice level (stronger than a merely named owner) and reduces it to one failing helper in one production file → **atomic**; the terminal validator still runs the full suite.
- "Touches every provider" cleanup where each change is mechanical but each provider is an independent owner with independent verification → **expandable**.
- Scout named an exact symbol but the failing tests span two unrelated behaviors of that symbol → **expandable**; one owner is not the same as one coherent change.
- Two named files where the second is a thin adapter that only re-exports or forwards to the first → **atomic**; the adapter is not an independent owner.

When unsure, prefer `team_planner`. A mis-routed `developer` that grows into a catch-all multi-owner fix under-covers the failing surface and forces a replan. A `team_planner` that routes to a single child `developer` adds one layer of indirection but keeps decomposition correct.

### Lane Selection

Lane selection is advisory, but apply it in this order. A single payload may mix lane names.

| Slice shape | Lane | When it fits |
| --- | --- | --- |
| Broad decomposition | `team_planner` | Broad, shared, clustered, multi-family, unresolved, benchmark, migration, compatibility, or large benchmark/test-matrix work. |
| Narrow implementation | `developer` | One coherent change with a known exact owner, bounded to a concrete file, symbol, or tight production surface. |
| Same-layer verification | `validator` | A distinct verification lane after implementation/planner lanes finish. It must depend on every same-payload non-validator id it verifies, including child `team_planner` ids. |

Never include `scout` or `team_replanner` in `new_tasks`; scouts run via `run_subagent(...)`, and replanners are spawned reactively by the runtime.

### Coverage and Evidence Rules

1. Build a coverage ledger for benchmark/fail-to-pass requests. Track every named failing cluster, variant, or command from the user request and scout notes.
2. Sibling target exclusivity gate: trigger -> the coverage ledger is split across multiple root families and one draft producer spec repeats a sibling family's named pytest ids, whole test file, or focused verification command; required action -> keep each named failing id, file-level command, or focused suite command only in the owning family's spec and in the terminal validator roll-up, and if a sibling file matters for context mention it as evidence only without duplicating its targets; failure signal -> one producer lane also carries another lane's ids or `pytest tests/test_beta.py -q` because the topics sound related. Example: ✓ owner A keeps `tests/test_alpha.py::test_case_a` while owner B keeps `tests/test_beta.py::test_case_b`; ✗ owner A also carries `pytest tests/test_beta.py -q` while owner B already owns that target.
3. Put benchmark tests and verification commands in `spec`, not `scope_paths`, unless tests are explicitly the owned surface.
4. Drop exact files disproved by live evidence. Use the nearest stable production boundary instead.
5. Cold/disproved path gate: trigger -> live scout evidence says the drafted exact file is missing, CI-cold, or replaced by a package/directory boundary; required action -> remove the disproved exact file from `scope_paths` and `Task Details`, then use the nearest stable production boundary or carry uncertainty to `team_planner`; failure signal -> the final payload still names the disproved exact path after the scout reported zero coverage or a package boundary.
6. Treat any scout conclusion that names benchmark tests, skips, xfails, rewrites, pytest configuration, or benchmark harness edits as evidence only. Translate it into a production, dependency, environment, or uncertainty hypothesis.
7. Never write a developer goal or task details that instruct the child to edit, skip, xfail, rewrite, or reconfigure benchmark tests unless the original user request explicitly asks to repair tests rather than production behavior.
8. Do not put a named failing cluster only in a validator spec. Give it a production repair/decomposition owner or hand it to a child `team_planner`.
9. Every root payload ends with exactly one terminal `validator`. It is a structural requirement of the payload, not an optional addition. See the `Terminal Validator` section below.
10. Make the terminal validator depend on every same-payload non-validator id, including child `team_planner` ids.

## Submission Rules

Build one `new_tasks` JSON list from the decided DAG.

1. Use repo-relative production `scope_paths` for every task, including validators.
2. Put owner evidence and sequencing in `2. Task Details:`. `Task Details` must name owner evidence, exact production scope, constraints, and dependency context.
3. Put concrete test-suite expectations in `3. Acceptance Criteria:`. `Acceptance Criteria` must be test-suite focused with concrete commands or pytest ids.
4. Use `deps` only for real output ordering or same-payload planner/validator ordering.
5. Ensure every `deps` entry resolves to another id in this same `new_tasks` list.
6. For a terminal validator, list every same-payload non-validator id it validates.
7. For fail-to-pass work, do not close a named target with skip, xfail, clear `ImportError`, missing optional dependency, or "not supported" prose.
8. Submit with top-level `new_tasks` only. Do not include summary, output, parent ids, or trailing prose.

OCC resolves concurrent edits to the same file. Overlapping sibling `scope_paths` are allowed; do not invent deps or merge lanes just to keep scopes disjoint.

## Terminal Tool Contract

The root has no graph to inherit. Every `deps` entry must resolve to another id in this `new_tasks` payload.

Call:

```ts
submit_plan({ new_tasks: NewTaskSpec[] })
```

Task object:

```ts
type NewTaskSpec = {
  id: string;
  name: "developer" | "validator" | "team_planner";
  spec: string;
  deps: string[];
  scope_paths: string[];
};
```

Field contract:

| Field | Contract |
| --- | --- |
| `id` | Unique lower-kebab id in this payload. Other tasks reference this exact string in `deps`. |
| `name` | Exactly `developer`, `team_planner`, or `validator`. `developer` means the slice passed every atomic test and no expandable signal fired; expandable slices must use `team_planner`. |
| `spec` | One string with `1. Goal:`, `2. Task Details:`, and `3. Acceptance Criteria:` in order. Each label starts its own line and has body text after the colon on that same line. |
| `deps` | List of ids from this same payload. Independent work uses `[]`. Validators must depend on at least one upstream same-payload task. |
| `scope_paths` | Non-empty list of repo-relative production paths owned or verified by the task. Use directories for broad planner or validator scopes. |

Root planner policy is stricter than the runtime minimum: the runtime accepts any schema-valid planner payload, but this playbook requires exactly one terminal root `validator`. For root planner submissions, use only the built-in lane names `developer`, `team_planner`, and `validator`, even though the tool can resolve some role hints from roster metadata.

## Payload Examples

### Complete Valid Root Payload

This is the final assistant action shape. It includes only `new_tasks`, all task objects use only the six allowed fields, every `spec` label starts its own line with body text after the colon, test commands stay in `spec`, and the terminal validator depends on every same-payload non-validator id.

```ts
submit_plan({
  new_tasks: [
    {
      id: "dev-task-center",
      name: "developer",
      spec: "1. Goal: Repair the focused TaskCenter invariant so dependent task state remains coherent after graph mutation.\n2. Task Details: Own backend/src/team/task_center.py. The failure evidence points at TaskCenter graph mutation behavior, not executor or DispatchQueue ownership. Preserve existing terminal submission tool names and do not change replan task spawning policy.\n3. Acceptance Criteria: Run uv run pytest backend/tests/team/test_task_center.py -q and uv run pytest backend/tests/team/test_replan_workflow.py -q; the suites prove the invariant holds without skip, xfail, or test reconfiguration.",
      deps: [],
      scope_paths: ["backend/src/team/task_center.py"]
    },
    {
      id: "plan-team-runtime-cluster",
      name: "team_planner",
      spec: "1. Goal: Decompose related team runtime failures across graph persistence, dispatch readiness, and executor handoff so each owner family is routed below this root layer.\n2. Task Details: Own decomposition under backend/src/team. The request spans multiple production families, so the child planner must emit exact owner lanes instead of one catch-all developer task. Treat failing pytest ids as evidence in child specs, not as test-edit instructions.\n3. Acceptance Criteria: The child plan emits owner-specific developer lanes, one child-layer validator, and coverage for uv run pytest backend/tests/team -q with no closure by skip, xfail, ImportError, or missing optional dependency.",
      deps: [],
      scope_paths: ["backend/src/team"]
    },
    {
      id: "val-root-team-runtime",
      name: "validator",
      spec: "1. Goal: Verify the focused TaskCenter repair and the decomposed team runtime follow-up after all root producer lanes complete.\n2. Task Details: Verify backend/src/team after dev-task-center and plan-team-runtime-cluster finish. This validator does not own implementation work and must report any uncovered failing cluster back to the responsible producer lane.\n3. Acceptance Criteria: Run uv run pytest backend/tests/team/test_task_center.py -q, uv run pytest backend/tests/team/test_replan_workflow.py -q, and uv run pytest backend/tests/team -q; report exact failing pytest ids and owner gaps if any remain.",
      deps: ["dev-task-center", "plan-team-runtime-cluster"],
      scope_paths: ["backend/src/team"]
    }
  ]
})
```

### Invalid Payload Shapes

Do not submit any of these shapes:

```ts
submit_plan({
  new_tasks: [],
  summary: "I made a plan."
})
```

Invalid because `summary` is not a tool input field and an empty plan is rejected by plan validation.

```ts
submit_plan({
  new_tasks: [
    {
      id: "dev-owner",
      name: "developer",
      spec: "1. Goal: Repair the owner.\n2. Task Details: Own backend/src/team/task_center.py.\n3. Acceptance Criteria: Run uv run pytest backend/tests/team/test_task_center.py -q.",
      deps: [],
      scope_paths: ["backend/src/team/task_center.py"],
      parent_id: "root"
    }
  ]
})
```

Invalid because task objects may not include `parent_id`; the runtime owns parent stamping.

```ts
submit_plan({
  new_tasks: [
    {
      id: "dev-test-file",
      name: "developer",
      spec: "1. Goal: Repair production behavior covered by the failing test.\n2. Task Details: Own backend/src/team/task_center.py; keep the test path as evidence only.\n3. Acceptance Criteria: Run uv run pytest backend/tests/team/test_task_center.py -q.",
      deps: [],
      scope_paths: ["backend/tests/team/test_task_center.py"]
    }
  ]
})
```

Invalid because test files and test directories are forbidden in `scope_paths`; put test paths and commands in `spec`.

```ts
submit_plan({
  new_tasks: [
    {
      id: "dev-bad-spec",
      name: "developer",
      spec: "1. Goal:\nRepair the owner.\n2. Task Details: Own backend/src/team/task_center.py.\n3. Acceptance Criteria: Run uv run pytest backend/tests/team/test_task_center.py -q.",
      deps: [],
      scope_paths: ["backend/src/team/task_center.py"]
    }
  ]
})
```

Invalid because each numbered label must have body text after the colon on the same line.

```ts
submit_plan({
  new_tasks: [
    {
      id: "dev-owner",
      name: "developer",
      spec: "1. Goal: Repair the owner.\n2. Task Details: Own backend/src/team/task_center.py.\n3. Acceptance Criteria: Run uv run pytest backend/tests/team/test_task_center.py -q.",
      deps: [],
      scope_paths: ["backend/src/team/task_center.py"]
    },
    {
      id: "val-root",
      name: "validator",
      spec: "1. Goal: Verify the repair after the producer lane completes.\n2. Task Details: Verify backend/src/team after dev-owner finishes.\n3. Acceptance Criteria: Run uv run pytest backend/tests/team/test_task_center.py -q.",
      deps: [],
      scope_paths: ["backend/src/team"]
    }
  ]
})
```

Invalid because the terminal validator must depend on every same-payload non-validator id it verifies; `deps: []` is rejected with `validator tasks must depend on at least one upstream sibling`.

## TaskSpec Examples

These examples show the detailed `spec` body each lane should carry in a real payload. Use them as a shape guide for `1. Goal:`, `2. Task Details:`, and `3. Acceptance Criteria:` wording. Only the `spec` string is shown; real payloads must also carry `id`, `name`, `deps`, and `scope_paths` per the Terminal Tool Contract. The dependency DAG examples further down abstract full payloads into diagrams so graph shape stays readable.

### Developer TaskSpec

Use `developer` for a narrow exact-owner implementation task.

```text
1. Goal: Evict stale entries from the routing service symbol cache when workspace files change so owner queries after an edit return fresh results instead of the pre-mutation owner.
2. Task Details: Own backend/src/code_intelligence/routing/service.py. The user reproduction pinned stale owner suggestions to cache entries that outlive workspace writes, and the scout note confirms the symbol cache inside the routing service is the single production boundary involved — this is one coherent change, not a multi-file refactor. Preserve the public lookup API used by callers in backend/src/agents, keep the hot path non-blocking, and do not reintroduce a global lock around symbol reads. Related invariant: routing responses must remain deterministic across repeated identical queries when no mutation has occurred between them.
3. Acceptance Criteria: Run uv run pytest backend/tests/code_intelligence/test_routing_service.py -q; the suite proves cache entries are evicted on workspace mutation events, a repeated query after a mutation returns the fresh owner rather than the cached one, and concurrent readers during invalidation never observe a partially written cache value. Do not close the named failing targets through xfail, skip, or by removing the test.
```

### Team Planner TaskSpec

Use `team_planner` when the root identifies an owner family but that family must be decomposed below this layer.

```text
1. Goal: Decompose daytona_shell toolkit compatibility failures into per-owner lanes across the overlay commit path, the sandbox command execution path, and the remote run cleanup path so each owner family is repaired on its own production boundary.
2. Task Details: Own decomposition under backend/src/tools/daytona_toolkit. Stage 1 raised a clustering flag because the failing surface spans overlay commits (_commit_changes), svc.cmd latency regressions in overlay_run, and remote run cleanup behavior, which map to at least three distinct production owners rather than one coherent fix. Flattening this into sibling developer lanes at the root would be catch-all hiding. Benchmark ids and overlay crash traces from the user request are routing evidence for the child planner, not test-edit instructions. The child planner must preserve the existing invariant that _cleanup_remote_run_dir stays on the foreground path (moving it to a background task has already been rejected for throughput reasons).
3. Acceptance Criteria: The child plan emits exact owner lanes for each decomposed slice, a single child-layer validator, and coverage for uv run pytest backend/tests/tools/daytona_toolkit -q plus any focused overlay or cleanup tests the child evidence identifies. No child acceptance criterion may close a named failing target through skip, xfail, ImportError handling, or by declaring a missing optional dependency as passing closure.
```

### Validator TaskSpec

Use `validator` for a distinct same-layer verification lane. A terminal validator depends on every same-payload non-validator id it verifies. Two specs are shown below because the validator's `deps` must resolve to a same-payload task — the upstream developer spec is included to ground that referent.

Upstream developer spec in the same payload:

```text
1. Goal: Raise GraphInvariantViolation when a replan attempts to rewire a non-pending dependent, rather than silently demoting or tolerating the state as a race.
2. Task Details: Own backend/src/team/task_center.py. Prior work established that dependents of a replanning or failed task must already be pending, and the scout note confirms the rewire path is the single production boundary involved. Do not relax the invariant into a warning or a retry. Do not alter DispatchQueue or executor ownership boundaries, and do not change terminal submission tool names.
3. Acceptance Criteria: Run uv run pytest backend/tests/team/test_replan_workflow.py -q; the suite proves pending dependents are rewired through the spawned replanner, any non-pending dependent raises GraphInvariantViolation with the offending task id in the message, and the original failed task does not cascade-cancel rewired dependents.
```

Validator spec:

```text
1. Goal: Verify the pending-dependents invariant is enforced end-to-end — the rewire path, the error path, and the no-cascade-cancel guarantee — and that no new silent-demotion regression has landed in task_center.
2. Task Details: Verify backend/src/team after the upstream developer task completes. This validator joins the only same-payload non-validator id and must not claim ownership of invariant logic itself. Any gap in coverage is reported back to the developer lane with the exact failing pytest id and the missing assertion, not patched inline here. Do not edit production files in this task's scope.
3. Acceptance Criteria: Run uv run pytest backend/tests/team/test_replan_workflow.py -q and uv run pytest backend/tests/team/test_task_center.py -q; report any invariant violation that slipped past, any rewire that still silently demotes a non-pending dependent, and any cascade-cancel regression with the exact failing pytest id and the rejected behavior.
```

## Terminal Validator

Every root payload ends with exactly one terminal `validator`. It is a single `validator` lane whose `deps` list every same-payload non-validator id — each `developer`, each `team_planner`, and every producer in a chain, not just the last link. The terminal validator is a structural requirement of the payload, never an optional addition.

Why it is required:

- Coverage — without it, an unverified `team_planner` subtree or an off-critical-path `developer` lane can ship with no same-layer check.
- Chain integrity — listing only the final link of a chain would let a broken upstream pass coverage if a downstream consumer happened to mask the defect; the terminal validator depends on every producer so the full chain is verified.
- One joining lane — multiple terminal validators split coverage and create ambiguity about which one is authoritative; the root always carries exactly one.

The Dependency DAG Examples below each show a terminal validator joining the non-validator ids above it. The Sequential Chain example omits the validator solely to keep the sequential-dep shape readable; a real Sequential Chain payload still carries a terminal validator whose `deps` include both non-validator ids.

## Dependency DAG Examples

These examples show common dependency shapes as diagrams with rationale. They abstract away lane `name`, `scope_paths`, and `spec` so edge structure stays readable. Real payloads must still carry every contract field, with detailed `spec` strings in the style shown in the TaskSpec Examples above.

Diagram convention: each arrow `A ──▶ B` means `B`'s `deps` list includes `A`. Ids prefixed `dev-` are `developer` lanes, `plan-` are `team_planner` lanes, `val-` are `validator` lanes.

### Parallel Fan-Out With Terminal Validator

```
dev-agent-registry      ─┐
dev-skill-loader        ─┼──▶  val-root-fanout
plan-provider-runtime   ─┘
```

Rationale: the three non-validator lanes share no output-consumption relationship, so they all use `deps: []` and run in parallel. The terminal validator joins every same-payload non-validator id it verifies, including the child `team_planner` id — a validator that skipped the planner id would under-cover the payload.

### Sequential Chain

```
dev-submission-schema  ──▶  dev-submission-renderer
```

Rationale: the renderer imports or consumes schema output, so it must wait on `dev-submission-schema`. No edge exists just to serialize — the chain reflects real output consumption. This diagram omits the terminal validator to keep the sequential-dep shape readable; a real payload still carries a terminal validator whose `deps` include both ids, since listing only the final link would let a broken schema pass coverage if the renderer happened to mask the defect (see the `Terminal Validator` section above).

### Diamond Fan-Out Then Fan-In

```
                dev-task-center-contract
                 │         │         │
       ┌─────────┘         │         └───────────┐
       ▼                   ▼                     │
dev-dispatch-         dev-replanner-             │
queue-consumer        consumer                   │
       │                   │                     │
       └─────────┬─────────┴─────────────────────┘
                 ▼
        val-team-contract-diamond
```

Edges:

- `dev-task-center-contract` → `dev-dispatch-queue-consumer`
- `dev-task-center-contract` → `dev-replanner-consumer`
- `dev-task-center-contract` → `val-team-contract-diamond`
- `dev-dispatch-queue-consumer` → `val-team-contract-diamond`
- `dev-replanner-consumer` → `val-team-contract-diamond`

Rationale: both downstream consumers import the contract shape produced by `dev-task-center-contract`, so each one depends on it individually. They stay parallel to each other because neither consumes the other's output — no edge exists just to serialize them. The validator joins every same-payload non-validator id, including the shared upstream.

### Planner Output Gating Downstream Integration

```
plan-provider-contract  ──▶  dev-agent-provider-bridge
         │                              │
         └──────────────┬───────────────┘
                        ▼
                val-provider-bridge
```

Rationale: the agent bridge consumes the provider contract established by the child planner's subtree, so it depends on the `team_planner` id — a planner id is a valid `deps` target when a same-payload task genuinely waits on that subtree's output. The validator still lists both same-payload non-validator ids, the planner and the bridge, not just the final integration lane.

### Mixed Sequential And Parallel Work

```
dev-tool-schema  ──▶  dev-tool-runtime  ───┐
       │                                   │
       │                                   ▼
       └─────────────────────────▶ val-mixed-root-dag
                                           ▲
dev-shell-fallback  ─────────────────────┤
                                           │
plan-routing-cluster  ─────────────────────┘
```

Edges:

- `dev-tool-schema` → `dev-tool-runtime`
- `dev-tool-schema` → `val-mixed-root-dag`
- `dev-tool-runtime` → `val-mixed-root-dag`
- `dev-shell-fallback` → `val-mixed-root-dag`
- `plan-routing-cluster` → `val-mixed-root-dag`

Rationale: only `dev-tool-runtime` consumes schema output, so only it depends on `dev-tool-schema`. `dev-shell-fallback` and `plan-routing-cluster` touch unrelated surfaces and stay on `deps: []` — adding edges between them just to "order" the payload would serialize independent work. The validator still lists every same-payload non-validator id it verifies.

### Overlapping Scopes Without Scope-Hygiene Deps

```
dev-tool-policy          ─┐
                          ├──▶  val-overlap-tools
dev-submission-contract  ─┘
```

Rationale: `backend/src/tools/submission` is nested inside `backend/src/tools`, but overlapping `scope_paths` alone do not imply ordering. OCC resolves concurrent edits, so both developers stay parallel unless one actually consumes the other's output. Inserting a dep here just to "keep scopes disjoint" would be a scope-hygiene dep and is forbidden.

## Final Checklist

| # | Check |
|---|---|
| 1 | Top-level input is only `new_tasks`. |
| 2 | Every task has only `id`, `name`, `spec`, `deps`, and `scope_paths`. |
| 3 | Every `id` is unique; every `deps` entry resolves to another id in this same payload. |
| 4 | No `deps` edge exists solely to serialize independent work or to keep scopes disjoint; chains appear only where real output consumption or terminal validator coverage requires them. |
| 5 | Every `name` is exactly `developer`, `team_planner`, or `validator`. |
| 6 | Every `scope_paths` is non-empty and uses repo-relative production paths. |
| 7 | Every `spec` contains `1. Goal:`, `2. Task Details:`, and `3. Acceptance Criteria:` in order, each label starting its own line. |
| 8 | Every `Acceptance Criteria` is test-suite focused with concrete commands or pytest ids. |
| 9 | No fail-to-pass acceptance criterion treats skipped tests, expected failures, clear `ImportError`, or missing optional dependencies as passing closure. |
| 10 | No named fail-to-pass cluster is covered only by a validator without a repair/decomposition owner. |
| 11 | Any clustering job includes at least one child `team_planner`. |
| 12 | Every non-validator task passed the atomic tests or was routed to `team_planner` under a named expandable signal. A task whose rationale says expandable, clustered, broad, multi-family, matrix-shaped, unresolved, mixed, or not atomic cannot have `name: "developer"`. |
| 13 | The payload ends with exactly one terminal `validator` whose `deps` list every same-payload non-validator id. |
| 14 | The final assistant action is the `submit_plan(...)` tool call, not prose. |
