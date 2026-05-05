# Gitignore Policy Edge-Case Experimental Report

## Summary

This experiment evaluated the new OCC-level gitignore policy edge-case suite for
the per-call snapshot layer-stack migration. The suite targets plan section E13:
tracked paths should use strict OCC-gated validation, gitignored paths should use
OCC-skipped last-writer-wins publication, and path classification should not leak
between the two policies.

The current implementation is sound for the core split:

- tracked stale shell writes abort with `aborted_version`
- gitignored stale writes and deletes publish through OCC-skipped last-writer-wins
- route decisions are fixed at prepare time and are not reclassified at commit
- the full OCC test package remains green

One plan/runtime mismatch remains intentionally marked as strict `xfail`:
mixed shell changesets with a tracked conflict currently drop accepted gitignored
outputs, while E13 expects the gitignored paths to publish anyway.

## Scope

New suite:

```text
backend/tests/test_sandbox/test_occ/test_gitignore_policy_edge_cases.py
```

Covered cases:

| Area | Expected Policy | Current Result |
|---|---|---|
| Gitignored same-path shell writes | OCC-skipped LWW | Pass |
| Gitignored shell delete from stale snapshot | OCC-skipped LWW | Pass |
| Tracked stale shell write | OCC reject with `aborted_version` | Pass |
| Mixed shell tracked conflict + gitignored output | Current atomic shell drop | Pass |
| Plan-expected mixed partial commit | Tracked path rejects, gitignored path publishes | Strict `xfail` |
| Gitignore path becomes tracked after prepare | Prepared `occ_skipped_merge` route remains `occ_skipped_merge` | Pass |
| Tracked path becomes gitignored after prepare | Prepared `occ_gated_merge` route remains `occ_gated_merge` | Pass |

The suite is deliberately OCC-level rather than shell-load-level. It exercises
`OccService`, `OccSerialMerger`, `OccCommitTransaction`, and `LayerStackManager`
directly, keeping the signal focused on policy semantics rather than overlay
process execution.

## Test Commands

Focused suite:

```bash
uv run pytest backend/tests/test_sandbox/test_occ/test_gitignore_policy_edge_cases.py -q
```

Result:

```text
6 passed, 1 xfailed in 0.19s
```

Full OCC package:

```bash
uv run pytest backend/tests/test_sandbox/test_occ -q
```

Result:

```text
44 passed, 1 xfailed in 0.27s
```

Lint:

```bash
uv run ruff check \
  backend/src/sandbox/occ/serial_merger.py \
  backend/tests/test_sandbox/test_occ/test_package_structure.py \
  backend/tests/test_sandbox/test_occ/test_gitignore_policy_edge_cases.py
```

Result:

```text
All checks passed!
```

## Plan Conformance

The plan's E13 pass bar says:

```text
zero leakage - no tracked path commits without CAS; no gitignored path is ever
rejected on CAS grounds. Mixed changesets correctly partial-commit by per-path
policy.
```

The first two parts are currently validated:

| Requirement | Evidence |
|---|---|
| Tracked path does not commit without CAS | stale `src/app.py` shell write returns `aborted_version` and leaves active bytes unchanged |
| Gitignored path is not rejected on CAS | stale `dist/out.js` writes both accept and final bytes are from the later writer |
| Gitignored delete is not rejected on CAS | stale `dist/cache.bin` delete accepts and removes the active file |
| Classification is prepare-time authority | prepared `occ_skipped_merge` route stays `occ_skipped_merge` after oracle changes; prepared `occ_gated_merge` route stays `occ_gated_merge` after oracle changes |

The mixed partial-commit requirement is not implemented yet. The current
runtime treats any shell changeset with a tracked validation failure as an
all-or-drop publish transaction for accepted paths. The suite captures both
facts:

- `test_current_mixed_shell_tracked_conflict_drops_gitignored_occ_skipped_output`
  asserts the live behavior: tracked file aborts, gitignored output is marked
  `dropped`, and nothing publishes.
- `test_plan_expected_mixed_shell_conflict_keeps_gitignored_occ_skipped_output` is
  strict `xfail` and encodes the E13 target behavior: tracked file aborts,
  gitignored output accepts and publishes.

## Correctness Assessment

The current route and merge model is sound for independent tracked and
gitignored paths. A stale tracked path cannot bypass CAS by becoming ignored
after prepare, and a gitignored `occ_skipped_merge` route cannot be forced back
into `occ_gated_merge` CAS by changing the oracle before commit.

The remaining question is semantic, not a detected data-corruption bug:
shell-captured changesets are currently atomic when any tracked path conflicts.
That behavior is conservative and matches the existing shell atomic conflict
test, but it disagrees with E13's per-path mixed partial-commit goal.

## Implementation Notes

While running the broader OCC package verification, two surrounding issues were
fixed:

- `OccSerialMerger` now preserves `atomic=True` when combining prepared
  changesets, so explicit atomic requests still suppress accepted paths on
  failure.
- The OCC package-structure allowlist now includes `serial_merger.py`, matching
  the current package layout.

These fixes are not part of the gitignore policy suite itself, but they were
necessary for the full OCC package test run to be green.

## Recommendation

Keep the strict `xfail` until the product decision is made:

1. If shell changesets should remain atomic on tracked conflict, update E13 to
   document current behavior and remove the plan-expected partial-commit test.
2. If E13 is authoritative, change `OccCommitTransaction` so accepted
   `occ_skipped_merge` path deltas can still publish when tracked shell groups fail, then flip the
   strict `xfail` into a normal passing test and update the existing shell
   atomic conflict expectation.

Do not leave both contracts active indefinitely. The current suite makes the
gap visible, but the runtime should have exactly one durable mixed-shell policy.
