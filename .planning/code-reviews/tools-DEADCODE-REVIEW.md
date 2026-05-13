---
phase: ad-hoc-tools-deadcode-review
reviewed: 2026-05-13
depth: deep
focus: dead_code_and_legacy
files_scanned: 58
findings:
  unused_exports: 17
  dead_symbols: 1
  orphan_files: 0
  dead_imports: 0
  legacy_patterns: 1
  total: 19
status: issues_found
---

# tools/ — Second-Pass Dead Code & Legacy Review

This pass scopes strictly to dead code and legacy leftovers (the first pass at
`tools-REVIEW.md` covered correctness/security). Findings below were validated
by ripgrep across `backend/src/` and `backend/tests/` after filtering the
definition site and the immediate re-export entries.

## Summary

The post-refactor (`eb02f72e`) `tools/` tree is generally well-trimmed — no
orphan files, no commented-out code blocks, no TODO/FIXME debt. The dead weight
is concentrated in three places:

1. **`tools/__init__.py` lazy-export surface** carries five symbols that no
   external module ever pulls (`create_tools`, `register_tool_instance`,
   `ToolPostHook`, `ToolPreHook`, `HookStatus`). Internal call sites use the
   private modules directly.
2. **`Submit*Input` / `Ask*Input` pydantic re-exports** in each submission /
   ask-helper `__init__.py`. Every one of these classes is referenced only by
   the `input_model=` arg of the tool that defines them, in the *same file*.
   The `__init__.py` re-exports are pure surface noise.
3. **One unused module-level constant** (`BackgroundMode` typealias) declared
   in `tools/_framework/core/base.py`'s `__all__` but with no callers
   anywhere — even within `tools/`, the `Literal` annotation is inlined into
   the `@tool` decorator and `decorator.py`.

Two soft observations that I am **not** recommending deletion for:

- `tools._framework.execution/__init__.py` and
  `tools._framework.introspection/__init__.py` re-export sets are dead in
  the sense that every external caller bypasses them and reaches the inner
  module directly. They are not zero-cost to keep but removing them would
  break the `__init__.py`-as-package-API discoverability convention; flag for
  consideration, not for deletion.
- `tools/submission/explorer/__init__.py` is empty (already noted in the
  first-pass review). The other empty packages
  (`tools/_framework/__init__.py`, `tools/sandbox/_lib/__init__.py`,
  `tools/background/_lib/__init__.py`, `tools/ask_helper/_lib/__init__.py`)
  are all zero-byte too — the inconsistency is mild.

---

## Unused Exports

### DC-01 — `create_tools` has no callers
**File:** `backend/src/tools/_framework/factory.py:69`
**Confidence:** HIGH
**Type:** unused_export
**Evidence:**
- `rg -n "\bcreate_tools\b" backend/src backend/tests` → only the definition,
  the `_LAZY_EXPORTS` entry in `tools/__init__.py:36`, and the `__all__` entry
  at `tools/__init__.py:102`. No call site anywhere.
- Sibling `create_tool` (singular) is called from 9 files; the plural version
  appears to be unused convenience scaffolding.
**Recommendation:** remove
**Why:** The function never executes. Drop the definition and its two entries
in `tools/__init__.py`.

### DC-02 — `register_tool_instance` has no external callers
**File:** `backend/src/tools/_framework/factory.py:47`
**Confidence:** HIGH (downgraded to MEDIUM if you want to keep the symmetric
factory API)
**Type:** unused_export
**Evidence:**
- `rg -n "\bregister_tool_instance\b" backend/src backend/tests` →
  definition site + 1 internal call from `_register_many` (`factory.py:96`)
  + the `_LAZY_EXPORTS` entry + the `__all__` entry. No other references.
- The public counterpart `register_tool_factory` is used by 3 files; this
  variant is used only inside `factory.py`.
**Recommendation:** investigate
**Why:** Either drop the `tools/__init__.py` re-export entry (the function
itself remains, just not as a top-level export), or remove the function and
inline its body into `_register_many`. The lazy-export advertises an API that
nobody consumes.

### DC-03 — `HookStatus` is exported but never imported externally
**File:** `backend/src/tools/_framework/core/hooks.py:14`
**Confidence:** HIGH
**Type:** unused_export
**Evidence:**
- `rg -n "\bHookStatus\b" backend/src backend/tests` (excluding the
  definition, `tools/__init__.py`, `tools/_framework/core/__init__.py`, and
  `hooks.py`) returns zero hits.
- The type is referenced only inside `hooks.py` (`HookResult.status`
  annotation).
