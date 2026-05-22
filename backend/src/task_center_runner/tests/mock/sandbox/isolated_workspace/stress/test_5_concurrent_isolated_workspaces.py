"""Maximum-load proof of TOTAL_CAP=5 + the install_veth contention bound.

Five distinct agents enter concurrently — all must succeed. A sixth
agent's enter must return ``quota_exceeded``. Per v2 §19.4 enrichment:
``max(phases_ms.install_veth across N=5) <= 5 * median(in-test samples)``.
"""

from __future__ import annotations

import asyncio

import pytest

from benchmarks.sweevo.models import _REPO_DIR
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import (
    _iws_invariants,
    _iws_rpc,
)


pytestmark = [pytest.mark.asyncio, pytest.mark.live_e2e_soak]
_AGENTS = ("agent-A", "agent-B", "agent-C", "agent-D", "agent-E")


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(900)
async def test_5_concurrent_isolated_workspaces(
    iws_clean_sandbox, iws_audit_jsonl,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    results = await asyncio.gather(
        *(
            _iws_rpc.enter(sandbox_id, agent, layer_stack_root=_REPO_DIR)
            for agent in _AGENTS
        )
    )
    try:
        assert all(r.get("success") for r in results), results

        sixth = await _iws_rpc.enter(
            sandbox_id, "agent-F", layer_stack_root=_REPO_DIR,
        )
        assert sixth.get("success") is False, sixth
        assert sixth.get("error", {}).get("kind") == "quota_exceeded", sixth

        # Contention bound (v2 §19.4 + Critic follow-up #7): bound the max
        # install_veth ms against the IN-TEST median (computed from the 5
        # enters we just observed). Per the plan, max <= 5 × median.
        jsonl = await iws_audit_jsonl()
        enters = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_enter",
        )
        veth_samples: list[float] = []
        ips: list[str] = []
        for row in enters:
            payload = row.get("payload") or {}
            phases = _iws_invariants.phase_timing_extractor(payload)
            if "install_veth" in phases:
                veth_samples.append(phases["install_veth"])
            ip = payload.get("ns_ip")
            if isinstance(ip, str):
                ips.append(ip)

        assert len(veth_samples) == 5, veth_samples
        median = _iws_invariants.median(veth_samples)
        max_ms = max(veth_samples)
        # Cap: 5x median (Critic follow-up #7). Don't enforce a hard ms
        # ceiling here — Tier 9 owns absolute budgets.
        assert max_ms <= 5 * median, (
            f"install_veth contention exceeded 5x median: max={max_ms} "
            f"median={median}; suggests sync subprocess bottleneck",
        )
        # 5 distinct IPs from the .2-.6 range.
        assert len(set(ips)) == 5, ips
    finally:
        for agent in _AGENTS:
            await _iws_rpc.exit_(sandbox_id, agent)
