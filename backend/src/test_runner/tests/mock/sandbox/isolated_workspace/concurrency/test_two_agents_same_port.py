"""Two agents bind TCP port 3000 — no EADDRINUSE thanks to per-ws netns.

Each workspace owns a fresh ``CLONE_NEWNET`` so port 3000 in ws-A and ws-B
are independent allocations. Cross-agent curl to the peer IP is still
blocked by bridge port isolation (Tier 2 property).
"""

from __future__ import annotations

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
_PORT = 3000


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(300)
async def test_two_agents_same_port(iws_clean_sandbox, iws_audit_jsonl) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    a = await _iws_rpc.enter(sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    b = await _iws_rpc.enter(sandbox_id, "agent-B", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    assert a.get("success") is True and b.get("success") is True, (a, b)
    server_sessions: dict[str, str] = {}
    try:
        # Start an http server in each workspace on the same port. Rust
        # command sessions keep the server owned until explicit cancellation.
        for agent in ("agent-A", "agent-B"):
            launched = await _iws_rpc.shell(
                sandbox_id, agent,
                f"cd /testbed && exec python3 -m http.server {_PORT}",
            )
            command_session_id = launched.get("command_session_id")
            if isinstance(command_session_id, str) and command_session_id:
                assert launched.get("status") == "running", (agent, launched)
                server_sessions[agent] = command_session_id
            else:
                assert launched.get("success") is True, (agent, launched)

        # Each agent reaches its OWN localhost:3000.
        for agent in ("agent-A", "agent-B"):
            curl = await _iws_rpc.complete_shell(
                sandbox_id,
                agent,
                await _iws_rpc.shell(
                    sandbox_id, agent,
                    "curl -s --max-time 3 -o /dev/null -w '%{http_code}' "
                    f"http://127.0.0.1:{_PORT}/ || echo BAD",
                ),
            )
            assert curl.get("status") != "error", (agent, curl)
            assert "200" in _iws_rpc.stdout(curl), (agent, curl)

        # Cross-agent reach must FAIL (bridge port-isolation).
        jsonl = await iws_audit_jsonl()
        enters = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_enter",
        )
        ip_b = next(
            (row.get("payload") or {}).get("ns_ip") for row in enters
            if (row.get("payload") or {}).get("agent_id") == "agent-B"
        )
        assert ip_b, enters
        cross = await _iws_rpc.complete_shell(
            sandbox_id,
            "agent-A",
            await _iws_rpc.shell(
                sandbox_id, "agent-A",
                f"curl -s --max-time 2 http://{ip_b}:{_PORT}/ || echo BLOCKED",
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
        for agent in ("agent-A", "agent-B"):
            await _iws_rpc.exit_(sandbox_id, agent)
