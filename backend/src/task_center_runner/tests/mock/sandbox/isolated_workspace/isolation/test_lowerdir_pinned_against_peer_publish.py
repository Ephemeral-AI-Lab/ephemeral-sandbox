"""A1 (snapshot-at-enter): the workspace view is frozen against peer writes.

Setup: peer publishes ``/testbed/pinned-{uuid}.txt`` with body ``A``.
Agent-A enters the isolated workspace. Peer publishes the SAME path with
body ``B`` (overwrite via the default flow). Inside ws-A, cat returns
``A`` — the lowerdir was pinned at enter time. Exit. Re-enter → now sees
``B`` (the post-publish state) because the new snapshot picks up the
latest manifest.
"""

from __future__ import annotations

import uuid

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    peer_publish_file,
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
@pytest.mark.timeout(240)
async def test_lowerdir_pinned_against_peer_publish(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    token = uuid.uuid4().hex[:12]
    path = f"/testbed/pinned-{token}.txt"

    # Pre-enter publish: body A goes into the default layer stack.
    await peer_publish_file(sandbox_id, path=path, body=f"version-A-{token}")

    agent_id = "agent-A"
    first = await _iws_rpc.enter(sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    assert first.get("success") is True, first
    try:
        # While agent-A's workspace is open, the peer publishes a fresh
        # version. This is the contention that A1 must defend against.
        await peer_publish_file(sandbox_id, path=path, body=f"version-B-{token}")

        readback = await _iws_rpc.read_file(sandbox_id, agent_id, path)
        assert readback.get("success") is True, readback
        body = readback.get("stdout", "")
        assert f"version-A-{token}" in body, (
            "ws-A must see the snapshot-at-enter view, not the post-publish version-B",
            readback,
        )
        assert f"version-B-{token}" not in body, readback
    finally:
        await _iws_rpc.exit_(sandbox_id, agent_id)

    # Re-enter: the new snapshot picks up the latest tip (version-B).
    second = await _iws_rpc.enter(sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    assert second.get("success") is True, second
    try:
        readback = await _iws_rpc.read_file(sandbox_id, agent_id, path)
        assert readback.get("success") is True, readback
        body = readback.get("stdout", "")
        assert f"version-B-{token}" in body, (
            "re-enter must pick up the post-publish state; lease lifetime extended past exit?",
            readback,
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, agent_id)
