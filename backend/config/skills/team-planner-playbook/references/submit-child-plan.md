# Team Planner Submit Child Plan Reference

Load this reference in Stage 3 before drafting any `submit_plan(...)` payload. It holds the synthesis rules, terminal tool contract, task-spec examples, and dependency DAG examples for child team-planner submissions.

This reference is a one-way Stage 3 transition. If a newly revealed production owner slice still needs scouting, the reference was loaded too early; after loading it, preserve that slice as uncertainty and route it to another child `team_planner` when allowed, or to a max-depth diagnostic/repair lane, instead of launching scouts or CI/workspace/symbol exploration. After the payload is ready, the final assistant action is the `submit_plan(...)` tool call.

## Synthesis Rules

Start from the Stage 1 owner ledger (inherited slices, unresolved slices, dependency outputs, evidence) plus Stage 2 scout notes and uncertainty. Produce a same-payload child DAG with task ids, lane names, `deps`, `scope_paths`, and validator coverage. Every named failing cluster must have a repair/decomposition owner or be explicitly handed to another child `team_planner`; a terminal validator is never an owner for otherwise unassigned failures.

Scout evidence gate: trigger -> the coverage ledger has an inherited benchmark/fail-to-pass/migration/compatibility family that Stage 1 marked `scout_required`; required action -> use its scout note, or preserve explicit uncertainty and route the family to another child `team_planner` when allowed, else a max-depth per-mechanism lane; failure signal -> a current-layer `developer` is called atomic using only inherited or first-pass owner labels.

### Clustering Guidance

Treat inherited benchmark, fail-to-pass, migration, compatibility, and broad upgrade slices as clustering jobs when they contain many failing tests, several production families, or a multi-engine, multi-dtype, multi-format, or multi-API matrix.

- Clear owner names do not override a clustering signal; a named file can still belong to a broader family that should be decomposed below another child planner.
- When clustering triggers and `grandchild_depth <= max_depth`, include at least one child `team_planner` in this payload. That child planner owns the next cluster-level split and may create developer leaves below it.
- A clustering child payload with four or more independent `developer` lanes and no child `team_planner` is invalid when `grandchild_depth <= max_depth`, even when scouts named plausible owners or files.
- When `grandchild_depth > max_depth`, emit direct `developer` and `validator` tasks with broader scopes instead of a child `team_planner`.
- Use another child `team_planner` for broad decomposition. Keep current-layer `developer` lanes for small leaf fixes with a single narrow production surface, one coherent failure mechanism, and a coherent verification command.
- Do not flatten independent failure mechanisms into one developer lane because they share nearby files or verification commands; overlapping `scope_paths` are allowed, split by mechanism when the work is otherwise independent.

### Atomic vs. Expandable Decision

Clustering Guidance above is a payload-level signal. The test below runs per owner slice: atomic slices become current-layer `developer` lanes; expandable slices route to another child `team_planner` when `grandchild_depth <= max_depth`, else to broader direct `developer` + `validator` tasks split by failure mechanism. A slice is atomic only when **every** atomic test holds; **any** expandable signal routes it to the expandable path.

Name-field lock: after classifying a slice, write the `name` field from that classification before writing the rest of the task. When `grandchild_depth <= max_depth`, a slice that is expandable, clustered, broad, multi-family, matrix-shaped, unresolved, mixed, or not atomic must use `name: "team_planner"`, never `name: "developer"`. When `grandchild_depth > max_depth`, do not call the fallback developer lanes atomic; label them in `Task Details` as max-depth per-mechanism fallback lanes.

Atomic tests — all must hold:

1. **Single production owner.** The fix lives in one file, symbol, or tight production surface that inherited evidence and live scout evidence pinned. Not a shortlist, not a guess, not "start here and see what else breaks".
2. **Single coherent verification.** One focused pytest file or a tight cluster of ids under one suite exercises the change. The acceptance command is one line, not a matrix.
3. **No cross-module spread.** Edits stay inside one module boundary. Multiple files inside the same module are fine when they share one coherent change.
4. **Bounded blast radius.** The slice touches one invariant, one API boundary, or one behavior — a reviewer can hold the whole repair in their head.
5. **Ownership settled.** Inherited evidence and scout notes do not leave ownership as "could also be X", "between A and B", or "depends on Y". The owner is identified, not shortlisted.
6. **One failure mechanism.** Every named failing test in the slice traces to the same root cause. Multiple independent root causes under one file is still expandable.

