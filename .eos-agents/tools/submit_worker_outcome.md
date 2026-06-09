---
intent: read_only
terminal: true
hooks: [no_background_sessions, advisor_approval]
---
Terminate the worker run with SUCCESS or FAILED for the assigned work item.

Inputs:
- `status`: `"success"` when the assigned work item is complete, or `"failed"` when it cannot be completed.
- `outcome`: 1-3 sentence factual result. Include changed artifacts and verification evidence for success, or the concrete blocker for failure.

Behavior:
- Records `TaskOutcome::Worker { is_pass, outcome }`.
- Updates the worker task status and lets the attempt schedule newly ready work items or close.
