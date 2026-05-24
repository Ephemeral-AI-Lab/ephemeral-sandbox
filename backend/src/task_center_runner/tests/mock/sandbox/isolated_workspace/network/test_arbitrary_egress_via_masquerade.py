"""Driver #4: workspace can reach the daemon-host over a MASQUERADE'd v4 path.

The bridge gateway (``10.244.0.1``) is the daemon host's own bridge
interface; a curl from the workspace to that IP exercises the same
postrouting path that arbitrary external traffic would.  If the
masquerade rule is missing, the SYN would be dropped at the host's
forwarding decision (10.244/24 with no NAT has nowhere to go).
"""

from __future__ import annotations

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(180)
async def test_arbitrary_egress_via_masquerade(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    try:
        # Reaching the bridge gateway proves both:
        #   1. The veth + masquerade postrouting is wired.
        #   2. conntrack is tracking the connection (return path works).
        probe = await _iws_rpc.shell(
            sandbox_id, "agent-A",
            "ping -c 1 -W 3 10.244.0.1 >/dev/null 2>&1 && echo OK || echo FAIL",
        )
        assert probe.get("success") is True, probe
        assert "OK" in (probe.get("stdout", "") or ""), (
            "egress to bridge gateway must succeed via masquerade", probe,
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
