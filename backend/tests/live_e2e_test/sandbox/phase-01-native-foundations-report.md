# Phase 1 Native Foundations Report

Date: 2026-05-05

Plan: `backend/tests/live_e2e_test/sandbox/IMPLEMENTATION_PLAN.md`, Phase 1.

Image:

```bash
EPHEMERALOS_SANDBOX_DEFAULT_IMAGE=registry:6000/daytona/sweevo-psf-requests-3738:v1
```

Verification:

```bash
EPHEMERALOS_SANDBOX_DEFAULT_IMAGE=registry:6000/daytona/sweevo-psf-requests-3738:v1 \
  .venv/bin/pytest \
  backend/tests/live_e2e_test/sandbox/layer_stack \
  backend/tests/live_e2e_test/sandbox/occ \
  backend/tests/live_e2e_test/sandbox/overlay/native \
  -v -rs -s
```

Result: `23 passed, 1 warning in 55.65s`.

## Scope Landed

| Suite | Files | Test cases | Coverage |
|---|---:|---:|---|
| `layer_stack/` | 3 | 5 | manifest lifecycle, publisher, merged view, race variants |
| `occ/` | 9 | 13 | commit transaction, orchestrator, serial merger, routing, gitignore oracle, direct/gated routes, merge engine, overlay capture conversion |
| `overlay/native/` | 3 | 5 | snapshot overlay runner, upperdir capture, change capture, race variants |

Every native test emits `resource_before` and `resource_after`; the host helper asserts mount counts and fd counts remain stable. The final run reported stable `fd_open=16`, `mounts=13`, and `overlay_mounts=3` across all probes.

## Race And Load-Shaped Metrics

Phase 1 is not the Phase 4 load-profile gate, but it includes correctness under race plus one 10k-path direct-route stress probe. These are the meaningful timing results from the passing live run.

| Probe | Workload | p50 ms | p99 ms | max ms | Result |
|---|---:|---:|---:|---:|---|
| `layer_stack.manifest_lifecycle_under_race` | 8 concurrent appenders | 4.59 | 6.58 | 6.63 | 8 versions, depth 8, no torn entries |
| `layer_stack.publisher_under_race` | 8 publishers, same digest | 2.59 | 3.00 | 3.01 | 1 canonical ref, 7 idempotent returns |
| `occ.commit_transaction_under_race` | 4 stale-snapshot commits | 6.13 | 6.45 | 6.46 | 1 accepted, 3 `aborted_version` |
| `occ.direct_route_under_race` | 8 disjoint direct commits | 7.86 | 8.53 | 8.56 | 8 accepted, no lock blow-up |
| `occ.serial_merger_under_race` | 16 FIFO waiters | 23.20 | 43.91 | 44.30 | FIFO upheld, no starvation |
| `overlay.native.snapshot_overlay_runner_under_race` | 4 parallel runners | 304.05 | 305.15 | 305.17 | Per-run captured path isolated |

10k-path direct route:

| Metric | Value |
|---|---:|
| Paths | 10,000 |
| Accepted paths | 10,000 |
| End-to-end large changeset elapsed | 1171.64 ms |
| `occ.prepare.total_s` | 60.31 ms |
| `occ.commit.validate_groups_s` | 413.45 ms |
| `layer_stack.publish.write_changes_s` | 474.65 ms |
| `layer_stack.publish.digest_check_s` | 128.66 ms |
| `layer_stack.publish.total_s` | 604.39 ms |
| `occ.commit.total_s` | 1021.91 ms |
| RSS delta | about +21 MiB |

## Complex And Edge Case Handling

`layer_stack/`:

| Area | Covered case | Outcome |
|---|---|---|
| Manifest lifecycle | open/read missing manifest, write/read round trip, reload from a new manager | Passed |
| Restart survival | active manifest reloaded from disk after publishes | Passed |
| Corruption | malformed manifest JSON is detected | Passed |
| Snapshot lease view | leased manifest keeps old content while active manifest advances | Passed |
| Publisher atomicity | bad content hash aborts, leaves active manifest/staging unchanged | Passed |
| Publisher idempotency | same digest publish returns existing head | Passed |
| Publisher race | 8 same-digest publishers converge to one layer ref | Passed |
| Merged view | depth 100+ stack, whiteout hiding, opaque-dir hiding, materialization | Passed |

`occ/`:

| Area | Covered case | Outcome |
|---|---|---|
| Commit transaction | accepted tracked write publishes exactly once | Passed |
| Rollback/retry | stale tracked base hash aborts without publishing | Passed |
| Atomic request | accepted path is dropped when same atomic changeset contains reject | Passed |
| Orchestrator routing | gated, direct, `.git` drop, path escape reject | Passed |
| Orchestrator recovery | service restarts over existing layer-stack root and commits | Passed |
| Serial merger | FIFO order, cancellation before commit, no starvation under 16 waiters | Passed |
| Gitignore oracle | nested `.gitignore`, `!` re-include, case variants | Passed |
| Direct route | empty changeset and 10k-path changeset | Passed |
| Gated route | first-commit-wins, both-reject, partial-overlap strict drop | Passed |
| Merge engine | non-conflict edit, missing-anchor conflict, binary reject, CRLF edit | Passed |
| Overlay capture conversion | whiteouts, opaque dirs, symlink, rename-as-delete+write | Passed |

`overlay/native/`:

| Area | Covered case | Outcome |
|---|---|---|
| Snapshot runner | run command against leased snapshot; active content unchanged | Passed |
| Failure cleanup | invoker exception releases lease | Passed |
| Runner race | 4 parallel runners capture disjoint outputs, no cross-leak | Passed |
| Upperdir capture | binary, sparse, symlink, hardlink, long path, unicode | Passed |
| Change capture | whiteouts, opaque dirs, rename pair, deterministic order | Passed |
| Capture race | 4 writers same path dedupe to one final captured change | Passed |

## Implementation Notes

- Added `backend/tests/live_e2e_test/sandbox/_harness/native_cases.py` so every Phase 1 native probe uses the same render/execute/parse/resource-assert path.
- Added the Phase 1 files under `layer_stack/`, `occ/`, and `overlay/native/` without importing `sandbox.layer_stack`, `sandbox.occ`, or `sandbox.overlay` at host collection time.
- Added `sandbox.occ.merge` as a narrow facade over existing direct/gated merge implementations because the Phase 1 plan names that import path.
- Made the OCC changeset runtime importable in the live image's Python 3.9 runtime by replacing `StrEnum`, `kw_only=True`, and runtime `|` type-alias usage in the OCC path.
- Fixed `GitignoreOracle` handling for verbose negated patterns from `git check-ignore --verbose --non-matching`; negated `!` records now classify as not ignored.
- Added digest metadata for published layers so same-digest publishes are idempotent and same-digest race tests converge to one canonical ref.

## Residual Notes

This report covers Phase 1 race/stress metrics only. The sustained load profiles, JSONL load artifacts, and soak gates remain Phase 4 and Phase 5 work in `IMPLEMENTATION_PLAN.md`.
