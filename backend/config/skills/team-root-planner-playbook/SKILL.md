---
name: team-root-planner-playbook
description: Playbook for the root_planner agent. Analyze, cluster, scout owner rows, synthesize, then submit a schema-valid root DAG with submit_plan(...).
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

## Overall Stage Flow

```text
Caption: root planner stage machine. Stages run in order; each has its own entry gate, exit gate, and (optional) reference.

  +-----------+    +-----------+    +-----------+    +-------------+    +---------------+
  |  analyze  | -> |  cluster  | -> |   scout   | -> |  synthesize | -> |  submit_plan  |
  +-----------+    +-----------+    +-----------+    +-------------+    +---------------+
        |                |                |                 |                   |
   request facts    owner ledger     scout notes       draft lanes        submit_plan(...)
        |                |                |                 |                   |
    exit: evidence   exit: one        exit: wave       exit: ref loaded   exit: one tool
    vs production    owner family     returned or      + lanes pass       call, no prose
    split            per row          skip justified   checklist
```

| # | Stage | Input | Exit gate | Reference |
| --- | --- | --- | --- | --- |
| 1 | analyze | User request | Request facts split from test/benchmark evidence and production clues. | none |
| 2 | cluster | Analyze output | Every owner row carries one owner family + changelog axes. | none |
| 3 | scout | Cluster ledger | Notes harvested for every scouted production path, or the row is marked unresolved. | none |
| 4 | synthesize | Scout findings | Reference loaded; no new scouts or note reads; lanes drafted and checklist passes. | `load_skill_reference(skill_name="team-root-planner-playbook", reference_name="synthesize-and-submit")` — load before drafting any lane. |
| 5 | submit_plan | Drafted lanes | Exactly one `submit_plan({ "new_tasks": [...] })` call, no later tool calls or prose. | none |

## 1. Analyze

Enter from the user request. Do not cluster, scout, or load references yet.

```text
Caption: split the request into evidence and production clues.

request
  |-- commands / benchmark ids / failing tests -> evidence (spec only)
  |-- exact production file or symbol ---------> production clue (exact)
  |-- broad family / matrix / migration -------> production clue (broad)
  `-- guessed or test-derived owner -----------> production clue (guess)
```

| Check | Action |
| --- | --- |
| Test/benchmark ids | Keep in evidence; never copy into `scope_paths` or scout targets. |
| Production clues | Tag each as exact file/symbol, broad family, or guess. |
| Forbid | Never inspect, scout, or assign test paths. |

**Exit:** request facts are split from test/benchmark evidence and production clues.

## 2. Cluster

Enter after Analyze. Build the owner ledger; do not scout yet.

```text
Caption: every owner row is a scout target until a scout returns. Pre-scout, all production claims are guesses from test names.

production clues
  |-- proven exact file or symbol ----> atomic owner row
  |-- mechanism / engine / format ----> mechanism owner row
  |-- package / subsystem ------------> directory owner row
  `-- guess / test-derived -----------> unresolved owner row
```

| Check | Root-planner action |
| --- | --- |
| Clustering axes | Make one row per owner family, then tag changelog axes (owner, mechanism, API, engine, format). F2P/P2P ids cannot join rows. |
| Cluster name | One row = one owner family. Slash/plus names like "CLI/Config/Compat" or "Storage I/O" signal unrelated owners; split now. |
| Benchmark evidence | Exact means explicit production path/symbol from user/notes or `ci_workspace_structure` on the parent dir. Before scouting or scoping a test-derived filename, verify it; if absent or replaced by a package directory, use the directory row. |

Routing stops at owner rows; HDF, JSON, parquet, groupby, utils, CLI, config, and compatibility are separate rows unless live evidence proves one tight producer-consumer pair. If several appear in one row, split it.

**Exit:** every owner row has a single owner family and recorded changelog axes.

## 3. Scout

Enter after Cluster. One scout per row; unrelated rows go in one parallel wave.

```text
Caption: scout mode is proportional to certainty: trivial exact files get depth; bundles/directories get relationship maps.

owner ledger
  |-- proven exact file/symbol ---> deep single-path scout
  |-- package/engine row ---------> superficial directory scout
  |-- tight same-owner bundle ----> superficial relationship scout
  |-- unrelated rows -------------> separate scouts in one wave
  `-- still broad after map ------> team_planner handoff
