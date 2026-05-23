# Running the Tier 8 soak tests

The 5 stress tests in `stress/` are deselected from the regular live-tier
run because each one is long-running (idle waits, 100-cycle loops, public
internet, N=5 maximum-load fan-out). They share the same Docker provider
+ heavy-gate environment as the other live tiers — only the marker
selection and budget differ.

`pytest -m "not live_e2e_soak"` is the default invocation for the regular
suite. To include them, run with `-m live_e2e_soak` instead. Mixing both
in one invocation is allowed but not recommended (you can't usefully
share a session-level sweevo container across a 30-minute idle test and
a tight enter/exit loop).

## What's in this tier

| Test | File | Budget | What it pins |
|---|---|---:|---|
| `test_5_concurrent_isolated_workspaces` | `stress/test_5_concurrent_isolated_workspaces.py` | 15 min (`timeout=900`) | TOTAL_CAP=5 boundary + `max(install_veth) ≤ 5 × median` contention bound (PLAN §19.4) |
| `test_disk_at_rest_bounded` | `stress/test_disk_at_rest_bounded.py` | 60 s + tear-down | Open idle workspace's scratch root stays ≤ 10 MiB after 60 s (PLAN §19.6) |
| `test_long_running_idle_freeze_at_rest` | `stress/test_long_running_idle_freeze_at_rest.py` | 30 s + tear-down | Idle workspace's `cpu.stat usage_usec` doesn't grow — proof the freezer holds between tool_calls |
| `test_pip_install_then_run_e2e` | `stress/test_pip_install_then_run_e2e.py` | 10 min (`timeout=600`) | Full stack: DNS + MASQUERADE + bridge + HTTPS + cross-tool-call pkg availability. **Needs public internet** (`pip install httpx` + `https://httpbin.org/get`) |
| `test_rapid_create_destroy_cycle` | `stress/test_rapid_create_destroy_cycle.py` | ~20 min (100 cycles × ~10 s) | Daemon FD count + host-side veth count stay bounded across 100 enter/exit cycles |

Wall-time budget for the full set: **30-90 min** depending on sweevo
image cache state and network. The `pip_install` test is the wild
card — first run pulls the index + wheels; warm cache makes it ~30 s.

## Prerequisites

Same as the regular live tier (see `RUNNING-LIVE-TESTS.md` §1-3):

1. Docker daemon reachable (`docker info`).
2. Sweevo image in your local cache (`sweevo-test-<instance>-*`).
3. `uv` installed + repo synced.
4. `ip` + `nft` reachable inside the sweevo container — installed
   from the cached deb closure at
   `backend/tests/_assets/iws_apt_cache/jammy-amd64/` by the test
   fixture, or the live apt fallback.
5. Cap preflight has passed: `bash backend/scripts/preflight_docker_a2_caps.sh`.

**Additional for `test_pip_install_then_run_e2e`:** the sweevo container
must be able to reach the public internet — `pip install httpx` and
`https://httpbin.org/get`. If your runner is behind a corporate proxy
or a CI shape without egress, that single test will fail; the other
four are self-contained.

## Run the full set

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
EOS_SANDBOX_PROVIDER=docker \
EOS_ISOLATED_WORKSPACE_ENABLED=true \
EOS__RUNNER__LIVE_E2E__HEAVY_ENABLED=true \
EOS__RUNNER__SANDBOX_REUSE_MODE=reuse \
EPHEMERALOS_DATABASE_URL="sqlite:///./.ephemeralos/ephemeralos.db" \
  .venv/bin/pytest \
    backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/stress/ \
    -m live_e2e_soak \
    --timeout=3600 \
    -v -p no:randomly
