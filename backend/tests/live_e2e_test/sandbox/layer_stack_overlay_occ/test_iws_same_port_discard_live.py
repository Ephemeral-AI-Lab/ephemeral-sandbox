"""Scenario 3 (live-e2e variant): 3 same-port servers + discard, real provider.

The mock real-kernel test
(``isolated_workspace/concurrency/test_3_workspaces_same_port_discarded_on_teardown``)
owns the full correctness surface (bind, loopback isolation, cross-agent block,
host-side upper removal). This live-e2e variant ports the headline — three
concurrent isolated workspaces serving the SAME port and discarding their
served artifacts on exit — onto a real provider sandbox, reusing
``_prepare_isolated_workspace_runtime`` from the auto-squash suite.

Skips on hosts without ``EOS_LIVE_E2E_IMAGE`` (every live-e2e cell does); runs
under the tiered runner on a Linux/CI host with the prebaked image.
"""

from __future__ import annotations

import asyncio

import pytest

import sandbox.host.daemon_client as daemon_client_mod

from .._harness.sandbox_fixture import SandboxHandle
from .._harness.workspace_base_public import seed_imported_base
from .test_auto_squash_edge_cases import (
    _iws_enter,
    _iws_exit,
    _prepare_isolated_workspace_runtime,
)


pytestmark = pytest.mark.asyncio

_AGENTS = ("agent-A", "agent-B", "agent-C")
_PORT = 3000


async def _iws_shell(handle: SandboxHandle, agent_id: str, command: str) -> dict[str, object]:
    return await daemon_client_mod.call_daemon_api(
        handle.sandbox_id,
        "api.v1.shell",
        {"agent_id": agent_id, "command": command},
        timeout=60,
    )


async def _iws_read(handle: SandboxHandle, agent_id: str, path: str) -> dict[str, object]:
    return await daemon_client_mod.call_daemon_api(
        handle.sandbox_id,
        "api.v1.read_file",
        {"agent_id": agent_id, "path": path},
        timeout=30,
    )


@pytest.mark.timeout(600)
async def test_iws_same_port_discard_live(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    await seed_imported_base(handle, {"iws-same-port/base.txt": "base\n"})
    await _prepare_isolated_workspace_runtime(handle)

    enters = await asyncio.gather(*(_iws_enter(handle, agent) for agent in _AGENTS))
    assert all(r.get("success") for r in enters), enters
    try:
        # Each workspace writes a unique artifact and binds the SAME port —
        # independent netns ⇒ three successful binds, no EADDRINUSE.
        launches = await asyncio.gather(
            *(
                _iws_shell(
                    handle, agent,
                    f"printf 'served-by-{agent}\\n' > /testbed/served-{agent}.html && "
                    f"cd /testbed && "
                    f"nohup python3 -m http.server {_PORT} >/tmp/srv.log 2>&1 & "
                    "sleep 0.6; echo $!",
                )
                for agent in _AGENTS
            )
        )
        assert all(r.get("success") for r in launches), launches

        fetches = await asyncio.gather(
            *(
                _iws_shell(
                    handle, agent,
                    f"curl -s --max-time 3 http://127.0.0.1:{_PORT}/served-{agent}.html "
                    "|| echo BAD",
                )
                for agent in _AGENTS
            )
        )
        for agent, res in zip(_AGENTS, fetches, strict=True):
            assert res.get("success") is True, (agent, res)
            assert f"served-by-{agent}" in str(res.get("stdout") or ""), (agent, res)
    finally:
        for agent in _AGENTS:
            await _iws_exit(handle, agent)

    # Discard: the served artifacts were never published to the default view.
    for agent in _AGENTS:
        miss = await _iws_read(handle, agent, f"/testbed/served-{agent}.html")
        assert miss.get("success") is True, (agent, miss)
        assert miss.get("exists") is False, (
            "served artifact must be discarded on exit, never published", agent, miss,
        )