Atomic grouping gate: trigger -> two or more atomic slices have different owner files, symbols, or verification commands; required action -> submit separate current-layer `developer` lanes, route the group to `team_planner` when `grandchild_depth <= max_depth`, or split by mechanism at max depth; failure signal -> one `developer` task spec lists multiple independent fixes across unrelated owners because each item looked atomic alone.

Mechanism contradiction gate: trigger -> a drafted `developer` spec names two or more independent failure mechanisms, says "fix each mechanism", or assigns separate production boundaries inside one lane; required action -> split into one current-layer `developer` per mechanism, route the whole slice to `team_planner` when `grandchild_depth <= max_depth`, or split by mechanism at max depth; failure signal -> one `developer` task details names multiple mechanisms and justifies the bundle with shared scope, nearby files, or one verification suite.

Same-file catch-all gate: trigger -> a drafted `developer` spec says "all failing tests" for one file, lists several behaviors, entry points, modes, or scenarios, or uses "root cause(s)" because the mechanism is not yet known; required action -> route the slice to `team_planner` when `grandchild_depth <= max_depth`, or split by known behavior/mechanism at max depth; failure signal -> one `developer` goal says to repair all failures in a file while `Task Details` enumerates many operations under that file.

Expandable signals — any one routes to the expandable path:

- **Multi-family failure span.** Failing clusters cross production families, layers, or modules.
- **Matrix shape.** Inherited context or scout notes name multi-engine, multi-dtype, multi-format, multi-API, multi-backend, or multi-version coverage.
- **Four-plus leaf fixes.** The slice requires four or more independent edits, even when each edit is narrow.
- **Unresolved ownership.** Inherited evidence or scout left ownership as a shortlist or gated it on further investigation.
- **Broad inherited surface.** Inherited benchmark, migration, compatibility, or framework upgrade slices; the parent framing is clustering, regardless of how many owners scouts named.
- **Catch-all drafting.** The draft `spec.detail` would have to say "repair everything in module X" or list more than one independent production surface.
- **Cross-cutting invariant.** The fix must be enforced at multiple independent call sites that each need their own verification.
- **Mixed intent.** A single slice bundles a bugfix with a refactor, a migration with a feature, or policy with plumbing.
- **Multiple failure mechanisms.** Inherited evidence or scout notes name two or more independent root causes under one scope; split by mechanism even when files overlap. At `grandchild_depth > max_depth`, emit one `developer` per mechanism with widened `scope_paths` and a spec that names the mechanism — a four-or-more-mechanism fusion into one catch-all `developer` is a routing bug, not an acceptable collapse.

Multi-API family gate: trigger -> inherited evidence or scout notes for one family list multiple public APIs, backend implementations, or helper surfaces such as read, write, metadata, wrappers, adapters, or engines; required action -> classify the family as expandable and use `team_planner` while `grandchild_depth <= max_depth`, else split by API or mechanism at max depth; failure signal -> a `developer` spec calls the family coherent while its `Task Details` lists those APIs or surfaces.

Shared-cause proof gate: trigger -> you want to call a multi-API or all-failures slice atomic because one shared dependency, library, version, or file likely changed; required action -> name the single internal helper, invariant, or adapter boundary proven by scout evidence, else use `team_planner` while `grandchild_depth <= max_depth`, or split by API/mechanism at max depth; failure signal -> a `developer` spec cites one broad cause while its `Task Details` lists read/write, load/save, import/export, or multiple public entrypoints.

Self-consistency gate: trigger -> your synthesis notes call any slice expandable or say no slice passed the atomic tests; required action -> when `grandchild_depth <= max_depth`, every named expandable slice is submitted with `name: "team_planner"`; failure signal -> notes say "expandable", "team_planner required", or "no slice passes atomic tests" but the final payload gives that slice `name: "developer"`. If that mismatch appears in your draft, change the `name` to `"team_planner"`; do not rewrite the rationale to make the developer assignment look acceptable.

Borderline cases:

