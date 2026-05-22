"""Every workspace-lifecycle audit event respects SUBSET-COVER.

A daemon-wide audit-bus sweep: for each emitted event with phases_ms, the
sum of timings <= total_ms + epsilon (PLAN §14). Pure assertion on the
session's recorded events; no capability gate (gates are about whether
the EMITTER ran, not about whether sums add up).
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
_LIFECYCLE_TYPES = (
    "sandbox_isolated_workspace_enter",
    "sandbox_isolated_workspace_exit",
    "sandbox_isolated_workspace_evicted",
    "sandbox_isolated_workspace_tool_call",
    "sandbox_isolated_workspace_gc_orphan",
)


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(240)
async def test_phases_ms_subset_cover_invariant(
    iws_clean_sandbox, iws_audit_jsonl,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    opened = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_REPO_DIR,
    )
    assert opened.get("success") is True, opened
    await _iws_rpc.shell(sandbox_id, "agent-A", "echo hi")
    await _iws_rpc.exit_(sandbox_id, "agent-A")

    jsonl = await iws_audit_jsonl()
    inspected = 0
    for event_type in _LIFECYCLE_TYPES:
        for row in _iws_invariants.events_of_type(jsonl, event_type):
            payload = row.get("payload") or {}
            phases = _iws_invariants.phase_timing_extractor(payload)
            if not phases:
                continue
            inspected += 1
            _iws_invariants.assert_subset_cover(
                phases, payload.get("total_ms", 0.0), label=event_type,
            )
    assert inspected > 0, "no lifecycle events carried phases_ms"
