---
name: team-replanner-playbook
description: Playbook for the team_replanner agent. Load recovery context, classify failure mode, diagnose only concrete blockers, and submit a schema-valid corrective replan with submit_replan(...).
---

# Team Replanner Playbook

Produce the smallest corrective task DAG justified by the failed task evidence. Finish with exactly one `submit_replan(...)` call and make no later tool calls.

Replanner-created tasks are limited to `developer` repair lanes and `validator` verification lanes. Do not create `team_planner`, `root_planner`, `team_replanner`, `scout`, or other agent roles in `new_tasks`; the replanner owns recovery synthesis.

## Workflow Map

| Stage | Output |
| --- | --- |
| 1. Load recovery context | Failed-task evidence vs gaps, graph structure, sibling states. |
| 2. Classify Failure Mode | `Classification: <scope_expansion|wrong_owner_or_role|unresolved_blocker>` and, for unresolved blockers, one diagnostics decision. |
| 3. Act | Corrective mapping, cancel-vs-add decision, matching action reference loaded. |
| 4. Submit | `terminal-contract` loaded, payload self-checked, one `submit_replan({ new_tasks, cancel_ids })`. |

Reference boundary: action references belong only to Stage 3, and `terminal-contract` belongs only to Stage 4. Do not call `load_skill_reference(...)` while the next action is still to read recovery context, classify the failure, decide diagnostics, run scouts, wait for scouts, or harvest scout notes.

Decision flow:

```text
load context -> classify failure
  scope_expansion or wrong_owner_or_role -> Direct replan
  unresolved_blocker + trivial_direct_replan -> Direct replan
  unresolved_blocker + deep_diagnostics -> Diagnostics -> Direct replan
Direct replan -> load action-add-tasks or action-cancel-and-redraft
Submit -> load terminal-contract -> submit_replan(...)
```

Every branch must load the matching action reference and then `terminal-contract` before drafting the payload. Do not skip those loads because the failure seems obvious.

## Reference Map

Load references with `load_skill_reference(skill_name="team-replanner-playbook", reference_name="...")`.

- `action-add-tasks`: use when the final payload has `cancel_ids=[]`.
- `action-cancel-and-redraft`: use when a stale non-terminal direct sibling must be cancelled and replaced.
- `terminal-contract`: use immediately before drafting and submitting the payload.

Post-load rule: after `load_skill(...)`, load recovery context and classify before loading any reference. A `load_skill_reference(...)` call immediately after `load_skill(...)` is invalid unless a prior assistant action already completed Stage 1 context loading, Stage 2 classification, and any required diagnostics.

Reference load gates: action reference trigger -> classification is final, diagnostics are complete or explicitly skipped, and the add-only vs cancel-and-redraft decision is final; `terminal-contract` trigger -> the matching action reference has been loaded and the corrective payload is ready for self-check. Failure signal -> `load_skill_reference(...)` appears before context reads, before the classification line, immediately after `load_skill(...)`, before diagnostics/scout harvesting finishes, or loads `terminal-contract` before the matching action reference.

## Workflow Details

| Section | Contract |
| --- | --- |
| Context | Read only live Task Center evidence needed to classify and preserve/cancel work. |
| Classification | Convert evidence into one failure mode and, when needed, one diagnostics decision. |
| Action | Create repair/verification work only after the failure mode and evidence justify it. |
| Submission | Submit schema-valid corrective tasks; never submit an empty or no-op replan. |

#### Steps

### 1. Load recovery context

Use exact UUIDs from the replanning header.

1. Read own task, parent task, failed task, and each declared dependency with `read_task_details(task_id=...)`.
2. Wait for all required `read_task_details` results before calling `read_task_graph()`. Do not batch `read_task_graph()` with any required task-detail read.
3. Read sibling details only for siblings you may preserve, cancel, depend on, or avoid.
4. Extract verified failed-task evidence separately from unresolved gaps: final summary, failure reason, root-cause trace, failing command, exit code, snippet, trace path, production mechanism, and candidate fix location.

