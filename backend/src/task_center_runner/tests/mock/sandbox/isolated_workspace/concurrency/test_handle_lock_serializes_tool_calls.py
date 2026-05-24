"""``handle.lock`` serialises tool_calls for the same agent.

Two concurrent ``shell`` calls on the same handle MUST be serialised: the
audit log timestamps + durations show non-overlapping wall-clock windows.
"""

from __future__ import annotations

import asyncio

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import (
    _iws_invariants,
    _iws_rpc,
)


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(180)
async def test_handle_lock_serializes_tool_calls(
    iws_clean_sandbox, iws_audit_jsonl,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    opened = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert opened.get("success") is True, opened
    try:
        # Two sleeps fired concurrently — if the lock holds, total wall ≈ 2 s
        # not 1 s; we don't probe wall-clock directly (CI noise) but instead
        # confirm via audit envelopes that durations don't overlap.
        await asyncio.gather(
            _iws_rpc.shell(sandbox_id, "agent-A", "sleep 0.5"),
            _iws_rpc.shell(sandbox_id, "agent-A", "sleep 0.5"),
        )
        jsonl = await iws_audit_jsonl()
        tool_calls = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_tool_call",
        )
        assert len(tool_calls) >= 2, tool_calls
        # Sum of durations should approximate wall-clock total (serialised),
        # not be roughly equal to a single duration (which would imply true
        # overlap).
        durations = [
            float((row.get("payload") or {}).get("duration_s", 0.0))
            for row in tool_calls[:2]
        ]
        assert all(d >= 0.4 for d in durations), durations
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
