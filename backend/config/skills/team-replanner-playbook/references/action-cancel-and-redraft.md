# Action Reference: Cancel And Redraft

Use after classification and diagnostics show stale same-layer work must be replaced. Final schema lives in `terminal-contract`.

## Decision Flow

```text
Caption: cancel-and-redraft names only stale running/pending/ready direct siblings.

same parent
  |-- failed request_replan/origin -> preserve; never cancel
  |-- this replanner     -> preserve
  |-- terminal/validator -> preserve
  |-- live useful sibling -> preserve
  `-- stale running/pending/ready sibling -> cancel_ids
```

| Candidate | Action |
| --- | --- |
| Stale direct sibling in `running`, `pending`, or `ready` | Add its id to `cancel_ids`. |
| Failed task or any `request_replan` task | Preserve; switch to add-only if this is the only stale item. |
| This replanner | Preserve. |
| Done, failed, cancelled, nested descendant, dependent, or validator continuation | Preserve. |
| Replacement for uncancelled sibling scope | Drop or switch to add-only. |

## Build

| Check | Rule |
| --- | --- |
| Cancellation proof | Each id is running/pending/ready, same parent, and does not drop a continuation validator. |
| Replacement scope | Include cancelled sibling scope only when that sibling id is in `cancel_ids`. |
| Original-contract coverage | Every uncompleted goal, acceptance criterion, and scope item from the failed developer/validator contract maps to a new recovery child or an explicitly preserved live owner; blocker-only repair is insufficient. |
| Children | Add `developer` or `validator`, or `team_planner` only for Planner handoff broad redraft. |
| Dependencies | Prefer local deps; existing deps need fresh schedulable graph proof. |
| No stale sibling left | Switch to `action-add-tasks` and submit `cancel_ids=[]`. |

Load `terminal-contract`, self-check, then submit exactly one `submit_replan(...)`.
