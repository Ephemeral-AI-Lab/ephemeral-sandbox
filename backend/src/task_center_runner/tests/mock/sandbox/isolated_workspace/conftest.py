"""pytest fixtures for the isolated_workspace mock-sandbox tier.

The Tier 0 pre-flight tests need none of these — they are pure AST walks.
The Tier 1+ tiers depend on a real ``sweevo_image_sandbox`` and a running
daemon with ``EOS_ISOLATED_WORKSPACE_ENABLED=true``.

Fixture layering:

    sweevo_image_sandbox  (existing, session-scoped)
        └── iws_sandbox           (this conftest, session-scoped)
            └── iws_clean_sandbox (this conftest, function-scoped reset)

Tests that need post-test state (daemon-restart, GC) skip
``iws_clean_sandbox`` and use ``iws_sandbox`` directly.

Capability gating:

    - Tier 0 (pre_flight/): no markers; runs everywhere.
    - Tier 1-8: gated on ``database_configured() and live_e2e_heavy_enabled()``
      by individual tests.
    - Tier 9 (performance/): additionally gated on
      ``_capability_probe`` (per PLAN §18).
"""

from __future__ import annotations

import asyncio
import shutil
import subprocess
from pathlib import Path
from typing import Any

import pytest


_IWS_APT_CACHE_DIR = (
    Path(__file__).resolve().parents[6]
    / "tests"
    / "_assets"
    / "iws_apt_cache"
    / "jammy-amd64"
)


# ---------------------------------------------------------------------------
# Capability probe (v2 §18)
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def iws_capability_probe() -> dict[str, bool]:
    """Empirical detection of kernel-touching surfaces.

    Probes run once at session setup. Tier 9 tests inspect this fixture to
    decide skip-vs-fail per the reference-CI policy.

    When the live tests run via the docker provider, the actual kernel
    work happens inside the sweevo container — a Linux VM/cgroup-v2 host
    that always has the overlay, freezer, and unshare surfaces present
    (the daemon refuses to come up without them). Probing the pytest host
    (often macOS) for those surfaces is the wrong observable; report
    True under the docker provider so Tier 9 tests can actually run
    against the daemon's container-side measurements.
    """
    import os as _os

    if _os.environ.get("EOS_SANDBOX_PROVIDER", "").lower() == "docker":
        return {
            "has_mount_overlay": True,
            "has_cgroup_freezer": True,
            "has_unshare_netns": True,
            "has_docker": shutil.which("docker") is not None,
        }

    from . import _iws_fixtures

    return {
        "has_mount_overlay": _iws_fixtures.can_mount_overlay_natively(),
        "has_cgroup_freezer": _iws_fixtures.has_cgroup_freezer(),
        "has_unshare_netns": _iws_fixtures.has_unshare_netns(),
        "has_docker": shutil.which("docker") is not None,
    }


# ---------------------------------------------------------------------------
# Sandbox + cleanup
# ---------------------------------------------------------------------------


