"""S1 quota: a second enter() for the same agent returns ``already_open``.

The error envelope must surface ``created_at`` and ``last_activity`` in
``details`` so operators have a diagnostic surface (PLAN §5).
"""

from __future__ import annotations

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(180)
async def test_quota_one_per_agent(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    first = await _iws_rpc.enter(sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    assert first.get("success") is True, first
    try:
        second = await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert second.get("success") is False, second
        err = second.get("error", {})
        assert err.get("kind") == "isolated_workspace_already_open", err
        details = err.get("details") or {}
        # Diagnostic surface from PLAN §5: created_at and last_activity must
        # be present so operators can answer "since when?" without RPC ping.
        assert "created_at" in details, details
        assert "last_activity" in details, details
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
