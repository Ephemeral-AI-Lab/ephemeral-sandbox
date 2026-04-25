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
| `developer` | Atomic slice: exact owner + one mechanism + small failure surface. |
| `team_planner` | Broad, matrix, clustered, complex, or unresolved row. |
| `validator` | Same-payload verification after producer lanes. |

| Gate | Action |
| --- | --- |
| Owner questions change this DAG | Scout one production owner-family row or small independent wave. |
| Test-only evidence | Keep in request/spec context, not workspace or scout targets. |

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
| Clustering | Group by changelog axes (owner, mechanism, API, engine, format). F2P/P2P ids are acceptance criteria, not grouping axes. |
| Benchmark evidence | Keep tests and ids in evidence/spec, not workspace or scout targets. |

Planner exploration stops at routing; use scouts for owner maps and preserve uncertainty instead of proving leaves.

## 2. Scout

```text
Caption: scout fan-out by cluster shape — trivial rows go deep on targeted files, complex rows take a superficial directory map.

owner ledger
  |-- exact file/symbol row ------> one deep single/multi-path scout
  |-- package/engine row ---------> one superficial directory scout
  |-- unrelated rows -------------> separate scouts or handoff
  `-- still broad after map ------> team_planner handoff
```

| Scout shape | Use when |
| --- | --- |
| Single/multi-path | One owner or one coupled pair (engine+adapter, producer+consumer); same mechanism. |
| Directory | Package, subsystem, engine matrix, or package-like import path; keep superficial. |
| Row wave | Independent production families; separate scouts in one parallel wave, never one batched call. |
| Forbidden batch | ≥2 unrelated owners (e.g. `cli.py`+`config.py`+`compat.py`, HDF+parquet+groupby) → use Row wave. |

Use `input.target_paths` (not `prompt`); production paths only; missing/disproved → directory scout or handoff.

## 3. Synthesize

Enter after scout/no-scout closure. Load the Stage 3 reference; synthesize scout findings into the DAG — it need not mirror Stage 1 clustering.

```text
Caption: root routing during synthesis.

atomic + small surface -> developer
broad / matrix cluster -> team_planner sibling
same-payload evidence  -> validator with production scopes
```

| Draft check | Expected result |
| --- | --- |
| Coverage | Every named cluster has a producer owner or sibling `team_planner`; tiny slices stay separate. |
| Developer lanes | Exactly one production owner file (or one tight coupled pair within one mechanism); ≥2 unrelated owner files in `scope_paths` (e.g. `cli.py`+`config.py`+`compat.py`, HDF+parquet+groupby) force a `team_planner` lane instead — a CLI→config→compat call chain is not "one mechanism". |
| Planner lanes | Preserve uncertainty and evidence without leaf-level overexploration. |
| Validators | Required when any producer lane writes a same-payload suite; depend on every such producer; `scope_paths` are production surfaces. |
| Payload | `id`, `agent`, `spec`, `deps`, and `scope_paths` only. |

Run this checklist, then make `submit_plan({ "new_tasks": [...] })` the final assistant action: no summary, output, parent ids, trailing prose, or later tool calls.
