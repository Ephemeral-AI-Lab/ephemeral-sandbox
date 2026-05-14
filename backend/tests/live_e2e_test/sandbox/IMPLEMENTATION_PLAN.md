# Live E2E Suite — Implementation Plan

Companion to
`.omc/plans/per-call-snapshot-layer-stack-migration/live-e2e-test-suite-plan.md`.
This file lives next to the suites it produces. It is the executable order for
landing the sandbox-only live tests, gated by verification at each phase.

**Scope.** Six suites: `overlay/syscall/` (existing, complete),
`overlay/native/`, `layer_stack/`, `occ/`, `layer_stack_overlay_occ/`. Five
phases, ~30 new files, P0/P1 tagged.

**Image (required for every phase).**
```bash
EPHEMERALOS_SANDBOX_DEFAULT_IMAGE=registry:6000/daytona/sweevo-psf-requests-3738:v1
```

**Running.**
```bash
.venv/bin/pytest backend/tests/live_e2e_test/sandbox -v -rs
.venv/bin/pytest backend/tests/live_e2e_test/sandbox/<suite> -v -rs
```

---

## Risk-Ordering Rationale

| Choice | Why |
|---|---|
| `layer_stack/` lands before `occ/` | Corrupted manifests cascade. If `manifest.append` is broken, every OCC test fails for the wrong reason. |
| `overlay/native/` parallel to OCC | No upstream dependencies on layer_stack/occ — can land any time after Phase 0. |
| Integrated P0 before P1 | Cutover cares about wiring correctness more than load budgets. A broken shell snapshot view ships bugs; a loose load budget can tighten post-cutover. |
| Load last within each subsystem | Load amplifies latent bugs into a torrent of failures. Debug is faster on small probes. |
| Soak last | 15-minute run; nightly not per-PR. |

---

## Phase 0 — Harness (1 PR, sequential, blocks everything)

Nothing else lands until the harness smoke probe is green.

### Files

| Path | Purpose |
|---|---|
| `_harness/native_probe.py` | Wrap `cd /tmp/eos-sandbox-runtime && python3 -c "<src>"`. Inject JSON config via `__CFG_JSON__` placeholder. Concatenate `_PROBE_PRELUDE` (resource sampler + helpers) with body. Provide `wrap_unshare(script)` for namespace-direct probes. Mirror `overlay_probe.py` shape. |
| `_harness/resource_metrics.py` | Single Python source string `RESOURCE_PRELUDE`. Emits `sample_resource()` returning the §3.5 dict. Probes call `before = sample_resource()` / `after = sample_resource()` and include both in output. |
| `sandbox/_harness/sandbox_fixture.py` | Add `native_sandbox` fixture: resets `/testbed`, clears `/tmp/eos-sandbox-runtime/layer-stack-test-*/`, asserts `.bundle-hash` exists. Re-export from `conftest.py`. |
| `sandbox/overlay/__init__.py` (rename) | Move existing `overlay/` → `overlay/syscall/`. Create `overlay/native/__init__.py`, `layer_stack/__init__.py`, `occ/__init__.py`. |
| `sandbox/_harness/test_harness_smoke.py` | Phase-0 gate test (deleted at end of phase). Probe imports `sandbox.layer_stack`, `sandbox.occ`, `sandbox.overlay`; verifies `.bundle-hash`; emits and parses one resource block. |

### Verification gate
```bash
.venv/bin/pytest backend/tests/live_e2e_test/sandbox/_harness/test_harness_smoke.py -v
# expect: 1 passed
```

### Done definition
- Smoke probe passes against the Daytona image.
- `pytest --collect-only backend/tests/live_e2e_test/sandbox` shows zero collection errors.
- Existing `overlay/syscall/` tests still pass post-rename.

---

## Phase 1 — P0 Native Foundations (dependency-ordered)

