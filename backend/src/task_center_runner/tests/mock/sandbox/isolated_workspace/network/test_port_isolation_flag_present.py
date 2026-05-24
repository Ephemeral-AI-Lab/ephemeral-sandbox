"""Host-side bridge slave shows ``isolated: on, mcast_flood: off``.

These two flags are what prevent inter-port traffic at the bridge; they
sit alongside (not behind) the iws nft rules so dropping
``bridge-nf-call-iptables`` cannot accidentally re-enable peer reach.
"""

from __future__ import annotations

import pytest

from sandbox.api import raw_exec
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
async def test_port_isolation_flag_present(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    try:
        # The host-side iws veth is named eos-iws-<short>h. Detect it from
        # `ip link` then read its bridge flags.
        listing = await raw_exec(
            sandbox_id,
            "host=$(ip -o link show 2>/dev/null | awk -F': ' '{print $2}' "
            "| awk '{print $1}' | sed 's/@.*//' | grep '^eos-iws-' "
            "| grep 'h$' | head -1); "
            "[ -z \"$host\" ] && { echo MISSING; exit 0; }; "
            "bridge -d link show dev \"$host\" 2>/dev/null || echo NOBRIDGE",
            cwd="/", timeout=15,
        )
        text = getattr(listing, "stdout", "") or ""
        assert "MISSING" not in text, text
        if "NOBRIDGE" in text:
            pytest.skip("bridge(8) not installed in this image; flag check skipped")
        assert "isolated on" in text, ("port isolation flag missing", text)
        assert "mcast_flood off" in text, ("mcast flood not disabled", text)
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
