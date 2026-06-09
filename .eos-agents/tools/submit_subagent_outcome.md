---
intent: read_only
terminal: true
hooks: []
---
Terminate as a subagent with your read-only findings.

Inputs:
- `outcome`: concise findings with verifiable references.

Behavior:
- Returns `ParentedOutcome::Subagent { outcome }` to the caller.