**Concurrent-variant requirement.** Several P0 files claim invariants
(idempotency, atomicity, no-starvation) that are vacuous under single-threaded
input. Each affected file ships a `*_under_race` test case alongside its
single-shot sibling — N=2..16 concurrent producers, short duration (<5 s),
correctness-only assertions. This is *not* load testing; it is functional
correctness under race. Files needing this are flagged **(+ race)** below.

### 1a. `layer_stack/` storage primitives (sequential, blocks 1b)

| Order | File | Backs | Probe imports | Pass bar |
|---:|---|---|---|---|
| 1 | `layer_stack/test_manifest_lifecycle.py` **(+ race)** | E11 | `sandbox.layer_stack.manifest.model` | open/append/seal/list/load round-trip; survive simulated process restart; corrupted manifest detected. Race: N=8 concurrent appenders → no torn entries; total = N |
| 2 | `layer_stack/test_publisher.py` **(+ race)** | E6, E9 | `sandbox.layer_stack.layer.publisher` | publish atomicity; idempotent on same digest; kill mid-publish leaves no dangling refs. Race: N=8 publishers same digest → exactly one canonical ref, 7 "already published" |
| 3 | `layer_stack/test_merged_view.py` | E5 | `sandbox.layer_stack.view.merged` | depth-100 path-to-content map correct; whiteouts override; opaque dirs respected |

**Gate after 1a:** `pytest backend/tests/live_e2e_test/sandbox/layer_stack -k 'manifest or publisher or merged_view'` green.

### 1b. `occ/` core + `overlay/native/` capture (parallel after 1a)

`occ/` files (P0, all required for cutover):

| Order | File | Probe imports | Pass bar |
|---:|---|---|---|
| 4 | `occ/test_commit_transaction.py` **(+ race)** | `sandbox.occ.commit_transaction` | atomicity; rollback on failure; idempotent retry. Race: N=4 concurrent commits → atomic per commit, no partial visible state |
| 5 | `occ/test_orchestrator.py` **(+ race)** | `sandbox.occ.routing.orchestrator` | happy/conflict/abort flows; restart recovery. Race: N=4 orchestrators interleave → conflict detection deterministic |
| 6 | `occ/test_serial_merger.py` **(+ race, required)** | `sandbox.occ.merge.serial` | ordering, fairness, no starvation; cancel mid-wait. Race: N=16 waiters → FIFO upheld, no waiter starves > 30 s |
| 7 | `occ/test_routing.py` | `sandbox.occ.routing` | direct vs gated decision per payload; route override priority |
| 8 | `occ/test_content_gitignore_oracle.py` | `sandbox.occ.content.gitignore_oracle` | nested .gitignore; `!` re-include; case-folding fs |
| 9 | `occ/test_direct_route.py` **(+ race)** | `sandbox.occ.merge.direct` | empty changeset; 10k-path changeset; no contention. Race: N=8 direct commits disjoint paths → all succeed, no lock blow-up |
| 10 | `occ/test_gated_route.py` **(race-by-definition)** | `sandbox.occ.merge.gated` | first-commits-wins; both-reject; partial overlap (test is inherently concurrent — confirm spec, not added) |
| 11 | `occ/test_merge_engine.py` | `sandbox.occ.merge` | non-conflict hunks; conflict hunks; binary; CRLF/LF |
| 12 | `occ/test_overlay_capture_to_changeset.py` | `sandbox.occ.capture.overlay` | overlay-with-whiteouts → changeset; renames; mixed tracked/gitignored |

`overlay/native/` files (P0):

| Order | File | Probe imports | Pass bar |
|---:|---|---|---|
| 13 | `overlay/native/test_snapshot_overlay_runner.py` **(+ race)** | `sandbox.overlay.runner` | mount + run + unmount round-trip; fd/mount Δ = 0; nested overlay; runner crash mid-run. Race: N=4 parallel runners → no fd/mount cross-leak |
| 14 | `overlay/native/test_capture_upperdir.py` | `sandbox.overlay.capture` | binary, sparse, symlink, hardlink, long path, unicode |
| 15 | `overlay/native/test_capture_changes.py` **(+ race)** | `sandbox.overlay.capture` | whiteouts, opaque dirs, rename detection, dedup, ordering. Race: N=4 producers same path -> dedup deterministic, ordering preserved |

