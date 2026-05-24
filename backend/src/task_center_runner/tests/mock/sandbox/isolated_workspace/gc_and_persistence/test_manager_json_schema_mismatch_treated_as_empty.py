"""Schema-mismatch ``manager.json`` → reconciliation falls back to empty.

If the on-disk record is from a newer/older daemon, the safe behaviour is to
treat the in-memory state as empty and rely on the naming-convention sweep
to clean up by-pattern. The daemon must log the mismatch (visible via
``logger.warning``) and must NOT crash.
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
    daemon_kill_and_respawn,
    iws_scratch_root,
    read_manager_json,
    write_manager_json,
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
async def test_manager_json_schema_mismatch_treated_as_empty(
    iws_clean_sandbox,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    # Bootstrap the daemon so the scratch_root exists.
    await _iws_rpc.enter(sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    await _iws_rpc.exit_(sandbox_id, "agent-A")

    scratch = await iws_scratch_root(sandbox_id)
    assert scratch, "scratch_root not discovered"
    # Inject a manager.json from the future.
    bogus = json.dumps({"schema_version": 999, "handles": [{"handle_id": "ghost"}]})
    await write_manager_json(sandbox_id, scratch_root=scratch, payload=bogus)

    await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

    # After restart + GC, the persisted file is empty (or rewritten with
    # schema_version=1 + empty handles). Either is acceptable per the
    # plan — what matters is that no ghost handle survives.
    enter_resp = await _iws_rpc.enter(
        sandbox_id, "agent-B", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter_resp.get("success") is True, enter_resp
    try:
        raw = await read_manager_json(sandbox_id, scratch_root=scratch)
        assert raw, "manager.json missing after enter"
        data = json.loads(raw)
        assert data.get("schema_version") == 1, data
        ids = [h.get("handle_id") for h in data.get("handles") or []]
        assert "ghost" not in ids, data
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-B")
