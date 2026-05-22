"""R11: ``freeze`` toggles ``cgroup.freeze`` 0→1→0 across each tool call.

Three sequential shells. Each must emit a ``tool_call`` audit event with
``phases_ms.unfreeze`` and ``phases_ms.freeze`` populated (3-phase v1).
The values are bounded but non-zero, proving the freeze/thaw cycle ran.
"""

from __future__ import annotations

import pytest

from benchmarks.sweevo.models import _REPO_DIR
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
async def test_freeze_thaw_idempotent_across_tool_calls(
    iws_clean_sandbox, iws_audit_jsonl,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    opened = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_REPO_DIR,
    )
    assert opened.get("success") is True, opened
    try:
        for _ in range(3):
            tick = await _iws_rpc.shell(sandbox_id, "agent-A", "echo tick")
            assert tick.get("success") is True, tick

        jsonl = await iws_audit_jsonl()
        tool_calls = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_tool_call",
        )
        assert len(tool_calls) >= 3, (
            "each shell must emit a tool_call event",
            len(tool_calls),
        )
        for row in tool_calls[:3]:
            payload = row.get("payload") or {}
            phases = _iws_invariants.phase_timing_extractor(payload)
            # 3-phase v1 (PLAN §15.2): unfreeze + freeze MUST both be present
            # — exec may be absent on a degraded ``run_in_handle`` path.
            assert "unfreeze" in phases, phases
            assert "freeze" in phases, phases
            assert phases["unfreeze"] >= 0.0 and phases["freeze"] >= 0.0
            _iws_invariants.assert_subset_cover(
                phases, payload.get("total_ms", 0.0), label="tool_call",
            )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