**Gate after 1b:** all 12 P0 native files in `occ/` and `overlay/native/` pass.

---

## Phase 2 — P0 Finish

| Order | File | Probe imports | Pass bar |
|---:|---|---|---|
| 16 | `layer_stack/test_squash.py` **(+ race)** | `sandbox.layer_stack.maintenance.squash` | coalesce N→1 correct; idempotent; kill mid-squash recovers. Race: squash + concurrent appender → no torn manifest, no lost append |
| 17 | `layer_stack/test_changes_aggregation.py` **(+ race)** | `sandbox.layer_stack.layer.change` | dedup; ordering; rename pairs; out-of-order writes. Race: N=8 concurrent producers → dedup invariant holds, ordering deterministic per-path |
| 18 | `layer_stack/test_lease_registry.py` **(+ race)** | `sandbox.layer_stack.lease.registry` | register/release/expire; killed-shell sweep; double-release. Race: N=16 concurrent register → unique lease ids, no double-allocation |
| 19 | `layer_stack/test_stack_manager_integration.py` **(+ race)** | `sandbox.layer_stack.manager` | full happy path end-to-end; failure injection at each phase. Race: N=4 agents through stack_manager concurrently → end-state consistent with per-agent records |

### Verification gate
```bash
.venv/bin/pytest backend/tests/live_e2e_test/sandbox/layer_stack \
                 backend/tests/live_e2e_test/sandbox/occ \
                 backend/tests/live_e2e_test/sandbox/overlay -v
# expect: all 19 P0 native tests pass
```

### Done definition
- All P0 native files green.
- Resource block emitted on every native test.
- No host-process imports of `sandbox.{layer_stack,overlay,occ}` (fence enforced).

---

## Phase 3 — Integrated P0 (replace skip stubs)

Native suite catches subsystem regressions; integrated suite catches wiring
breakage. Cutover requires both.

| Order | File | Replaces | Pass bar |
|---:|---|---|---|
| 21 | `layer_stack_overlay_occ/test_shell_call_isolation.py` | 3 skip stubs | 100 paired runs, drift = 0 |
| 22 | `layer_stack_overlay_occ/test_concurrent_agents.py` | 4 skip stubs | sustained mixed shell+edit; replay reconciles; rejected-write absent |
| 23 | `layer_stack_overlay_occ/test_codegen_race.py` | 3 skip stubs | tracked race rejects; gitignored race LWW via pathspec oracle |
| 24 | `layer_stack_overlay_occ/test_failure_recovery.py` | 3 skip stubs | kill mid-publish/squash → fsck 0 dangling; killed leases reaped |

### Verification gate
```bash
.venv/bin/pytest backend/tests/live_e2e_test/sandbox/layer_stack_overlay_occ -v -rs
# expect: 14 passed, 4 skipped (load profiles only)
```

### Done definition — **migration cutover-ready**
- All 24 P0 files (20 native + 4 integrated) green.
- Phase 5 of `per-call-snapshot-layer-stack.md` (cutover) is unblocked.

---

## Phase 4 — P1 Load + Resource + Edge Cases (parallel-friendly)

Order within each subsystem: **edge → resource → load**. Edges debug fastest;
load amplifies latent bugs.

