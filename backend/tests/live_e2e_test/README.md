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
- **Step 3b — occ/integrated (pending):** test bodies for the remaining
  9 skeleton files in §4.3–§4.4 of the plan, plus the two sandbox
  fixtures (`occ_sandbox`, `integrated_sandbox`). See *What's left*
  below.
- **Step 5 — `overlay_sandbox` fixture + overlay/ suite (landed):**
  fixture in `_harness/sandbox_fixture.py` registers an `OverlayClient`
  over a host-local `LayerStackManager` and detaches any leaked
  overlayfs mounts under `/dev/shm/o` between tests. Probe scripts in
  `_harness/overlay_probe.py` ship via `raw_exec` and exercise direct
  `mount(2)` syscalls under `unshare -Urm`. `assert_no_torn_reads`
  consumes the captures the upper-dir test produces.
- **Step 6 — `occ_sandbox` fixture + occ/ suite (landed):** fixture in
  `_harness/sandbox_fixture.py` builds a host-side `LayerStackManager`
  + `git init`'d workspace and registers an `OccService` via
  `register_occ_service` so `SandboxHandle.occ_service` is non-None.
  17 typed-changeset tests cover per-path CAS, gitignore-driven CAS/LWW
  routing, the `WriteChange`/`EditChange`/`SymlinkChange`/`OpaqueDirChange`
  pipeline, and `manifest_lag` staleness telemetry. The CAS load loop
  records 200 iterations of W/W conflicts to
  `.omc/results/live-e2e-occ-<utc>.jsonl` and prints an aggregate
  ascii table at session teardown.

Current overlay run (2026-05-05, full battery, 1000 iter × 8 depths):

| Test | Result |
|---|---|
| `test_mount_depth.py` (3 tests) | mount(2) rc=0 across {1,5,10,30,50,80,100,200}; mount(8) fails at depth 100 with options_len=7772 ("Too many levels of symbolic links") |
| `test_snapshot_latency.py` (3 tests) | 0 failures across 8000 calls; p99 at depth 100 = **0.30 ms** (budget 5 ms); depth 200 p99 = **0.43 ms** |
| `test_read_latency.py` (2 tests) | warm at depth 100 = **1.05×** depth-1 baseline (budget 2×); cold at depth 50 = **1.28×** baseline (budget 5×) |
| `test_upper_capture.py` (2 tests) | OverlayCapture round-trip identical; 1000 captures preserve ordering, mean 9.3 ms / call |

`pytest backend/tests/live_e2e_test/overlay`: **10 passed in 33.3 s**
(session bring-up ~3.5 s).

Current occ run (2026-05-05, full suite + 200-iter CAS load loop):

| Test | Result |
|---|---|
| `test_changeset_pipeline.py` (8 tests) | 8 passed — write/edit/binary/symlink/opaquedir round-trip; intra-changeset Write+Edit ordering; `expected_occurrences` is informational; OpaqueDir + paired Write preserves a kept child |
| `test_gitignore_routing.py` (6 tests) | 6 passed — tracked CAS, gitignored LWW, mixed partial commit (api_write), EditChange on gitignored path, overlay-capture all-or-nothing demotion, snapshot-time evaluation |
| `test_per_path_cas.py` (6 tests) | 6 passed — 200/200 W/W conflict rounds: 200 accepted + 200 aborted (0 false-accept, 0 false-reject); p50=12.88 ms, p99=20.85 ms, mean=13.21 ms, max=40.15 ms; delete+edit same path in one changeset |
| `test_staleness_telemetry.py` (3 tests) | 3 passed — `occ.apply.manifest_lag` populated; lag grows monotonically; no age- or lag-based rejection |

`pytest backend/tests/live_e2e_test/occ -v -s`: **23 passed in 16.6 s**
(session bring-up ~3.5 s; load loop ~2.6 s for 200 iters).
JSONL artifact: `.omc/results/live-e2e-occ-<utc>.jsonl` (200 records,
schema `{ts, test, accepted, rejected, latency_ms, manifest_version,
manifest_lag}`).

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
| `layer_stack_overlay_occ/*` | 5 | 17 | `integrated_sandbox` fixture + `sandbox.api.tool` end-to-end through registered overlay/occ |

Notes on the harder buckets:

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

### Step 5 — `overlay_sandbox` fixture + overlay/ suite (10 tests, landed)

The fixture in
`backend/tests/live_e2e_test/_harness/sandbox_fixture.py::overlay_sandbox`
brings up `live_sandbox`, registers an `OverlayClient` over a host-local
`LayerStackManager`, and detaches any leaked overlayfs mounts under
`/dev/shm/o` between tests via `_purge_overlay_mounts`.

Probe scripts in `_harness/overlay_probe.py` ship inline Python via
`raw_exec` and use `unshare -Urm` so direct `mount(2)` calls work without
host privilege:

