# Running the isolated_workspace live e2e suite

**Audience:** anyone running the live-gated iws tests (Tiers 1-9). All
kernel-touching work (overlay mount, cgroup v2, netns, setns)
happens **inside a SWE-EVO docker container** spawned by the test
fixture — the host just runs pytest and `docker run`. The 17 Tier 0
static fences pass on any OS.

**Status:** all 94 tests collect cleanly on macOS and Linux. Live tiers
need Docker + the heavy gate flipped on. **macOS dev boxes work via
Docker Desktop** because Docker Desktop runs containers in a Linux VM
that provides the kernel surfaces the iws daemon needs.

**Known partial green:** as of 2026-05-23, tests pass in **isolation**
but the full combined `pytest -m "not live_e2e_soak"` run hits a
test-isolation bug (zombie `ns_holder` accumulation → daemon dies
mid-run). See [DEFERRED-WORK.md](./DEFERRED-WORK.md) for the punch list.
The 152-test static surface (§4 below) is fully green.

---

## 0. What "Linux" means here (read this first)

The iws daemon and every test-driven syscall (`unshare`, `mount -t
overlay`, `ip link`, `nft`, cgroup v2 writes) run **inside the
session-scoped SWE-EVO docker container** — never on the pytest host.
That container is always Linux because docker requires it:

- **macOS host** → Docker Desktop runs the container inside its bundled
  LinuxKit VM. The VM kernel is currently ≥ 6.x and supports everything
  the iws daemon needs (overlay, cgroup v2, namespaces). Just install
  Docker Desktop and start it. No WSL, no nested VMs.
- **Native Linux host** → containers run on the host kernel directly. No
  VM overhead, slightly faster fixture setup.

Both paths land at the same place: a Linux container with conda + the
project-pinned Python in `/opt/miniconda3` + `testbed`. The dask image
(`dask__dask_2023.3.2_2023.4.0`) is the default test fixture — it ships
the full miniconda stack and is what every iws live test boots against.

The host requirement is just "anything that can run Docker." This doc
calls out macOS-vs-Linux only where Docker Desktop's VM differs from
native Linux (mostly socket/tmpfs behavior in §7 troubleshooting).

---

## 1. Prerequisites

| Item | Why | How to verify |
|---|---|---|
| Docker daemon reachable on `unix:///var/run/docker.sock` (or Docker Desktop) | the `docker` provider spawns the sweevo container | `docker info` succeeds |
| Container kernel ≥ 5.11 (for required mount syscalls) | overlay `fsmount`, `setns(CLONE_NEWUSER)`, and cgroup v2 all need it. Docker Desktop's VM and any modern Linux host already satisfy this. | `docker run --rm ubuntu:22.04 uname -r` |
| `uv` installed + repo synced | project standard wrapper for the venv — `uv run` is how `task_center_runner/read.md` documents every command | `uv --version`; `uv sync --extra dev` |
| `nft` + `ip` binaries available inside the sweevo image | iws bridge/MASQUERADE + veth wiring | `bash backend/scripts/preflight_docker_a2_caps.sh` (skips on macOS — see §3) |
| A valid SWE-EVO instance id baked into your local image cache | every test fixture boots a sweevo container by instance id | `docker images` lists `sweevo-test-<instance>-*`, or first-run pulls it |

If any line is red, fix it first. The iws live tests **assume** all five.

### Note: conda lives INSIDE the sweevo container, not on the host

You do NOT need conda on the host. Every command the test driver runs
inside the sweevo container is auto-prefixed with
``. /opt/miniconda3/etc/profile.d/conda.sh && conda activate testbed`` —
see ``backend/src/benchmarks/sweevo/models.py:_CONDA_ACTIVATE`` (line 24).
The SWE-EVO image carries Miniconda + a ``testbed`` env with the
project-specific Python version pinned. The host side just uses
``uv run pytest``.

If you're trying to reach into a running sweevo container manually
(rare — most diagnostics go through ``raw_exec`` from tests),
``docker exec -it <sweevo-container> bash -lc '<command>'`` gives you a
login shell where ``conda activate testbed`` is already on PATH via the
container's ``/etc/environment`` + PAM bootstrap.

---

## 2. One-time host config

The repo's `ephemeralos.yaml` already sets `sandbox.default_provider: docker`,
so the provider knob is the right shape out of the box. Three env vars
need to be set explicitly per shell:

```bash
# (a) Pick the sweevo instance to drive the test fixture. Every iws live
#     test boots from a sweevo container; the instance id tells the
#     fixture which image to spin up. See task_center_runner/read.md.
export EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0   # known-good default

# (b) Flip the live-e2e gate. Tier 1-9 tests skip without this.
export EOS__RUNNER__LIVE_E2E__HEAVY_ENABLED=true

# (c) Point at a usable database (any URL the project accepts).
export EPHEMERALOS_DATABASE_URL="sqlite:///./.ephemeralos/ephemeralos.db"
```