### 2. Classify Failure Mode

State exactly one line:

```text
Classification: <scope_expansion|wrong_owner_or_role|unresolved_blocker>
```

Use:

- `scope_expansion` only when evidence proves the repair belongs to a different live production path outside assigned scope. Budget exhaustion or unfinished same-scope implementation is not scope expansion.
- `wrong_owner_or_role` only when evidence proves a different owner or role must handle the repair.
- `unresolved_blocker` when a concrete blocker remains as a production trace gap. If the fix target remains under any failed-task `scope_paths` entry, classify it as `unresolved_blocker`.

For `unresolved_blocker`, also state one line:

```text
Diagnostics decision: trivial_direct_replan
```

or:

```text
Diagnostics decision: deep_diagnostics
```

Choose `trivial_direct_replan` only when file notes and CI already name every failing production seam. Choose `deep_diagnostics` when any seam is still unresolved.

Before choosing `trivial_direct_replan`, check it against every observed value in the same failing assertion. For merge/config/dispatch/state bugs, make a compact value table: input path/state, observed value, expected value, proposed rule. If the rule breaks any row or contradicts the failed summary, use diagnostics or create a diagnostic developer instead of copying the handoff into a repair task.

Never treat another function, line range, or checklist item in the same owner file as scope expansion. A failed task's "test design issue" label does not drop a named fail-to-pass variant.

### 3. Act

Enter this stage only after Stage 1 context is loaded, Stage 2 classification is written, and required diagnostics are complete or explicitly skipped. The action reference load is the stage transition; if the cancel-vs-add decision is not final, do not load it yet.

#### Direct replan

Use for `scope_expansion`, `wrong_owner_or_role`, and `unresolved_blocker` with `trivial_direct_replan`.

1. Preserve valid live siblings and downstream validators.
2. Drop test-edit, doc-only, benchmark-only, and value-table contradiction candidates.
3. Ensure every named failing variant maps to a repair/diagnostic task or an explicitly preserved live repair owner. Do not submit an empty or no-op replan.
4. Decide whether to add only or cancel-and-redraft.
5. Load `action-add-tasks` if `cancel_ids=[]`; load `action-cancel-and-redraft` if a stale direct sibling must be cancelled.

The failed/original request_replan task can appear as a same-parent sibling in `read_task_graph()`; it is never stale sibling work and must stay out of `cancel_ids`. If your draft `cancel_ids` contains the failed task id from the prompt, remove it before submission.

#### Diagnostics

Use for `unresolved_blocker` with `deep_diagnostics`.

1. Read existing file notes for suspected production paths; skip scouting when notes already contain root-cause-grade evidence.
2. Enumerate distinct trace-gap triplets in visible reasoning before any scout call: one failing test id or cluster, one suspected production path, one named symbol or seam.
3. Launch one scout per remaining triplet with `run_subagent(agent_name="scout", input={"target_paths": ["<one production path>"], "context": "Diagnostic for <triplet>; ..."})`. Keep failing tests in scout `context`, not `target_paths`.
4. Queue the scout wave before checking progress; then use `check_background_progress` / `wait_for_background_task`.
5. Harvest notes with `read_file_note(file_path=...)`.
6. Synthesize repair mapping yourself from confirmed, partial, and disproved findings. Do not delegate synthesis to a child planner.
7. Load the action reference matching the final cancel-vs-add decision.

### 4. Submit

Enter this stage only after the matching Stage 3 action reference has been loaded and the corrective mapping is ready to self-check.

1. Load `terminal-contract`.
2. Self-check against its checklist: top-level keys are only `new_tasks` and `cancel_ids`; `new_tasks` is non-empty; every `name` is `developer` or `validator`; every spec uses `1. Goal:`, `2. Task Details:`, `3. Acceptance Criteria:`; `cancel_ids` contains only stale non-terminal direct siblings; no `cancel_ids` entry equals the failed task id from the prompt.
3. Emit exactly one `submit_replan({ new_tasks, cancel_ids })` call. Make no further tool calls.
