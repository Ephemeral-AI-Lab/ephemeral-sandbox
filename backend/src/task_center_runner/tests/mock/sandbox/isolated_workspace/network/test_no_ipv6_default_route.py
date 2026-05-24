"""No IPv6 default route inside the workspace.

The IPv4-only MASQUERADE is the only egress. If a v6 default route slips
through (router advertisement repopulating), traffic could bypass the
filter chain entirely. The ns_holder's ``_purge_ipv6_default_routes``
runs once net-ready fires; ``accept_ra=0`` keeps RAs from regenerating
the route during the lifetime of the workspace.
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
async def test_no_ipv6_default_route(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    try:
        routes = await _iws_rpc.shell(
            sandbox_id, "agent-A",
            "ip -6 route show default 2>/dev/null || true",
        )
        assert (routes.get("stdout", "") or "").strip() == "", (
            "no IPv6 default route should be visible", routes,
        )
        accept_ra = await _iws_rpc.shell(
            sandbox_id, "agent-A",
            "cat /proc/sys/net/ipv6/conf/eth0/accept_ra 2>/dev/null || echo 0",
        )
        assert (accept_ra.get("stdout", "") or "").strip() == "0", (
            "accept_ra must be zeroed inside the workspace", accept_ra,
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