async def _try_install_from_cache(sandbox_id: str) -> bool:
    """Install iproute2+nftables from the committed offline cache.

    Returns ``True`` if both binaries are present on the container at exit
    (whether already-installed or installed-from-cache). Returns ``False`` if
    no cache exists, or if the install completed but ``ip``/``nft`` still
    aren't on PATH — the caller then falls back to the live apt path.

    Why this design:
      - The cache directory is populated by ``backend/scripts/cache_iws_apt_debs.sh``.
        Hosts that haven't run it (or new clones before the cache is committed)
        skip cleanly through ``cache_dir.is_dir()``.
      - ``docker cp`` is used directly (not the provider adapter) because the
        provider's interface is sandbox-id-keyed exec, not file copy. The
        docker daemon accepts the sandbox_id as the container reference, so we
        don't need to convert to a container name.
      - ``dpkg -i /tmp/iws-debs/*.deb`` is used (not ``apt-get install --no-download``)
        because the base sweevo image's ``/var/lib/apt/lists/`` has no entries
        for iproute2/nftables, and apt would need an ``apt-get update`` (network)
        to discover them. With all 17 deps in the closure passed at once, dpkg
        topologically sorts them and the install completes with no apt index.
    """
    from sandbox.api import raw_exec

    if not _IWS_APT_CACHE_DIR.is_dir():
        return False
    debs = sorted(_IWS_APT_CACHE_DIR.glob("*.deb"))
    if not debs:
        return False

    # Stage cache into /tmp/iws-debs/ inside the container. Reset the dir
    # first so a stale stash from a prior session doesn't interfere.
    try:
        await raw_exec(
            sandbox_id,
            "rm -rf /tmp/iws-debs && mkdir -p /tmp/iws-debs",
            cwd="/",
            timeout=10,
        )
    except (TimeoutError, asyncio.TimeoutError):
        return False

    # ``docker cp`` is a host-side shell-out. ``subprocess.run`` is fine here;
    # the sweevo provider is docker-only for this test surface, and a host
    # without docker would already have failed at the sweevo fixture layer.
    cp_cmd = [
        "docker", "cp",
        f"{_IWS_APT_CACHE_DIR}/.",
        f"{sandbox_id}:/tmp/iws-debs/",
    ]
    try:
        result = subprocess.run(
            cp_cmd, check=False, capture_output=True, timeout=60,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError):
        return False
    if result.returncode != 0:
        return False

    # dpkg -i with the staged cache. Passing all 17 .debs at once lets dpkg
    # topologically sort them itself (libmnl0 → libnftnl11 → nftables, etc.) —
    # an iterative one-by-one install would fail on missing deps. The
    # ``command -v`` short-circuit makes the warm-container path a fast no-op.
    try:
        await raw_exec(
            sandbox_id,
            (
                "command -v ip >/dev/null 2>&1 && "
                "command -v nft >/dev/null 2>&1 && "
                "command -v ping >/dev/null 2>&1 && "
                "command -v host >/dev/null 2>&1 "
                "|| DEBIAN_FRONTEND=noninteractive "
                "dpkg -i /tmp/iws-debs/*.deb 2>&1 | tail -3"
            ),
            cwd="/",
            timeout=60,
        )
    except (TimeoutError, asyncio.TimeoutError):
        return False

    # Verify both binaries are on PATH after install. apt may have refused the
    # cache (deb-version mismatch between cache's ubuntu:22.04 and the sweevo
    # base) — in that case ``ip``/``nft`` still won't exist and we want the
    # fallback path to try.
    try:
        verify = await raw_exec(
            sandbox_id,
            "command -v ip >/dev/null 2>&1 && command -v nft >/dev/null 2>&1",
            cwd="/",
            timeout=10,
        )
    except (TimeoutError, asyncio.TimeoutError):
        return False
    return getattr(verify, "returncode", 1) == 0


