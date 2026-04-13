# Root Cause Debugging

Use this reference when the first reproduction still leaves the bug ambiguous, the traceback lands far from the likely source, or you catch yourself cycling through reads without a falsifiable hypothesis.

## Required checkpoint before first edit

Before the first source edit, write down all three:

1. `Observed failure`: the exact failing command, node, import, warning, or assertion.
2. `First failing boundary`: the first production function, module, helper, import chain, or config surface where behavior diverges.
3. `Hypothesis`: one concrete statement of what is wrong and why the evidence points there.

If you cannot state all three after the first reproduction, gather one more bounded piece of evidence instead of patching.

## Debug loop

1. Reproduce exactly once on the owned verify surface.
2. Read the traceback or assertion carefully.
3. Identify the first failing boundary, not just the final test assertion.
4. Gather one bounded confirming datum.
5. State one hypothesis.
6. Make one minimal edit or one minimal proving check.
7. Re-verify on the same narrow surface.

## Dead-cycle breaker

If one scoped packet, one symbol/reference query, and one proving repro all land on the same boundary, stop exploring. The next action must be one of:

1. Make the smallest production edit at that boundary.
2. Repair or revert your own last experiment first if it broadened the red surface into a shared startup, import, or warning-filter crash.
3. Surface one concrete blocker tied to that boundary.
4. Replan because the boundary is shared or unowned.

## Stop signs

Stop and gather evidence instead of editing when:

- You are about to say "let me read a few more files" without a new question.
- You are about to open `git status`, `git log`, `git show`, `git diff`, `git stash`, `git checkout`, or `git restore`, reason from failure counts or cluster size, or treat payload prose, themed work-item text, or repo history as stronger evidence than the current red node.
- You are about to blame pytest config, warning aliases, caller-stack tricks, a supposedly missing alias/module, or a "wrong" test after your last edit moved startup imports through a warning-producing hook, lazy export, or new module.
- You are about to treat the verify target list as edit ownership because a verify file imports a missing private compat alias or module.
- You are about to add a root-only skip, xfail, or verify-file rewrite before you can name the owned loader or access gate.
- You have re-read the same test or source file and still cannot state a hypothesis.
- The same boundary already survived one proving repro and you are still reading siblings instead of patching or replanning.
- You are about to call a still-red owned verify failure "pre-existing" or plan to ignore, deselect, or xfail it instead of tracing the current boundary.
- You have already restated the same boundary in different words and still have not edited, shared a blocker, or replanned.

## Few-shot examples

- Example: `{"observed_failure":"pytest pkg/tests/test_hdf.py -x dies while parsing warning filters after from pkg._compatibility import FLAG","first_failing_boundary":"startup import chain pkg/base.py -> pkg.compatibility","hypothesis":"a new deprecation hook now fires during package import instead of only on explicit public access"}`
  The first failing boundary is the shared compat/export surface.
  Confirm the importer chain once, then switch startup callers like `pkg/base.py` to a quiet supported path such as `pkg._compat`, or widen one step on that chain.
  Deprecation hooks belong on explicit public access paths only; do not add `-W` or bypass pytest config, and do not rewrite the test import or add a module-level deprecation hook on the public wrapper while startup still uses it.
- Example: a chmod-based permission test runs as UID 0 and repeated probes still succeed.
  Treat the owned loader or access gate as the first boundary. Read that gate once; do not jump straight to a root-only skip, xfail, or verify-file rewrite.
- Example: the exact pytest target returns `ERROR: not found`, exit code 4, or `no tests ran`.
  Treat that as a wrong-target or stale-target control failure, not proof the owned surface is green. Re-collect the current target or replan from the latest healthy checkpoint.