- One named file with three independent failures that touch different APIs inside it → **expandable**; the file is a scope coincidence, not a coherent fix. Split by mechanism.
- Three files in one module that all consume one shared contract change → **atomic**; sibling files are incidental scope, not independent owners.
- Inherited benchmark slice where live scout evidence *disproves* the clustering signal at the slice level (stronger than a merely named owner) and reduces it to one failing helper in one production file → **atomic**; a terminal validator, if included, still covers the full suite.
- "Touches every provider" cleanup where each change is mechanical but each provider is an independent owner with independent verification → **expandable**.
- Scout named an exact symbol but the failing tests span two unrelated behaviors of that symbol → **expandable**; one owner is not the same as one coherent change.
- Two named files where the second is a thin adapter that only re-exports or forwards to the first → **atomic**; the adapter is not an independent owner.

When unsure, prefer the expandable path. A mis-routed `developer` that grows into a catch-all multi-owner fix under-covers the inherited surface and forces a replan. An extra planner layer (when `grandchild_depth <= max_depth`) or a wider per-mechanism split (when `grandchild_depth > max_depth`) adds structure but keeps decomposition correct.

### Lane Selection

Lane selection is advisory, but apply it in this order. A single payload may mix lane names.

| Slice shape | Lane | When it fits |
| --- | --- | --- |
| Broad decomposition | `team_planner` | Broad, shared, clustered, multi-family, unresolved, benchmark, migration, compatibility, or large benchmark/test-matrix work that must be split below this layer, only when `grandchild_depth <= max_depth`. When `grandchild_depth > max_depth`, use broader direct `developer` and `validator` tasks instead. |
| Narrow implementation | `developer` | One coherent change with a known exact owner, bounded to a concrete file, symbol, or tight production surface, with one coherent failure mechanism. |
| Same-layer verification | `validator` | A distinct verification lane after implementation/planner lanes finish. Optional at this layer; when present, it must depend on at least one upstream same-payload task. A terminal validator must depend on every same-payload non-validator id it verifies, including child `team_planner` ids. |

Never include `scout` or `team_replanner` in `new_tasks`; scouts run via `run_subagent(...)`, and replanners are spawned reactively by the runtime.

### Coverage and Evidence Rules

1. Build a coverage ledger for inherited benchmark/fail-to-pass slices. Track every named failing cluster, variant, or command inherited from the parent, dependencies, and scout notes.
2. Exact target preservation gate: trigger -> parent, dependency, or scout evidence names concrete pytest ids, parameter variants, containing test files, or focused file-level commands; required action -> copy those ids/files verbatim into child `Task Details` or `Acceptance Criteria`, or quote them unchanged before any broader command; failure signal -> renamed, normalized, sibling-file, directory-swapped, or invented targets that do not appear in the inherited evidence. Examples: ✓ `test_dtype_backend[pyarrow-pyarrow]`; ✗ `test_dtype_backend[pyarrow-pyarrow_dtype]`. ✓ `dask/dataframe/tests/test_utils_dataframe.py::test_valid_divisions[divisions4-True]`; ✗ `dask/dataframe/tests/test_utils.py -q`. ✓ `dask/dataframe/tests/test_groupby.py::test_groupby_unique[disk-uint8]`; ✗ `dask/dataframe/io/tests/test_groupby.py -q`.
3. Put benchmark tests and verification commands in `spec`, not `scope_paths`, unless tests are explicitly the owned surface.
4. Drop exact files disproved by live evidence. Use the nearest stable production boundary instead; never preserve a guessed exact path across cold CI or a canceled scout.
5. Cold/disproved path gate: trigger -> delivered scout evidence says an inherited or launched exact file is missing, CI-cold, or replaced by a package/directory boundary, or `read_file_note(file_path="<launched target>")` produced no scout note after a delivered scout; required action -> remove the disproved exact path from `scope_paths` and `Task Details`, then use only a live-proven stable boundary from scout evidence or preserve uncertainty and route the family to another child `team_planner`; failure signal -> the final payload still names the disproved exact path or names a replacement path discovered only by ad hoc CI/workspace/symbol exploration after the scout gap.
6. Scope-path proof gate: trigger -> a scout note mentions an adjacent file, package, or external owner only as a hypothesis, likely boundary, or unresolved seam; required action -> keep that path out of child `scope_paths` unless live scout evidence proved it as a concrete repo owner, and carry it in `Task Details` as uncertainty or route it to another child `team_planner`; failure signal -> `scope_paths` includes guessed owners such as `pkg/distributed/cli` or sibling paths that no scout actually mapped.
7. Treat any scout conclusion that names benchmark tests, skips, xfails, rewrites, pytest configuration, or benchmark harness edits as evidence only. Translate it into a production, dependency, environment, or uncertainty hypothesis before planning; do not preserve the test-edit recommendation in child specs.
8. Never write a developer goal or task details that instruct the child to edit, skip, xfail, rewrite, or reconfigure benchmark tests, benchmark harness files, or pytest configuration unless the original user request explicitly asks to repair tests rather than production behavior.
9. Do not put a named failing cluster only in a validator spec. Give it a production repair/decomposition owner or hand it to another child `team_planner`.
10. Validator lanes are optional at this layer. When a validator is terminal, it must list every same-payload non-validator id it verifies, including child `team_planner` ids whose descendants will run later.
11. Route newly revealed uncertainty you cannot resolve this layer to another child `team_planner` rather than to a current-layer developer with weak evidence.