@pytest.fixture(scope="session")
async def iws_sandbox(
    sweevo_image_sandbox: dict[str, Any],  # noqa: F811 (fixture from sweevo)
) -> dict[str, Any]:
    """Yield a sweevo sandbox configured for isolated workspaces.

    The daemon must boot with ``EOS_ISOLATED_WORKSPACE_ENABLED=true``.
    Approach (session-scoped, idempotent):

      1. ``raw_exec`` an append to ``/etc/environment`` (idempotent grep-guard).
      2. ``pkill -f sandbox.daemon`` so the next host RPC re-runs
         ``launch_daemon.sh``. Because the launcher uses ``bash -lc`` and the
         daemon module reads ``os.environ`` once at startup via
         ``_ManagerConfig.from_env()``, sourcing ``/etc/environment`` is
         sufficient to carry the flag.

    Modifying the underlying sweevo sandbox would change behavior for
    unrelated test surfaces, so this wrapper does the env-flip locally and
    returns the same dict.
    """
    from sandbox.api import raw_exec

    sandbox_id = str(
        sweevo_image_sandbox.get("sandbox_id")
        or sweevo_image_sandbox.get("id")
        or ""
    )
    if sandbox_id:
        # Install iproute2 + nftables if missing. SWE-EVO base images (incl.
        # the dask test fixture) don't ship them, but iws bridge/veth/MASQUERADE
        # need ``ip`` and ``nft``. There are two install paths:
        #
        #   1. Offline-cache fast-path (preferred): if the host has prebuilt
        #      ``backend/tests/_assets/iws_apt_cache/jammy-amd64/*.deb`` (run
        #      ``backend/scripts/cache_iws_apt_debs.sh`` once), ``docker cp``
        #      the 17-deb closure into the container and ``dpkg -i`` the lot.
        #      Zero network dependency, survives apt-mirror outages.
        #   2. Live apt-get fallback: best-effort ``apt-get update +
        #      install``. Wrapped in a 300 s timeout because Ubuntu's mirror
        #      regularly 502s through Docker Desktop's NAT. The individual
        #      iws tests that genuinely need ip/nft will surface the missing
        #      binary clearly via the network module's preflight.
        installed_via_cache = await _try_install_from_cache(sandbox_id)
        if not installed_via_cache:
            try:
                await raw_exec(
                    sandbox_id,
                    (
                        "command -v ip >/dev/null 2>&1 && "
                        "command -v nft >/dev/null 2>&1 "
                        "|| (apt-get update -qq && "
                        "DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "
                        "iproute2 nftables) >/dev/null 2>&1 || true"
                    ),
                    cwd="/",
                    timeout=300,
                )
            except (TimeoutError, asyncio.TimeoutError):
                import warnings
                warnings.warn(
                    "iws_sandbox: iproute2+nftables install timed out; tests "
                    "exercising bridge/veth/MASQUERADE will fail with missing "
                    "ip/nft binaries.",
                    stacklevel=2,
                )
        await raw_exec(
            sandbox_id,
            "grep -q '^EOS_ISOLATED_WORKSPACE_ENABLED=' /etc/environment "
            "2>/dev/null || "
            "echo 'EOS_ISOLATED_WORKSPACE_ENABLED=true' >> /etc/environment",
            cwd="/",
            timeout=10,
        )
        # Authorize the test_reset janitor RPC. Production deployments never
        # see this image, so leaving the flag in /etc/environment is scoped to
        # the test fixture. The handler returns ``forbidden`` without it.
        await raw_exec(
            sandbox_id,
            "grep -q '^EOS_ISOLATED_WORKSPACE_TEST_HARNESS=' /etc/environment "
            "2>/dev/null || "
            "echo 'EOS_ISOLATED_WORKSPACE_TEST_HARNESS=true' >> /etc/environment",
            cwd="/",
            timeout=10,
        )
        # Shrink the per-handle upperdir reservation so the host RAM gate
        # admits the 2-5 concurrent handles the isolation/network/concurrency
        # tiers require. Default production value is 1 GiB × handle; with the
        # default memavail_fraction=0.5 and a ~3 GiB sweevo container budget,
        # two handles would be refused (2 GiB > 1.5 GiB). 256 MiB × 5 fits.
        # The cap is a reservation accounting unit (not an enforced quota), so
        # shrinking only widens what tests can probe — never relaxes real
        # backpressure.
        await raw_exec(
            sandbox_id,
            "grep -q '^EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES=' /etc/environment "
            "2>/dev/null || "
            "echo 'EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES=268435456' "
            ">> /etc/environment",
            cwd="/",
            timeout=10,
        )
        # Docker Desktop's default cgroupns=private mounts /sys/fs/cgroup
        # read-only; the iws daemon's ``create_cgroup`` requires write access
        # to ``mkdir /sys/fs/cgroup/eos-iws-<handle>``. Remount rw inside the
        # container — idempotent (no-op if already rw), and the container's
        # CAP_SYS_ADMIN is enough to perform it. Production deployments using
        # ``--privileged`` or ``--cgroupns=host`` don't hit this; the remount
        # is silently a no-op for them too.
        await raw_exec(
            sandbox_id,
            "mount -o remount,rw /sys/fs/cgroup 2>/dev/null || true",
            cwd="/",
            timeout=10,
        )
        # Force daemon respawn so it inherits the new env on the next RPC.
        # pkill returns 1 if no process matches; that's fine. The respawned
        # daemon sources /etc/environment via the spawn-command wrapper in
        # sandbox.host.daemon_client._daemon_spawn_command.
        await raw_exec(
            sandbox_id,
            "pkill -f '^.*python.*-m sandbox\\.daemon' || true",
            cwd="/",
            timeout=10,
        )
        # Idempotently ensure /testbed/workspace.json exists. iws.enter()
        # passes layer_stack_root=/testbed and the daemon's
        # prepare_workspace_snapshot calls require_workspace_binding(/testbed),
        # which raises if missing. Reused sandboxes from earlier sessions may
        # have skipped this (e.g. if the daemon crashed during initial
        # provisioning), so re-establish the binding directly via
        # call_daemon_api with the iws layer_stack_root.
        from benchmarks.sweevo.models import _REPO_DIR
        from sandbox.host.daemon_client import call_daemon_api

        from . import _iws_rpc as _iws_rpc_mod

        try:
            await call_daemon_api(
                sandbox_id,
                "api.ensure_workspace_base",
                {"workspace_root": _REPO_DIR},
                layer_stack_root=_iws_rpc_mod.IWS_LAYER_STACK_ROOT,
                timeout=180,
            )
        except Exception as exc:  # noqa: BLE001 — surface in test, don't crash setup
            import warnings

            warnings.warn(
                f"iws_sandbox: ensure_workspace_base({_REPO_DIR}) failed: "
                f"{type(exc).__name__}: {exc}; iws tests may fail with "
                "workspace_binding errors",
                stacklevel=2,
            )
    return sweevo_image_sandbox


