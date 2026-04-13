---
name: verification-replan
description: Failure-triage contract for verifier agents that need to request retry or replan.
---

# Verification Replan

Use this skill only after verification fails. Triage failures for retry or replan. Never create fix tasks yourself.

## Conditional references

- Load `triage-format` when you need to produce a manual FAIL summary because `request_retry()` or `request_replan()` is absent.
- Load `triage-format` when multiple failing clusters need to be grouped into one structured report.

## Workflow

1. Cluster failing tests by root cause.
2. Map each cluster to the likely owner surface and, when available, the sibling task that touched it.
3. Preserve one root-cause packet per cluster with `observed_failure`, `first_boundary`, and `hypothesis`.
4. Classify each cluster as `implementation_bug`, `integration_gap`, `missing_coverage`, `systemic_runtime`, or `transient_runtime`.
5. Keep pass-to-pass regressions explicit even when fail-to-pass targets are still red.

## Action rules

- If `request_retry()` is available and the failure is transient, use it first.
- If `request_replan()` is available and the failure is not transient, use it.
- If the needed tool is absent, emit the same triage in the final FAIL summary.

## Required triage block

Must include:

- `REPLAN_REASON: ...`
- `FAIL_TO_PASS: N/M failing`
- `ROOT_CAUSE_PACKET: {"observed_failure":"...","first_boundary":"...","hypothesis":"..."}`
- One `CLUSTER:` block per root cause with exact test ids, exact error summaries, likely owner surface, and sibling task when known
- `PASS_TO_PASS:` results and regressions

## Hard rules

1. Stay read-only.
2. Stay specific.
3. Group by root cause.
4. Preserve regression context.
5. Never emit vague summaries such as "tests failed".