```

| Scout shape | Use when |
| --- | --- |
| Trivial deep | One proven exact file/symbol; ask for line-level functions, likely edit seam, and concrete gaps. |
| Bundled superficial | Several paths in one owner family or one tight pair; same parent directory or call chain alone is not enough. Ask only for relationship map and handoff seams. |
| Directory superficial | Package, subsystem, engine matrix, or package-like import path; map files and relationships without deep leaf RCA. |
| Row wave | Independent families; issue one `run_subagent` per row in one wave. Never batch `cli.py`+`config.py`+`compat.py`, HDF+JSON/parquet, groupby+utils, or HDF+parquet+groupby. |

Dispatch each scout with `run_subagent(agent_name="scout", prompt="<scout prompt>")`; `prompt` is the only channel. State the scout mode in `## Task`. Missing/disproved exact targets become directory scouts in Stage 3 or unresolved handoff. Rewrite every scout prompt as production-only; test paths, benchmark filenames, and F2P/P2P ids stay out.

### Scout Prompt Format

```text
## Task
Mode: <trivial_deep | bundled_superficial | directory_superficial>. <one production routing question>

## Exploration Path
<production path 1>
<production path 2>

## Terminal Contract
submit_file_note(paths=[<exploration_paths>], content="<finding>")
```

| Section | Contains |
| --- | --- |
| `## Task` | One production routing question; no test path, F2P id, or benchmark file name. |
| `## Exploration Path` | Repo-relative production paths only — no test paths, no globs, no parent-dir batching. |
| `## Terminal Contract` | Literal `submit_file_note(paths=[...], content="...")` call template. Every path in `## Exploration Path` must appear in the `paths` argument of at least one submitted note. |

**Exit:** the scout wave returns, or every broad row has a documented reason for skipping scout.

## 4. Synthesize

Enter after the scout wave returns and notes are read. Do not backtrack to scout after loading the reference.

**Required first action this stage — before drafting any lane:**

```text
load_skill_reference(skill_name="team-root-planner-playbook", reference_name="synthesize-and-submit")
```

Synthesize from the exploration-note ledger, not the Stage-2 cluster ledger. Missing notes or guessed root causes stay unresolved and route to `team_planner`, not `developer`.

```text
Caption: root routing during synthesis.

note proves exact owner + edit seam -> developer
note maps relationship / unresolved gap -> team_planner
notes reveal shared dispatch file ----> collapse or chain deps
same-payload evidence ---------------> validator
```

| Draft check | Expected result |
| --- | --- |
| Coverage | Every note-backed owner or unresolved gap has a lane; Stage-2 clusters are not lane templates. |
| Developer lanes | Exactly one production owner file (or one tight coupled pair within one mechanism); ≥2 unrelated owner files in `scope_paths` (e.g. `cli.py`+`config.py`+`compat.py`, HDF+parquet+groupby) force a `team_planner` lane instead — a CLI→config→compat call chain is not "one mechanism". |
| Planner lanes | Preserve uncertainty and evidence without leaf-level overexploration. |
| Validators | Required when any producer lane writes a same-payload suite; depend on every such producer; `scope_paths` are production surfaces. |
| Payload | `id`, `agent`, `spec`, `deps`, and `scope_paths` only. |

**Exit:** the reference is loaded and every lane passes the draft checklist.

## 5. submit_plan

Enter after the draft checklist passes. Make `submit_plan({ "new_tasks": [...] })` the final assistant action.

```text
Caption: terminal contract.

draft lanes
  -> submit_plan({ "new_tasks": [...] })
  -> end (no further tool calls, no trailing prose)
```

| Submit check | Expected result |
| --- | --- |
| Tool count | Exactly one `submit_plan(...)` call this turn. |
| Trailing prose | None — `submit_plan` is the final assistant action. |
| Schema | Each task has `id`, `agent`, `spec`, `deps`, and `scope_paths` only. |
| Tests in scope | None — tests stay in `spec`, never in `scope_paths`. |

**Exit:** one `submit_plan` tool call emitted; no summary, output, parent ids, trailing prose, or later tool calls.
