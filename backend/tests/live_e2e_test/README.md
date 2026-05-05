# live_e2e_test — per-call snapshot layer-stack live suite

Implementation of `.omc/plans/per-call-snapshot-layer-stack-migration/live-e2e-test-suite-plan.md`.

## Status

This package is being built incrementally from that plan.

- **Step 1 (landed):** `_harness/`, `conftest.py`, README, load-testing
  standard.
- **Step 2 (landed):** `layer_stack/test_manifest_atomicity.py` —
  vertical slice that validates the harness contract end-to-end.
- **Step 3a (landed):** layer-stack harness scaffolding —
  `_harness.with_thresholds()` (plan §3.4) plus lease/squash workload
  helpers in `_harness.workload` (`commit_layer`, `commit_layers`,
  `acquire_lease`, `release_lease`, `squash_to`, `make_write_change`).
- **Step 3b — layer_stack slice (landed):** test bodies for
  `test_squash_throughput.py` (3), `test_layer_gc.py` (3), and
  `test_lease_budget.py` (3/4; `MAX_PINNED_OLD_MANIFESTS` stays
  skipped pending the corresponding knob on `LeaseBudgetWorker`).
- **Step 3b — overlay/occ/integrated (pending):** test bodies for the
  remaining 13 skeleton files in §4.2–§4.4 of the plan, plus the three
  sandbox fixtures (`overlay_sandbox`, `occ_sandbox`,
  `integrated_sandbox`). See *What's left* below.

Current layer_stack run (2026-05-05):
`12 passed, 1 skipped, 0 failed` in ~9 s (session bring-up ~3.5 s;
all 13 tests run in <1 s combined). The single skip is the documented
`MAX_PINNED_OLD_MANIFESTS` cap that has no backing in
`LeaseBudgetWorker`.

## What's left

The remaining skips break down as follows:

| Bucket | Files | Tests | Blocker |
|---|---|---:|---|
| `layer_stack/test_lease_budget.py` | 1 | 1 | `MAX_PINNED_OLD_MANIFESTS` knob on `LeaseBudgetWorker` |
| `overlay/*` | 4 | 10 | `overlay_sandbox` fixture + `OverlayClient` registration path |
| `occ/*` | 4 | 17 | `occ_sandbox` fixture + `OccApplyService` registration path |
| `layer_stack_overlay_occ/*` | 5 | 17 | `integrated_sandbox` fixture + `sandbox.api.tool` end-to-end through registered overlay/occ |

Notes on the harder buckets:

