"""Overlay mount failure releases the lease and surfaces the failed step.

Failure injection: ``EOS_ISOLATED_WORKSPACE_TEST_FAIL_AT=overlay_mount``.
Enter must return ``setup_failed`` with ``failed_step=overlay_mount``;
manager state is empty afterwards (no leftover ``_by_agent`` entry).
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
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(360)
async def test_overlay_mount_fails(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    await set_daemon_env(
        sandbox_id,
        pairs={"EOS_ISOLATED_WORKSPACE_TEST_FAIL_AT": "overlay_mount"},
        layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    try:
        resp = await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert resp.get("success") is False, resp
        err = resp.get("error", {})
        assert err.get("kind") == "setup_failed", err
        assert (err.get("details") or {}).get("failed_step") == "overlay_mount", err

        # status must report no open workspace.
        status = await _iws_rpc.status(sandbox_id, "agent-A")
        assert status.get("open") is False, status
    finally:
        await clear_daemon_env(
            sandbox_id,
            keys=["EOS_ISOLATED_WORKSPACE_TEST_FAIL_AT"],
            layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
