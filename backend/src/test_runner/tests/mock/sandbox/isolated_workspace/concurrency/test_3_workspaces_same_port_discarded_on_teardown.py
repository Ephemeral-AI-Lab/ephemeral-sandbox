"""3 workspaces serve the SAME port, stay loopback-private, vanish on exit.

The scenario plan asks for one combined assertion rather than the two
separate properties the suite already proves
(``test_two_agents_same_port`` / ``test_5_concurrent_network_no_interference``
for same-port binding; ``isolation/test_upperdir_discarded_on_exit`` for
teardown discard). This test threads them through one lifecycle:

  1. Three concurrent isolated workspaces each write a unique served artifact
     into ``/testbed`` and start the SAME real server
     (``python3 -m http.server 8000``). All three ``bind`` succeed — no
     ``EADDRINUSE`` — because each workspace owns a fresh ``CLONE_NEWNET``.
  2. Each agent reaches its OWN artifact on ``127.0.0.1:8000``; a cross-agent
     fetch to a peer's bridge IP is dropped (bridge port isolation).
  3. After ``exit_isolated_workspace`` for all three, the served artifacts are
     gone: no upper survives host-side under the scratch root, and a
     default-mode read misses (the writes were never OCC-published).

Gating mirrors the sibling same-port network tests (heavy + database gates);
the real ``unshare``/``CLONE_NEWNET`` capability is provided by the daemon's
container, which is where the namespaces are actually created — so a
host-side ``has_unshare_netns()`` skip (which would observe the macOS pytest
host, not the container) is intentionally not used here, matching
``test_5_concurrent_network_no_interference``.
"""

from __future__ import annotations

import asyncio

import pytest

from sandbox.api import raw_exec
from test_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from test_runner.tests.mock.sandbox.isolated_workspace import (
    _iws_invariants,
    _iws_rpc,
)
from test_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    iws_scratch_root,
)


pytestmark = pytest.mark.asyncio

_AGENTS = ("agent-A", "agent-B", "agent-C")
_PORT = 8000


def _served_path(agent: str) -> str:
    return f"/testbed/served-{agent}.html"


def _served_body(agent: str) -> str:
    return f"served-by-{agent}"


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(420)
async def test_3_workspaces_same_port_discarded_on_teardown(
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
        # Each workspace writes its own served artifact, then binds the SAME
        # port. Independent netns ⇒ three successful binds, no EADDRINUSE.
        launches = await asyncio.gather(
            *(
                _iws_rpc.shell(
                    sandbox_id, agent,
                    f"printf '{_served_body(agent)}\\n' > {_served_path(agent)} && "
                    f"cd /testbed && "
                    f"exec python3 -m http.server {_PORT}",
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

        # Each agent fetches ITS OWN artifact from its own loopback server.
        for agent in _AGENTS:
            for _attempt in range(12):
                res = await _iws_rpc.complete_shell(
                    sandbox_id,
                    agent,
                    await _iws_rpc.shell(
                        sandbox_id,
                        agent,
                        f"curl -s --max-time 3 http://127.0.0.1:{_PORT}/served-{agent}.html "
                        "|| echo BAD",
                    ),
                )
                if _served_body(agent) in _iws_rpc.stdout(res):
                    break
                await asyncio.sleep(0.25)
            else:
                raise AssertionError((agent, res))

        # Cross-agent reach via a peer's bridge IP must be dropped.
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
        cross = await _iws_rpc.complete_shell(
            sandbox_id,
            "agent-A",
            await _iws_rpc.shell(
                sandbox_id, "agent-A",
                f"curl -s --max-time 2 http://{ip_by_agent['agent-B']}:{_PORT}/ "
                "|| echo BLOCKED",
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

    # Teardown discard: no upper survives, and the served artifacts were never
    # published to the default workspace.
    scratch = await iws_scratch_root(sandbox_id)
    assert scratch, "iws scratch_root not discovered after enter+exit"
    find = await raw_exec(
        sandbox_id,
        f"find {scratch} -type f -not -name manager.json 2>/dev/null || true",
        cwd="/",
        timeout=20,
    )
    leftover = (getattr(find, "stdout", "") or "").strip()
    assert leftover == "", (
        f"exit must rmtree every handle's upper; leftover files:\n{leftover}"
    )
    for agent in _AGENTS:
        miss = await _iws_rpc.read_file(sandbox_id, agent, _served_path(agent))
        assert miss.get("success") is True, (agent, miss)
        assert miss.get("exists") is False, (
            "served artifact must be discarded, never OCC-published", agent, miss,
        )
