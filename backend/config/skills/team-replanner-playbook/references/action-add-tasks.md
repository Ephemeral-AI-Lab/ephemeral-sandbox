# Action Reference: Add Corrective Tasks

Use after classification and diagnostics produce corrective work with no stale sibling cancellation. Final schema lives in `terminal-contract`.

## Decision Flow

```text
Caption: add-only recovery keeps valid siblings and adds missing corrective children.

classification + root-cause trace
  -> valid live siblings stay running
  -> new developer repair / diagnostic tasks
  -> optional validator with deps on local producers
  -> cancel_ids=[]
```

| Candidate | Action |
| --- | --- |
| Production repair or bounded diagnostic with root-cause trace | Add `developer`. |
| Same-payload evidence sweep not already covered downstream | Add `validator`. |
| Same-scope continuation with no trace | Diagnose further before tasking. |
| Work already owned by uncancelled live sibling | Preserve that sibling. |
| Test, benchmark, skip/xfail, pytest-config, doc-only, or benchmark-harness change | Drop and assign production repair/diagnostic instead. |
| `team_planner`, replanner, or scout | Use `team_planner` only for broad redraft with `Planner handoff: scope_expansion` or `planner_redraft`; drop scout/replanner. |

## Build

| Check | Rule |
| --- | --- |
| Failure coverage | Every named variant maps to a repair/diagnostic task or preserved live owner. |
| Original-contract coverage | Every uncompleted goal, acceptance criterion, and scope item from the failed developer/validator contract maps to a new recovery child or an explicitly preserved live owner; blocker-only repair is insufficient. |
| Dependencies | Use local deps only for real output ordering; overlapping scopes alone are fine. |
| Scope | Production paths only; put tests in specs. |
| Moves/removals | Name `daytona_move_file` or `daytona_delete_file` when that is the production repair. |
| Value rules | If one proposed rule contradicts another observed expected/actual row, create a diagnostic developer. |

Load `terminal-contract`, self-check, then submit exactly one `submit_replan(...)`.
