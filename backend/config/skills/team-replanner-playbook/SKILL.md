---
name: team-replanner-playbook
description: Playbook for the team_replanner agent. Load recovery context, classify failure mode, diagnose only concrete blockers, and submit a schema-valid corrective replan with submit_replan(...).
---

# Team Replanner Playbook

Produce the smallest corrective DAG justified by failed-task evidence and the failed task's original contract. Finish with exactly one `submit_replan(...)` call and make no later tool calls.

Replanner-created tasks use `developer` repairs, `validator` checks, or a `team_planner` redraft only when recovery is still broad. The replanner coordinates recovery; it does not patch code and does not create scout or replanner children.

<Forbid Rule>
Never plan test suite or test-file related tasks.
Never assign subagents to explore test suites or test files.
</Forbid Rule>

## Stage Flow

```text
Caption: replanner recovery path. References load only inside the action or submit stage.

[1 Load recovery context]
  -> failed evidence vs gaps + graph/sibling ledger
  -> [2 Classify]
       | scope_expansion
       | wrong_owner_or_role
       | unresolved_blocker + trivial_direct_replan
       ` unresolved_blocker + deep_diagnostics -> diagnostic scout wave
  -> [3 Act]
       | add-only -------------> load action-add-tasks
       | cancel-and-redraft ---> load action-cancel-and-redraft
  -> [4 Submit]
       load terminal-contract -> checklist -> submit_replan(...)
```

| Stage | Output |
| --- | --- |
| 1. Load recovery context | Failed-task evidence vs gaps, graph structure, sibling states. |
| 2. Classify failure | `Classification: <scope_expansion|wrong_owner_or_role|unresolved_blocker>` plus diagnostics decision when needed. |
| 3. Act | Corrective mapping, original-contract coverage, and add-only vs cancel-redraft decision. |
| 4. Submit | Schema-valid `submit_replan({ new_tasks, cancel_ids })`. |

## 1. Load Recovery Context

Use exact UUIDs from the replanning header.

| Context item | Action |
| --- | --- |
| Own, parent, failed, dependency tasks | Read with `read_task_details(task_id=...)`. |
| Graph topology | Call `read_task_graph()` only after required task reads return. |
| Relevant siblings | Read only siblings you may preserve, cancel, depend on, or avoid. |
| Failed evidence | Separate verified command/trace/fix-location facts from unresolved gaps. |

```text
Caption: evidence ledger. Recovery plans should not treat gaps as facts.

failed task
  |-- verified: command, exit, trace, mechanism, candidate fix
  |-- unresolved: owner, rule, value mapping, missing path
  |-- original contract: assigned goal, criteria, scope, uncompleted work
  |-- live siblings: useful work to preserve
  `-- stale siblings: running/pending/ready direct siblings that may be cancelled
```

## 2. Classify Failure

State exactly one classification line:

```text
Classification: <scope_expansion|wrong_owner_or_role|unresolved_blocker>
```

| Classification | Use when |
| --- | --- |
| `scope_expansion` | Repair belongs outside the failed task's assigned production scope. |
| `wrong_owner_or_role` | Another owner or role must handle the repair. |
| `unresolved_blocker` | A production trace gap remains, including same-scope fixes without enough evidence. |

For `unresolved_blocker`, add one line:

```text
Diagnostics decision: <trivial_direct_replan|deep_diagnostics>
```

Choose `trivial_direct_replan` only when task details, notes, and CI already name every production seam. Choose `deep_diagnostics` when owner, path, rule, value mapping, or production seam remains unresolved.

## Diagnostic Scouts

```text
Caption: each scout answers one missing recovery decision.

trace gap triplet
  -> scout(target_paths=["scoped production path"])
  -> read_file_note(file_paths=["scoped production path"])
  -> corrective mapping
```

| Scout shape | Use when |
| --- | --- |
| Single path | One file is the likely seam. |
| Multi-path | A small coupled call chain forms one triplet. |
| Directory | Exact files are unknown inside a package/subsystem. |
| Parallel wave | Independent trace gaps block different recovery lanes. |
| No scout | Existing notes already provide root-cause-grade evidence. |

Harvest notes for every assigned production path; missing notes create uncertainty for that path only.

## 3. Act

Enter after classification is written and diagnostics are complete or intentionally skipped.
Action references: add-only -> `action-add-tasks`; cancel-redraft -> `action-cancel-and-redraft`.

```text
Caption: cancellation boundary.

same parent:
  failed request_replan/origin -> preserve; never cancel
  this replanner     -> preserve
  terminal/validator -> preserve
  live useful sibling -> preserve
  stale running/pending/ready sibling -> may appear in cancel_ids
```

| Action | Use when |
| --- | --- |
| Add-only | Only new corrective work is needed, or the failed task itself is the only stale item. |
| Cancel and redraft | Direct siblings in `running`, `pending`, or `ready` are stale, duplicate, or depend on the failed assumption. |
| Preserve terminal work | Sibling is `done`, `failed`, `cancelled`, `request_replan`, outside the stale region, or a validator continuation. |
| Preserve live useful work | Objective remains valid after corrective work. |

| Coverage row | Action |
| --- | --- |
| Named failing variant | Map to a repair/diagnostic child or preserved live repair owner. |
| Validator-discovered child-owned suite or uncompleted criterion | Map to repair plus validator, or preserved live owner. |
| Blocker-only fix leaves contract uncovered | Add continuation validator. |
| Test/benchmark/pytest-config restore/edit, skip/xfail, doc-only, or contradictory value rule | Evidence only; add a production diagnostic or blocker task. |

## 4. Submit

Enter after the Stage 3 reference has shaped the corrective mapping.
Terminal reference: `terminal-contract`.

| Submit check | Expected result |
| --- | --- |
| Top-level keys | Only `new_tasks` and `cancel_ids`; use `cancel_ids: []` when add-only. |
| New tasks | Direct repair/check children or Planner handoff redraft; agents are `developer`, `validator`, or `team_planner`. |
| Specs | Structured `goal`, `detail`, and `acceptance_criteria`; unresolved blockers include diagnostics decision and planner redrafts include Planner handoff. |
| Dependencies | Local recovery ids or freshly proven schedulable existing ids only. |
| Cancellations | Only stale running/pending/ready direct siblings; never failed, replanner, terminal, descendant, or validator-continuation work. |

Emit exactly one `submit_replan({ new_tasks, cancel_ids })` call. Make no further tool calls or prose.
