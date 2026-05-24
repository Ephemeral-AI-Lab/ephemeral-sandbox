"""``enter`` ``phases_ms`` covers the expected 7-phase key set.

Pins the per-phase boundary contract from PLAN §14: every successful enter
emits a phases_ms whose key set is a subset of the documented set, every
value is non-negative, SUBSET-COVER holds.
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
_ALLOWED = {
    "prepare_snapshot", "spawn_ns_holder", "open_ns_fds", "install_veth",
    "mount_overlay", "configure_dns", "create_cgroup",
}


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(180)
async def test_enter_phase_breakdown_complete(
    iws_clean_sandbox, iws_audit_jsonl, iws_capability_probe,
) -> None:
    gate_or_skip(iws_capability_probe, "has_mount_overlay")
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    opened = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert opened.get("success") is True, opened
    try:
        jsonl = await iws_audit_jsonl()
        for payload in event_payloads(jsonl, "sandbox_isolated_workspace_enter"):
            phases = _iws_invariants.phase_timing_extractor(payload)
            _iws_invariants.assert_phases_within_keys(
                phases, _ALLOWED, label="enter",
            )
            for value in phases.values():
                assert isinstance(value, (int, float)) and value >= 0.0
            _iws_invariants.assert_subset_cover(
                phases, payload.get("total_ms", 0.0), label="enter",
            )
            # Required: mount_overlay key must be present when capability
            # probe reported True (we gated above) — its absence would
            # indicate the stub still raises NotImplementedError.
            assert "mount_overlay" in phases, phases
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
