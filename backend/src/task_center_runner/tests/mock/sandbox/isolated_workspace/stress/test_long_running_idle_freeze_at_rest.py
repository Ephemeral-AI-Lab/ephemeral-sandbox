"""Idle freeze-at-rest: an open workspace consumes near-zero CPU between calls.

After a single tool_call, idle 30 s. The workspace's cgroup ``cpu.stat``
must report essentially no usage_usec growth in that window — proof that
``cgroup.freeze`` is in effect between tool_calls.
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


pytestmark = [pytest.mark.asyncio, pytest.mark.live_e2e_soak]


async def _usage_usec(sandbox_id: str, agent: str) -> int:
    res = await _iws_rpc.shell(
        sandbox_id, agent,
        "awk '$1==\"usage_usec\" {print $2}' "
        "/sys/fs/cgroup/$(awk -F: '{print $3}' /proc/self/cgroup | head -1)/cpu.stat "
        "2>/dev/null || echo 0",
    )
    text = (res.get("stdout", "") or "").strip().splitlines()
    last = text[-1] if text else "0"
    return int(last) if last.strip().isdigit() else 0


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(420)
async def test_long_running_idle_freeze_at_rest(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    opened = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_REPO_DIR,
    )
    assert opened.get("success") is True, opened
    try:
        # Warm the cgroup state.
        warm = await _iws_rpc.shell(sandbox_id, "agent-A", "echo warm")
        assert warm.get("success") is True, warm
        before = await _usage_usec(sandbox_id, "agent-A")
        # Idle 30 s — between tool_calls the freezer should hold; only the
        # next read fires a brief unfreeze.
        await asyncio.sleep(30.0)
        after = await _usage_usec(sandbox_id, "agent-A")

        # Allow 200 ms (200_000 usec) of accounting noise — that's headroom
        # for the brief unfreeze the read tool_call itself caused.
        delta_us = after - before
        assert delta_us <= 200_000, (
            f"workspace consumed CPU while idle: delta={delta_us}us (~{delta_us/1e6:.3f}s)",
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
