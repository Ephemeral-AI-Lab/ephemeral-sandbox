"""Driver #1 of the source plan: server keeps running across tool-call boundaries.

A background ``python -m http.server`` started in tool_call A must still be
serving requests in tool_call B. The PID inside the workspace stays the same
across calls — this is the entire point of the daemon-native model.
"""

from __future__ import annotations

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
async def test_server_survives_tool_call_boundary(
    iws_clean_sandbox, iws_audit_jsonl
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    agent_id = "agent-A"
    await _iws_rpc.enter(sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    try:
        # Tool call A: launch the server in the background.
        launch = await _iws_rpc.shell(
            sandbox_id,
            agent_id,
            "cd /tmp && (python3 -m http.server 18080 >/tmp/http.log 2>&1 & "
            "echo $! > /tmp/http.pid) && sleep 1",
        )
        assert launch.get("success") is True, launch

        pid_a = await _iws_rpc.shell(sandbox_id, agent_id, "cat /tmp/http.pid")
        first_pid = pid_a.get("stdout", "").strip()
        assert first_pid.isdigit(), pid_a

        # Tool call B: prove the server is still up. Same PID, responds to curl.
        probe = await _iws_rpc.shell(
            sandbox_id, agent_id,
            "curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18080/",
        )
        assert probe.get("success") is True, probe
        assert "200" in probe.get("stdout", ""), probe

        pid_b = await _iws_rpc.shell(sandbox_id, agent_id, "cat /tmp/http.pid")
        assert pid_b.get("stdout", "").strip() == first_pid, (
            "server PID changed across tool calls — daemon recycled the ns?",
            pid_a,
            pid_b,
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, agent_id)

    # Sequence: enter, multiple tool_calls (≥4 shells above), exit.
    jsonl_path = await iws_audit_jsonl()
    _iws_invariants.assert_audit_sequence(
        jsonl_path,
        [
            "sandbox_isolated_workspace_enter",
            "sandbox_isolated_workspace_tool_call",
            "sandbox_isolated_workspace_tool_call",
            "sandbox_isolated_workspace_exit",
        ],
    )
