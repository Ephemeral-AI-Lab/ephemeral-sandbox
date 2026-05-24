"""Daemon-host introspection: from the bridge gateway, ws is reachable.

"REJECT inbound" is scoped to the forward chain. The daemon host's own
netns (where 10.244.0.1 lives) must be able to curl an iws-internal HTTP
server — operators need to debug from the host.
"""

from __future__ import annotations

import pytest

from sandbox.api import raw_exec
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
@pytest.mark.timeout(240)
async def test_daemon_host_introspection_allowed(
    iws_clean_sandbox, iws_audit_jsonl
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    try:
        jsonl = await iws_audit_jsonl()
        ns_ip = next(
            (row.get("payload") or {}).get("ns_ip")
            for row in _iws_invariants.events_of_type(
                jsonl, "sandbox_isolated_workspace_enter"
            )
            if (row.get("payload") or {}).get("agent_id") == "agent-A"
        )
        assert ns_ip

        # Start an HTTP server inside the workspace.
        launch = await _iws_rpc.shell(
            sandbox_id, "agent-A",
            "cd /tmp && (python3 -m http.server 18181 >/tmp/iws_http.log 2>&1 &) && sleep 1",
        )
        assert launch.get("success") is True, launch

        # From the daemon host's own netns (NOT unshare -n), curl the ws.
        probe = await raw_exec(
            sandbox_id,
            f"curl -s --max-time 3 -o /dev/null -w '%{{http_code}}' "
            f"http://{ns_ip}:18181/ || echo TIMEOUT",
            cwd="/", timeout=15,
        )
        out = (getattr(probe, "stdout", "") or "").strip()
        assert out == "200" or out.startswith("2") or out.startswith("4"), (
            "daemon host must reach ws over bridge gateway", probe,
        )
        assert out != "TIMEOUT", probe
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