@pytest.fixture
async def iws_clean_sandbox(iws_sandbox: dict[str, Any]) -> dict[str, Any]:
    """Drive the daemon's janitor RPC, then yield.

    The previous implementation hardcoded ``agent-A..E`` and called
    ``exit`` for each at a 10 s timeout — a fixture that ran 5 RPCs even when
    nothing was open, and that silently leaked any handle owned by an agent
    outside the canonical list (``agent-latency-baseline``,
    ``agent-restart-bootstrap``, …). The new ``test_reset`` RPC enumerates
    open handles inside the daemon and exits them all in one round trip;
    on the cheap path (nothing open) it returns immediately.

    Idempotent. Falls back to a single ``list_open`` probe if ``test_reset``
    is unavailable (older daemon bundles), and finally to a per-agent loop.

    Also purges any test-only injection env vars that a prior test may have
    leaked into ``/etc/environment`` (if e.g. its ``set_daemon_env`` call
    raised before the ``try/finally`` cleanup could run). Without this,
    a single failing failure_modes test poisons every subsequent test in
    the session with the same setup_failed inject. Cheap idempotent grep.
    """
    from sandbox.api import raw_exec
    from . import _iws_rpc

    sandbox_id = str(iws_sandbox.get("sandbox_id") or iws_sandbox.get("id") or "")
    if not sandbox_id:
        return iws_sandbox
    # Purge any leaked test-injection vars. If grep finds none, this is a
    # ~1ms no-op. If grep finds something, the sed strips it and we force a
    # daemon respawn so the next RPC sees a clean env.
    #
    # Also re-add UPPERDIR_BYTES=256MiB if missing. Some failure_modes /
    # resource_controls tests set their own UPPERDIR_BYTES via
    # ``set_daemon_env`` and ``clear_daemon_env`` the line in their
    # finally block, which removes the conftest-session default. Without
    # this idempotent restore the next test that opens N=2+ concurrent
    # handles trips the 1 GiB-default RAM gate (production fallback).
    purge = await raw_exec(
        sandbox_id,
        "changed=0; "
        "if grep -qE '^(EOS_ISOLATED_WORKSPACE_TEST_FAIL_AT|"
        "EOS_ISOLATED_WORKSPACE_TEST_HANG_AT|"
        "EOS_ISOLATED_WORKSPACE_TEST_PHASE_DELAY|"
        "EOS_ISOLATED_WORKSPACE_TEST_HOLDER_CRASH)=' /etc/environment; "
        "then sed -i '/^EOS_ISOLATED_WORKSPACE_TEST_FAIL_AT=/d;"
        "/^EOS_ISOLATED_WORKSPACE_TEST_HANG_AT=/d;"
        "/^EOS_ISOLATED_WORKSPACE_TEST_PHASE_DELAY=/d;"
        "/^EOS_ISOLATED_WORKSPACE_TEST_HOLDER_CRASH=/d' /etc/environment; "
        "changed=1; fi; "
        "if ! grep -q '^EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES=' /etc/environment; "
        "then echo 'EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES=268435456' "
        ">> /etc/environment; changed=1; fi; "
        "if [ \"$changed\" = 1 ]; "
        "then pkill -9 -f '^.*python.*-m sandbox\\.daemon' || true; "
        "echo PURGED; else echo OK; fi",
        cwd="/",
        timeout=10,
    )
    # Stdout includes PURGED only when we had to clean — fine either way.
    _ = purge
    try:
        response = await _iws_rpc.test_reset(sandbox_id, timeout=15)
        if response.get("success"):
            return iws_sandbox
    except Exception:
        pass
    # Fallback path: if the daemon predates the janitor RPC, drive exits by
    # whatever ``list_open`` reports and finish with the canonical fixture
    # agents so a half-rolled-back enter still gets cleaned up.
    open_ids: list[str] = []
    try:
        listed = await _iws_rpc.list_open(sandbox_id, timeout=10)
        open_ids = list(listed.get("open_agent_ids") or [])
    except Exception:
        pass
    for agent_id in (*open_ids, "agent-A", "agent-B", "agent-C", "agent-D", "agent-E"):
        try:
            await _iws_rpc.exit_(sandbox_id, agent_id, timeout=10)
        except Exception:  # pragma: no cover — best-effort reset
            pass
    return iws_sandbox


