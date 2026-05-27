"""N=5 concurrent enters do not inflate layer_stack disk usage by N×.

Behavioural backstop for ``test_lowerdir_layer_paths_shared_*``. ``du
--bytes`` of the layer-stack root before vs. after 5 concurrent enters
must grow by at most ``5 × upperdir_overhead_max`` (10 MB each by
convention) — significantly less than 5 × the layer-stack size.

If somebody flips ``acquire_snapshot(...) with a per-call tree copy``, the
delta balloons to O(N × layer-stack size) and this test fails loudly.
"""

from __future__ import annotations

import asyncio

import pytest

from sandbox.api import raw_exec
from task_center_runner.benchmarks.sweevo.models import _REPO_DIR
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc


pytestmark = pytest.mark.asyncio

_AGENTS = ("agent-A", "agent-B", "agent-C", "agent-D", "agent-E")
_UPPERDIR_OVERHEAD_MAX_BYTES = 10 * 1024 * 1024


async def _du_bytes(sandbox_id: str, path: str) -> int:
    result = await raw_exec(
        sandbox_id,
        f"du -sb {path} 2>/dev/null | awk '{{print $1}}'",
        cwd="/",
        timeout=60,
    )
    text = (getattr(result, "stdout", "") or "").strip()
    return int(text) if text.isdigit() else 0


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(420)
async def test_lowerdir_disk_usage_is_o1(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    before = await _du_bytes(sandbox_id, _REPO_DIR)
    assert before > 0, "layer_stack root du returned 0 — path mismatch?"

    enters = await asyncio.gather(
        *(
            _iws_rpc.enter(sandbox_id, agent, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
            for agent in _AGENTS
        )
    )
    try:
        assert all(r.get("success") for r in enters), enters
        after = await _du_bytes(sandbox_id, _REPO_DIR)
        ceiling = before + len(_AGENTS) * _UPPERDIR_OVERHEAD_MAX_BYTES
        assert after <= ceiling, (
            f"layer_stack du grew O(N): before={before} after={after} "
            f"ceiling={ceiling} (5x tree-copy regression suspected)"
        )
    finally:
        for agent in _AGENTS:
            await _iws_rpc.exit_(sandbox_id, agent)