### `overlay/native/` P1
| Order | File | Pass bar |
|---:|---|---|
| 25 | `overlay/native/test_namespace_command.py` | invalid namespace; missing CAP_SYS_ADMIN; signal propagation |
| 26 | `overlay/native/test_namespace_mounts.py` | force-kill mid-mount; orphaned upperdir; double-umount |
| 27 | `overlay/native/test_daemon_invoker.py` | exec failure; stdout overflow; timeout; non-UTF8 |
| 28 | `overlay/native/test_overlay_edge_cases.py` | depth=0/1/cap+1; ENOSPC/EBUSY/ENOMEM injection; missing lowerdir; dirty workdir |
| 29 | `overlay/native/test_overlay_resource.py` | per-probe budgets §6.2 |
| 30 | `overlay/native/test_overlay_runner_load.py` | N=20 concurrent runners, no fd/mount leaks; subsystem profile §6.3 |

### `layer_stack/` P1
| Order | File | Pass bar |
|---:|---|---|
| 31 | `layer_stack/test_layer_stack_edge_cases.py` | empty layer; one whiteout; >1 GiB single file; unicode + long paths; symlink loops |
| 32 | `layer_stack/test_layer_stack_resource.py` | depth-100 + depth-200 budgets §6.2 |
| 33 | `layer_stack/test_layer_stack_load.py` | `manifest.append + publisher.publish` 1:1 mix, 100 ops/s × 60 s × concurrency 32; append p99 < 1 ms; publish p99 < 50 ms; squash coalesce ≤ 20 layers/s |
| 33s | `layer_stack/test_layer_stack_stress.py` (P2, nightly) | publish-rate ramp 100 → 2000 ops/s; record knee; no crash, no orphans; 10k stacked-layer + 100k-path changeset + GB-scale upperdir stages |

### `occ/` P1
| Order | File | Pass bar |
|---:|---|---|
| 34 | `occ/test_patching.py` | apply success; reject hunk; whitespace-only; EOF-no-newline |
| 35 | `occ/test_changeset_model.py` | empty; max size; mixed add/modify/delete; unicode normalization |
| 36 | `occ/test_occ_edge_cases.py` | huge changeset (10k paths); conflicting concurrent commits; gitignored partial commit; UTF-8 boundary |
| 37 | `occ/test_occ_resource.py` | per-probe budgets §6.2 |
| 38 | `occ/test_occ_load.py` | `orchestrator.commit` at 50 ops/s × 60 s × concurrency 16, 5-path changeset, 50 % overlap; p99 < 200 ms; queue depth ≤ 64; 0 starvation |
| 38s | `occ/test_occ_stress.py` (P2, nightly) | concurrency ramp 16 → 256 with 100 % path overlap; rejection rate scales linearly; serial-merger queue < 1024; no starvation > 30 s |

### Integrated load (smoke + sustained + burst only here; soak in Phase 5)
| Order | File | Pass bar |
|---:|---|---|
| 39 | `layer_stack_overlay_occ/test_load_profiles.py::test_smoke_profile_passes` | p99 ≤ 500 ms; 0 drift |
| 40 | `layer_stack_overlay_occ/test_load_profiles.py::test_sustained_profile_meets_p99_budget` | p99 ≤ 1000 ms |
| 41 | `layer_stack_overlay_occ/test_load_profiles.py::test_burst_profile_recovers_within_squash_window` | p99 ≤ 2500 ms; coalesce ≤ 20 layers/s |

### Verification gate
```bash
.venv/bin/pytest backend/tests/live_e2e_test/sandbox -v -rs \
    --deselect backend/tests/live_e2e_test/sandbox/layer_stack_overlay_occ/test_load_profiles.py::test_soak_profile_no_regression_over_15_min
# expect: 41 passed, 1 deselected
```

---

## Phase 5 — Soak + Extreme Soak (gates sign-off)

| Order | File | Pass bar |
|---:|---|---|
| 42 | `layer_stack_overlay_occ/test_load_profiles.py::test_soak_profile_no_regression_over_15_min` | 15 min × 4 shells/s × 8 edits/s; p99 ≤ 1200 ms; no leak Δ at end |
| 42x | `layer_stack_overlay_occ/test_extreme_soak.py` (P2, nightly) | 4 hr × 8 shells/s × 16 edits/s; drift = 0; RSS regression < 5 % between hour 1 and hour 4; zero orphan refs at end |