# ---------------------------------------------------------------------------
# Audit JSONL snapshot (PLAN §2)
# ---------------------------------------------------------------------------


_IN_CONTAINER_AUDIT_PATH = "/tmp/sandbox_isolated_workspace_events.jsonl"


@pytest.fixture
async def iws_audit_jsonl(iws_clean_sandbox: dict[str, Any], tmp_path):
    """Provide a callable that snapshots the daemon-side iws audit JSONL.

    The daemon writes lifecycle events to ``_IN_CONTAINER_AUDIT_PATH`` inside
    the sandbox (wired by ``sandbox.isolated_workspace.handlers._JsonlAuditSink``).
    The file is truncated at fixture entry so each test sees only its own
    events; ``await snapshot()`` returns a ``pathlib.Path`` on the host with
    the bytes read at that moment.
    """
    from sandbox.api import raw_exec

    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    # Truncate the daemon-side log so we don't leak events from a previous
    # test into the assertion window. ``: > path`` is idempotent and creates
    # the file if missing.
    await raw_exec(
        sandbox_id, f": > {_IN_CONTAINER_AUDIT_PATH}", cwd="/", timeout=10,
    )

    async def snapshot():
        result = await raw_exec(
            sandbox_id,
            f"cat {_IN_CONTAINER_AUDIT_PATH} 2>/dev/null || true",
            cwd="/",
            timeout=10,
        )
        out_path = tmp_path / "iws_events.jsonl"
        out_path.write_text(getattr(result, "stdout", "") or "")
        return out_path

    return snapshot


# ---------------------------------------------------------------------------
# Audit-tail (PLAN §2)
# ---------------------------------------------------------------------------


@pytest.fixture
def iws_audit_tail(tmp_path):
    """Return a callable that waits for an audit event by type + predicate.

    The full implementation tails ``sandbox_events.jsonl`` written by the
    in-sandbox recorder. The Tier 0 tests don't need this fixture; live
    tiers consume it.
    """
    import asyncio
    import time
    from typing import Callable

    async def wait_for(
        jsonl_path: Path,
        event_type: str,
        *,
        timeout_s: float = 5.0,
        predicate: Callable[[dict], bool] | None = None,
    ) -> dict:
        from . import _iws_invariants

        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            for row in _iws_invariants.events_of_type(jsonl_path, event_type):
                if predicate is None or predicate(row):
                    return row
            await asyncio.sleep(0.05)
        raise AssertionError(
            f"timed out after {timeout_s}s waiting for {event_type} in {jsonl_path}"
        )

    return wait_for


