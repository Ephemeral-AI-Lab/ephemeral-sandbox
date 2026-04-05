---
name: sweevo-project-context
description: Stable SWE-EVO benchmark constraints and project context template for coordinator and worker agents operating on software evolution tasks.
---

# SWE-EVO Project Context

These are the stable constraints that apply to all SWE-EVO benchmark tasks. They are independent of the specific instance being evaluated.

---

## Benchmark Task Type

SWE-EVO benchmark task. Implement all changelog items from the release notes. Multiple agents work in parallel on different parts. OCC (optimistic concurrency control) handles concurrent edits.

---

## Key Constraints

- **Withheld test patches**: Benchmark test patches are withheld; do not expect FAIL_TO_PASS test bodies in the checkout. The test patch is applied only during evaluation, not during implementation.
- **Conda environment**: Activate conda before any Python commands. Use the activation command provided in the project metadata.
- **Parallel execution**: Multiple agents work on different parts of the changelog concurrently. The workspace uses OCC to merge concurrent edits.
- **No test body leakage**: FAIL_TO_PASS test *file paths* are provided as planning signals. Test *bodies* are never available during planning or execution.

---

## Planning Context Template

When building project context for a SWE-EVO instance, include:

1. **Repository metadata**: repo name, base commit, workspace path, version transition
2. **Instance identifier**: for traceability
3. **Changelog statistics**: bullet count, size classification
4. **Discovery budget**: root planning inspection turns budget
5. **Test command**: conda activation + test runner invocation
6. **Precomputed planning brief**: top-level structure, FAIL_TO_PASS focus files, PASS_TO_PASS guardrail files

Planning policy (decomposition strategy, lane shaping, frontier budgeting) lives in coordinator skills (`changelog-decompose`, `deep-codebase-planning`), NOT in the project context string.
