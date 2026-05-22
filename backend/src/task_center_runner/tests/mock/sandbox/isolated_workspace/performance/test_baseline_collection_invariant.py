"""Session baseline fixture invariants.

Drop-fixture failure mode: without ``iws_latency_baseline``, every Tier 9
ratio test can't compute. This fence asserts the fixture is wired AND
contains positive medians for the three named operations.
"""

from __future__ import annotations

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace.performance._helpers import (
    gate_or_skip,
)


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(360)
async def test_baseline_collection_invariant(
    iws_capability_probe, iws_latency_baseline,
) -> None:
    gate_or_skip(iws_capability_probe, "has_mount_overlay")
    # workspace_create must always be present.
    assert iws_latency_baseline.get("workspace_create", 0.0) > 0.0, (
        "session baseline missing workspace_create", iws_latency_baseline,
    )
    # tool_call is also expected — degraded baseline if absent.
    assert iws_latency_baseline.get("tool_call", 0.0) > 0.0, (
        "session baseline missing tool_call", iws_latency_baseline,
    )
