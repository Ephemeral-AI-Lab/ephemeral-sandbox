"""N=5 agents bind the same port — independent netns, no EADDRINUSE.

Each workspace's CLONE_NEWNET means port 8080 is a private allocation per
agent. ``curl 127.0.0.1:8080`` from each agent reaches ITS OWN server.
Cross-agent reach via the peer veth IP MUST fail (bridge port isolation,
proven in Tier 2; cross-checked here at scale).
"""

from __future__ import annotations

import asyncio

import pytest

from test_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from test_runner.tests.mock.sandbox.isolated_workspace import (
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
async def test_5_concurrent_network_no_interference(
    iws_clean_sandbox, iws_audit_jsonl,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enters = await asyncio.gather(
        *(
            _iws_rpc.enter(sandbox_id, agent, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
            for agent in _AGENTS
        )
    )
    assert all(r.get("success") for r in enters), enters
    server_sessions: dict[str, str] = {}
    try:
        # 5 servers on the same port — no EADDRINUSE thanks to per-ws netns.
        launches = await asyncio.gather(
            *(
                _iws_rpc.shell(
                    sandbox_id, agent,
                    "cd /testbed && exec python3 -m http.server 8080",
                )
                for agent in _AGENTS
            )
        )
        for agent, launch in zip(_AGENTS, launches, strict=True):
            command_session_id = launch.get("command_session_id")
            if isinstance(command_session_id, str) and command_session_id:
                assert launch.get("status") == "running", (agent, launch)
                server_sessions[agent] = command_session_id
            else:
                assert launch.get("success") is True, (agent, launch)

        # Each agent reaches localhost:8080 successfully.
        for agent in _AGENTS:
            for _attempt in range(12):
                res = await _iws_rpc.complete_shell(
                    sandbox_id,
                    agent,
                    await _iws_rpc.shell(
                        sandbox_id, agent,
                        "curl -s --max-time 3 -o /dev/null -w '%{http_code}' "
                        "http://127.0.0.1:8080/ || echo BAD",
                    ),
                )
                if "200" in _iws_rpc.stdout(res):
                    break
                await asyncio.sleep(0.25)
            else:
                raise AssertionError((agent, res))

        # Cross-agent reach via peer's bridge IP must fail.
        jsonl = await iws_audit_jsonl()
        ip_by_agent: dict[str, str] = {}
        for row in _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_enter",
        ):
            payload = row.get("payload") or {}
            agent = payload.get("agent_id")
            ns_ip = payload.get("ns_ip")
            if isinstance(agent, str) and isinstance(ns_ip, str):
                ip_by_agent[agent] = ns_ip
        assert set(_AGENTS) <= set(ip_by_agent), ip_by_agent

        peer_ip = ip_by_agent["agent-B"]
        cross = await _iws_rpc.complete_shell(
            sandbox_id,
            "agent-A",
            await _iws_rpc.shell(
                sandbox_id, "agent-A",
                f"curl -s --max-time 2 http://{peer_ip}:8080/ || echo BLOCKED",
            ),
        )
        assert "BLOCKED" in _iws_rpc.stdout(cross), cross
    finally:
        for agent, command_session_id in server_sessions.items():
            await _iws_rpc.cancel_command_session(
                sandbox_id,
                agent,
                command_session_id,
            )
        for agent in _AGENTS:
            await _iws_rpc.exit_(sandbox_id, agent)
