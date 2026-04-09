---
name: verification-replan
description: Skill for verifier agents to analyze test failures and report structured summaries via request_replan. A downstream replanner agent reads these summaries and creates fix tasks.
---

# Verification Replan

When assigned as a verifier for a coordination node, use this skill to analyze test results and — if failures are detected — report structured summaries for the replanner agent to act on.

Current runtime note:
- This repository does not currently expose a universal `request_replan` tool in every verifier run.
- If the tool is present in your runtime surface, use it.
- If it is absent, emit the same structured triage in your final FAIL summary so a downstream coordinator or human can replan manually.

You do NOT create fix tasks. Your job is to test, triage, and report. The replanner reads your report and decides what to do.

---

## Workflow

### 1. Run scoped tests

Run the FAIL_TO_PASS tests listed in your task description, then run relevant PASS_TO_PASS tests to check for regressions. Collect structured results:
- Test ID
- Pass/fail status
- Error message and traceback (for failures)
- Which source files the test exercises

### 2. If all tests pass

Complete successfully with a summary of test results. No replanning needed.

### 3. If tests fail — triage failures

Before reporting, analyze the failures:

1. **Cluster failures by root cause.** Multiple test failures often share a single root cause (e.g., a wrong return type, a missing import, an incorrect conditional). Group related failures together.

2. **Map each cluster to implementation area.** Use test file paths, import chains, and error messages to identify which source files and functions are responsible. Cross-reference with the `touches_paths` of completed sibling tasks to determine which task's work needs rework.

3. **Classify each cluster:**
   - **Implementation bug** — the sibling task's code has a logic error, missing edge case, or wrong API usage.
   - **Integration gap** — two sibling tasks' outputs don't connect properly (e.g., mismatched interface, wrong import path).
   - **Missing coverage** — the original plan didn't include a task for this area.

### 4. Call `request_replan()` when available

If the runtime exposes `request_replan()`, call it with a structured summary:

- **`reason`**: Brief summary (e.g., `"3/10 FAIL_TO_PASS tests failing: parser return type mismatch and missing validation"`)
- **`context`**: Full structured test output — failed test IDs, error messages, tracebacks, and your triage analysis (clusters, root causes, affected files).
- **`suggestion`**: Hints for the replanner — which files need rework, which completed tasks to build on, what the correct behavior should be.

If the runtime does not expose `request_replan()`, put the same structured content into your final FAIL summary under clearly labeled clusters so replanning can happen outside the verifier turn.

---

## Rules

- **Do NOT modify source files.** Your job is verification and reporting only.
- **Do NOT create tasks.** If `request_replan()` is available, the replanner handles task creation via `update_plan`. Otherwise your job stops at a structured FAIL summary.
- **Be specific in your report.** Vague reports like "tests failed" waste the replanner's time. Include test IDs, error messages, file paths, and root cause analysis.
- **Cluster by root cause.** Don't list every test failure independently — group them so the replanner creates one targeted fix per cluster.
- **Map failures to sibling tasks.** Tell the replanner which completed task's work needs rework and what files are involved.
- **Include regression context.** If PASS_TO_PASS tests regressed, note which ones so the replanner knows to preserve existing behavior.

---

## Context format

Structure your `context` argument like this:

```
FAIL_TO_PASS results: N/M failing

Cluster 1 (K tests): <root cause summary>
  - <test_id>: <error summary>
  - <test_id>: <error summary>
  Root: <file:line> — <what's wrong>
  Sibling task: <task_id that produced the buggy code>
  Fix: <what the correct behavior should be>

Cluster 2 (K tests): <root cause summary>
  ...

PASS_TO_PASS results: N/M passing
  Regressions (if any):
  - <test_id>: <error summary>
```

---

## Example when `request_replan()` is available

```
request_replan(
    reason="2 failure clusters: DateParser.parse returns wrong type, validate_email missing null check",
    context="""
FAIL_TO_PASS results: 4/7 failing

Cluster 1 (3 tests): DateParser.parse return type
  - test_parse_iso_format: AssertionError: expected datetime, got str
  - test_parse_relative: AssertionError: expected datetime, got str
  - test_parse_with_timezone: TypeError: strftime requires datetime, not str
  Root: src/parsers/date_parser.py:45 — parse() returns raw string instead of datetime
  Sibling task: implement-date-parser
  Fix: wrap return value with datetime.fromisoformat()

Cluster 2 (1 test): Email validation null handling
  - test_validate_none_email: AttributeError: 'NoneType' has no attribute 'strip'
  Root: src/validators/email.py:12 — validate_email() doesn't handle None input
  Sibling task: implement-validators
  Fix: add null guard returning ValidationError

PASS_TO_PASS results: 15/15 passing (no regressions)
    """,
    suggestion="Two independent fixes needed. fix-date-parser can depend on implement-date-parser. fix-email-validate can depend on implement-validators."
)
```

## What happens after

When `request_replan()` is available, the replanner agent reads your `replan_request` artifact and calls `update_plan` with:
- `add_tasks`: targeted fix tasks based on your triage
- `cancel_task_ids`: any pending tasks that are no longer relevant

When `request_replan()` is not available, the same triage block should appear in the verifier's final FAIL summary and a later coordinator or human turn must perform the replan explicitly.

The `update_plan` posthook automatically resets the verifier to PENDING with dependencies on the new fix tasks. Once the fix tasks complete, you (the verifier) run again automatically to re-verify. This cycle continues until all tests pass.