**Recommendation:** remove
**Why:** Drop from `tools._framework.core.__all__` (line 20),
`tools/__init__.py` line 14 import and line 85 `__all__`. The annotation
inside `hooks.py` keeps working — only the re-export chain dies.

### DC-04 — `ToolPostHook` Protocol is exported but never imported externally
**File:** `backend/src/tools/_framework/core/hooks.py:79`
**Confidence:** HIGH
**Type:** unused_export
**Evidence:**
- `rg -n "\bToolPostHook\b" backend/src backend/tests` shows hits only inside
  `hooks.py`, `core/__init__.py`, and `tools/__init__.py`. No consumer module
  declares a type with this Protocol, no test references it.
**Recommendation:** remove
**Why:** Drop the export wiring. The Protocol can stay defined as
documentation of the shape, but pull it out of `__all__` so the public
surface area matches reality.

### DC-05 — `ToolPreHook` Protocol is exported but never imported externally
**File:** `backend/src/tools/_framework/core/hooks.py:66`
**Confidence:** HIGH
**Type:** unused_export
**Evidence:** Same pattern as DC-04. Zero external consumers.
**Recommendation:** remove
**Why:** Same as DC-04.

### DC-06 — `BackgroundMode` typealias is declared in `__all__` but never imported
**File:** `backend/src/tools/_framework/core/base.py:24`
**Confidence:** HIGH
**Type:** unused_export
**Evidence:**
- `rg -n "BackgroundMode" backend/src backend/tests` → only the `__all__`
  entry on line 15, the typealias on line 24, and the `background: BackgroundMode`
  annotation on line 39 of the same file. Nothing else imports the alias.
- Even the `@tool` decorator and `decorate_schemas_for_background` rebuild
  the `Literal` inline rather than importing this alias.
**Recommendation:** remove
**Why:** Drop the `__all__` entry. The typealias itself can stay (it makes
the `background:` annotation in `base.py` readable) but it should not pretend
to be a public symbol.

### DC-07 — `SubmitAdvisorFeedbackInput` re-export has no external consumer
**File:** `backend/src/tools/submission/advisor/__init__.py:4`
**Confidence:** HIGH
**Type:** unused_export
**Evidence:**
- `rg -n "SubmitAdvisorFeedbackInput" backend/src backend/tests` → only
  appears in `submit_advisor_feedback.py` (class def + `input_model=` arg)
  and the two `__init__.py` lines (line 4 import, line 9 `__all__`). No
  external reader.
**Recommendation:** remove
**Why:** Drop the class from the `__init__.py` import and `__all__`. Keep the
top-level `submit_advisor_feedback` tool export.

### DC-08 — `SubmitResolverResultInput` re-export has no external consumer
**File:** `backend/src/tools/submission/resolver/__init__.py:4`
**Confidence:** HIGH
**Type:** unused_export
**Evidence:** `rg -n "SubmitResolverResultInput"` only finds the definition
and the two re-export lines. No external consumer.
**Recommendation:** remove
**Why:** Same pattern as DC-07.

### DC-09 — `SubmitEvaluationFailureInput` / `SubmitEvaluationSuccessInput` re-exports unused
**File:** `backend/src/tools/submission/evaluator/__init__.py:3-9`
**Confidence:** HIGH
**Type:** unused_export
**Evidence:** `rg -n "SubmitEvaluationFailureInput|SubmitEvaluationSuccessInput"`
returns only the definition sites in
`submit_evaluation_failure.py` / `submit_evaluation_success.py`, plus the four
lines (two imports + two `__all__` entries) in `evaluator/__init__.py`.
**Recommendation:** remove
**Why:** Same pattern. The pydantic input classes serve as `input_model=`
locally — they don't need to be re-exported.

### DC-10 — `RequestMissionSolutionInput` / `SubmitExecutionFailureInput` / `SubmitExecutionSuccessInput` re-exports unused
**File:** `backend/src/tools/submission/executor/__init__.py:3-23`
**Confidence:** HIGH
**Type:** unused_export
**Evidence:** Same grep pattern returns zero external references for any of
the three Input classes; only the definition and `executor/__init__.py`
itself touch them. (External callers import the tool functions
`request_mission_solution`, `submit_execution_*` — those are real public
API and remain in the file.)
**Recommendation:** remove
**Why:** Strip the three Input class re-exports from
`executor/__init__.py` (lines 4-5, 8-9, 12-13, 17-19 of the `__all__`).

### DC-11 — `SubmitVerificationFailureInput` / `SubmitVerificationSuccessInput` re-exports unused
**File:** `backend/src/tools/submission/verifier/__init__.py:3-18`
**Confidence:** HIGH
**Type:** unused_export
**Evidence:** Same pattern, zero external references for the Input classes
themselves.
**Recommendation:** remove
**Why:** Same as DC-10.

