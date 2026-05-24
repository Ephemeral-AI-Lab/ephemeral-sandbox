"""N1: a wedged setup phase raises ``setup_timeout`` and rolls back cleanly.

Failure injection: ``EOS_ISOLATED_WORKSPACE_TEST_HANG_AT=overlay_mount``
makes the manager raise ``setup_timeout`` at the overlay phase. The
``failed_step`` field must echo back so operators see which phase wedged.
After the rollback, a subsequent ``enter`` with the knob cleared must
succeed — proving the wedge didn't strand any persistent state (lease,
veth, cgroup, scratch).
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
    list_host_eos_iws_resources,
    set_daemon_env,
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
@pytest.mark.timeout(360)
async def test_setup_timeout_wedge(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    await set_daemon_env(
        sandbox_id,
        pairs={"EOS_ISOLATED_WORKSPACE_TEST_HANG_AT": "overlay_mount"},
        layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    try:
        wedged = await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert wedged.get("success") is False, wedged
        err = wedged.get("error", {})
        assert err.get("kind") == "setup_timeout", err
        details = err.get("details") or {}
        assert details.get("failed_step") == "overlay_mount", details

        # No persistent state left behind.
        leftover = await list_host_eos_iws_resources(sandbox_id)
        assert leftover["veth"] == [], leftover
    finally:
        await clear_daemon_env(
            sandbox_id,
            keys=["EOS_ISOLATED_WORKSPACE_TEST_HANG_AT"],
            layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )

    # With the knob cleared, enter succeeds.
    recover = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert recover.get("success") is True, recover
    await _iws_rpc.exit_(sandbox_id, "agent-A")