### Verification gates
```bash
# Required for sign-off (P0/P1)
.venv/bin/pytest backend/tests/live_e2e_test/sandbox/layer_stack_overlay_occ \
                 -k 'soak and not extreme' -v --timeout=1200
# expect: 1 passed in <16 min

# Nightly only (P2)
.venv/bin/pytest backend/tests/live_e2e_test/sandbox/layer_stack_overlay_occ \
                 -k extreme_soak -v --timeout=15000
# expect: 1 passed in <4.2 hr
```

### Sign-off definition
- All 42 files green.
- JSONL artifacts present at `.omc/results/live-e2e-*.jsonl` for every load run.
- `git grep "pytest.skip(_PENDING)" backend/tests/live_e2e_test/` returns empty.

---

## Per-File Workflow

For every probe file, the same loop:

1. **Write probe body** in the test file (or in `_harness/<suite>_probe.py`
   if the script is shared across multiple tests).
2. **Render via `native_probe.render(body, cfg=...)`**.
3. **Drive via `handle.raw_exec(handle.sandbox_id, cmd, timeout=...)`** in an
   `async def test_*` with `@pytest.mark.asyncio`.
4. **Parse the trailing JSON line**, assert `resource_after.fd_open ==
   resource_before.fd_open` etc. per §6.2.
5. **Run the test once locally** against the Daytona image. Iterate until green.
6. **Verify resource invariants by re-running 3×** to catch flakiness.

## Anti-patterns (collection-time errors)

- `from sandbox.layer_stack import ...` at module top of any live-suite file.
- Building probe scripts that import sandbox internals instead of `sandbox.api` or
  `sandbox.host.*` (host-side packages even when staged).
- `ignored_paths=[...]` parameters used to fake gitignore — use real
  `.gitignore` writes inside `/testbed` and let the pathspec oracle classify.
- Adding `*_load.py` files without a `SubsystemLoadProfile` row in §6.3.
- Touching `DEFAULT_LAYER_STACK_ROOT` (`/tmp/eos-sandbox-runtime/layer-stack`)
  from a probe — use a per-probe path
  `/tmp/eos-sandbox-runtime/layer-stack-test-<pid>/`.

## Estimate

| Phase | Files | Race cases | P-tier | Estimate |
|---|---:|---:|---|---:|
| 0 | 5 | — | gate | 0.5 d |
| 1a | 3 | 2 | P0 | 0.75 d |
| 1b | 12 | 7 | P0 | 2.5 d |
| 2 | 5 | 5 | P0 | 1.5 d |
| 3 | 4 | — | P0 | 1 d |
| 4 (P1 only) | 17 | — | P1 | 3 d |
| 4 (P2 stress) | 2 | — | P2 | 1 d |
| 5 (P0/P1 soak) | 1 | — | P1 | 0.25 d (run) |
| 5 (P2 extreme soak) | 1 | — | P2 | 0.5 d (write) + nightly run |
| **Total** | **50** | **14** | | **~10.5 d** |

(50 files authored, 49 retained at end of Phase 5. The 14 **(+ race)** cases
are additional `*_under_race` test functions inside their existing files —
not new files; they add ~0.1 d each. P2 stress + extreme soak add 3 files
gated to nightly runs, not required for cutover sign-off.)

---

## Cross-references

- Plan: `.omc/plans/per-call-snapshot-layer-stack-migration/live-e2e-test-suite-plan.md`
- Migration: `.omc/plans/per-call-snapshot-layer-stack-migration/per-call-snapshot-layer-stack.md`
- Load standard: `backend/tests/live_e2e_test/sandbox/load_testing_standard.md`
- Suite README: `backend/tests/live_e2e_test/sandbox/README.md`
