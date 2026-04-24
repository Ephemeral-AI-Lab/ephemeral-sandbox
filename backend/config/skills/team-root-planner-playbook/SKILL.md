---
name: team-root-planner-playbook
description: Playbook for the root_planner agent. Analyze the user request, optionally scout routing-changing production ownership, then submit a schema-valid root plan with submit_plan(...).
---

# Team Root Planner Playbook

Produce the top-level task DAG from the user request. Finish with exactly one `submit_plan(...)` call.

| Route | Use when |
| --- | --- |
| `developer` | Atomic slice: exact owner + one mechanism. Default for every atomic piece, even inside a broad request. |
| `team_planner` | Clustered/complex/unresolved residue only. Never wrap an atomic slice or delegate the whole request. |
| `validator` | Same-payload verification after producer lanes. |

| Gate | Action |
| --- | --- |
| Owner questions change this DAG | Scout by production owner family. |
| Several owner families | Fan out routing scouts by owner family; synthesize sibling lanes. |
| Test or benchmark path | Keep as evidence in `spec`, not `target_paths`. |

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
  |-- yes --> [2 Scout] -> harvest notes -> update ledger
  |-- several rows -> [2 Scout] -> sibling lanes
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
| 3. Synthesize | Top-level local DAG with `developer`, `team_planner`, and optional `validator` nodes. |

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
| Clustering | Group many failures by owner family, mechanism, API, dtype, engine, or format. |
| Benchmark evidence | Keep tests and ids as verification evidence, not owner proof. |
| Boundary probe | Use at most one targeted CI structure/symbol query when it changes scout shape. |

Planner exploration stops at routing; use scouts for owner maps and preserve uncertainty instead of proving leaves.

## 2. Scout

Use this stage for route-changing exploration: superficial directory/multi-file maps for broad clusters, deep checks for single files or tight call chains.

```text
Caption: scout fan-out supports the next sibling wave.

row: parquet family -> scout(["pkg/io/parquet"]) -> read_file_note(["pkg/io/parquet"])
row: CLI family     -> scout(["pkg/cli"])        -> read_file_note(["pkg/cli"])
row: config seam    -> scout(["pkg/config", "pkg/options"])
```

| Scout shape | Use when |
| --- | --- |
| Single path | Deep scout when one file or module is the likely owner. |
| Multi-path | Deep scout when paths form one tight dependency, entrypoint, adapter, or shared mechanism. |
| Directory | Superficial scout when owner is a package/subsystem and exact files are unknown. |
| Wave size | Cluster by mechanism; avoid one-per-test and all-purpose scouts. |
| No scout | Leaf-only detail; preserve uncertainty in expandable task specs. |

Keep `target_paths` production-only: one directory or short file list. Put tests, benchmark ids, optional-dependency signals, and hypotheses in scout context; put commands/repro steps in developer or validator specs. Launch before polling; missing notes become uncertainty for that path only.

## 3. Synthesize

Enter after the ledger is complete and scouts are done or intentionally skipped.

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
