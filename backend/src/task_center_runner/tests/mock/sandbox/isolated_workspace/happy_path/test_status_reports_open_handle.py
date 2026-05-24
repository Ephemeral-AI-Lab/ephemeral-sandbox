"""S1: ``status`` surfaces the workspace's open state and activity timestamps.

After ``enter``:
- ``status.open`` is True.
- ``status.manifest_version`` matches the enter response.
- ``status.created_at`` and ``status.last_activity`` are non-zero floats.
- ``last_activity`` advances after every tool call.
- ``freezer_degraded`` is False on a healthy host (cgroup v2 + freezer).
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
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(180)
async def test_status_reports_open_handle(iws_clean_sandbox, iws_audit_jsonl) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    agent_id = "agent-A"
    enter_response = await _iws_rpc.enter(
        sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter_response.get("success") is True, enter_response
    try:
        first = await _iws_rpc.status(sandbox_id, agent_id)
        assert first.get("open") is True, first
        assert first.get("manifest_version") == enter_response.get("manifest_version")
        first_activity = float(first.get("last_activity") or 0.0)
        first_created = float(first.get("created_at") or 0.0)
        assert first_activity > 0.0, first
        assert first_created > 0.0, first
        # freezer_degraded would only be True if R11's SIGSTOP fallback
        # tripped; on a healthy host the field stays False.
        assert first.get("freezer_degraded") is False, first

        # Activity timestamp must advance after a tool call.
        await asyncio.sleep(0.05)
        await _iws_rpc.shell(sandbox_id, agent_id, "true")
        second = await _iws_rpc.status(sandbox_id, agent_id)
        second_activity = float(second.get("last_activity") or 0.0)
        assert second_activity > first_activity, (first, second)
    finally:
        await _iws_rpc.exit_(sandbox_id, agent_id)

    # ``status`` does NOT emit an audit event (read-only). Verify enter →
    # one tool_call → exit shows up in the JSONL.
    jsonl_path = await iws_audit_jsonl()
    _iws_invariants.assert_audit_sequence(
        jsonl_path,
        [
            "sandbox_isolated_workspace_enter",
            "sandbox_isolated_workspace_tool_call",
            "sandbox_isolated_workspace_exit",
        ],
    )