`heavy_enabled` is the ONE knob `task_center_runner.tests._live_config`
gates on; flipping it is what un-skips every Tier 1-9 test in this
directory. The database URL just has to be valid — SQLite is fine for the
test driver. `EOS_SWEEVO_INSTANCE` need not change between iws runs once
you've verified the image cache has it.

You can ALSO set these durably in `ephemeralos.yaml`:

```yaml
runner:
  live_e2e:
    heavy_enabled: true     # un-skips Tier 1-9
  sandbox_reuse_mode: reuse # 'fresh' / 'reuse' / 'force_fresh'
```

`sandbox_reuse_mode: reuse` keeps a sweevo container alive across tests
(the iws session-scoped `iws_sandbox` fixture inherits this); use
`force_fresh` when debugging provisioning — every test boots a clean
container at ~10 s per setup.

Optional knobs:

```bash
# Tier 9 §18 policy: probe-False = HARD FAIL on the reference CI host,
# loud SKIP elsewhere. Keep unset on dev shells.
export EOS_CI_REFERENCE_HOST=true     # reference CI only

# Override docker run flags (default flags already include SYS_ADMIN +
# NET_ADMIN per backend/src/sandbox/provider/docker/client.py:25). Don't
# touch this unless you're isolating a cap-strip regression.
# export EOS_DOCKER_PRIVILEGED=1
# export EOS_DOCKER_NO_PRIVILEGE=1

# Tier 9 baseline tuning: number of warm-up cycles for iws_latency_baseline
# (default 3).
# export EOS_ISOLATED_WORKSPACE_BASELINE_RUNS=5
```

---

## 3. Verify the cap set is sufficient (preflight)

Before running tests, run the cap preflight. It bails clean on non-Linux,
so you can paste it into any CI matrix without branching:

```bash
bash backend/scripts/preflight_docker_a2_caps.sh
```

The script runs 5 probes inside a vanilla `ubuntu:22.04` container with
the same cap set tests use:

1. `unshare -Urm true`
2. private mount namespace detection
3. single-lowerdir overlay mount
4. `ip link` bridge create/delete (CAP_NET_ADMIN)
5. `nft` table create/delete (CAP_NET_ADMIN)

A green run prints `preflight: PASS — Option A.2 sufficient on this Linux host`
and writes the log to `.planning/ralplan-docker-provider/preflight-logs/`.
A red run halts with the failing probe — fix it before going further; the
iws live tests will fail at the same surface.

---

## 4. Static surface (always-on)

Runs on any OS, in ~1 s. Confirms nothing structural broke since the last
session — keep this in your edit-test loop:

```bash
uv run pytest \
    backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/pre_flight/ \
    backend/tests/unit_test/test_sandbox/test_daemon/ \
    backend/tests/unit_test/test_sandbox/test_import_fence.py \
    backend/tests/unit_test/test_audit/ \
    backend/tests/unit_test/test_task_center/test_audit/ \
    -q
```

Expected: **152 passed**. Tier 0 fences live in `pre_flight/` and pin
R3/R10/N2/C1/C2 (import graph, setns_exec discipline, handle shape, exit
path no-OCC).

---

## 5. Live tier surface (Docker + gates required)

### 5a. Tiers 1-7 + Tier 9 — heavy_enabled gated only

```bash
EOS_SANDBOX_PROVIDER=docker \
EOS_ISOLATED_WORKSPACE_ENABLED=true \
EOS__RUNNER__LIVE_E2E__HEAVY_ENABLED=true \
EPHEMERALOS_DATABASE_URL="sqlite:///./.ephemeralos/ephemeralos.db" \
    uv run pytest \
        backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/ \
        -m "not live_e2e_soak" \
        -v
```

The `-m "not live_e2e_soak"` deselects the 5 Tier 8 stress tests (those
are ≤ 30 min each — opt-in only; see §5b). With the heavy gate flipped
on, expect:

- **Tier 0** (5 files, 17 cases) — already passed in §4.
- **Tier 1 happy_path** (5 tests) — golden enter/shell/exit + audit
  sequence + mount_overlay backstop.
- **Tier 2 isolation** (5 tests) — full-cycle no-OCC + upperdir discard
  + lowerdir pin + cross-agent unreachable.
- **Tier 3 network** (15 tests) — masquerade, IMDS drop, DNS branches,
  IPv6 default route, bridge port-isolation, RFC1918 opt-in, 4 inbound
  rejection via `unshare -n`.
