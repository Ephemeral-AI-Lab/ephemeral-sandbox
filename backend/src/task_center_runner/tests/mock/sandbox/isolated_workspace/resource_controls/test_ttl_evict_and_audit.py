"""Idle TTL eviction: stale handles are reaped + audited as ``reason=ttl``.

Sets ``EOS_ISOLATED_WORKSPACE_TTL_S=1`` then sleeps 6 s — the background
``_ttl_loop`` MUST fire at least once in that window and emit an
``isolated_workspace_evicted`` event with ``reason=ttl``. After eviction a
fresh enter() succeeds with the same agent id (the slot is released).
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
async def test_ttl_evict_and_audit(iws_clean_sandbox, iws_audit_jsonl) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    await set_daemon_env(
        sandbox_id,
        pairs={"EOS_ISOLATED_WORKSPACE_TTL_S": "1"},
        layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    try:
        opened = await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert opened.get("success") is True, opened
        # TTL=1s, tick=0.5s; 6s wait is comfortably > 2 sweeps past expiry.
        await asyncio.sleep(6.0)
        jsonl = await iws_audit_jsonl()
        evicted = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_evicted",
        )
        assert evicted, (
            "TTL sweep must emit at least one evicted event",
            evicted,
        )
        ttl_evicted = [
            row for row in evicted
            if (row.get("payload") or {}).get("reason") == "ttl"
        ]
        assert ttl_evicted, ("expected reason=ttl in evicted payload", evicted)

        # Slot released — re-enter as the same agent id succeeds.
        reopen = await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert reopen.get("success") is True, reopen
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
        await clear_daemon_env(
            sandbox_id,
            keys=["EOS_ISOLATED_WORKSPACE_TTL_S"],
            layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
