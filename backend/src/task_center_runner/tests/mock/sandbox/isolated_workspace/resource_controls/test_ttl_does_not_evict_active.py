"""TTL counts inactivity, not age — a workspace under load survives.

With ``EOS_ISOLATED_WORKSPACE_TTL_S=2`` and a shell call every ~0.8 s,
``last_activity`` resets past each tool_call. The total session lasts ~5 s
(longer than TTL × 2) yet no ``evicted`` event must appear.
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
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    clear_daemon_env,
    set_daemon_env,
)


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(240)
async def test_ttl_does_not_evict_active(
    iws_clean_sandbox, iws_audit_jsonl,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    await set_daemon_env(
        sandbox_id,
        pairs={"EOS_ISOLATED_WORKSPACE_TTL_S": "2"},
        layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    try:
        opened = await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert opened.get("success") is True, opened
        # 5 short calls spaced 0.8 s — each refreshes last_activity. Session
        # spans ~4 s, well past TTL=2 s; eviction must NOT fire.
        for _ in range(5):
            await asyncio.sleep(0.8)
            tick = await _iws_rpc.shell(sandbox_id, "agent-A", "true")
            assert tick.get("success") is True, tick
        jsonl = await iws_audit_jsonl()
        _iws_invariants.assert_no_event(
            jsonl, "sandbox_isolated_workspace_evicted",
        )
        # status should still report open
        st = await _iws_rpc.status(sandbox_id, "agent-A")
        assert st.get("open") is True, st
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
        await clear_daemon_env(
            sandbox_id,
            keys=["EOS_ISOLATED_WORKSPACE_TTL_S"],
            layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
