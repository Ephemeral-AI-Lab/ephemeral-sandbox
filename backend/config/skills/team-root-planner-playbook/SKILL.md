---
name: team-root-planner-playbook
description: Playbook for the root_planner agent. Analyze the user request, optionally scout routing-changing production ownership, then submit a schema-valid root plan with submit_plan(...).
---

# Team Root Planner Playbook

Produce the top-level task DAG from the user request. Finish with exactly one `submit_plan(...)` call.

<Forbid Rule>
Never plan test suite or test-file related tasks.
Never assign subagents to explore test suites or test files.
</Forbid Rule>

| Route | Use when |
| --- | --- |
| `developer` | Atomic slice: exact owner + one mechanism. Default for every atomic piece, even inside a broad request. |
| `team_planner` | Clustered/complex/unresolved residue only. Never wrap an atomic slice or delegate the whole request. |
| `validator` | Same-payload verification after producer lanes. |

| Gate | Action |
| --- | --- |
| Owner questions change this DAG | Scout by production mechanism. |
| Several owner/mechanism rows | Launch one scout per independent row; synthesize sibling lanes. |

## Stage Flow

```text
Caption: root planner stage machine. Each reference is loaded only at the stage that uses it.

user request
  |
  v
[1 Load context]
  | request evidence -> owner ledger
  |
  | owner questions would change this level's routing?
  |-- yes --> [2 Scout row wave] -> harvest notes -> update ledger
  |-- several rows -> [2 Scout row wave] -> sibling lanes
  |-- no / test-only -> carry uncertainty in expandable spec
  |
  v
[3 Synthesize]
  Stage 2 closed -> load synthesize-and-submit -> submit_plan(...)
```

| Stage | Output |
| --- | --- |
| 1. Load context | Owner ledger: clear owners, scout candidates, unresolved clusters, verification evidence. |
| 2. Scout | Superficial directory/multi-file maps or deep tight-seam checks; production `target_paths` only. |
| 3. Synthesize | After scouts or no-scout decision, load reference and emit local DAG. |

## 1. Load Context

```text
Caption: split evidence from ownership before making lanes.

request
  |-- commands / benchmark ids / failing tests -> evidence
  |-- exact production file or symbol ---------> clear owner
  |-- broad family / matrix / migration -------> scout candidate
  `-- guessed or test-derived owner -----------> unresolved
```

| Check | Root-planner action |
| --- | --- |
| Intent | Mark bugfix, refactor, feature, migration, benchmark, or mixed. |
| Clustering | Group many failures by changelog group, owner, mechanism, API, dtype, engine, or format. |
| Benchmark evidence | Keep tests and ids as verification evidence, not owner proof. |
| Boundary probe | Use at most one targeted CI structure/symbol query when it changes scout shape. |

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

Enter after the first useful row wave is done or intentionally skipped; do not load the Stage 3 reference to decide whether to scout.

```text
Caption: root routing during synthesis.

atomic slice          -> developer
clustered residue     -> team_planner sibling
same-payload evidence -> validator with deps=[verified producers]
```

| Draft check | Expected result |
| --- | --- |
| Coverage | Every named cluster has a producer owner or sibling `team_planner`; trivial slices stay separate. |
| Developer lanes | Exact owner and one mechanism; not a hidden broad cluster. |
| Planner lanes | Preserve uncertainty and evidence without leaf-level overexploration. |
| Validators | Depend on every same-payload producer they verify. |
| Payload | `id`, `agent`, `spec`, `deps`, and `scope_paths` only. |

Run this checklist, then make `submit_plan({ "new_tasks": [...] })` the final assistant action: no summary, output, parent ids, trailing prose, or later tool calls.
