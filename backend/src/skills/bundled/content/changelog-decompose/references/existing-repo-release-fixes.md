# Existing-Repo Release Fixes — Decomposition Reference

Use when a release changelog targets an established repository with a mix of bug fixes, behavior deltas, test updates, and fixture work.

---

## Planning Loop

1. Anchor on the exact FAIL_TO_PASS tests or release bullets that imply a failing behavior.
2. Trace each failure into the production symbol or fixture that actually controls the behavior.
3. Cluster work by shared root cause, not by changelog bullet wording.
4. Emit only the dependency edges that represent real producer/consumer relationships.

If two bullets both collapse into one production symbol or one tightly-coupled file cluster, they belong in one lane even if the changelog lists them separately.

---

## Root-Level Graph Shape

For small or medium release bumps, prefer:

```
[behavior-lane-a] ──┐
[behavior-lane-b] ──┼── [bridge-if-needed] ── [verification]
[fixture-or-test-support] ─┘
```

- `behavior-lane-*`: production-facing fixes with their coupled tests
- `fixture-or-test-support`: only if fixture or environment work is required for the real failing tests to run
- `bridge-if-needed`: only for cross-cutting renames, shared interface shifts, or final wiring
- `verification`: runs FAIL_TO_PASS and the relevant PASS_TO_PASS coverage

Do not create a lane just because a changelog bullet exists. A lane needs owned code or fixture surfaces and a concrete expected output.

---

## Atomic vs Expandable

### Keep a lane atomic when

- One worker can own the production change and its immediate tests end-to-end
- The lane is centered on one failure mode
- The lane mostly lives in one primary directory or one cohesive file cluster
- The expected edit is surgical even if it touches a few supporting files

### Make a lane expandable when

- It spans multiple independent production surfaces
- It mixes production code, broad test updates, and fixture/environment work
- It contains multiple independent FAIL_TO_PASS clusters
- It would otherwise turn into “implement all release work for module X”

### Anti-patterns

- One atomic task that bundles unrelated production fixes and fixture updates
- A “verification” implementation lane whose job is only “check whether this already works”
- A PASS_TO_PASS-only support lane that blocks real fixes without owning required code

---

## Dependency Rules

Add `depends_on` only when one lane must consume a concrete artifact from another:

- shared fixture or helper that must exist before the consumer can edit against it
- signature or rename changes that downstream lanes import
- final bridge or verification work

Do **not** add `depends_on` just because lanes might touch related tests or because they both mention the same subsystem family.

---

## Worker Description Template

Each lane description should include:

1. The exact changelog bullets it owns
2. The FAIL_TO_PASS or PASS_TO_PASS tests that justify the lane
3. The specific production or fixture files/symbols to inspect
4. The current bug/behavior and the required outcome

Example:

```text
Own the params falsy-value regression.

Changelog:
- params: fix skipping of params dvc.lock when it's a falsy value (#4185)

FAIL_TO_PASS:
- tests/unit/dependency/test_params.py::test_params_with_false_values

Production surface:
- dvc/dependency/param.py -> ParamsDependency.fill_values()

Current behavior:
- fill_values() skips falsy YAML values because it gates writes on `if value:`

Required outcome:
- preserve falsy-but-present values in lock/state handling without regressing missing-key behavior
```
