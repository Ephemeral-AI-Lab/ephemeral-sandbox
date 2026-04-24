---
name: team-planner-playbook
description: Playbook for the team_planner agent. Load inherited Task Center context, scout risk-bearing production ownership, and submit a schema-valid child plan with submit_plan(...).
---

# Team Planner Playbook

Produce a child task DAG from inherited Task Center context. Route clear exact-owner work to `developer` lanes, push broad or clustered work down to another child `team_planner`, and reserve `validator` lanes for distinct same-layer verification. Finish with exactly one `submit_plan(...)` call.

## Workflow Map

| Stage | Purpose | Output contract |
| --- | --- | --- |
| 1. Load context | Consume inherited Task Center evidence and build an owner ledger for this layer. | Owner ledger split into inherited, scout-required, unresolved, deps, and evidence groups. |
| 2. Scout | Resolve unresolved or benchmark-risk production ownership. | Scout notes or explicit uncertainty for each launched target. |
| 3. Synthesize and submit | Convert inherited context and scout evidence into a schema-valid child DAG and emit the terminal payload. | One `submit_plan({ "new_tasks": [...] })` call and no later tools. |

Stage boundary: `load_skill_reference(...)` belongs only to Stage 3. Do not call it while the next action is still to read task context, inspect topology, build the owner ledger, launch scouts, wait for scouts, or read scout notes. The reference load is a one-way transition: after it, do not launch scouts, poll/wait/read scout notes, or make CI/workspace/symbol exploration calls.

Decision flow:

```text
[assigned planner task]
  -> [1. Load context: task details, graph topology, owner ledger]
  -> unresolved owners or benchmark-risk owner families?
     -> yes: [2. Scout production-only owner slices, then read notes]
     -> no: continue
  -> Stage 1 owner ledger complete and no scout still needed/running?
     -> no: stay in [1. Load context] or [2. Scout]
     -> yes: [3. load submit-child-plan, apply clustering + lane routing]
             newly-revealed distinct owner slice: carry as uncertainty
             route complete: submit_plan({ "new_tasks": [...] })
```

## Reference Map

Catalog only. Do not load references from this map during Stage 1 or Stage 2.

Loadable reference used in Stage 3 via `load_skill_reference(skill_name="team-planner-playbook", reference_name="...")`:

- `submit-child-plan`: synthesis and submission rules, `submit_plan` contract plus `NewTaskSpec` field table, valid and invalid payload examples, task-spec examples for `developer`, `team_planner`, and `validator`, dependency DAG examples with rationale, and the final checklist. Load in Stage 3 before drafting any `submit_plan(...)` payload.

Reference load gate: trigger -> own task, parent task, dependency context, graph topology, and owner ledger are loaded, every `scout_required` or unresolved production owner has either completed scout evidence or explicit uncertainty, every scout wave has been joined, and every available scout note has been read; required action -> load `submit-child-plan` as the first Stage 3 action via `load_skill_reference(skill_name="team-planner-playbook", reference_name="submit-child-plan")` and then only draft/check/submit; failure signal -> `load_skill_reference(..., reference_name="submit-child-plan")` appears before context reads, before owner-ledger evidence, immediately after `load_skill(...)`, while scouts are running, or before later scout/CI exploration.

Pre-load checklist — answer all five Yes in visible reasoning before the first `load_skill_reference(...)` call:

1. Have `read_task_details` returns for own task, parent, and every dep UUID plus a `read_task_graph` result been processed this turn?
2. Is the owner ledger (inherited, scout_required, unresolved, deps, evidence) written out as text, not just implied by task details?
3. For every `scout_required` or unresolved slice, has a scout been launched OR the slice been explicitly carried as uncertainty, and have all launched scouts joined with every available note read?
4. Is the next productive step drafting `submit_plan(...)` rather than more context or scout work?
5. Can the rest of the run finish with drafting/checklist/`submit_plan(...)` only, with no later scout, note-read, CI, workspace, or symbol exploration?

Any No means the load is premature. Stay in Stage 1 or Stage 2 and do not load the reference yet.

## Workflow Details

### 1. Load context

| Step | Action |
| --- | --- |
| Read context | Call `read_task_details(task_id=...)` for own task, parent, and each dep UUID from the prompt header. |
| Inspect topology | Call `read_task_graph()` for dependency topology only; do not read sibling task details from graph output. |
| Classify intent | Mark bugfix, refactor, feature, migration, or mixed; raise a clustering flag for many failing tests, several production families, or a matrix under one broad subsystem. |
| Build owner ledger | Group inherited owner slices, unresolved owner slices, dependency outputs, and evidence to pass to children. |
| Mark scout-required | For inherited benchmark/fail-to-pass/migration/compatibility clusters, put each broad family, matrix family, or likely expandable first-pass owner in `scout_required`; failure signal: only unknown families are scouted while clear-looking families enter synthesis with no live scout evidence. For a restructured package/directory scope with multiple plausible owner files, keep the slice `scout_required`; do not assign sibling-file owners from failing test names, backend labels, or file-name affinity alone. |

