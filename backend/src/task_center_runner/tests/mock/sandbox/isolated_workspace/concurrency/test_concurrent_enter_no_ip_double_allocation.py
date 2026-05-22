"""Concurrent ``enter()`` for 3 distinct agents allocates 3 distinct IPs.

The ``_map_lock`` + IP-pool reservation must be atomic — racing enters can
neither double-allocate the same IP nor leak an allocation on rollback.
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


pytestmark = pytest.mark.asyncio
_AGENTS = ("agent-A", "agent-B", "agent-C")


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(240)
async def test_concurrent_enter_no_ip_double_allocation(
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
        jsonl = await iws_audit_jsonl()
        enters = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_enter",
        )
        ips: list[str] = []
        for row in enters:
            payload = row.get("payload") or {}
            ip = payload.get("ns_ip")
            if isinstance(ip, str):
                ips.append(ip)
        assert len(ips) >= len(_AGENTS), (
            "expected one enter event per agent", ips,
        )
        assert len(set(ips)) == len(ips), (
            "ns_ip allocations must be distinct", ips,
        )
        _iws_invariants.assert_handle_ids_unique_per_enter(jsonl)
    finally:
        for agent in _AGENTS:
            await _iws_rpc.exit_(sandbox_id, agent)
