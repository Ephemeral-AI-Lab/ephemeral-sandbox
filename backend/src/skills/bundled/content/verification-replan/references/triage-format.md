# Triage Format

Use this reference only when you need a manual FAIL summary or a multi-cluster replan report.

## Required structure

Must emit:

```text
REPLAN_REASON: <short reason>
FAIL_TO_PASS: N/M failing
ROOT_CAUSE_PACKET: {"observed_failure":"...","first_boundary":"...","hypothesis":"..."}

CLUSTER: <root cause summary>
- TEST: <exact test id>
  ERROR: <exact short error summary>
  OWNER: <likely owner surface>
  SIBLING_TASK: <task id or unknown>

PASS_TO_PASS: N/M passing
REGRESSIONS:
- <exact test id>: <exact short error summary>
```

## Rules

- Group failures by root cause.
- Keep test ids exact.
- Keep owner surfaces concrete.
- Mark an unknown sibling task as `unknown` instead of guessing.
- Keep collection or import crashes visible instead of replacing them with narrower substitute tests.
- Never emit a vague summary such as "tests failed".

## Few-shot examples

- Example: three failing tests all point to the same serializer output shape.
  Emit one `CLUSTER:` block for that serializer bug, not three independent clusters.
- Example: one fail-to-pass target and twenty pass-to-pass tests all die during collection on the same missing import.
  Emit one systemic-runtime cluster for the collection crash and keep the regressions explicit in `PASS_TO_PASS`.
