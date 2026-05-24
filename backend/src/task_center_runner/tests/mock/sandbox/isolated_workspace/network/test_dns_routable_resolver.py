"""When the lowerdir resolv.conf points at a routable resolver, DNS works.

If ``/etc/resolv.conf`` already names a resolver reachable via the bridge
(e.g., the daemon host's resolver), no fallback substitution should fire
— DNS lookups complete via the normal stub resolver path.
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
async def test_dns_routable_resolver(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    try:
        # The image installs a routable resolv.conf at /etc/resolv.conf;
        # if the daemon's configure_dns left it intact, getent succeeds.
        result = await _iws_rpc.shell(
            sandbox_id, "agent-A",
            "getent hosts cloudflare.com >/dev/null 2>&1 && echo OK || echo FAIL",
        )
        assert "OK" in (result.get("stdout", "") or ""), (
            "in-ws DNS lookup must succeed on a routable resolver", result,
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
