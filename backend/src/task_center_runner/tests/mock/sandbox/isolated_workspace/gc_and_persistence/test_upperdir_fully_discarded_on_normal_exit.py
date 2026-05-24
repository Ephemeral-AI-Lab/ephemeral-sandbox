"""Normal exit discards upperdir completely — no leftover bytes, no row, no lease.

This is the v2 cousin of ``test_upperdir_discarded_on_exit``: it expands
the assertion surface to include the persisted ``manager.json`` row and
the audit's reported ``upperdir_bytes_discarded`` figure.
"""

from __future__ import annotations

import json

import pytest

from sandbox.api import raw_exec
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import (
    _iws_invariants,
    _iws_rpc,
)
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
@pytest.mark.timeout(240)
async def test_upperdir_fully_discarded_on_normal_exit(
    iws_clean_sandbox, iws_audit_jsonl
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    agent_id = "agent-A"
    enter = await _iws_rpc.enter(sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    assert enter.get("success") is True, enter
    # Write a measurable chunk so upperdir_bytes_discarded > 0.
    write = await _iws_rpc.shell(
        sandbox_id, agent_id,
        "dd if=/dev/zero of=/testbed/discard.bin bs=1024 count=100 2>/dev/null",
    )
    assert write.get("success") is True, write

    exit_resp = await _iws_rpc.exit_(sandbox_id, agent_id)
    assert exit_resp.get("success") is True, exit_resp
    assert exit_resp.get("evicted_upperdir_bytes", 0) > 0, exit_resp

    scratch = await iws_scratch_root(sandbox_id)
    # Whole subtree (except manager.json) must be gone.
    find = await raw_exec(
        sandbox_id,
        f"find {scratch} -mindepth 1 -not -name manager.json 2>/dev/null || true",
        cwd="/",
        timeout=20,
    )
    leftover = (getattr(find, "stdout", "") or "").strip()
    assert leftover == "", (
        "normal exit must rmtree every handle directory", leftover,
    )

    # manager.json no longer references the closed handle.
    data = json.loads(await read_manager_json(sandbox_id, scratch_root=scratch))
    assert data.get("handles") == [], data

    jsonl = await iws_audit_jsonl()
    _iws_invariants.assert_audit_sequence(
        jsonl,
        [
            "sandbox_isolated_workspace_enter",
            "sandbox_isolated_workspace_tool_call",
            "sandbox_isolated_workspace_exit",
        ],
    )
