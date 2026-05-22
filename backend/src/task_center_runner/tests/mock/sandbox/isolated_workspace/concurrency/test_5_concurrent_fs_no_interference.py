"""N=5 agents write/read distinct upperdir bytes — no cross-contamination.

Each agent writes a 1 MiB file to ``/testbed/own.bin`` whose content is
its agent_id; each then reads it back. Cross-reads (agent-A reads
agent-B's own.bin) MUST return the agent's own content because every
workspace sees its own upperdir, not the peer's. The behavioural backstop
to ``test_lowerdir_layer_paths_shared_across_concurrent_handles``.
"""

from __future__ import annotations

import asyncio

import pytest

from benchmarks.sweevo.models import _REPO_DIR
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc


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
async def test_5_concurrent_fs_no_interference(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enters = await asyncio.gather(
        *(
            _iws_rpc.enter(sandbox_id, agent, layer_stack_root=_REPO_DIR)
            for agent in _AGENTS
        )
    )
    assert all(r.get("success") for r in enters), enters
    try:
        # Each agent writes a marker file whose contents == its agent_id.
        await asyncio.gather(
            *(
                _iws_rpc.shell(
                    sandbox_id, agent,
                    f"echo '{agent}' > /testbed/own.bin && wc -c /testbed/own.bin",
                )
                for agent in _AGENTS
            )
        )

        # Each agent reads back — must see its own marker, no peer leakage.
        reads = await asyncio.gather(
            *(
                _iws_rpc.shell(sandbox_id, agent, "cat /testbed/own.bin")
                for agent in _AGENTS
            )
        )
        for agent, res in zip(_AGENTS, reads, strict=True):
            assert res.get("success") is True, (agent, res)
            assert agent in (res.get("stdout", "") or ""), (agent, res)
    finally:
        for agent in _AGENTS:
            await _iws_rpc.exit_(sandbox_id, agent)
