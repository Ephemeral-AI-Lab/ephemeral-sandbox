"""``exit`` ``phases_ms`` covers the documented 5-phase key set.

PLAN §14 key set: ``{kill_holder, teardown_veth, release_snapshot,
cgroup_rmdir, rmtree_scratch}``. Back-compat fields ``lifetime_s`` and
``upperdir_bytes_discarded`` still present.
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
    "kill_holder", "teardown_veth", "release_snapshot",
    "cgroup_rmdir", "rmtree_scratch",
}


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(180)
async def test_exit_phase_breakdown_complete(
    iws_clean_sandbox, iws_audit_jsonl, iws_capability_probe,
) -> None:
    gate_or_skip(iws_capability_probe, "has_mount_overlay")
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    opened = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert opened.get("success") is True, opened
    await _iws_rpc.exit_(sandbox_id, "agent-A")

    jsonl = await iws_audit_jsonl()
    payloads = event_payloads(jsonl, "sandbox_isolated_workspace_exit")
    assert payloads, "expected at least one exit event"
    for payload in payloads:
        phases = _iws_invariants.phase_timing_extractor(payload)
        _iws_invariants.assert_phases_within_keys(
            phases, _ALLOWED, label="exit",
        )
        _iws_invariants.assert_subset_cover(
            phases, payload.get("total_ms", 0.0), label="exit",
        )
        # Back-compat: lifetime_s + upperdir_bytes_discarded MUST stay.
        assert "lifetime_s" in payload, payload
        assert "upperdir_bytes_discarded" in payload, payload
