"""Open isolated ws is a side channel — the default flow keeps working.

Same agent has an open isolated workspace AND issues a default
``api.write_file`` on a different path. Both succeed. The isolated ws's
view of the world stays pinned at the snapshot-at-enter manifest; the
default flow advances the layerstack tip normally.

This proves the two modes are independent for the same agent: opening an
isolated workspace does not lock the agent out of the default OCC path
(would otherwise be a silent UX regression where collaborators inside an
isolated debug session can't ship a fix).
"""

from __future__ import annotations

import uuid

import pytest

from sandbox.host.daemon_client import call_daemon_api
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
@pytest.mark.timeout(240)
async def test_default_mode_unaffected_during_pinned(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    agent_id = "agent-A"
    token = uuid.uuid4().hex[:12]
    default_path = f"/testbed/default-write-{token}.txt"

    enter_resp = await _iws_rpc.enter(
        sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter_resp.get("success") is True, enter_resp
    pinned_manifest = enter_resp.get("manifest_version")
    try:
        # Default mode: the same agent writes via the standard layerstack
        # flow. The OCC commit MUST land successfully.
        write_resp = await call_daemon_api(
            sandbox_id,
            "api.write_file",
            {"path": default_path, "content": f"default-{token}"},
            timeout=30,
        )
        assert write_resp.get("success") is True, write_resp
        # ``api.write_file`` already commits via OCC and advances the
        # layer-stack tip — that IS the publish for the assertion below.
        # ``api.overlay.flush`` would collapse layer storage and is blocked
        # by the active iws lease (correctly: flush rewrites the storage
        # the iws is pinned to). It isn't needed to prove the "tip advances
        # while iws stays pinned" invariant.

        # Inside the isolated ws, the default write is INVISIBLE
        # (lowerdir was pinned at enter; the new commit lives on the tip).
        inside = await _iws_rpc.read_file(sandbox_id, agent_id, default_path)
        assert inside.get("success") is False, (
            "isolated ws must not see committed writes that landed after enter",
            inside,
        )

        # The isolated ws's manifest_version stays unchanged.
        status = await _iws_rpc.status(sandbox_id, agent_id)
        assert status.get("manifest_version") == pinned_manifest, status
    finally:
        await _iws_rpc.exit_(sandbox_id, agent_id)
