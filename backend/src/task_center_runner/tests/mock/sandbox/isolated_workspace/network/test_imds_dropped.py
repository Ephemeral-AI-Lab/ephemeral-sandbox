"""``169.254.169.254`` (IMDS) is dropped on the iws forward chain.

A curl to the IMDS address from inside the workspace must time out — the
filter rule installed in ``IsolatedNetwork._install_static_rules`` drops
``ip daddr 169.254.169.254`` on the forward hook.
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
async def test_imds_dropped(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    try:
        result = await _iws_rpc.shell(
            sandbox_id, "agent-A",
            "curl -s --max-time 2 -o /dev/null -w '%{http_code}' "
            "http://169.254.169.254/ || echo BLOCKED",
        )
        out = (result.get("stdout", "") or "").strip()
        assert "BLOCKED" in out or out == "000", (
            "IMDS must be dropped on the forward chain", result,
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