# ---------------------------------------------------------------------------
# Latency baseline (v2 §15.1, full impl deferred to PR 6)
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
async def iws_latency_baseline(iws_sandbox) -> dict[str, float]:
    """Session-collected per-op + per-phase medians.

    Runs ``EOS_ISOLATED_WORKSPACE_BASELINE_RUNS`` warm-up enter→shell→exit
    cycles (default 3) against the real sandbox; computes the median total
    ms per operation AND per phase from the captured audit events. Returns
    a flat ``{op_name: median_ms}`` dict consumed by the Tier 9
    :class:`LatencyBudget` helper.

    Skips loudly when the live tier isn't reachable — the same gates the
    Tier 1-8 tests use. The dict is empty in that case so each Tier 9 test
    skips with a precise reason ("baseline unavailable").
    """
    import asyncio
    import json
    import os

    from sandbox.api import raw_exec

    from task_center_runner.tests._live_config import (
        database_configured,
        live_e2e_heavy_enabled,
    )

    if not (database_configured() and live_e2e_heavy_enabled()):
        return {}

    from . import _iws_invariants, _iws_rpc

    sandbox_id = str(iws_sandbox.get("sandbox_id") or iws_sandbox.get("id") or "")
    if not sandbox_id:
        return {}

    runs = int(os.environ.get("EOS_ISOLATED_WORKSPACE_BASELINE_RUNS", "3"))
    samples: dict[str, list[float]] = {
        "workspace_create": [],
        "tool_call": [],
        "kill_holder": [],
    }
    agent_id = "agent-latency-baseline"

    # Truncate the daemon-side log so the warm-up reads come back clean.
    await raw_exec(
        sandbox_id,
        f": > {_IN_CONTAINER_AUDIT_PATH}",
        cwd="/", timeout=10,
    )

    for _ in range(runs):
        await _iws_rpc.enter(sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
        await _iws_rpc.shell(sandbox_id, agent_id, "true")
        await _iws_rpc.exit_(sandbox_id, agent_id)
        await asyncio.sleep(0.05)

    raw = await raw_exec(
        sandbox_id, f"cat {_IN_CONTAINER_AUDIT_PATH}", cwd="/", timeout=10,
    )
    rows: list[dict] = []
    for line in (getattr(raw, "stdout", "") or "").splitlines():
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            continue

    for row in rows:
        et = row.get("type")
        payload = row.get("payload") or {}
        total = float(payload.get("total_ms") or 0.0)
        phases = payload.get("phases_ms") or {}
        if et == "sandbox_isolated_workspace_enter" and total > 0:
            samples["workspace_create"].append(total)
        elif et == "sandbox_isolated_workspace_tool_call" and total > 0:
            samples["tool_call"].append(total)
        elif et == "sandbox_isolated_workspace_exit" and isinstance(phases, dict):
            kh = phases.get("kill_holder")
            if kh:
                samples["kill_holder"].append(float(kh))

    return {
        op: _iws_invariants.median(values)
        for op, values in samples.items()
        if values
    }


@pytest.fixture(scope="session")
def iws_latency_budget_path():
    """Path to the committed ``_data/latency_budget.json`` (PR 7 artifact).

    Returns ``None`` when the file is absent so Tier 9 tests can skip
    cleanly per PLAN §17 governance.
    """
    from pathlib import Path

    candidate = (
        Path(__file__).resolve().parent / "_data" / "latency_budget.json"
    )
    return candidate if candidate.exists() else None


def reference_ci_host() -> bool:
    """Reference CI host check (PLAN §18 capability-probe policy).

    On the reference host, probe-False is a hard failure; off-host, it is
    a skip. Toggled by ``EOS_CI_REFERENCE_HOST=true``.
    """
    import os as _os
    return _os.environ.get("EOS_CI_REFERENCE_HOST", "").lower() == "true"
