---
name: team-planner-playbook
description: Playbook for the team_planner agent. Load inherited Task Center context, scout risk-bearing production ownership, and submit a schema-valid child plan with submit_plan(...).
---

# Team Planner Playbook

Produce a child task DAG from inherited Task Center context. Finish with exactly one `submit_plan(...)` call.

Planner lane routing:

- Exact, live-proven, single-owner work -> `developer`.
- Broad, clustered, matrix-shaped, unresolved, or large benchmark/test-matrix work -> child `team_planner` when `grandchild_depth <= max_depth`.
- Max-depth fallback -> broader direct `developer` or `validator` tasks, with uncertainty kept in `spec.detail`.
- Same-layer verification -> `validator`.

## Workflow Map

| Stage | Output |
| --- | --- |
| 1. Load context | Owner ledger split into inherited, `scout_required`, unresolved, deps, and evidence groups. |
| 2. Scout | Optional small scout wave, or explicit uncertainty delegated downward. |
| 3. Synthesize and submit | `submit-child-plan` reference available for synthesis, payload checked, one `submit_plan(...)`. |

```text
Caption: child planner stage machine. References support synthesis.

[assigned planner task]
  |
  v
[1 Load context]
  | read own task, parent, deps, and graph topology
  | build owner ledger
  |
  | unresolved or benchmark-risk owner
  | and scout would change routing?
	  |-- yes --> [2 Scout]
  |             | join scouts
  |             | read notes by scoped path
  |             v
	  +----------- update owner ledger
  |-- no --> carry uncertainty
  |
  v
[3 Synthesize]
	  synthesis guidance:
	    load_skill_reference(
	      skill_name="team-planner-playbook",
	      reference_name="submit-child-plan"
	    )
  then: draft -> checklist -> submit_plan(...)
```

## Workflow Details

### 1. Load context

| Step | Action |
| --- | --- |
| Read context | Call `read_task_details(task_id=...)` for own task, parent, and each dep UUID from the prompt header. |
| Inspect topology | Call `read_task_graph()` for dependency topology only; graph output is not a license to read every sibling. |
| Classify intent | Mark bugfix, refactor, feature, migration, benchmark, or mixed; raise a clustering flag for many failing tests, several production families, or a matrix under one subsystem. |
| Build owner ledger | Group inherited owner slices, `scout_required` slices, unresolved slices, dependency outputs, and verification evidence. |

```text
Caption: inherited context becomes routing rows.

parent/deps/scout notes
  |-- proven owner + coherent mechanism ------------> inherited
  |-- broad family / matrix / benchmark cluster ----> scout_required
  |-- missing or ambiguous owner -------------------> unresolved
  |-- upstream result needed by this layer ---------> deps
  `-- pytest ids / commands / repro details --------> evidence
```

For inherited benchmark, fail-to-pass, migration, or compatibility clusters, put each broad family, matrix family, or likely expandable first-pass owner in `scout_required`. For restructured packages with multiple plausible owner files, scout first instead of assigning sibling-file owners from test names, backend labels, or file-name affinity.

Keep inherited detail wording intact when passing parent or dependency context to child specs.

### 2. Scout

```text
Caption: one scout per owner-ledger row; notes are harvested per assigned path.

row: parquet owner family
  -> run_subagent(... target_paths=["pkg/io/parquet"])
  -> read_file_note(file_paths=["pkg/io/parquet"])

row: config owner family with two scoped paths
  -> run_subagent(... target_paths=["pkg/config", "pkg/options"])
  -> read_file_note(file_paths=["pkg/config", "pkg/options"])

Different rows stay in different scout calls.
```

| Step | Action |
| --- | --- |
| Shape wave | Launch scouts only for high-value `scout_required` or unresolved owner families. A useful wave is usually 1-3 families; avoid one scout per failing test and one broad catch-all scout. Pick scout shape by dependency hypothesis; a reasonable guess is enough before launching. |
| Keep scope clean | Keep `target_paths` production-only. Put tests, `test_*.py`, benchmark harnesses, verification paths, missing test-derived files, skipped variants, optional-dependency errors, and verification commands in scout `context`. |
| Launch and supervise | Fire every useful scout before polling. Poll while scouts are `running`; cancel halted, blocked, off-scope, or unchanged scouts and carry that slice as explicit uncertainty. |
| Harvest notes | Call `read_file_note(file_paths=[...])` with every path in every launched scout's `target_paths`. Missing notes, cold CI, canceled scouts, or disproved exact files create uncertainty only for the affected path. |

| Scout shape | Use when |
| --- | --- |
| Single path | One file or module is the likely owner. |
| Multi-path, one row | Paths are coupled by dependency, entrypoint, adapter, or shared mechanism. |
| Directory | Owner is a package/subsystem, or exact files are not knowable without mapping. |
| Separate scouts | Paths belong to different owner families. |
| No scout, route to `team_planner` | Boundary is broad enough that exploration becomes decomposition. |

If an adjacent owner is only a hypothesis, launch a separate scout for that path when it changes current-layer routing, or carry it as uncertainty; do not ask one scout to inspect files outside its `target_paths`.

### 3. Synthesize and submit

	Enter this stage after context is loaded, the owner ledger is written, and scouts are either done or explicitly skipped. Load the synthesis reference when it helps draft or check the child plan:

```text
load_skill_reference(
  skill_name="team-planner-playbook",
  reference_name="submit-child-plan"
)
```

After loading the reference, normally continue with drafting and submission. If a new distinct owner slice would require exploration, carry it as uncertainty or make a bounded routing check before assigning it to another child `team_planner` when depth allows, or to a max-depth diagnostic/repair lane.

```text
Caption: lane routing with depth.

expandable slice + grandchild_depth <= max_depth -> team_planner
expandable slice + grandchild_depth > max_depth  -> broader developer/validator
atomic exact-owner slice                         -> developer
same-layer verification                          -> validator with producer deps
```

| Step | Action |
| --- | --- |
| Draft tasks | Use id, agent, deps, scope_paths, and a structured `spec` with non-empty `goal`, `detail`, and `acceptance_criteria`. Choose each task's agent while drafting, cover every named failing cluster with a repair/decomposition owner or child `team_planner`, and preserve concrete pytest ids or test files verbatim in child specs. |
| Submit | Walk the reference Final Checklist, then submit top-level `new_tasks` only: no summary, output, parent ids, trailing prose, or later tools. |

Put owner evidence, exact production scope, constraints, and dependency context inside each `spec.detail`. Before submit, audit every `developer` task: it either passed every atomic test, or it is an explicit max-depth per-mechanism fallback from the reference.
