---
name: team-planner-playbook
description: Playbook for the team_planner agent. Load inherited Task Center context, optionally scout routing-changing production ownership, and submit a schema-valid child plan with submit_plan(...).
---

# Team Planner Playbook

Produce a child task DAG from inherited Task Center context. Finish with exactly one `submit_plan(...)` call.

<Forbid Rule>
Never plan test suite or test-file related tasks.
Never assign subagents to explore test suites or test files.
</Forbid Rule>

| Route | Use when |
| --- | --- |
| `developer` | Atomic slice: exact owner + one mechanism. Default for every atomic piece, even inside a broad task. |
| `team_planner` | Clustered/complex/unresolved residue only, while depth remains. Never wrap an atomic slice or delegate the whole task. |
| `developer` / `validator` fallback | Max depth reached; split by mechanism and keep uncertainty in `spec.detail`. |
| `validator` | Same-layer verification after producer lanes. |

| Gate | Action |
| --- | --- |
| Owner questions change this DAG | Scout by production mechanism. |
| Several owner/mechanism rows | Launch one scout per independent row; synthesize sibling lanes. |

## Stage Flow

```text
Caption: child planner stage machine. Each reference is loaded only at the stage that uses it.

assigned planner task
  |
  v
[1 Load context]
  | task + parent + deps + graph topology -> owner ledger
  |
  | owner questions would change this level's routing?
  |-- yes --> [2 Scout row wave] -> harvest notes -> update ledger
  |-- several rows -> [2 Scout row wave] -> sibling lanes
  |-- no / test-only -> carry uncertainty in expandable spec
  |
  v
[3 Synthesize]
  Stage 2 closed -> load submit-child-plan -> submit_plan(...)
```

| Stage | Output |
| --- | --- |
| 1. Load context | Owner ledger: inherited owners, scout candidates, unresolved clusters, deps, verification evidence. |
| 2. Scout | Superficial directory/multi-file maps or deep tight-seam checks; production `target_paths` only. |
| 3. Synthesize | After scouts or no-scout decision, load reference and emit local DAG. |

## 1. Load Context

```text
Caption: inherited context becomes routing rows.

parent/deps/notes
  |-- proven owner + mechanism -----------> inherited owner
  |-- broad family / matrix / benchmark --> scout candidate
  |-- missing or ambiguous owner ---------> unresolved
  |-- upstream result needed here --------> deps
  `-- pytest ids / commands / repro ------> evidence
```

| Step | Action |
| --- | --- |
| Read context | Call `read_task_details(task_id=...)` for own task, parent, and each dep UUID. |
| Inspect topology | Call `read_task_graph()` for dependency topology only. |
| Classify intent | Mark bugfix, refactor, feature, migration, benchmark, or mixed. |
| Build ledger | Group inherited changelog rows, owners, mechanisms, deps, and evidence. |

Planner exploration stops at routing; use scouts for owner maps and preserve uncertainty instead of proving leaves.

## 2. Scout

```text
Caption: scout fan-out supports the next sibling wave.

HDF row           -> run_subagent(agent_name="scout", input={"target_paths":["pkg/io/hdf.py"], "context":"Objective: map HDF owner."})
parquet package   -> run_subagent(agent_name="scout", input={"target_paths":["pkg/io/parquet"], "context":"Depth: superficial package map."})
groupby row       -> run_subagent(agent_name="scout", input={"target_paths":["pkg/dataframe/groupby.py"], "context":"Objective: map dtype owner."})
config row        -> run_subagent(agent_name="scout", input={"target_paths":["pkg/config.py"], "context":"Objective: map config owner."})
compatibility row -> run_subagent(agent_name="scout", input={"target_paths":["pkg/compatibility.py"], "context":"Objective: map compatibility owner."})
```

| Scout shape | Use when |
| --- | --- |
| Single path | Deep scout when one file or module is the likely owner. |
| Multi-path | Deep scout when paths form one tight dependency, entrypoint, adapter, or shared mechanism. |
| Directory | Superficial scout when owner is a package/subsystem, engine matrix, or package-like import path. |
| Wave size | One scout per independent row; avoid unrelated bundles and one-per-test. |

Use `input`, not `prompt`, so assigned `target_paths` reach the scout. Keep paths production-only: one directory or short coupled file list. If a guessed file is missing or becomes a package, do one superficial directory scout or hand off to `team_planner`.

## 3. Synthesize

Enter after context is loaded and the first useful row wave is done or intentionally skipped; do not load the Stage 3 reference to decide whether to scout.

```text
Caption: child routing with depth.

atomic slice                         -> developer
clustered residue + depth remains    -> team_planner sibling
owner cluster + max depth reached    -> per-mechanism developer/validator split
same-layer evidence                  -> validator with deps=[verified producers]
```

| Draft check | Expected result |
| --- | --- |
| Coverage | Every inherited cluster has a producer owner or sibling `team_planner`; trivial slices stay separate. |
| Developer lanes | Exact owner and one mechanism, unless this is a max-depth fallback. |
| Planner lanes | Preserve uncertainty and evidence without leaf-level overexploration. |
| Validators | Depend on every same-payload producer they verify. |
| Payload | `id`, `agent`, `spec`, `deps`, and `scope_paths` only. |

Run this checklist, then make `submit_plan({ "new_tasks": [...] })` the final assistant action: no summary, output, parent ids, trailing prose, or later tool calls.
