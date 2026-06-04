---
intent: read_only
terminal: true
hooks: [no_background_sessions, advisor_approval]
---
Terminate the root request with SUCCESS or FAILED.

- `status`: "success" when the user request is complete and verified; "failed" when it cannot be completed.
- `outcome`: the user-facing request result (for success) or the concrete blocker (for failure). The outcome is returned to the user.
