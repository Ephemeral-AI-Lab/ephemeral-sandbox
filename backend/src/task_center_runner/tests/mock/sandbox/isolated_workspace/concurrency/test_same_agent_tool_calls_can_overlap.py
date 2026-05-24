"""Tool calls for the same isolated workspace can overlap.

Isolated workspaces rely on quotas and cgroups for resource control, so
concurrent calls against the same open workspace should overlap.
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


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(180)
async def test_same_agent_tool_calls_can_overlap(
    iws_clean_sandbox, iws_audit_jsonl,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    opened = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert opened.get("success") is True, opened
    try:
        loop = asyncio.get_running_loop()
        t0 = loop.time()
        results = await asyncio.gather(
            _iws_rpc.shell(sandbox_id, "agent-A", "sleep 0.5"),
            _iws_rpc.shell(sandbox_id, "agent-A", "sleep 0.5"),
        )
        wall = loop.time() - t0
        assert all(result.get("success") for result in results), results
        jsonl = await iws_audit_jsonl()
        tool_calls = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_tool_call",
        )
        assert len(tool_calls) >= 2, tool_calls
        durations = [
            float((row.get("payload") or {}).get("duration_s", 0.0))
            for row in tool_calls[-2:]
        ]
        assert all(d >= 0.4 for d in durations), durations
        assert wall < sum(durations) * 0.75, (
            "same-agent isolated calls should overlap materially; "
            f"wall={wall:.2f}s durations={durations!r}",
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
