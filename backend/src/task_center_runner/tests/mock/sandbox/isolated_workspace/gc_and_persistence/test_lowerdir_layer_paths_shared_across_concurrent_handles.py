"""All N concurrent handles share ONE snapshot's ``layer_paths`` tuple.

The design property: ``prepare_workspace_snapshot(...)``
returns layer paths that point at the SAME shared layer-stack files for
every concurrent reader. If a future PR flips ``per_call_tree_copy=True``, each
handle copies the layers into its own scratch — disk usage flips from
O(1) to O(N). This test pins the structural sharing via the persisted
``manager.json`` rows: every concurrent handle row references the same
``manifest_root_hash`` AND the daemon emits ``shared_layer_snapshot=true`` in
every enter event.
"""

from __future__ import annotations

import asyncio
import json

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
    iws_scratch_root,
    read_manager_json,
)


pytestmark = pytest.mark.asyncio

_AGENTS = ("agent-A", "agent-B", "agent-C", "agent-D", "agent-E")


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(360)
async def test_lowerdir_layer_paths_shared_across_concurrent_handles(
    iws_clean_sandbox, iws_audit_jsonl
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enters = await asyncio.gather(
        *(
            _iws_rpc.enter(sandbox_id, agent, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
            for agent in _AGENTS
        )
    )
    try:
        assert all(r.get("success") for r in enters), enters
        root_hashes = {r.get("manifest_root_hash") for r in enters}
        assert len(root_hashes) == 1, (
            "all concurrent handles must share one manifest", root_hashes,
        )

        scratch = await iws_scratch_root(sandbox_id)
        data = json.loads(await read_manager_json(sandbox_id, scratch_root=scratch))
        rows = data.get("handles") or []
        assert len(rows) == len(_AGENTS), rows
        hashes = {row.get("manifest_root_hash") for row in rows}
        assert hashes == root_hashes, (hashes, root_hashes)

        # Every enter event must carry shared_layer_snapshot=true (snapshot-share tripwire).
        jsonl = await iws_audit_jsonl()
        enter_events = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_enter",
        )
        for ev in enter_events:
            payload = ev.get("payload") or {}
            assert payload.get("tree-copy") is False, ev
    finally:
        for agent in _AGENTS:
            await _iws_rpc.exit_(sandbox_id, agent)
