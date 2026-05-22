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

import shutil
from typing import Any

import pytest


# ---------------------------------------------------------------------------
# Capability probe (v2 §18)
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def iws_capability_probe() -> dict[str, bool]:
    """Empirical detection of kernel-touching surfaces.

    Probes run once at session setup. Tier 9 tests inspect this fixture to
    decide skip-vs-fail per the reference-CI policy. The Linux-vs-other
    branch is intentionally absent: the daemon only runs inside the Linux
    sweevo container, and every probe degrades cleanly when its kernel
    surface is missing.
    """
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
        await raw_exec(
            sandbox_id,
            "grep -q '^EOS_ISOLATED_WORKSPACE_ENABLED=' /etc/environment "
            "2>/dev/null || "
            "echo 'EOS_ISOLATED_WORKSPACE_ENABLED=true' >> /etc/environment",
            cwd="/",
            timeout=10,
        )
        # Force daemon respawn so it inherits the new env on the next RPC.
        # pkill returns 1 if no process matches; that's fine.
        await raw_exec(
            sandbox_id,
            "pkill -f '^.*python.*-m sandbox\\.daemon' || true",
            cwd="/",
            timeout=10,
        )
    return sweevo_image_sandbox


@pytest.fixture
async def iws_clean_sandbox(iws_sandbox: dict[str, Any]) -> dict[str, Any]:
    """Drive ``api.isolated_workspace.exit`` for known test agents, then yield.

    Idempotent: a "no workspace open" response is fine.
    """
    from . import _iws_rpc

    sandbox_id = str(iws_sandbox.get("sandbox_id") or iws_sandbox.get("id") or "")
    for agent_id in ("agent-A", "agent-B", "agent-C", "agent-D", "agent-E"):
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
    from pathlib import Path
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
    from benchmarks.sweevo.models import _REPO_DIR

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
        await _iws_rpc.enter(sandbox_id, agent_id, layer_stack_root=_REPO_DIR)
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
