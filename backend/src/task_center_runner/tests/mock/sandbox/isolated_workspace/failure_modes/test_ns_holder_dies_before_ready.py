"""``ns_holder`` exits before signalling ``ready`` → enter() fails cleanly.

Failure injection: ``EOS_ISOLATED_WORKSPACE_TEST_HOLDER_CRASH=true``
forces the holder to ``sys.exit(7)`` immediately after ``ns-up``. The
parent's read of the readiness pipe sees EOF before ``ready`` → enter()
returns ``setup_failed``. No orphan veth, cgroup, or scratch dir remains.
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
async def test_ns_holder_dies_before_ready(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    await set_daemon_env(
        sandbox_id,
        pairs={"EOS_ISOLATED_WORKSPACE_TEST_HOLDER_CRASH": "true"},
        layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    try:
        resp = await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert resp.get("success") is False, resp
        err = resp.get("error", {})
        assert err.get("kind") in {"setup_failed", "setup_timeout"}, err

        leftover = await list_host_eos_iws_resources(sandbox_id)
        assert leftover["veth"] == [], leftover
        assert leftover["cgroup"] == [], leftover
    finally:
        await clear_daemon_env(
            sandbox_id,
            keys=["EOS_ISOLATED_WORKSPACE_TEST_HOLDER_CRASH"],
            layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