- **Tier 4 failure_modes** (7 tests) — every adversarial setup path
  rolls back; uses `EOS_ISOLATED_WORKSPACE_TEST_HANG_AT` / `_FAIL_AT` /
  `_HOLDER_CRASH` env knobs the manager honours.
- **Tier 5 resource_controls** (6 tests) — quota, TOTAL_CAP, host-RAM
  gate, TTL evict + non-evict, ENOSPC.
- **Tier 6 concurrency** (11 tests) — same-agent overlap, map-lock,
  init_complete, fresh-handle on re-enter, 4 N=5 noisy-neighbor proofs.
- **Tier 7 gc_and_persistence** (13 tests) — daemon-restart reaping of
  veth/cgroup/scratch/netns/lease, IP-pool reconciliation,
  v2 lowerdir O(1) checks, upperdir discard on abnormal exit.
- **Tier 9 performance** (7 tests) — capability-gated; if the kernel
  surface looks good (probes return True), enforces SUBSET-COVER,
  ratio-to-baseline, phase-breakdown completeness.

Expect ~12 min wall time for the full suite minus Tier 8 (the §8 budget
in PLAN §8 is conservative — varies with sweevo image cache state).

Tier 9 absolute-p95 checks silently skip until `_data/latency_budget.json`
is committed (§6 below).

### 5b. Tier 8 — soak / stress (opt-in)

The 4 stress tests are marked `pytest.mark.live_e2e_soak`. They include
a 100-cycle create/destroy loop, a 60 s at-rest disk probe, an N=5
max-load fan-out, and a full
pip-install + httpx network e2e. Budget: 30-90 min total depending on
package cache state.

```bash
EOS_SANDBOX_PROVIDER=docker \
EOS_ISOLATED_WORKSPACE_ENABLED=true \
EOS__RUNNER__LIVE_E2E__HEAVY_ENABLED=true \
EPHEMERALOS_DATABASE_URL="sqlite:///./.ephemeralos/ephemeralos.db" \
    uv run pytest \
        backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/stress/ \
        -m live_e2e_soak \
        -v
```

The `pip_install_then_run_e2e` test additionally requires public
internet (`https://httpbin.org`) — your CI runner must allow outbound.

### 5c. Targeted single-tier run

Each tier directory is independently runnable. Examples:

```bash
# Just Tier 6 (concurrency) for an iws-network regression check
uv run pytest \
    backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/concurrency/ \
    -v

# Just a single failure_mode (e.g. when iterating on rollback paths)
uv run pytest \
    backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/failure_modes/test_setup_timeout_wedge.py \
    -v
```

The same heavy_enabled + database gates apply.

---

## 6. Tier 9 baseline + budget (HYBRID, PLAN §15.1 + §17)

Tier 9 latency tests use TWO baselines:

- **Session baseline** — computed at fixture setup by running 3 warm-up
  enter→shell→exit cycles and taking the per-op median. Captures
  in-PR drift; portable across CI hosts.
- **Committed budget** — `_data/latency_budget.json` (PR 7 artifact).
  Captures absolute hardware regression + multi-PR drift.

The budget file doesn't exist yet — the first refresh is its own PR per
PLAN §17. Until it lands, the absolute-p95 half of every Tier 9 latency
test silently passes; the ratio-to-baseline half still runs.

To refresh the budget on the reference CI host:

```bash
EOS_CI_REFERENCE_HOST=true \
EOS_ISOLATED_WORKSPACE_BASELINE_RUNS=100 \
    uv run pytest \
        backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/performance/ \
        -v
```

Then dump the captured medians + p95s/p99s from the audit JSONL into
`backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/_data/latency_budget.json`
with this shape:

```json
{
  "_schema_version": 1,
  "_reference_host": "ci-linux-x86_64-2vcpu-4gb",
  "_refreshed_at": "YYYY-MM-DD",
  "_refreshed_by_pr": "<URL>",
  "workspace_create": {"total_ms_p95": 800,  "total_ms_p99": 1200},
  "tool_call":        {"total_ms_p95": 250,  "total_ms_p99": 400},
  "kill_holder":      {"total_ms_p95": 150,  "total_ms_p99": 250},
  "gc_orphan":        {"total_ms_p95_per_orphan": 50}
}
```

---

## 7. Troubleshooting

### `RuntimeExecFailed: sandbox daemon failed to bind socket within 10s`

The pre-existing macOS dev-box symptom. Cause: Docker Desktop on macOS
has trouble exposing the daemon Unix socket the in-container Python
process tries to bind. On Linux Docker this works fine. If you hit this
on Linux, double-check:

- `--cap-add=SYS_ADMIN --cap-add=NET_ADMIN` are present (use
  `docker inspect <container>` to confirm).
