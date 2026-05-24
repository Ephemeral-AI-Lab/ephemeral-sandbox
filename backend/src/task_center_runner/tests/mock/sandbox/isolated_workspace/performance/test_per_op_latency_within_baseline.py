"""Per-op latency stays within the HYBRID baseline + budget envelope.

For each of ``{workspace_create, tool_call}``, drive 5 in-test samples
and assert ``LatencyBudget.assert_stable_and_within_budget`` — median in
[0.3x, 3x] of session baseline, p95 ≤ budget × 1.5 when committed.
"""

from __future__ import annotations

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc
from task_center_runner.tests.mock.sandbox.isolated_workspace.performance._helpers import (
    build_budget,
    event_payloads,
    gate_or_skip,
    require_baseline,
)


pytestmark = pytest.mark.asyncio
_SAMPLES = 5


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(360)
async def test_per_op_latency_within_baseline(
    iws_clean_sandbox,
    iws_audit_jsonl,
    iws_capability_probe,
    iws_latency_baseline,
    iws_latency_budget_path,
) -> None:
    gate_or_skip(iws_capability_probe, "has_mount_overlay")
    require_baseline(iws_latency_baseline, "workspace_create")
    require_baseline(iws_latency_baseline, "tool_call")
    budget = build_budget(iws_latency_baseline, iws_latency_budget_path)

    for _ in range(_SAMPLES):
        opened = await _iws_rpc.enter(
            sandbox_id := str(iws_clean_sandbox["sandbox_id"]),
            "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert opened.get("success") is True, opened
        await _iws_rpc.shell(sandbox_id, "agent-A", "true")
        await _iws_rpc.exit_(sandbox_id, "agent-A")

    jsonl = await iws_audit_jsonl()
    enter_totals = [
        float(p.get("total_ms") or 0.0)
        for p in event_payloads(jsonl, "sandbox_isolated_workspace_enter")
        if p.get("total_ms")
    ]
    tool_totals = [
        float(p.get("total_ms") or 0.0)
        for p in event_payloads(jsonl, "sandbox_isolated_workspace_tool_call")
        if p.get("total_ms")
    ]
    budget.assert_stable_and_within_budget(
        enter_totals, op_name="workspace_create",
    )
    budget.assert_stable_and_within_budget(tool_totals, op_name="tool_call")
