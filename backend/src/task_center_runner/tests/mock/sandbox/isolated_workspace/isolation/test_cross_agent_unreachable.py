"""Bridge port isolation: ws-A cannot reach ws-B's interface IP.

The two workspaces sit on the same Linux bridge (``eos-shared0``) but the
host end of each veth is set with ``isolated on``. Kernel-level port
isolation means inter-port forwarding is dropped at the bridge regardless
of nftables — so accidentally dropping ``bridge-nf-call-iptables`` cannot
re-open peer reach.

We learn each workspace's IP from the audit log (the ``ns_ip`` field on
``sandbox_isolated_workspace_enter`` payloads).
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
@pytest.mark.timeout(240)
async def test_cross_agent_unreachable(iws_clean_sandbox, iws_audit_jsonl) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])

    a_enter = await _iws_rpc.enter(sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    assert a_enter.get("success") is True, a_enter
    b_enter = await _iws_rpc.enter(sandbox_id, "agent-B", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    assert b_enter.get("success") is True, b_enter

    try:
        # Discover each agent's bridge IP from the audit log.
        jsonl = await iws_audit_jsonl()
        enters = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_enter"
        )
        ip_by_agent: dict[str, str] = {}
        for row in enters:
            payload = row.get("payload") or {}
            agent = payload.get("agent_id")
            ns_ip = payload.get("ns_ip")
            if isinstance(agent, str) and isinstance(ns_ip, str):
                ip_by_agent[agent] = ns_ip
        assert {"agent-A", "agent-B"} <= set(ip_by_agent), (
            "enter events must carry ns_ip for both agents",
            ip_by_agent,
        )
        assert ip_by_agent["agent-A"] != ip_by_agent["agent-B"], ip_by_agent

        # From ws-A, attempt to reach ws-B's IP. Both probes must fail.
        ping = await _iws_rpc.shell(
            sandbox_id, "agent-A",
            f"ping -c 1 -W 2 {ip_by_agent['agent-B']} >/dev/null 2>&1; echo $?",
        )
        assert ping.get("success") is True, ping  # the shell itself exits 0
        rc = (ping.get("stdout", "") or "").strip()
        assert rc != "0", (
            f"ping from ws-A to ws-B IP {ip_by_agent['agent-B']} must fail",
            ping,
        )

        curl = await _iws_rpc.shell(
            sandbox_id, "agent-A",
            f"curl -s --max-time 2 -o /dev/null -w '%{{http_code}}' "
            f"http://{ip_by_agent['agent-B']}/ || echo BLOCKED",
        )
        out = curl.get("stdout", "")
        assert "BLOCKED" in out or "000" in out, (
            "curl from ws-A to ws-B should be blocked (no route or refused)",
            curl,
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
        await _iws_rpc.exit_(sandbox_id, "agent-B")
