"""Lowerdir visibility: the workspace mntns sees the pre-enter file view.

Publishes a sentinel file via the DEFAULT flow (api.write_file +
api.overlay.flush) BEFORE entering the isolated workspace, then asserts the
file is readable inside the new mntns. Guards against setns(CLONE_NEWNS) +
fsmount propagation mistakes — if the workspace mntns is privately
propagated and lowerdir paths are inaccessible, the cat would fail.
"""

from __future__ import annotations

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import (
    _iws_invariants,
    _iws_rpc,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    publish_sentinel,
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
@pytest.mark.timeout(180)
async def test_lowerdir_visible_inside_mntns(
    iws_clean_sandbox, iws_audit_jsonl
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    sentinel = await publish_sentinel(sandbox_id)
    agent_id = "agent-A"
    enter_response = await _iws_rpc.enter(
        sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter_response.get("success") is True, enter_response
    try:
        read = await _iws_rpc.read_file(sandbox_id, agent_id, sentinel.path)
        assert read.get("success") is True, read
        assert sentinel.body in read.get("stdout", ""), (sentinel, read)
    finally:
        await _iws_rpc.exit_(sandbox_id, agent_id)

    # ``read_file`` flows through ``run_in_handle``, which emits a
    # ``tool_call`` event between enter and exit.
    jsonl_path = await iws_audit_jsonl()
    _iws_invariants.assert_audit_sequence(
        jsonl_path,
        [
            "sandbox_isolated_workspace_enter",
            "sandbox_isolated_workspace_tool_call",
            "sandbox_isolated_workspace_exit",
        ],
    )
