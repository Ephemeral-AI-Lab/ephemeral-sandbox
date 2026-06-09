---
intent: read_only
terminal: true
hooks: [no_background_sessions, advisor_approval]
---
Terminate the root request with SUCCESS or FAILED.

Inputs:
- `status`: `"success"` when the user request is complete and verified, or `"failed"` when it cannot be completed.
- `outcome`: the user-facing request result or concrete blocker.

Behavior:
- Records `TaskOutcome::Root { is_pass, outcome }`.