- **overlay/** — needs in-sandbox overlayfs mount probes via `raw_exec`
  and direct `OverlayClient.shell` calls; baseline data lives in
  `.omc/results/stack-overlay-live-*.jsonl`. The `overlay_sandbox`
  fixture is the natural unit.
- **occ/** — runs the changeset pipeline (`WriteChange`, `EditChange`,
  `DeleteChange`, `BinaryChange`, `SymlinkChange`, `OpaqueDirChange`)
  against a synthetic layer-stack base view; needs `register_occ_service`
  wiring inside the fixture.
- **layer_stack_overlay_occ/** — only allowed to import `sandbox.api.tool`
  (per the import fence in `conftest.py`). Validates the per-call flow
  end-to-end and runs the four named load profiles. Depends on the two
  fixtures above plus a real Daytona sandbox configured per
  `tests/unit_test/test_sandbox/test_live_setup_api.py`.

Recommended landing order follows the dependency chain —
`overlay_sandbox` and `occ_sandbox` both feed `integrated_sandbox`,
so neither integrated work nor load profiles can start before the
two single-layer fixtures land. The plan suggests fanning the rest
out via ultrawork.

### Step 4 — `MAX_PINNED_OLD_MANIFESTS` cap (1 test)

`backend/tests/live_e2e_test/layer_stack/test_lease_budget.py::test_max_pinned_old_manifests_evicts_oldest`
stays skipped until `LeaseBudgetWorker` grows a fourth cap. Concrete
deltas:

1. Add `max_pinned_old_manifests: int | None` to
   `backend/src/sandbox/layer_stack/lease_budget.py::LeaseBudgetWorker`
   alongside `max_active_depth` etc. When the count of distinct
   pinned manifest versions across all snapshots exceeds the cap,
   return a `BudgetDecision(kind="evict_session", lease_id=<oldest>)`
   pointing at the oldest snapshot pinning a retired manifest.
2. Drop the `NotImplementedError` raise in
   `backend/tests/live_e2e_test/_harness/thresholds.py::with_thresholds`
   and forward the parameter to
   `LeaseBudgetWorker(max_pinned_old_manifests=...)`.
3. Replace the `pytest.skip` in the test with a body: hold N+1
   leases against N+1 distinct manifest versions, assert
   `evaluate_lease_budget()` returns `evict_session` targeting the
   oldest lease.
4. Add a parallel unit test next to
   `backend/tests/unit_test/test_sandbox/test_layer_stack/test_lease_budget.py`
   to keep the cap matrix symmetric with the other three.

### Step 5 — `overlay_sandbox` fixture + overlay/ suite (10 tests)

The fixture replaces the current
`pytest.skip("overlay_sandbox fixture lands with the overlay suite")`
in `backend/tests/live_e2e_test/_harness/sandbox_fixture.py`.

1. Reuse `live_sandbox`, then call `register_overlay_client(sandbox_id, ...)`
   from `backend/src/sandbox/overlay/client.py` (registration path
   already used by
   `backend/src/sandbox/control/ops/runtime_services.py`).
   Populate `SandboxHandle.overlay_client`.
2. Per-test reset: tear down any leaked overlayfs mounts the
   previous test left behind (under whatever work-dir root the
   overlay client uses); `git reset --hard` on `/testbed` is not
   sufficient.
3. Test bodies for plan §4.2:
   - `test_mount_depth.py` — `raw_exec` runs an in-sandbox probe
     issuing `mount(2)` directly at depths {1, 5, 10, 30, 50, 80,
     100, 200}; util-linux `mount(8)` is the negative control.
     Pass bar: `mount(2)` rc=0 at every depth in {1..200}; `mount(8)`
     documented as failing at depth ≥ 10.
   - `test_snapshot_latency.py` — `OverlayClient.snapshot` p99 < 5 ms
     at depth 100; 0 failures across 1000 iterations × 8 depths.
   - `test_read_latency.py` — warm 2× / cold 5× of baseline. Cold
     path skips with explicit reason if `drop_caches` denied.
   - `test_upper_capture.py` — capture serialization matches
     `OverlayCapture` schema; ordering preserved across 1k captures.
4. Implement `_harness/assertions.py::assert_no_torn_reads` to
   consume the captures these tests produce (currently raises
   `NotImplementedError`).

Baseline: `.omc/results/stack-overlay-live-*.jsonl`.

### Step 6 — `occ_sandbox` fixture + occ/ suite (17 tests)

Same shape as Step 5, for the changeset pipeline.

1. Replace the `pytest.skip` in
   `_harness/sandbox_fixture.py::occ_sandbox` with a fixture that
   calls `register_occ_service(sandbox_id, ...)` from
   `backend/src/sandbox/occ/client.py` and populates
   `SandboxHandle.occ_service`. Build a synthetic layer-stack base
   view via a host-side `LayerStackManager` (mirror
   `layer_stack_sandbox`) so the changeset pipeline has something
   to apply against without standing up overlay.
2. Test bodies for plan §4.3:
   - `test_per_path_cas.py` (5) — write/write conflict, disjoint
     paths, anchor miss, existence change, delete idempotency.
     Pass bar: 0 false-accept and 0 false-reject across 10k
     iterations of the gated matrix (scale per test budget; record
     the ratio).
   - `test_gitignore_routing.py` (4) — tracked → CAS, gitignored →
     LWW, mixed changeset partial commit, snapshot-time evaluation.
   - `test_changeset_pipeline.py` (5) — round-trip every typed
     `Change`: `WriteChange`, `EditChange`, `BinaryChange`,
     `SymlinkChange`, `OpaqueDirChange`.
   - `test_staleness_telemetry.py` (3) — long-shell clean CAS
     accept; `manifest_lag` increments; no age- or lag-based
     rejection.
3. Implement `_harness/assertions.py::assert_classification_pure`
   and `assert_telemetry_present` (currently `NotImplementedError`).

### Step 7 — `integrated_sandbox` fixture + layer_stack_overlay_occ/ suite (17 tests)

Boundary: per the import fence in `conftest.py`, this suite imports
**only** `sandbox.api.tool.{read_file, write_file, edit_file, shell}`
and the harness. Internal layer/overlay/occ modules are banned and
the conftest's `pytest_collection_modifyitems` hook will raise
`pytest.UsageError` if they're imported.

1. Replace the `pytest.skip` in
   `_harness/sandbox_fixture.py::integrated_sandbox` with a fixture
   that composes the Step 5 and Step 6 fixtures (overlay client and
   occ service both registered) and confirms the existing
   `ToolBundle` verbs route through the registered services.
2. Test bodies for plan §4.4:
   - `test_shell_call_isolation.py` (3) — drift incidents = 0
     across 100 paired runs.
   - `test_concurrent_agents.py` (4) — E4 pass bar: 8 shells/s +
     16 edits/s × 60 s, 0 correctness violations across 10 runs;
     final manifest reproducible from per-call captures.
   - `test_codegen_race.py` (3) — tracked race → 1 accept + 1
     reject deterministically; gitignored race → both accept;
     mixed partial commit.
   - `test_failure_recovery.py` (3) — kill mid-layer-publish /
     mid-squash; fsck reports 0 dangling refs; `mutations='killed_lease_overrun'`
     surfaced.
   - `test_load_profiles.py` (4) — runs `smoke` / `sustained` /
     `burst` / `soak` profiles from `_harness/load_profiles.py`.
     `soak` is 15 min; do not run on every push.
3. Wire JSONL emission to
   `.omc/results/live-e2e-<profile>-<utc>.jsonl` per plan §6 (one
   record per call) so the load profiles produce the data the
   aggregation scripts can reduce later.
4. Finish `_harness/assertions.py::assert_accepts_visible_rejects_invisible`
   (currently `NotImplementedError`).

### Sequencing

With the layer_stack slice green, the next safe parallel split is:

- One worker on **Step 5** (overlay_sandbox + overlay tests).
- One worker on **Step 6** (occ_sandbox + occ tests).
- **Step 4** (`MAX_PINNED_OLD_MANIFESTS`) is independent and can
  ride alongside either.

**Step 7** is sequential — start it only after both Step 5 and Step
6 land. **Step 7's `soak` profile is 15 min** and should not run
on every push; gate it behind a separate marker or a nightly job.

## How to run

The suite is opt-in **by directory**: pyproject's
`[tool.pytest.ini_options].norecursedirs` keeps the default
`pytest backend/tests` invocation from walking into it. Run by pointing
pytest at the directory:

```bash
# Whole suite
.venv/bin/pytest backend/tests/live_e2e_test

# Single suite (layer_stack | overlay | occ | layer_stack_overlay_occ)
.venv/bin/pytest backend/tests/live_e2e_test/layer_stack

# Single file
.venv/bin/pytest backend/tests/live_e2e_test/layer_stack/test_manifest_atomicity.py

# Verbose, show skip reasons (handy while skeletons skip with "pending: ...")
.venv/bin/pytest backend/tests/live_e2e_test -v -rs

# Only the cases that already have real implementations
.venv/bin/pytest backend/tests/live_e2e_test -v -k manifest_atomicity
```

All tests still carry the `live` marker, so `pytest -m "not live"` from
anywhere also excludes them.

### Prerequisites

The session fixture brings up a real Daytona sandbox via
`setup_after_create`, so before running you need:

- Daytona credentials configured for the environment (same path as
  `tests/unit_test/test_sandbox/test_live_setup_api.py`).
- `settings.sandbox.default_image` set to a prebaked image that
  already has `git`, `/testbed`, and
  `/tmp/eos-sandbox-runtime/.bundle-hash`. Without these, `ensure_git`
  falls into apt-install and the session bring-up balloons; the
  fixture deliberately requires a prebaked image and `pytest.skip`s
  if `default_image` is empty. Set in `.env`:

  ```
  EPHEMERALOS_SANDBOX_DEFAULT_IMAGE=registry:6000/daytona/sweevo-psf-requests-3738:v1
  ```

The session-scoped `live_sandbox` fixture brings up a Daytona sandbox
via `setup_after_create` exactly once per pytest run (~3.5 s on the
prebaked image) and tears it down in `finally`. Per-test fixtures
reset `/testbed` with `git reset --hard HEAD && git clean -fdx`.

## Defaults adopted from plan §8

The plan left six items "confirm before implementing"; this
implementation adopts the recommended defaults:

| §8 question                  | Adopted default                                          |
|------------------------------|----------------------------------------------------------|
| sandbox lifecycle scope      | session-scoped sandbox + per-test `/testbed` reset       |
| load JSONL location          | `.omc/results/live-e2e-<profile>-<utc>.jsonl`            |
| import fence enforcement     | `pytest_collection_modifyitems` hook in `conftest.py`    |
| drift definition under load  | realtime check **and** post-run replay reconciliation    |
| burst emergency-depth budget | tightened to 0 (matches E5)                              |
| provider neutrality          | facade-first; only `live_sandbox` mentions Daytona       |

If any of these need to change, edit the harness — the layer/overlay/occ
suites consume the harness contract, not the live-sandbox bring-up.

## Layer-stack scope note

`LayerStackManager` is host-side Python with no remote-storage variant.
The plan's "tmpfs root inside the sandbox via raw_exec" framing is
realized by giving the host-side manager a *local* `tmp_path` storage
root while the sandbox stays up to satisfy the gate. Tests that genuinely
need remote shell access reach for `handle.raw_exec` directly.

## Directory layout

See plan §2.

## Pass bars and load profiles

See `load_testing_standard.md`.
