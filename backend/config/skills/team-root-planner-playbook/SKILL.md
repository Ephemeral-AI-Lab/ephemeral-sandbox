---
name: team-root-planner-playbook
description: Playbook for the root_planner agent. Analyze the user request, scout risk-bearing production ownership, then synthesize and submit a schema-valid root plan with submit_plan(...).
---

# Team Root Planner Playbook

Produce the root task DAG from the user request. Finish with exactly one `submit_plan(...)` call.

The root routes top-down:

- Exact, live-proven, single-owner work -> `developer`.
- Broad, clustered, matrix-shaped, or unresolved work -> child `team_planner`.
- Same-payload verification -> `validator`.

## Workflow Map

| Stage | Output |
| --- | --- |
| 1. Analyze | Owner ledger: `{ clear, scout_required, unresolved, evidence }`. |
| 2. Scout | Optional small scout wave, or explicit uncertainty handed to child planning. |
| 3. Synthesize and submit | `synthesize-and-submit` reference available for synthesis, payload checked, one `submit_plan(...)`. |

```text
Caption: root planner stage machine. References support the stage that uses them.

User request
  |
  v
[1 Analyze]
  | owner ledger complete?
  |-- no ------------------------------+
  |                                    |
  | unresolved or benchmark-risk owner
  | and scout would change routing?
  |-- yes --> [2 Scout]                |
  |             | join small scout wave|
  |             | read notes by path   |
  |             v                      |
  |-- no --> carry uncertainty --------+
  +----------- back to ledger ---------+
  |
  v
	[3 Synthesize]
	  synthesis guidance:
	    load_skill_reference(
	      skill_name="team-root-planner-playbook",
	      reference_name="synthesize-and-submit"
	    )
  then: draft -> checklist -> submit_plan(...)
```

## Workflow Details

### 1. Analyze

Build the owner ledger before routing. The root planner has no parent, deps, or Task Center graph context to load.

```text
Caption: split evidence from ownership before making lanes.

request text
  |-- failing tests / benchmark ids / commands ------> evidence
  |-- exact production path or symbol ----------------> clear
  |-- broad family, matrix, migration, compatibility -> scout_required
  `-- guessed or ambiguous production owner ----------> unresolved
```

- Classify the request as bugfix, refactor, feature, migration, benchmark, or mixed.
- Raise a clustering flag for many failing tests, several production families, or an engine/dtype/format/API matrix under one subsystem.
- For benchmark, fail-to-pass, migration, or compatibility work, mark each broad family as `scout_required` even when the first-pass owner label looks plausible.
- Use at most one targeted `ci_workspace_structure` or `ci_query_symbol` call before scouting if one live boundary check would materially improve the scout wave.
- Keep benchmark test paths as verification evidence; they are not owner proof.

Avoid implementation work here: no patching, validation, broad file reading, or test-edit ownership unless the user requested test repair.

### 2. Scout

	Use this stage only when a bounded scout wave will materially improve this layer's routing. The root does not have to fully explore unresolved or expandable work; it may preserve uncertainty and assign the slice to a child `team_planner`.

```text
	Caption: fan out by owner family when scouting is worth the routing cost.

owner ledger row A -> scout(target_paths=["pkg/io/parquet"]) -> read_file_note(file_paths=["pkg/io/parquet"])
owner ledger row B -> scout(target_paths=["pkg/cli"])        -> read_file_note(file_paths=["pkg/cli"])
owner ledger row C -> scout(target_paths=["pkg/config"])     -> read_file_note(file_paths=["pkg/config"])

A scout wave is usually 1-3 owner families. Avoid both one scout per failing test and one giant all-purpose scout.
Do not merge unrelated rows into one scout. Multi-path scout = same row only.
```

- Launch scouts only for high-value `scout_required` or unresolved production owner families with `run_subagent(agent_name="scout", input={"target_paths": ["<one or more scoped production paths for that one owner family>"], "context": "..."})`.
- Pick scout shape by dependency hypothesis; a reasonable guess is enough before launching.
- Route low-value uncertainty to a child `team_planner` or diagnostic lane instead of widening root exploration.
- Keep `target_paths` production-only. Put tests, `test_*.py`, benchmark harnesses, verification paths, missing test-derived files, skipped variants, optional-dependency errors, and verification commands in scout `context`.
- Fire the useful scout wave before polling. Use `check_background_progress(task_id="all")` and `wait_for_background_task(task_id="all")` until no scout is running.
- Track every launched scout's `target_paths`. After the wave joins, call `read_file_note(file_paths=[...])` with all assigned paths; `submit_file_notes(...)` stores one note per scoped path.
- If a delivered scout leaves one assigned path without a note, carry missing-note uncertainty for that path without discarding sibling path notes.

| Scout shape | Use when |
| --- | --- |
| Single path | One file or module is the likely owner. |
| Multi-path, one row | Paths are coupled by dependency, entrypoint, adapter, or shared mechanism. |
| Directory | Owner is a package/subsystem, or exact files are not knowable without mapping. |
| Separate scouts | Paths belong to different owner families. |
| No scout, route to `team_planner` | Boundary is broad enough that exploration becomes decomposition. |

### 3. Synthesize and submit

	Enter this stage after the ledger is complete and scouts are either done or explicitly skipped. Load the synthesis reference when it helps draft or check the plan:

```text
load_skill_reference(
  skill_name="team-root-planner-playbook",
  reference_name="synthesize-and-submit"
)
```

After loading the reference, normally continue with draft/check/submit. If a new owner gap appears, preserve it as uncertainty or make a bounded routing check before assigning it to a child `team_planner` or scoped diagnostic lane.

```text
Caption: lane routing during synthesis.

single live-proven owner + one coherent mechanism
  -> developer

broad, clustered, matrix-shaped, mixed, or unresolved owner
  -> team_planner

same-payload verification of all producers
  -> validator with deps=[every producer it verifies]
```

- Use the reference's clustering, lane selection, coverage/evidence, dependency DAG, and submission rules.
- Draft each task with `id`, `agent`, `deps`, `scope_paths`, and a structured `spec` containing non-empty `goal`, `detail`, and `acceptance_criteria`.
- Before submit, audit every `developer` task: it must be exact-owner work, and its own `goal` / `detail` must not describe the same slice as expandable.
- Every named failing cluster is owned by a repair/decomposition task or handed to a child `team_planner`; a terminal validator is never the owner of otherwise unassigned work.
- Run the reference's Final Checklist, then emit `submit_plan({ "new_tasks": [...] })` as the final assistant action: no summary, output, parent ids, trailing prose, or later tool calls.
