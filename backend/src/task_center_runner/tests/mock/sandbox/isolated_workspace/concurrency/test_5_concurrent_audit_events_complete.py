"""N=5 concurrent enters all produce distinct audit events — none dropped.

The JSONL audit sink uses ``append_jsonl_event`` (line-level append). If
the sink ever dedups by agent_id, this test fails — we need exactly 5
distinct enters with 5 distinct handle_ids, and ``phases_ms.install_veth``
populated on each.
"""

from __future__ import annotations

import asyncio

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import (
    _iws_invariants,
    _iws_rpc,
)


pytestmark = pytest.mark.asyncio
_AGENTS = ("agent-A", "agent-B", "agent-C", "agent-D", "agent-E")


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(420)
async def test_5_concurrent_audit_events_complete(
    iws_clean_sandbox, iws_audit_jsonl,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    results = await asyncio.gather(
        *(
            _iws_rpc.enter(sandbox_id, agent, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
            for agent in _AGENTS
        )
    )
    assert all(r.get("success") for r in results), results
    try:
        jsonl = await iws_audit_jsonl()
        enters = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_enter",
        )
        agent_ids: list[str] = []
        handle_ids: list[str] = []
        install_veth_ms: list[float] = []
        for row in enters:
            payload = row.get("payload") or {}
            if (a := payload.get("agent_id")) in _AGENTS:
                agent_ids.append(a)
            if h := payload.get("handle_id"):
                handle_ids.append(h)
            phases = _iws_invariants.phase_timing_extractor(payload)
            if "install_veth" in phases:
                install_veth_ms.append(phases["install_veth"])

        # Five distinct agent_ids, five distinct handle_ids — no dedup.
        assert set(agent_ids) == set(_AGENTS), agent_ids
        assert len(set(handle_ids)) == len(handle_ids) == 5, handle_ids
        # install_veth phase was recorded for every enter.
        assert len(install_veth_ms) == 5, install_veth_ms
        # No event was dropped: every recorded phases_ms passes SUBSET-COVER.
        for row in enters:
            payload = row.get("payload") or {}
            _iws_invariants.assert_subset_cover(
                _iws_invariants.phase_timing_extractor(payload),
                payload.get("total_ms", 0.0),
                label="enter@N=5",
            )
    finally:
        for agent in _AGENTS:
            await _iws_rpc.exit_(sandbox_id, agent)