Keep `2. Task Details:` wording intact when carrying parent or dependency context. The output of this stage is an owner ledger plus any clustering signal; `scout_required` and unresolved slices drive Stage 2, and empty `scout_required` plus unresolved groups route straight to Stage 3.

### 2. Scout

| Step | Action |
| --- | --- |
| Shape wave | Launch one scout per `scout_required` or unresolved production owner family with `target_paths: ["<one production owner path>"]`. Split target paths from different owner-ledger rows into separate calls in the same wave; for a package family, use one directory path rather than several sibling files. Keep tests, `test_*.py`, benchmark harnesses, verification paths, missing test-derived files, skipped variants, optional-dependency errors, and verification commands in scout `context`, not `target_paths`. For restructured package/directory scopes, do not route `core.py`/`arrow.py`-style sibling ownership from test parameters or backend names before live scout evidence names that file. |
| Launch and supervise | Fire every useful scout before polling. Poll while scouts are `running`; cancel halted, blocked, off-scope, or unchanged scouts and carry that slice as explicit uncertainty. |
| Harvest notes | Read every available note for exact launched target paths. On cold CI, canceled scouts, or disproved exact files, treat the launched target as unresolved coverage: either launch one fresh scout on a single stable production boundary before Stage 3 or carry explicit uncertainty. Do not keep the disproved exact path or replace it via ad hoc CI/workspace/symbol exploration in the same layer. |

If any candidate target matches `*/tests/*`, `test_*.py`, a benchmark harness, or a verification-only path, do not launch a scout on it. Move that path into scout `context` and keep `target_paths` production-only.
Do not use scout `context` to ask for source reads, symbol queries, or ownership checks on files or directories outside the assigned `target_paths`. If `groupby.py` is the scout target and `core.py` is only a hypothesis, either launch a separate scout on `core.py` or carry that adjacent owner as uncertainty; do not smuggle it through one single-file scout prompt.
If `read_file_note(file_path="<launched target>")` returns no scout note after a delivered scout, treat that target as unresolved scout coverage, not as permission to keep the path or self-correct it with direct CI/workspace/symbol exploration. Relaunch one scout on a stable boundary or carry uncertainty.

### 3. Synthesize and submit

Enter this stage only after Stage 1 context and owner-ledger output exists and Stage 2 is complete or explicitly skipped because no unresolved production owners remain. The reference load is the stage transition; if you are still loading context, building the owner ledger, or might need exploration, do not load it yet. Once the pre-load checklist in the Reference Map is all Yes, load `submit-child-plan` as the first Stage 3 action, then proceed without further scout, note-read, CI, workspace, or symbol exploration.

| Section | Contract |
| --- | --- |
| **Input** | Stage 1 owner ledger plus Stage 2 scout notes and uncertainty. |
| **Output** | Exactly one valid `submit_plan(...)` call and no later tool calls. Every named failing cluster is owned by a repair/decomposition task or handed to another child `team_planner`; a coverage ledger of every named failing cluster or variant is built before drafting, and a terminal validator is not an owner for otherwise unassigned failures; no named failing cluster may appear only in a validator spec. |
| **Forbidden** | Hiding multi-owner work in a catch-all developer; submitting a child `team_planner` together with its imagined child tasks; preserving scout recommendations to edit, skip, xfail, rewrite, or reconfigure tests unless the user asked for test repair; including `scout` or `team_replanner` in `new_tasks`; any tool call after `submit_plan(...)`. |

| Step | Action |
| --- | --- |
| Load synthesis reference | Per the Reference Map gate above; do not duplicate the call here before the gate is satisfied. |
| Draft tasks | Use id, name, deps, scope_paths, and a `spec` with `1. Goal:`, `2. Task Details:`, and `3. Acceptance Criteria:`. When parent, dependency, or scout evidence names concrete pytest ids or test files, preserve those targets verbatim in child specs; do not swap in sibling or similarly named test modules, directories, or broad suite aliases. |
| Route lanes | Use child `team_planner` lanes for broad, shared, unresolved, multi-family, clustered, or large benchmark/test-matrix work only when `grandchild_depth <= max_depth`; otherwise emit broader direct `developer` or `validator` tasks. Name-field lock: when `grandchild_depth <= max_depth`, any slice you call expandable, clustered, broad, multi-family, matrix-shaped, unresolved, mixed, or not atomic must have `name: "team_planner"`, never `name: "developer"`. |
| Close gaps | If a new distinct production owner slice would require exploration after the reference load, carry it as uncertainty and route it to another child `team_planner` when allowed, or to a max-depth diagnostic/repair lane; do not call scouts or CI/workspace/symbol tools after the Stage 3 transition. |
| Submit | Walk the Final Checklist in the reference, then submit top-level `new_tasks` only: no summary, output, parent ids, trailing prose, or later tools. |

Put owner evidence, exact production scope, constraints, and dependency context inside each `Task Details` body so downstream workers inherit the routing you decided at this layer. Before submit, audit every `developer` task: it either passed every atomic test, or it is an explicit max-depth per-mechanism exception from the reference.
