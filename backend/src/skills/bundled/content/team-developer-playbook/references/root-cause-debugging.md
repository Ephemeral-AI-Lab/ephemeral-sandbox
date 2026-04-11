# Root Cause Debugging

Use this reference when the first reproduction still leaves the bug ambiguous, the traceback lands far from the likely source, or you catch yourself cycling through reads without a falsifiable hypothesis.

## Goal

Find the first failing boundary and form one testable root-cause hypothesis before editing code.

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

## Bounded evidence you may gather

- Read the owned production file and the immediate consumer or importer.
- Use `ci_query_symbols(...)` or `ci_query_references(...)` once to identify the next caller/callee boundary before writing custom runtime probes.
- Run one narrow import-smoke, assertion-smoke, or helper-level repro through `daytona_codeact`.
- Read one adjacent shared production file when the traceback first lands there.
- Compare one working sibling implementation in the same package when the pattern is unclear.

## What counts as the first failing boundary

- The first owned helper that receives wrong data.
- The first import or warning-filter path that crashes before the named test runs.
- The first warning or error producer that no longer matches the owned assertion.
- The first schema/config/public API layer where the live behavior departs from the expected contract.

The failing test file itself is usually symptom evidence, not the boundary.

## Hypothesis rules

- Keep exactly one active hypothesis at a time.
- Make it falsifiable.
- Tie it to concrete evidence from the current run.
- Prefer source-of-bad-data explanations over symptom-level rewrites.

## Multi-boundary systems

When behavior crosses layers such as test -> public API -> helper -> downstream library:

1. Confirm the input at the public API boundary.
2. Confirm the value or option passed into the next helper.
3. Confirm the first downstream call where behavior changes.
4. Fix the earliest owned boundary that can legitimately correct the bug.

Do not jump to the deepest stack frame if an earlier owned boundary already explains the failure, and do not keep tracing sibling paths once that boundary survives a proving repro.

## Stop signs

Stop and gather evidence instead of editing when:

- You are about to say "let me read a few more files" without a new question.
- You want to patch based on test names alone.
- You are reasoning from failure counts or cluster size instead of runtime evidence.
- You are about to change multiple files to "cover possibilities".
- You have re-read the same test or source file and still cannot state a hypothesis.
- The same boundary already survived one proving repro and you are still reading siblings instead of patching or replanning.

## Escalation rules

- After one failed hypothesis, return to the failing boundary and gather one new datum.
- After two failed hypotheses, check whether the boundary is wrong and whether one adjacent shared surface owns the bug.
- After three failed hypotheses or fixes, stop local thrashing and surface replanning evidence.

## Few-shot examples

- Example: pytest dies while parsing a warning filter because resolving `pkg.tests.warning_aliases.RemovedInXWarning` imports `pkg/__init__.py`, then `pkg/base.py`, then `pkg/compat.py`.
  The first failing boundary is that shared import chain or compatibility shim, not `setup.cfg`, the warning alias, or the owned assertion body.
  Confirm the production import path once, then fix the caller that still imports the deprecated private symbol or widen one step on that chain; do not patch warning filters, tests, or add a new quiet alias for the deprecated name first.
- Example: `pytest.warns(FutureWarning, match="deprecated_option")` should warn only on an explicit opt-in path, but the default path now warns or errors too.
  Check the deprecation guard or option normalization first.
  Do not chase downstream dtype conversion, parser, or backend code until the quiet default path is restored.
- Example: an aggregate result has the right values but the wrong MultiIndex shape or dtype.
  Treat the result-construction step as the first boundary and inspect the constructed index or levels directly. Use one symbol or reference query to map the aggregate builder before inventing custom probe scripts.
  Do not spelunk unrelated grouper internals until the aggregate assembly path is proven correct.
