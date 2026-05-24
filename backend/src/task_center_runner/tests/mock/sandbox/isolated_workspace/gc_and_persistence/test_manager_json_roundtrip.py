"""Persisted ``manager.json`` shape matches the daemon contract.

After an ``enter`` the file should:
- carry ``schema_version=1``
- contain exactly one row for the open handle
- carry the row's ``lease_id``, ``ns_ip``, and ``cgroup_path`` as non-empty strings
- NEVER expose raw FDs (``ns_fds`` key is a transient runtime field)
"""

from __future__ import annotations

import json

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    iws_scratch_root,
    read_manager_json,
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
async def test_manager_json_roundtrip(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    agent_id = "agent-A"
    enter_resp = await _iws_rpc.enter(
        sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter_resp.get("success") is True, enter_resp
    try:
        scratch = await iws_scratch_root(sandbox_id)
        assert scratch, "scratch_root not discovered after enter"
        raw = await read_manager_json(sandbox_id, scratch_root=scratch)
        assert raw, f"manager.json missing under {scratch}"
        data = json.loads(raw)
        assert data.get("schema_version") == 1, data
        handles = data.get("handles") or []
        assert len(handles) == 1, handles
        row = handles[0]
        for key in ("lease_id", "ns_ip", "cgroup_path"):
            value = row.get(key)
            assert isinstance(value, str) and value, (key, row)
        # transient FDs must not have been persisted.
        assert "ns_fds" not in row, row
    finally:
        await _iws_rpc.exit_(sandbox_id, agent_id)
