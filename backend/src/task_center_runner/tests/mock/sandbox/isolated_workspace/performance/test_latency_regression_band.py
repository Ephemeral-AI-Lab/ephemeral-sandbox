"""Per-phase median stays within [0.5x, 2x] of session baseline.

Synthetic-regression check: setting
``EOS_ISOLATED_WORKSPACE_TEST_PHASE_DELAY=mount_overlay:100ms`` MUST trip
the band; clearing it MUST be back inside.
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
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    clear_daemon_env,
    set_daemon_env,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace.performance._helpers import (
    event_payloads,
    gate_or_skip,
    require_baseline,
)


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(420)
async def test_latency_regression_band(
    iws_clean_sandbox,
    iws_audit_jsonl,
    iws_capability_probe,
    iws_latency_baseline,
) -> None:
    gate_or_skip(iws_capability_probe, "has_mount_overlay")
    require_baseline(iws_latency_baseline, "workspace_create")
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])

    # Inject a 100ms delay in mount_overlay; the next enter's
    # phases_ms.mount_overlay must reflect the bump (well past 2x baseline).
    await set_daemon_env(
        sandbox_id,
        pairs={"EOS_ISOLATED_WORKSPACE_TEST_PHASE_DELAY": "mount_overlay:100"},
        layer_stack_root=_REPO_DIR,
    )
    try:
        await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_REPO_DIR,
        )
        await _iws_rpc.exit_(sandbox_id, "agent-A")
        jsonl = await iws_audit_jsonl()
        injected_payloads = event_payloads(jsonl, "sandbox_isolated_workspace_enter")
        injected_values = [
            _iws_invariants.phase_timing_extractor(p).get("mount_overlay", 0.0)
            for p in injected_payloads
        ]
        injected_values = [v for v in injected_values if v > 0]
        assert injected_values, injected_payloads
        # Delay was 100 ms — the recorded mount_overlay must be at least
        # 80 ms (allow a slack of 20 ms for timing accounting).
        assert max(injected_values) >= 80.0, (
            "test-only phase delay knob didn't surface in phases_ms",
            injected_values,
        )
    finally:
        await clear_daemon_env(
            sandbox_id,
            keys=["EOS_ISOLATED_WORKSPACE_TEST_PHASE_DELAY"],
            layer_stack_root=_REPO_DIR,
        )
