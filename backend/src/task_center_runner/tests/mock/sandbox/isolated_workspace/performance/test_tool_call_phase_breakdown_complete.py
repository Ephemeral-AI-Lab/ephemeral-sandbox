"""``tool_call`` ``phases_ms`` covers the current single-phase key set.

The runtime performs no hidden pause/resume work between tool calls. ``exec``
is therefore the only tool-call phase; ``argv0`` and ``exit_code`` are still
populated.
"""

from __future__ import annotations

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import (
    _iws_invariants,
    _iws_rpc,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace.performance._helpers import (
    event_payloads,
    gate_or_skip,
)


pytestmark = pytest.mark.asyncio
_ALLOWED = {"exec"}


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(180)
async def test_tool_call_phase_breakdown_complete(
    iws_clean_sandbox, iws_audit_jsonl, iws_capability_probe,
) -> None:
    gate_or_skip(iws_capability_probe, "has_mount_overlay")
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    opened = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert opened.get("success") is True, opened
    try:
        await _iws_rpc.shell(sandbox_id, "agent-A", "echo hi")
        jsonl = await iws_audit_jsonl()
        payloads = event_payloads(jsonl, "sandbox_isolated_workspace_tool_call")
        assert payloads, "expected at least one tool_call event"
        for payload in payloads:
            phases = _iws_invariants.phase_timing_extractor(payload)
            _iws_invariants.assert_phases_within_keys(
                phases, _ALLOWED, label="tool_call",
            )
            _iws_invariants.assert_subset_cover(
                phases, payload.get("total_ms", 0.0), label="tool_call",
            )
            assert "argv0" in payload and "exit_code" in payload, payload
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
