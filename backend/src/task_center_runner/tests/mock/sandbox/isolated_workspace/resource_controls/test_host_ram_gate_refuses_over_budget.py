"""R6: per-handle upperdir budget exceeding host MemAvailable → host_capacity_exceeded.

Set ``UPPERDIR_BYTES`` larger than any reasonable host's free RAM (4 TB);
enter() must short-circuit at the budget gate with ``required_bytes`` and
``budget_bytes`` populated for the operator.
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


_FOUR_TB = str(4 * 1024**4)


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(240)
async def test_host_ram_gate_refuses_over_budget(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    await set_daemon_env(
        sandbox_id,
        pairs={"EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES": _FOUR_TB},
        layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    try:
        rejected = await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert rejected.get("success") is False, rejected
        err = rejected.get("error", {})
        assert err.get("kind") == "host_capacity_exceeded", err
        details = err.get("details") or {}
        assert "required_bytes" in details and "budget_bytes" in details, details
        assert details["required_bytes"] >= int(_FOUR_TB), details
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
        await clear_daemon_env(
            sandbox_id,
            keys=["EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES"],
            layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
