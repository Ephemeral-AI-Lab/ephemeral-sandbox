"""External TCP inbound to the workspace is rejected (v2 §19.3).

Probe mechanism: spawn ``unshare -n`` from the daemon container's host
netns and attempt a TCP connect to the workspace's ``ns_ip:22``. The
connect must time out or be refused — the bridge subnet has no inbound
DNAT path from non-bridge sources.
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
@pytest.mark.timeout(180)
async def test_external_inbound_tcp_rejected(
    iws_clean_sandbox, iws_audit_jsonl
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    try:
        jsonl = await iws_audit_jsonl()
        enters = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_enter",
        )
        payload = next(
            (row.get("payload") or {}) for row in enters
            if (row.get("payload") or {}).get("agent_id") == "agent-A"
        )
        ns_ip = payload.get("ns_ip")
        assert ns_ip, payload

        probe = await raw_exec(
            sandbox_id,
            "unshare -n -- python3 -c \""
            "import socket, sys; "
            "s = socket.socket(socket.AF_INET, socket.SOCK_STREAM); "
            "s.settimeout(2.0); "
            f"sys.exit(0 if (lambda: (lambda r: r != 0)(s.connect_ex(('{ns_ip}', 22))))() else 1)"
            "\"",
            cwd="/", timeout=15,
        )
        assert probe.exit_code == 0, (
            "external netns must be unable to TCP-connect to ws ns_ip", probe,
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
