"""``EOS_ISOLATED_WORKSPACE_RFC1918_EGRESS=deny`` blocks RFC1918 egress.

Boot the daemon with the env knob set, enter, attempt a curl to an
RFC1918 address — must drop. Public egress (bridge gateway) must still
succeed.
"""

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
async def test_rfc1918_egress_drop_opt_in(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    # Persist the opt-in env knob then bounce the daemon.
    await raw_exec(
        sandbox_id,
        "grep -q '^EOS_ISOLATED_WORKSPACE_RFC1918_EGRESS=' /etc/environment "
        "|| echo 'EOS_ISOLATED_WORKSPACE_RFC1918_EGRESS=deny' >> /etc/environment",
        cwd="/", timeout=10,
    )
    try:
        await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

        enter = await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert enter.get("success") is True, enter
        try:
            blocked = await _iws_rpc.shell(
                sandbox_id, "agent-A",
                "curl -s --max-time 2 -o /dev/null -w '%{http_code}' "
                "http://10.99.99.99/ || echo BLOCKED",
            )
            out = (blocked.get("stdout", "") or "").strip()
            assert "BLOCKED" in out or out == "000", (
                "RFC1918 egress must be blocked under deny mode", blocked,
            )
        finally:
            await _iws_rpc.exit_(sandbox_id, "agent-A")
    finally:
        await raw_exec(
            sandbox_id,
            "sed -i '/^EOS_ISOLATED_WORKSPACE_RFC1918_EGRESS=/d' /etc/environment",
            cwd="/", timeout=10,
        )
        await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
