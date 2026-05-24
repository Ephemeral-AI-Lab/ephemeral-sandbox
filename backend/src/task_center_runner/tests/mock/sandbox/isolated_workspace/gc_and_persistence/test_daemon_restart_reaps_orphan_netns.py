"""Orphan named netns (if any) and PID-owned anonymous netns are cleaned up.

The default iws path uses ``unshare --net`` which creates an anonymous
netns owned by the holder process. When the holder dies (kernel reaps PID
on daemon SIGKILL), the netns is reclaimed automatically. This test pins
that property: no ``eos-iws-*`` shows up in ``ip netns list`` after
restart, regardless of whether the daemon adopts named netns later.
"""

from __future__ import annotations

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    daemon_kill_and_respawn,
    list_host_eos_iws_resources,
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
async def test_daemon_restart_reaps_orphan_netns(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter

    await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

    after = await list_host_eos_iws_resources(sandbox_id)
    assert after["netns"] == [], (
        "daemon restart must leave no eos-iws-* named netns; the anonymous "
        "PID-owned netns gets reaped by the kernel along with the holder PID",
        after,
    )