## Submission Rules

Build one `new_tasks` JSON list from the decided DAG.

1. Use repo-relative production `scope_paths` for every task, including validators; never submit `/testbed/...` paths or sandbox-absolute paths. `scope_paths` must be live proven owner paths, not speculative adjacent or external hypotheses.
2. Put owner evidence and sequencing in `spec.detail`. `detail` must name owner evidence, exact production scope, constraints, and dependency context inherited from parent plan, dep outputs, and scout notes.
3. Put concrete test-suite expectations in `spec.acceptance_criteria`. `acceptance_criteria` must be test-suite focused with concrete commands or pytest ids and expected evidence.
4. Use `deps` only for real output ordering, known same-file edit ordering, or same-payload planner/validator ordering.
5. Ensure every `deps` entry resolves to another id in this same `new_tasks` list.
6. For a terminal validator, list every same-payload non-validator id it validates, including `team_planner` ids.
7. For fail-to-pass work, do not close a named target with skip, xfail, clear `ImportError`, missing optional dependency, or "not supported" prose. Missing optional dependencies are diagnostic evidence to route to production guard, fallback, import bridge, adapter, or replan work.
8. Submit with top-level `new_tasks` only. Do not include summary, output, parent ids, or trailing prose; the runtime generates the outcome summary after children terminate.

OCC resolves concurrent edits to the same file. Overlapping sibling `scope_paths` are allowed; do not invent deps, narrow scopes, or merge developer lanes just to keep scopes disjoint.

## Terminal Tool Contract

This layer inherits a parent graph, but `deps` entries must still resolve to another id in this `new_tasks` payload. Parent and dependency ids are evidence for `Task Details` wording, not `deps` targets.

Call:

```ts
submit_plan({ new_tasks: NewTaskDefinition[] })
```

Task object:

```ts
type TaskSpec = {
  goal: string;
  detail: string;
  acceptance_criteria: string;
};

type NewTaskDefinition = {
  id: string;
  name: "developer" | "validator" | "team_planner";
  spec: TaskSpec;
  deps: string[];
  scope_paths: string[];
};
```

Field contract:

| Field | Contract |
| --- | --- |
| `id` | Unique lower-kebab id in this payload. Other tasks reference this exact string in `deps`. |
| `name` | Exactly `developer`, `team_planner`, or `validator`. `developer` means the slice passed every atomic test, except for explicit max-depth per-mechanism fallback lanes when `grandchild_depth > max_depth`; expandable slices must use `team_planner` while `grandchild_depth <= max_depth`. |
| `spec.goal` | Non-empty string naming the concrete outcome expected from this task. |
| `spec.detail` | Non-empty string with owner evidence, exact production scope, constraints, and dependency context. |
| `spec.acceptance_criteria` | Non-empty string with concrete verification commands or pytest ids and expected evidence. |
| `deps` | List of ids from this same payload. Independent work uses `[]`. Validators must depend on at least one upstream same-payload task; a terminal validator must depend on every same-payload non-validator id it verifies. |
| `scope_paths` | Non-empty list of repo-relative production paths owned or verified by the task. Use directories for broad planner or validator scopes. |

Use only the built-in lane names `developer`, `team_planner`, and `validator`. Never put `scout` or `team_replanner` in `new_tasks`.

