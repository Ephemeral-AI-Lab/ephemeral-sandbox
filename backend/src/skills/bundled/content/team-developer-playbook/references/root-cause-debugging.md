# Root Cause Debugging

Use this reference when the first reproduction still leaves the bug ambiguous, the traceback lands far from the likely source, or you catch yourself cycling through reads without a falsifiable hypothesis.

## Task/Goal

- The first reproduction is still ambiguous, or you are rereading without a falsifiable hypothesis.

## Avoid

- Do not call the red verify target inverted or a "wrong" test while the owned loader or access gate is still red.
- Do not treat the verify target list as edit ownership, and never reach for a root-only skip, xfail, or verify-file rewrite instead of fixing the owned loader or access gate.

## Workflow

- Before the first source edit, write down `{"observed_failure":"exact failing command, node, import, warning, or assertion","first_boundary":"the first production function, module, helper, import chain, or config surface where behavior diverges","hypothesis":"one concrete statement of what is wrong and why the evidence points there"}`. If you cannot state all three after the first reproduction, gather one more bounded piece of evidence instead of patching.

- Reproduce once, identify the first failing boundary, gather one bounded confirming datum, state one hypothesis, make one minimal edit or proving check, then re-verify on the same narrow surface.
- If one scoped packet, one symbol/reference query, and one proving repro all land on the same boundary, stop exploring. The next action must be one minimal production edit, a repair or revert of your own last experiment, one concrete blocker, or replanning.

## Expected Outcome

- If the first failing boundary is a shared compat/export surface, prove both the public access path and any package-startup import path before editing. Deprecation hooks belong on explicit public access paths only; do not emit warnings at module import time, from module-level `__getattr__`, or from a wrapper still used during startup. Route startup callers like `pkg/base.py` to a quiet supported path such as `pkg._compat`, or request replanning when that path is outside scope.
- If a failing test names a missing private module, shim, re-export, or import bridge, do not create it from the test spelling alone. Confirm the package structure or adjacent production import chain shows that surface is real; otherwise submit the collection/import failure for replanning with the missing module and owner evidence.