```

The `--timeout=3600` overrides any per-test default that would clip the
`test_rapid_create_destroy_cycle` 100-cycle loop on a slow image. The
per-test `@pytest.mark.timeout` decorators still apply; this is a
pytest-timeout floor for the session.

## Run one test at a time

Useful when iterating on a single invariant — the other four are
expensive enough that you don't want them in the loop:

```bash
# 100-cycle FD/veth drift check
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
EOS_SANDBOX_PROVIDER=docker EOS_ISOLATED_WORKSPACE_ENABLED=true \
EOS__RUNNER__LIVE_E2E__HEAVY_ENABLED=true \
EOS__RUNNER__SANDBOX_REUSE_MODE=reuse \
EPHEMERALOS_DATABASE_URL="sqlite:///./.ephemeralos/ephemeralos.db" \
  .venv/bin/pytest \
    backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/stress/test_rapid_create_destroy_cycle.py \
    -m live_e2e_soak \
    -v -p no:randomly
```

Same recipe with the file path swapped for any of the other four
soak tests.

## Excluding the internet-bound test

If your runner can't reach the public internet, deselect just that
test via `-k`:

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
EOS_SANDBOX_PROVIDER=docker EOS_ISOLATED_WORKSPACE_ENABLED=true \
EOS__RUNNER__LIVE_E2E__HEAVY_ENABLED=true \
EOS__RUNNER__SANDBOX_REUSE_MODE=reuse \
EPHEMERALOS_DATABASE_URL="sqlite:///./.ephemeralos/ephemeralos.db" \
  .venv/bin/pytest \
    backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/stress/ \
    -m live_e2e_soak \
    -k "not test_pip_install_then_run_e2e" \
    --timeout=3600 \
    -v -p no:randomly
```

## CI cadence

Soak tests are nightly-only by convention (`RUNNING-LIVE-TESTS.md` §8):

```bash
# Nightly soak job — keep separate from the regular PR live-tier job
.venv/bin/pytest \
    backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/stress/ \
    -m live_e2e_soak \
    --timeout=3600 \
    -v
```

The regular `pytest -m "not live_e2e_soak"` live-tier job stays at the
~16 min budget — the 89 tests across tiers 0-7 + 9 land there.

## Known interactions with the session-2026-05-23 fixes

The closure session (see `NEXT-AGENT-NOTES.md`) made three changes that
touch soak-test invariants. Re-validate these specifically the first
time you run soak after pulling those commits:

1. **`test_long_running_idle_freeze_at_rest`** — relies on `cpu.stat
   usage_usec` not growing during the 30 s idle. Commit `466058dd3`
   moves orphan PIDs out of the iws cgroup before freeze, so any
   long-running process the previous tool_call left behind is in the
   root cgroup, not the iws cgroup. `cpu.stat` of the iws cgroup will
   show 0 growth (the move-out drained it), but the test should still
   pass — it only reads the iws's own `cpu.stat`. Worth confirming the
   numbers it actually observes.

2. **`test_rapid_create_destroy_cycle`** — 100 enter/exit cycles. The
   conftest `iws_clean_sandbox` UPPERDIR_BYTES restoration (commit
   `643a9f6bd`) and the launch_daemon.sh zombie detection
   (commit `81b127e96` from the prior session) together remove the
   accumulation failure modes that previously made this test flake.
   Should be clean now but it's the longest test, so watch for FD
   creep that takes 50+ cycles to manifest.

3. **`test_5_concurrent_isolated_workspaces`** — relies on
   `install_veth` being fast enough that
   `max(phases_ms.install_veth) ≤ 5 × median`. Commit `85f23c368`
   moved `run_in_handle`'s `subprocess.run` into a thread pool — that
   benefits parallel tool_calls but doesn't affect `install_veth`,
   which is still synchronous in `enter()`'s `_wire_handle`. The
   contention bound is unchanged.

## Cross-references

- `RUNNING-LIVE-TESTS.md` §5b — soak invocation alongside the regular
  live tier
- `PLAN.md` §19 — soak test contracts
- `DEFERRED-WORK.md` — historical rationale for Tier 8 being opt-in
- `NEXT-AGENT-NOTES.md` — session-by-session landing log