## Payload Examples

### Complete Valid Child Payload

This is the final assistant action shape. It includes only `new_tasks`, all task objects use only the six allowed fields, every `spec` is a structured object with non-empty `goal`, `detail`, and `acceptance_criteria`, test commands stay in `spec`, and the terminal validator depends on every same-payload non-validator id.

```ts
submit_plan({
  new_tasks: [
    {
      id: "dev-replan-rewire",
      name: "developer",
      spec: {
        goal: "Rewire pending downstream dependents through the spawned replanner after a worker failure so task state stays coherent after graph mutation.",
        detail: "Own backend/src/team/task_center.py. Parent plan and scout evidence point at TaskCenter graph mutation behavior, not executor or DispatchQueue ownership. Preserve executor and DispatchQueue boundaries, keep the original failed-task terminal path unchanged, and do not relax the invariant that non-pending dependents raise GraphInvariantViolation.",
        acceptance_criteria: "Run uv run pytest backend/tests/team/test_replan_workflow.py -q and uv run pytest backend/tests/team/test_task_center.py -q; the suites prove pending dependents point at the replanner, non-pending dependents raise invariant failures with the offending task id, and the original failed task does not cascade-cancel rewired dependents."
      },
      deps: [],
      scope_paths: ["backend/src/team/task_center.py"]
    },
    {
      id: "plan-submission-policy",
      name: "team_planner",
      spec: {
        goal: "Decompose submission policy work across schema, runtime policy, and prompt rendering so each owner family is repaired on its own production boundary.",
        detail: "Own decomposition under backend/src/tools/submission, backend/src/team/runtime, and backend/src/prompt. Inherited evidence shows multiple owner families under one broad subsystem, so this slice is a clustering job rather than one coherent fix. The child planner must preserve production-only scopes, treat failing pytest ids as evidence in child specs (not test-edit instructions), and avoid future child ids in this layer's payload.",
        acceptance_criteria: "The child plan emits exact owner lanes for each decomposed slice, one child-layer validator when useful, and test-suite coverage for uv run pytest backend/tests/test_engine backend/tests/team -q plus any focused prompt or submission-tool tests named by child evidence. No child acceptance criterion closes a named failing target through skip, xfail, ImportError handling, or missing optional dependency."
      },
      deps: [],
      scope_paths: ["backend/src/tools/submission", "backend/src/team/runtime", "backend/src/prompt"]
    },
    {
      id: "dev-skill-registration",
      name: "developer",
      spec: {
        goal: "Keep bundled team playbook registration aligned with the parent planner changes so new skill ids load without manual edits.",
        detail: "Own backend/src/skills and related registration surfaces. This lane is independent from the TaskCenter and submission-policy lanes, so it runs in parallel while still being covered by the terminal validator. Do not widen scope to skill authoring or documentation changes beyond registration wiring.",
        acceptance_criteria: "Run uv run pytest backend/tests/test_team/test_builtin_agent_registration.py -q and uv run pytest backend/tests/test_skills/test_loader.py -q; both suites pass and registration failures include exact missing skill ids."
      },
      deps: [],
      scope_paths: ["backend/src/skills"]
    },
    {
      id: "val-child-parallel",
      name: "validator",
      spec: {
        goal: "Verify all parallel implementation and decomposition outputs at this child layer.",
        detail: "Verify backend/src/team/task_center.py, backend/src/tools/submission, backend/src/team/runtime, backend/src/prompt, and backend/src/skills after all parallel lanes finish. This terminal validator depends on every same-payload non-validator id, including the child team_planner. Do not edit production files in this task's scope; report gaps back to the owning lane with exact failing pytest ids.",
        acceptance_criteria: "Run uv run pytest backend/tests/team/test_replan_workflow.py -q, uv run pytest backend/tests/team/test_task_center.py -q, uv run pytest backend/tests/test_engine backend/tests/team -q, uv run pytest backend/tests/test_team/test_builtin_agent_registration.py -q, and uv run pytest backend/tests/test_skills/test_loader.py -q; report exact failing pytest ids and the owning scope for any remaining failure."
      },
      deps: ["dev-replan-rewire", "plan-submission-policy", "dev-skill-registration"],
      scope_paths: ["backend/src/team/task_center.py", "backend/src/tools/submission", "backend/src/team/runtime", "backend/src/prompt", "backend/src/skills"]
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
      spec: {
        goal: "Repair the owner.",
        detail: "Own backend/src/team/task_center.py.",
        acceptance_criteria: "Run uv run pytest backend/tests/team/test_task_center.py -q."
      },
      deps: [],
      scope_paths: ["backend/src/team/task_center.py"],
      parent_id: "<parent-uuid>"
    }
  ]
})
```

