"""TOTAL_CAP enforcement: an Nth agent past the cap returns ``quota_exceeded``.

Default cap is 5 (PLAN §11). Override to ``total_cap=2`` for wall-clock
budget — the test still exercises the override path AND the cap boundary.
"""

from __future__ import annotations

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    clear_daemon_env,
    set_daemon_env,
)


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(300)
async def test_total_cap_blocks_new_agent(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    await set_daemon_env(
        sandbox_id,
        pairs={"EOS_ISOLATED_WORKSPACE_TOTAL_CAP": "2"},
        layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    try:
        a = await _iws_rpc.enter(sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
        assert a.get("success") is True, a
        b = await _iws_rpc.enter(sandbox_id, "agent-B", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
        assert b.get("success") is True, b
        c = await _iws_rpc.enter(sandbox_id, "agent-C", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
        assert c.get("success") is False, c
        err = c.get("error", {})
        assert err.get("kind") == "quota_exceeded", err
        details = err.get("details") or {}
        assert details.get("total_cap") == 2, details
    finally:
        for agent in ("agent-A", "agent-B", "agent-C"):
            await _iws_rpc.exit_(sandbox_id, agent)
        await clear_daemon_env(
            sandbox_id,
            keys=["EOS_ISOLATED_WORKSPACE_TOTAL_CAP"],
            layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