### DC-12 — `SubmitFullPlanInput` / `SubmitPartialPlanInput` re-exports unused (`PlanTaskInput` kept)
**File:** `backend/src/tools/submission/planner/__init__.py:3-19`
**Confidence:** HIGH for `SubmitFullPlanInput`/`SubmitPartialPlanInput`,
KEEP for `PlanTaskInput`
**Type:** unused_export
**Evidence:**
- `rg -n "SubmitFullPlanInput|SubmitPartialPlanInput"` → only definition
  sites and the planner/`__init__.py` re-exports. No tests, no other modules.
- `PlanTaskInput` IS used by
  `backend/tests/unit_test/test_tools/test_submission_tool_registration.py:11`
  and is part of the public planner schema; keep it.
**Recommendation:** remove `SubmitFullPlanInput` and `SubmitPartialPlanInput`
from the import and `__all__`; keep `PlanTaskInput`.
**Why:** Two of the three re-exported Inputs have no consumer; trimming them
matches the existing pattern used by `submit_*` tool re-exports.

### DC-13 — `AskAdvisorInput` re-export has no external consumer
**File:** `backend/src/tools/ask_helper/__init__.py:11,22`
**Confidence:** HIGH
**Type:** unused_export
**Evidence:** `rg -n "AskAdvisorInput"` → only `ask_advisor.py` (def +
`input_model=` arg) and the `ask_helper/__init__.py` re-export lines.
**Recommendation:** remove
**Why:** Same pattern as the submission Inputs.

### DC-14 — `AskResolverInput` re-export has no external consumer
**File:** `backend/src/tools/ask_helper/__init__.py:12,23`
**Confidence:** HIGH
**Type:** unused_export
**Evidence:** `rg -n "AskResolverInput"` → only `ask_resolver.py` (def +
`input_model=` arg) and the `ask_helper/__init__.py` re-export lines.
**Recommendation:** remove
**Why:** Same as DC-13.

### DC-15 — `make_subagent_tools` is test-only
**File:** `backend/src/tools/subagent/_factory.py:90` (re-exported at
`tools/subagent/__init__.py:5,10` and `tools/__init__.py:53,115`)
**Confidence:** MEDIUM
**Type:** unused_export
**Evidence:**
- `rg -n "\bmake_subagent_tools\b" backend/src backend/tests` → only the
  definition and its internal use inside `make_subagent_tool_from_context`,
  plus a single test consumer
  (`backend/tests/unit_test/test_prompt/test_runtime_prompt.py:17,47`).
- No production caller. The production path is
  `make_subagent_tool_from_context`, registered as the `run_subagent`
  factory.
**Recommendation:** investigate
**Why:** If `make_subagent_tools` is intentionally retained for test
ergonomics, leave a comment to that effect. Otherwise inline its body inside
`make_subagent_tool_from_context` and switch the one test to call that. As-is
it's exported through three layers of `__all__` (`_factory` → `subagent` →
`tools`) for a single test caller.

### DC-16 — `tools._framework.execution/__init__.py` re-exports unused
**File:** `backend/src/tools/_framework/execution/__init__.py:3-12`
**Confidence:** MEDIUM
**Type:** unused_export
**Evidence:**
- All external consumers reach into the inner module by absolute path:
  `from tools._framework.execution.tool_call import execute_tool_call,
  execute_tool_call_streaming, execute_tool_once` (squad runner, every test).
- The top-level `tools/__init__.py` `_LAZY_EXPORTS` also bypasses the
  subpackage and points to `tools._framework.execution.tool_call`.
- The subpackage `__init__.py` is therefore re-importing names that nobody
  actually imports through it.
**Recommendation:** investigate
**Why:** Either keep as a stylistic package-API anchor, or remove the
re-export lines. Removing would shave 14 lines and one duplicate
`__all__`.

### DC-17 — `tools._framework.introspection/__init__.py` re-exports unused
**File:** `backend/src/tools/_framework/introspection/__init__.py:3-13`
**Confidence:** MEDIUM
**Type:** unused_export
**Evidence:**
- External callers either go through `tools/__init__.py` lazy export
  (`tools.collect_tool_catalog`, `tools.collect_schema_tools`,
  `tools.format_tool_schema_summary`) or directly to the inner module
  (e.g. `tests/unit_test/test_tools/test_schema_summary.py:10` imports from
  `tools._framework.introspection.schema_summary`).
- Nobody writes `from tools._framework.introspection import ...`.
**Recommendation:** investigate
**Why:** Same as DC-16. The package-`__init__` re-export adds no value.

## Dead Symbols (intra-file unused helpers)

