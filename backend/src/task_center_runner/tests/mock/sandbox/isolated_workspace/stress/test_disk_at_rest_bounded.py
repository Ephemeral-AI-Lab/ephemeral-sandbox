"""v2 §19.6: at-rest disk usage ≤ 10 MB for an open, idle workspace.

The upperdir is lazily allocated — opening a workspace and idling for
60 s without writes must NOT balloon the scratch directory. ``du -sb``
of the handle's scratch root must stay under ~10 MiB.
"""

from __future__ import annotations

import asyncio

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
)


pytestmark = [pytest.mark.asyncio, pytest.mark.live_e2e_soak]
_UPPERDIR_OVERHEAD_MAX_BYTES = 10 * 1024 * 1024


async def _du_bytes(sandbox_id: str, path: str) -> int:
    result = await raw_exec(
        sandbox_id,
        f"du -sb {path} 2>/dev/null | awk '{{print $1}}'",
        cwd="/", timeout=60,
    )
    text = (getattr(result, "stdout", "") or "").strip()
    return int(text) if text.isdigit() else 0


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(360)
async def test_disk_at_rest_bounded(iws_clean_sandbox, iws_audit_jsonl) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    opened = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert opened.get("success") is True, opened
    try:
        jsonl = await iws_audit_jsonl()
        enters = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_enter",
        )
        handle_id = next(
            (row.get("payload") or {}).get("handle_id") for row in enters
            if (row.get("payload") or {}).get("agent_id") == "agent-A"
        )
        assert handle_id, enters
        scratch_root = await iws_scratch_root(sandbox_id)
        assert scratch_root, "scratch_root must be discoverable"

        # 60-s idle window. No writes, no shells — just observe.
        await asyncio.sleep(60.0)

        scratch_size = await _du_bytes(
            sandbox_id, f"{scratch_root}/{handle_id}",
        )
        assert scratch_size <= _UPPERDIR_OVERHEAD_MAX_BYTES, (
            f"at-rest scratch ballooned to {scratch_size} bytes (cap "
            f"{_UPPERDIR_OVERHEAD_MAX_BYTES}); upperdir was pre-sized?",
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
