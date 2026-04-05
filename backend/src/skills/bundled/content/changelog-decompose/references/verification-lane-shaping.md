# Verification Lane Shaping

Use this reference when a release-planning run includes explicit failing and
guardrail test evidence.

## Default Verification Order

1. Run targeted FAIL_TO_PASS regressions first.
2. Run PASS_TO_PASS guardrails second.
3. Run the broader project or suite-wide verification command last.

## Why

- Early targeted verification gives fast signal on the benchmark-critical
  behavior.
- Guardrails catch regressions before the expensive broad suite.
- The final broad suite remains important, but it should not be the first
  verification action when more focused evidence already exists.

## Writing The Task

- Mention the staged order explicitly in the verification task description.
- Cite the focus and guardrail file families when they are already present in
  the planning context.
- Keep implementation lanes production-owned; do not create a verification-only
  root frontier ahead of the main fixes.
