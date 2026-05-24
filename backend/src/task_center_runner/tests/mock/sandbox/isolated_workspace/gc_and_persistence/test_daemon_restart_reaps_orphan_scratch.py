"""Orphan scratch directories under the iws scratch root are removed on restart."""

from __future__ import annotations

import pytest

from sandbox.api import raw_exec
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    daemon_kill_and_respawn,
    iws_scratch_root,
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
@pytest.mark.timeout(300)
async def test_daemon_restart_reaps_orphan_scratch(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    # Write something into the upperdir so the orphan dir is non-empty.
    write = await _iws_rpc.shell(
        sandbox_id, "agent-A", "echo orphan > /testbed/scratch.txt",
    )
    assert write.get("success") is True, write

    scratch = await iws_scratch_root(sandbox_id)
    before = await raw_exec(
        sandbox_id,
        f"ls -1 {scratch} 2>/dev/null | grep -v manager.json || true",
        cwd="/",
        timeout=15,
    )
    assert (getattr(before, "stdout", "") or "").strip(), (
        "scratch directory should exist before kill", before,
    )

    await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

    after = await raw_exec(
        sandbox_id,
        f"ls -1 {scratch} 2>/dev/null | grep -v manager.json | grep -v '^agent-restart' || true",
        cwd="/",
        timeout=15,
    )
    leftover = (getattr(after, "stdout", "") or "").strip()
    assert leftover == "", (
        "daemon restart must rmtree orphan scratch dirs", leftover,
    )
