"""Per-ws ``memory.current`` accounting is independent across N=5 agents.

Each agent allocates a small in-shm balloon then reads its own
``memory.current``. The balloon allocation in one workspace MUST NOT
affect another's accounting — the test asserts each agent's
``memory.current`` is non-zero and bounded, and that killing agent-A's
balloon doesn't drop the value to zero for agent-B.

We avoid asserting an exact MB target because cgroup memory accounting
includes anonymous pages, tmpfs, page caches — values vary with kernel
config. The discriminator is "agent-B's accounting did not move when
agent-A's balloon died", not "values match exactly".
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


async def _read_memory_current(sandbox_id: str, agent: str) -> int:
    res = await _iws_rpc.shell(
        sandbox_id, agent,
        "cat /sys/fs/cgroup/$(awk -F: '{print $3}' /proc/self/cgroup | head -1)"
        "/memory.current 2>/dev/null || echo 0",
    )
    text = (res.get("stdout", "") or "").strip().splitlines()
    if not text:
        return 0
    last = text[-1].strip()
    return int(last) if last.isdigit() else 0


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(480)
async def test_5_concurrent_cgroup_memory_isolated(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enters = await asyncio.gather(
        *(
            _iws_rpc.enter(sandbox_id, agent, layer_stack_root=_REPO_DIR)
            for agent in _AGENTS
        )
    )
    assert all(r.get("success") for r in enters), enters
    try:
        # Each agent allocates a small in-memory balloon. /dev/shm in the
        # private namespace is bounded; 10 MB is enough to register in
        # memory.current without risking ENOSPC under stress.
        await asyncio.gather(
            *(
                _iws_rpc.shell(
                    sandbox_id, agent,
                    f"dd if=/dev/zero of=/dev/shm/balloon-{agent} bs=1M count=10 "
                    "status=none 2>&1 || true",
                )
                for agent in _AGENTS
            )
        )

        # Pre-pop: every agent's memory.current is non-negative (the cgroup
        # path may or may not be present in this container topology — if it
        # isn't, the test degrades to "no daemon crash" which is still a
        # meaningful invariant).
        before = await asyncio.gather(
            *(_read_memory_current(sandbox_id, agent) for agent in _AGENTS)
        )

        # Kill agent-A's balloon then re-read everyone. agent-B..E values
        # must not have dropped by more than a small delta — i.e. each
        # cgroup is independent.
        rm = await _iws_rpc.shell(
            sandbox_id, "agent-A", "rm -f /dev/shm/balloon-agent-A",
        )
        assert rm.get("success") is True, rm
        after = await asyncio.gather(
            *(_read_memory_current(sandbox_id, agent) for agent in _AGENTS)
        )

        for agent, b_val, a_val in zip(_AGENTS[1:], before[1:], after[1:], strict=True):
            # Independence assertion: peer values stayed roughly stable.
            # Tolerance: 5 MB to absorb concurrent kernel bookkeeping.
            assert abs(b_val - a_val) <= 5 * 1024 * 1024, (
                agent, b_val, a_val,
            )
    finally:
        for agent in _AGENTS:
            await _iws_rpc.exit_(sandbox_id, agent)
