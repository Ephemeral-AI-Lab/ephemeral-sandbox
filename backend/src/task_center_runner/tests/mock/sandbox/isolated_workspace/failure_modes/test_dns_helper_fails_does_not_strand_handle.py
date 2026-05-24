"""DNS configuration failure rolls back cleanly — handle isn't stranded in ``exiting``.

Failure injection at ``configure_dns``. After the rollback the state
machine ends at "stopped" (no in-memory handle, no persisted row); a
fresh ``status`` returns ``open=False``.
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
async def test_dns_helper_fails_does_not_strand_handle(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    await set_daemon_env(
        sandbox_id,
        pairs={"EOS_ISOLATED_WORKSPACE_TEST_FAIL_AT": "configure_dns"},
        layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    try:
        resp = await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert resp.get("success") is False, resp

        status = await _iws_rpc.status(sandbox_id, "agent-A")
        assert status.get("open") is False, status

        leftover = await list_host_eos_iws_resources(sandbox_id)
        # veth + cgroup are reaped by _rollback_partial.
        assert leftover["veth"] == [], leftover
        assert leftover["cgroup"] == [], leftover
    finally:
        await clear_daemon_env(
            sandbox_id,
            keys=["EOS_ISOLATED_WORKSPACE_TEST_FAIL_AT"],
            layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
