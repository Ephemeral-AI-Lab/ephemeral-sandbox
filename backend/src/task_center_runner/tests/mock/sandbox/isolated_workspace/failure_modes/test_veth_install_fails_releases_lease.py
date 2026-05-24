"""R6: a veth install failure releases the lease — no IP leak, no zombie.

Failure injection at the ``install_veth`` phase. Because the failure
fires BEFORE ``IPPool.allocate``, no IP gets reserved on the failing
path; the post-rollback enter must therefore succeed with a fresh IP
allocation and the OCC flush path still works (lease released).
"""

from __future__ import annotations

import pytest

from sandbox.host.daemon_client import call_daemon_api
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
async def test_veth_install_fails_releases_lease(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    await set_daemon_env(
        sandbox_id,
        pairs={"EOS_ISOLATED_WORKSPACE_TEST_FAIL_AT": "install_veth"},
        layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    try:
        resp = await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert resp.get("success") is False, resp
        err = resp.get("error", {})
        assert err.get("kind") == "setup_failed", err
        assert (err.get("details") or {}).get("failed_step") == "install_veth", err

        # Lease was released → an unrelated OCC flush still works.
        flush = await call_daemon_api(
            sandbox_id, "api.overlay.flush", {}, timeout=30,
        )
        assert flush.get("success") is not False, flush
    finally:
        await clear_daemon_env(
            sandbox_id,
            keys=["EOS_ISOLATED_WORKSPACE_TEST_FAIL_AT"],
            layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )

    # Post-rollback enter succeeds; the IP pool isn't leaking.
    recover = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert recover.get("success") is True, recover
    await _iws_rpc.exit_(sandbox_id, "agent-A")