- `script_mount_depths` / `script_mount8_negative_control` — E1.
- `script_snapshot_latency` — E2 (1000 iter × 8 depths).
- `script_read_latency` — E3 (warm/cold reads, drop_caches gated).
- `script_concurrent_mounts` — E2.1 (N mounts held open at depth 50).
- `OverlayClient.shell` — E4 upper-dir capture round-trip.

Baseline: `.omc/results/stack-overlay-live-*.jsonl`. Live results
table is in *Status* above.

#### `mount(2)` vs `mount(8)` and the util-linux ~16-layer cap

The runtime issues `mount(2)` directly via `ctypes` (libc `mount`)
rather than calling the `mount(8)` binary. This matters because the
two have very different ceilings on `lowerdir=` length:

- **`mount(2)`** accepts an options string up to one **kernel page**
  (typically 4096 bytes). With basenames after `chdir` (~9 chars per
  layer entry) that's room for ~400 lower layers in practice.
- **`mount(8)`** (util-linux) parses options into an internal
  **fixed-size buffer** that's much smaller. With absolute paths to
  lower layers — the natural shape when shelling out from agent code —
  the buffer overflows around depth **10–16**. The binary fails with
  `wrong fs type, bad option, bad superblock on overlay …` (rc=32),
  even though the kernel itself would happily accept the same options
  via `mount(2)`.

The negative-control test
`test_mount8_binary_negative_control_fails_at_depth_ge_10` reproduces
this exactly: at depth 100 with absolute paths it pushes the options
string to **7 772 bytes**, well past util-linux's buffer, and
`mount(8)` fails with *Too many levels of symbolic links*. The same
kernel call via `mount(2)` succeeds at depth 200 with options_len=919
(`test_snapshot_latency`).

Design impact: the migration plan originally proposed a hybrid mode
that fell back to `mount(8)` once the stack got deep, which would have
capped practical depth at ~16 layers. Once we characterised the
failure as **util-linux's argv buffer, not a kernel limit**, the
runtime switched to `mount(2)` exclusively. This lifts the cap to the
kernel-imposed limit (~200 layers in practice; zero failures across
8 000 mounts in `test_1000_iter_zero_failures_per_depth`) and lets
squash run on its own schedule rather than being driven by
toolchain-imposed depth panic.

### Step 6 — `occ_sandbox` fixture + occ/ suite (17 tests, landed)

Same shape as Step 5, for the changeset pipeline. The fixture in
`backend/tests/live_e2e_test/_harness/sandbox_fixture.py::occ_sandbox`
brings up `live_sandbox`, builds a host-side `LayerStackManager`
rooted at `tmp_path/occ_storage`, runs `git init` against
`tmp_path/occ_workspace` so `GitignoreOracle` can answer
`check-ignore`, instantiates an `OccService` over the pair, and binds
it via `register_occ_service(sandbox_id, ...)`. The live sandbox is
held up only to keep the gate honest — the OCC state under test is
fully host-side.

`_harness/occ_workload.py` ships the supporting workload helpers:
`init_git_workspace`, `write_gitignore`, `publish_base_file` (seeds
the layer-stack base view), and a `LoadCollector` that aggregates
per-iteration latency + accept/reject counts and flushes one JSONL
record per iteration.

The CAS load loop in `test_per_path_cas.py::test_write_write_conflict_rejects_loser`
runs 200 paired-commit rounds against the registered service. The
session-end `pytest_sessionfinish` hook in `conftest.py` summarises
latency p50/p99 + accept/reject ratios and prints them under the
`== live-e2e-occ load metrics ==` banner so `pytest -s` consumers can
copy-paste numbers into the Status table.

`assert_classification_pure` rejects any direct-routed path that
gets a CAS-style status (`aborted_version`/`aborted_overlap`) or any
tracked-routed path tagged `dropped`. `assert_telemetry_present`
verifies committed results carry `occ.apply.total_s>0` and, when a
manifest is published, `occ.apply.manifest_lag` as a non-negative int.

Telemetry: `OccService.apply_changeset` emits
`occ.apply.manifest_lag = published_manifest_version - snapshot.version - 1`
(clamped at 0) on every committed result whose changeset had a
non-None snapshot. `shell_age_seconds` is intentionally left for
Step 7's integrated/shell path; the assertion soft-tolerates its
absence.

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

With layer_stack, overlay, and occ green, the remaining work is:

- **Step 4** (`MAX_PINNED_OLD_MANIFESTS`) is independent and can land
  any time.
- **Step 7** (integrated_sandbox + load profiles) — start it now that
  Step 6 has landed. **Step 7's `soak` profile is 15 min** and should
  not run on every push; gate it behind a separate marker or a
  nightly job.

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