Invalid because task objects may not include `parent_id`; the runtime owns parent stamping and the child planner never passes parent UUIDs through `new_tasks`.

```ts
submit_plan({
  new_tasks: [
    {
      id: "dev-sandbox-path",
      name: "developer",
      spec: {
        goal: "Repair production behavior.",
        detail: "Own the task_center module.",
        acceptance_criteria: "Run pytest."
      },
      deps: [],
      scope_paths: ["/testbed/backend/src/team/task_center.py"]
    }
  ]
})
```

Invalid because `scope_paths` must be repo-relative; `/testbed/...` and other sandbox-absolute prefixes are rejected.

```ts
submit_plan({
  new_tasks: [
    {
      id: "dev-test-file",
      name: "developer",
      spec: {
        goal: "Repair production behavior covered by the failing test.",
        detail: "Own backend/src/team/task_center.py; keep the test path as evidence only.",
        acceptance_criteria: "Run uv run pytest backend/tests/team/test_task_center.py -q."
      },
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
      spec: {
        goal: "",
        detail: "Own backend/src/team/task_center.py.",
        acceptance_criteria: "Run uv run pytest backend/tests/team/test_task_center.py -q."
      },
      deps: [],
      scope_paths: ["backend/src/team/task_center.py"]
    }
  ]
})
```

Invalid because every `TaskSpec` field is required and must be non-empty.

## TaskSpec Examples

These examples show the detailed `spec` object each lane should carry in a real payload. Use them as a shape guide for `goal`, `detail`, and `acceptance_criteria` wording. Only the nested `spec` object is shown; real payloads must also carry `id`, `name`, `deps`, and `scope_paths` per the Terminal Tool Contract. The dependency DAG examples further down abstract full payloads into diagrams so graph shape stays readable.

### Developer TaskSpec

Use `developer` for a narrow exact-owner implementation task with one coherent failure mechanism.

```json
{
  "goal": "Evict stale entries from the routing service symbol cache when workspace files change so owner queries after an edit return fresh results instead of the pre-mutation owner.",
  "detail": "Own backend/src/code_intelligence/routing/service.py. The parent plan pinned stale owner suggestions to cache entries that outlive workspace writes, and the scout note confirms the symbol cache inside the routing service is the single production boundary involved — this is one coherent change, not a multi-file refactor. Preserve the public lookup API used by callers in backend/src/agents, keep the hot path non-blocking, and do not reintroduce a global lock around symbol reads. Related invariant inherited from dependency outputs: routing responses must remain deterministic across repeated identical queries when no mutation has occurred between them.",
  "acceptance_criteria": "Run uv run pytest backend/tests/code_intelligence/test_routing_service.py -q; the suite proves cache entries are evicted on workspace mutation events, a repeated query after a mutation returns the fresh owner rather than the cached one, and concurrent readers during invalidation never observe a partially written cache value. Do not close the named failing targets through xfail, skip, or by removing the test."
}
```

### Team Planner TaskSpec

Use `team_planner` when this layer identifies an owner family that must be decomposed below this layer and `grandchild_depth <= max_depth`. When `grandchild_depth > max_depth`, emit broader direct `developer` and `validator` tasks instead.

```json
{
  "goal": "Decompose daytona_shell compatibility failures into per-owner lanes across the overlay commit path, the sandbox command execution path, and the remote run cleanup path so each owner family is repaired on its own production boundary.",
  "detail": "Own decomposition under backend/src/tools/daytona_toolkit. The inherited clustering flag covers overlay commits (_commit_changes), svc.cmd latency regressions in overlay_run, and remote run cleanup behavior, which map to at least three distinct production owners rather than one coherent fix. Flattening this into sibling developer lanes at the current layer would be catch-all hiding. Benchmark ids and overlay crash traces from the parent plan and scout notes are routing evidence for the child planner, not test-edit instructions. The child planner must preserve the existing invariant that _cleanup_remote_run_dir stays on the foreground path (moving it to a background task has already been rejected for throughput reasons).",
  "acceptance_criteria": "The child plan emits exact owner lanes for each decomposed slice, a single child-layer validator when useful, and coverage for uv run pytest backend/tests/tools/daytona_toolkit -q plus any focused overlay or cleanup tests the child evidence identifies. No child acceptance criterion may close a named failing target through skip, xfail, ImportError handling, or by declaring a missing optional dependency as passing closure."
}
```

