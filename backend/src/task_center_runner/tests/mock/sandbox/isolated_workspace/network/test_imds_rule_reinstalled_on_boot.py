"""R13: IMDS drop rule re-installs idempotently on daemon boot.

Delete the iws filter table host-side, restart daemon, verify the IMDS
drop reappears — observable via a second ``test_imds_dropped`` repro.
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
async def test_imds_rule_reinstalled_on_boot(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    # Bootstrap so initial rules are installed.
    await _iws_rpc.enter(sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    await _iws_rpc.exit_(sandbox_id, "agent-A")
    await raw_exec(
        sandbox_id,
        "nft delete table inet eos_iws_filter 2>/dev/null || true",
        cwd="/", timeout=10,
    )

    await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    try:
        listing = await raw_exec(
            sandbox_id,
            "nft list table inet eos_iws_filter 2>/dev/null || echo MISSING",
            cwd="/", timeout=10,
        )
        text = getattr(listing, "stdout", "") or ""
        assert "MISSING" not in text, text
        assert "169.254.169.254" in text, ("IMDS drop rule absent after reboot", text)
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
