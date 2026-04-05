# Withheld Tests And Production-Owned Lanes

Use this reference when the benchmark says FAIL_TO_PASS test patches are withheld from the solver and applied only during evaluation.

---

## Core Rule

Hidden benchmark tests are symptom locators, not deliverables.

- Read existing neighboring tests only to understand the API, fixtures, and assertion style.
- If the exact FAIL_TO_PASS body is absent from the checkout, do not recreate it.
- Every implementation lane must still name at least one owned production file and one expected behavioral fix.

If a lane cannot name the production file it owns, it is probably not ready to exist as an implementation lane.

---

## Good Lane Descriptions

### Good

```text
Own the params falsy-value regression.

FAIL_TO_PASS:
- tests/unit/dependency/test_params.py::test_params_with_false_values

Production surface:
- dvc/dependency/param.py -> ParamsDependency.fill_values()

Behavioral delta:
- preserve falsy-but-present values instead of dropping them from info/state
```

### Good

```text
Own the ignore parent-path behavior.

FAIL_TO_PASS:
- tests/func/test_ignore.py::test_ignore_file_in_parent_path[...]

Production surface:
- dvc/ignore.py -> the pattern-matching logic that resolves parent-directory excludes

Behavioral delta:
- honor gitignore-style parent-directory exclusion rules without inventing new benchmark tests
```

---

## Bad Lane Descriptions

### Bad

```text
Write the missing FAIL_TO_PASS tests for ignore parent paths.
```

Why it is wrong:

- the benchmark already supplies those tests during evaluation
- the lane owns only tests, not the behavior under test
- workers will waste time writing code that is overwritten by the evaluator

### Bad

```text
Check whether CLI help is already implemented.
```

Why it is wrong:

- it is a speculative verification chore, not an implementation deliverable
- it consumes a root worker slot without directly advancing benchmark-critical behavior

---

## Interaction With Root Frontier Budgeting

When withheld tests exist:

- keep the first frontier on the proven production root causes
- move uncertain CLI, fixture, or release-note follow-ups behind those fixes
- if several secondary bullets remain, group them into one downstream expandable macro instead of one root lane per bullet

This avoids the common failure mode where a hidden-test benchmark spends half of its worker budget on speculative chores while the real behavior fixes are still running.

---

## DVC 1.1.7 -> 1.1.8 Example

The dangerous misread for this instance is:

- seeing `Add more tests according to gitignore`
- noticing `tests/func/test_ignore.py` exists
- turning that into a tests-only lane that edits or recreates the hidden FAIL_TO_PASS tests

The correct interpretation is:

- the hidden tests point to a behavior gap in `dvc/ignore.py`
- read existing ignore tests only to understand the API and current semantics
- give the lane ownership of `dvc/ignore.py` and the parent-path behavior, not the withheld tests