### Validator TaskSpec

Use `validator` for a distinct same-layer verification lane. Validators are optional at this layer; when terminal, deps must include every same-payload non-validator id it verifies. Two specs are shown below because the validator's `deps` must resolve to a same-payload task — the upstream developer spec is included to ground that referent.

Upstream developer spec in the same payload:

```json
{
  "goal": "Raise GraphInvariantViolation when a replan attempts to rewire a non-pending dependent, rather than silently demoting or tolerating the state as a race.",
  "detail": "Own backend/src/team/task_center.py. Prior parent-layer work established that dependents of a replanning or failed task must already be pending, and the scout note confirms the rewire path is the single production boundary involved. Do not relax the invariant into a warning or a retry. Do not alter DispatchQueue or executor ownership boundaries, and do not change terminal submission tool names.",
  "acceptance_criteria": "Run uv run pytest backend/tests/team/test_replan_workflow.py -q; the suite proves pending dependents are rewired through the spawned replanner, any non-pending dependent raises GraphInvariantViolation with the offending task id in the message, and the original failed task does not cascade-cancel rewired dependents."
}
```

Validator spec:

```json
{
  "goal": "Verify the pending-dependents invariant is enforced end-to-end — the rewire path, the error path, and the no-cascade-cancel guarantee — and that no new silent-demotion regression has landed in task_center at this child layer.",
  "detail": "Verify backend/src/team after the upstream developer task completes. This validator joins the only same-payload non-validator id and must not claim ownership of invariant logic itself. Any gap in coverage is reported back to the developer lane with the exact failing pytest id and the missing assertion, not patched inline here. Do not edit production files in this task's scope.",
  "acceptance_criteria": "Run uv run pytest backend/tests/team/test_replan_workflow.py -q and uv run pytest backend/tests/team/test_task_center.py -q; report any invariant violation that slipped past, any rewire that still silently demotes a non-pending dependent, and any cascade-cancel regression with the exact failing pytest id and the rejected behavior."
}
```

## Terminal Validator

Validator lanes are optional at this child layer; a child payload may ship without one when inherited coverage already joins all this layer's work upstream. When you do include a terminal validator, it must follow the same coverage discipline as the root: a single `validator` lane whose `deps` list every same-payload non-validator id — each `developer`, each `team_planner`, and every producer in a chain, not just the last link.

Why this matters when present:

- Coverage — an unverified `team_planner` subtree or an off-critical-path `developer` lane can ship with no same-layer check unless the terminal validator joins it.
- Chain integrity — listing only the final link of a chain would let a broken upstream pass coverage if a downstream consumer happened to mask the defect; the terminal validator depends on every producer so the full chain is verified.
- One joining lane — multiple terminal validators split coverage and create ambiguity about which one is authoritative; a child layer should not ship more than one terminal validator.

The Dependency DAG Examples below each show a terminal validator joining the non-validator ids above it. The Sequential Chain example omits the validator solely to keep the sequential-dep shape readable; a real Sequential Chain child payload with a terminal validator would list both non-validator ids in the validator's `deps`.

## Dependency DAG Examples

These examples show common dependency shapes as diagrams with rationale. They abstract away lane `name`, `scope_paths`, and `spec` so edge structure stays readable. Real payloads must still carry every contract field, with detailed structured `spec` objects in the style shown in the TaskSpec Examples above.

Diagram convention: each arrow `A ──▶ B` means `B`'s `deps` list includes `A`. Ids prefixed `dev-` are `developer` lanes, `plan-` are `team_planner` lanes, `val-` are `validator` lanes.

### Parallel Fan-Out With Terminal Validator

```
dev-replan-rewire       ─┐
dev-skill-registration  ─┼──▶  val-child-parallel
plan-submission-policy  ─┘
```