- `apparmor=unconfined` is set if AppArmor is active on the host.
- `/eos-mount-scratch` is a writable tmpfs in the container
  (`EOS_DOCKER_DISABLE_OVERLAY_WRITABLE_TMPFS` must NOT be `1`).

### Test passes Tier 5/6 but Tier 9 phase-breakdown tests fail "mount_overlay key missing"

Means the capability probe reported `has_mount_overlay=True` but the
emit didn't include the `mount_overlay` phase — usually a sign that
`_LinuxRuntime.mount_overlay` raised before the `with timer.measure(...)`
block exited normally. Re-run with `EOS_ISOLATED_WORKSPACE_TEST_HANG_AT=
overlay_mount` to confirm the rollback path is intact, then check the
container's `dmesg` for the actual mount syscall errno.

### Audit JSONL shows phases_ms sum exceeds total_ms

SUBSET-COVER invariant violation (PLAN §14). Either:
- A phase is being measured TWICE (`with timer.measure("install_veth")`
  inside another `with timer.measure(...)`).
- The clock function passed to `_PhaseTimer` is not monotonic.
- A new emit site forgot the `with` context manager.

Pin via `pre_flight/test_phase_timer_invariants.py` first (pure-Python),
then audit the new code path.

### Tier 6 N=5 noisy-neighbor tests flake on contention

The Tier 8 `test_5_concurrent_isolated_workspaces` carries a contention
bound (`max(phases_ms.install_veth) ≤ 5 × median`). If you see > 5×,
the sync subprocess calls in `_LinuxRuntime.install_veth` /
`spawn_ns_holder` need to be widened to `asyncio.create_subprocess_exec`
(NEXT-AGENT-GUIDE §4.2). `mount_overlay` + `configure_dns` are already
async (Phase 7 prerequisite).

### Tier 4 failure-injection knobs aren't taking effect

The knobs (`EOS_ISOLATED_WORKSPACE_TEST_HANG_AT`, `_FAIL_AT`,
`_HOLDER_CRASH`, `_PHASE_DELAY`) are read by the **daemon process**, not
by pytest. The `set_daemon_env` helper writes to `/etc/environment` then
SIGKILLs the daemon so the next RPC respawns it with the new env via
PAM. If a knob isn't kicking in: check that `clear_daemon_env` from a
prior test ran (left-over from a flake can keep an old knob set).

---

## 8. CI cookbook

### Quick CI smoke (~2 min)

```bash
uv run pytest \
    backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/pre_flight/ \
    backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/happy_path/ \
    -v
```

Goes red if structural fences fall OR happy-path daemon boot regresses.

### Full live gate (~15 min, no soak)

```bash
bash backend/scripts/preflight_docker_a2_caps.sh
uv run pytest \
    backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/ \
    -m "not live_e2e_soak" \
    -v
```

### Soak gate (run nightly only)

```bash
uv run pytest \
    backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/stress/ \
    -m live_e2e_soak \
    --timeout=3600 \
    -v
```

### Reference CI: enforce probe-False as a HARD FAIL

```bash
EOS_CI_REFERENCE_HOST=true \
    uv run pytest \
        backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/performance/ \
        -v
```

Without `EOS_CI_REFERENCE_HOST=true`, probe-False is a loud skip — fine
for local dev, false-coverage hazard on reference CI.

---

## 9. What this doc deliberately does NOT cover

- **How to build the sweevo image** — that's `backend/tests/live_e2e_test/sandbox/README.md`'s scope. The iws tier consumes the image, doesn't build it.
- **Daytona provider** — works in principle (`EOS_SANDBOX_PROVIDER=daytona`) but the iws tests have never been validated against it; docker is the default and the rest of this doc assumes it.
- **`run_tiered.py` orchestration** — that's the broader live_e2e_test suite's driver (`backend/scripts/run_live_e2e_docker.sh`). The iws tier uses pytest directly because its 9-tier directory layout is the orchestration; no tier-router needed.
- **Refreshing `latency_budget.json` end-to-end** — outlined in §6 above but the PR-7 governance (owner rotation, staleness CI cron) lives in PLAN §17.

---

## 10. Cross-references

- **PLAN.md** — `§§5, 7, 14-23` for the per-test contract, HYBRID baseline, audit-payload SUBSET-COVER invariant.
- **NEXT-AGENT-GUIDE.md** — phase-by-phase landed work + deferred items.
- **IMPLEMENTATION-REPORT.md** — what landed in each session (1-4).
- **`backend/scripts/preflight_docker_a2_caps.sh`** — the cap-set probe.
- **`backend/scripts/run_live_e2e_docker.sh`** — the broader live_e2e suite's runner (separate from iws).
- **`backend/src/sandbox/provider/docker/client.py:25`** — `DEFAULT_RUN_FLAGS` (the SYS_ADMIN + NET_ADMIN flag set).