### DC-18 — `_register_many` is called once
**File:** `backend/src/tools/_framework/factory.py:94`
**Confidence:** LOW
**Type:** dead_function (single-call helper, not strictly dead)
**Evidence:** `_register_many` is only called from `_register_builtins`
within the same file. Not actually dead — flagging because if `DC-02` is
applied (`register_tool_instance` reworked), this trampoline becomes
deletable too.
**Recommendation:** keep (revisit if DC-02 is taken)
**Why:** Coherent micro-helper today; only suspect in conjunction with
DC-02.

## Orphan files

None found. Every module under `tools/` has at least one caller.

## Dead imports

No unused `import` statements detected across the 58 source files. The
factory.py `_register_many` body uses every name in scope; ask_helper /
sandbox / submission / subagent / background modules all consume every
top-level import.

## Re-export chains

(Already covered by DC-07 through DC-17.)

The `tools._framework.core` → `tools/__init__.py` chain is two layers but
intentional (sub-package barrel + lazy top-level façade) and every symbol
on the chain is actually consumed. Not flagged.

## Legacy patterns

### DC-19 — Stale docstring reference to `agno` "progressive discovery"
**File:** `backend/src/tools/skills/_factory.py:8`
**Confidence:** MEDIUM (interpretation)
**Type:** legacy_pattern
**Evidence:** Module docstring says
`Follows Agno's progressive discovery pattern: ...`. `Agno` is an external
framework that's no longer referenced anywhere else in this repo
(`rg -n "agno|Agno" backend/src backend/tests --type py` returns only this
single docstring hit). The pattern described (skill summaries → on-demand
load) is implemented as-described, but the credit to "Agno" reads like a
holdover from an earlier design memo.
**Recommendation:** investigate
**Why:** Either drop the framework reference or update it to point to the
in-repo skill registry contract. Not deletion-eligible without product
context — flagging for the orchestrator to confirm.

## Legacy patterns NOT found

For the record, the following did NOT turn up:

- **TODO / FIXME / XXX / HACK / DEPRECATED markers** — zero hits across all
  85 files under `tools/`. Clean.
- **Commented-out code blocks** — `rg` for 3+ consecutive `#`-prefixed
  lines that look like code returned only legitimate comment blocks (the
  `_LAZY_EXPORTS` comment in `tools/__init__.py`, the
  `RestrictedRunSubagentTool` invariant comments in
  `subagent/_factory.py`, etc.). No leftover code-as-comment.
- **Compatibility shims for renamed APIs** — none. The `eb02f72e` refactor
  reorganized the package but did not leave backwards-compatible wrappers
  behind.
- **Transition stubs labelled "backward compat"** — `rg -n "backward
  compat|backwards compat|legacy"` returns nothing inside `tools/`.

The refactor looks like it cleared its own debris cleanly.

---

## Top-10 Deletion Shortlist (HIGH-confidence only)

For one-shot orchestrator approval, the following are safe to delete with
no functional impact and no test churn (assuming the affected `__all__`
entries are dropped together with the imports/definitions):

1. **DC-01** — `create_tools` definition + lazy export + `__all__` entry
2. **DC-03** — `HookStatus` exports from `tools/__init__.py` and
   `tools/_framework/core/__init__.py` (typealias may stay)
3. **DC-04** — `ToolPostHook` exports (Protocol may stay defined)
4. **DC-05** — `ToolPreHook` exports (Protocol may stay defined)
5. **DC-06** — `BackgroundMode` entry in `tools/_framework/core/base.py:__all__`
6. **DC-07** — `SubmitAdvisorFeedbackInput` re-export in advisor `__init__.py`
7. **DC-08** — `SubmitResolverResultInput` re-export in resolver `__init__.py`
8. **DC-09** — `SubmitEvaluation{Failure,Success}Input` re-exports in
   evaluator `__init__.py`
9. **DC-10** — `RequestMissionSolutionInput` + `SubmitExecution{Failure,Success}Input`
   re-exports in executor `__init__.py`
10. **DC-11** + **DC-12 (partial)** + **DC-13** + **DC-14** —
    `SubmitVerification{Failure,Success}Input`,
    `SubmitFullPlanInput` / `SubmitPartialPlanInput` (keep `PlanTaskInput`),
    `AskAdvisorInput`, `AskResolverInput` re-exports

Total estimated touched lines: ~40-50 import/`__all__` entries plus one
function body. Tests will continue to pass because none of the removed
symbols are referenced outside their defining modules.

---

_Reviewed: 2026-05-13_
_Reviewer: Claude (gsd-code-reviewer, dead-code pass)_
_Depth: deep (cross-module grep validation)_