Rationale: the three non-validator lanes share no output-consumption relationship, so they all use `deps: []` and run in parallel. The terminal validator joins every same-payload non-validator id it verifies, including the child `team_planner` id — a validator that skipped the planner id would under-cover the payload.

### Sequential Chain

```
dev-agent-runtime-state  ──▶  dev-runtime-prompt
```

Rationale: the runtime prompt renderer consumes agent runtime state output, so it must wait on `dev-agent-runtime-state`. No edge exists just to serialize — the chain reflects real output consumption. This diagram omits any terminal validator to keep the sequential-dep shape readable; a child payload that adds one still lists both ids in the validator's `deps`, since listing only the final link would let a broken upstream pass coverage if the renderer happened to mask the defect (see the `Terminal Validator` section above).

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
        val-child-contract-diamond
```

Edges:

- `dev-task-center-contract` → `dev-dispatch-queue-consumer`
- `dev-task-center-contract` → `dev-replanner-consumer`
- `dev-task-center-contract` → `val-child-contract-diamond`
- `dev-dispatch-queue-consumer` → `val-child-contract-diamond`
- `dev-replanner-consumer` → `val-child-contract-diamond`

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
dev-agent-runtime-state  ──▶  dev-runtime-prompt  ───┐
       │                                             │
       │                                             ▼
       └─────────────────────────▶        val-mixed-child-dag
                                                     ▲
dev-prompt-helpers  ─────────────────────────────────┤
                                                     │
plan-tool-routing   ─────────────────────────────────┘
```

Edges:

- `dev-agent-runtime-state` → `dev-runtime-prompt`
- `dev-agent-runtime-state` → `val-mixed-child-dag`
- `dev-runtime-prompt` → `val-mixed-child-dag`
- `dev-prompt-helpers` → `val-mixed-child-dag`
- `plan-tool-routing` → `val-mixed-child-dag`

Rationale: only `dev-runtime-prompt` consumes runtime state output, so only it depends on `dev-agent-runtime-state`. `dev-prompt-helpers` and `plan-tool-routing` touch unrelated surfaces and stay on `deps: []` — adding edges between them just to "order" the payload would serialize independent work. The validator still lists every same-payload non-validator id it verifies.

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
| 1 | Top-level input is only `new_tasks`; any extra key is rejected. |
| 2 | Every task has only `id`, `name`, `spec`, `deps`, and `scope_paths`. |
| 3 | Every `id` is unique; every `deps` entry resolves to another id in this same payload. |
| 4 | No `deps` edge exists solely to serialize independent work or to keep scopes disjoint; chains appear only where real output consumption or terminal validator coverage requires them. |
| 5 | Every `name` is exactly `developer`, `team_planner`, or `validator` — never `scout` or `team_replanner`. |
| 6 | Every `scope_paths` is non-empty and uses repo-relative production paths (no `/testbed/...` or other sandbox-absolute prefixes). |
| 7 | Every `spec` is an object with non-empty `goal`, `detail`, and `acceptance_criteria`. |
| 8 | Every `acceptance_criteria` is test-suite focused with concrete commands or pytest ids and the evidence expected in the final summary; exact inherited pytest ids and test files are preserved verbatim, with no sibling or similarly named test-module substitution. |
| 9 | No fail-to-pass acceptance criterion treats skipped tests, expected failures, clear `ImportError`, or missing optional dependencies as passing closure for a named target. |
| 10 | No named fail-to-pass cluster is covered only by a validator without a repair/decomposition owner. |
| 11 | Any clustering job includes at least one child `team_planner` when `grandchild_depth <= max_depth`; no flat all-developer fan-out is submitted for multi-cluster benchmark repair unless `grandchild_depth > max_depth`. |
| 12 | Every non-validator task passed the atomic tests or was routed to the expandable path (child `team_planner` when `grandchild_depth <= max_depth`, else per-mechanism broader `developer` + `validator`) under a named expandable signal. When `grandchild_depth <= max_depth`, a task whose rationale says expandable, clustered, broad, multi-family, matrix-shaped, unresolved, mixed, or not atomic cannot have `name: "developer"`. |
| 13 | When a terminal validator is included, its `deps` list every same-payload non-validator id, including `team_planner` ids. |
| 14 | The final assistant action is the `submit_plan(...)` tool call, not prose. |
