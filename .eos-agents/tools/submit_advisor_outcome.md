---
intent: read_only
terminal: true
hooks: []
---
Terminate the advisor with a verdict and outcome.

Inputs:
- `verdict`: `"approve"` or `"reject"`.
- `outcome`: focused prose covering tool selection, payload support, and residual risk or required fix.

Behavior:
- Returns `ParentedOutcome::Advisor { verdict, outcome }` to the caller.
