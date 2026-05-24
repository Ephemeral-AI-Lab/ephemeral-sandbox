"""Re-enter after exit yields a brand-new handle (no upperdir carry-over).

The handle_id changes, and a file written before exit is GONE on the next
enter — the upperdir is fully discarded (PLAN §5 Tier 5 ↔ Tier 2 cross).
"""

from __future__ import annotations

import uuid

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import (
    _iws_invariants,
    _iws_rpc,
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
async def test_re_enter_after_exit_gets_fresh_handle(
    iws_clean_sandbox, iws_audit_jsonl,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    token = uuid.uuid4().hex[:8]
    scratch = f"/testbed/scratch-{token}.txt"

    first = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert first.get("success") is True, first
    write = await _iws_rpc.shell(
        sandbox_id, "agent-A", f"echo first > {scratch}",
    )
    assert write.get("success") is True, write
    await _iws_rpc.exit_(sandbox_id, "agent-A")

    second = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert second.get("success") is True, second
    try:
        # Scratch file MUST be gone — the upperdir was discarded.
        peek = await _iws_rpc.shell(
            sandbox_id, "agent-A", f"cat {scratch} 2>&1 || echo MISSING",
        )
        assert "MISSING" in (peek.get("stdout", "") or ""), peek

        # Two distinct handle_ids in the audit log for agent-A's two enters.
        jsonl = await iws_audit_jsonl()
        enters = _iws_invariants.events_of_type(
            jsonl, "sandbox_isolated_workspace_enter",
        )
        handle_ids = [
            (row.get("payload") or {}).get("handle_id") for row in enters
            if (row.get("payload") or {}).get("agent_id") == "agent-A"
        ]
        assert len(set(handle_ids)) == len(handle_ids), (
            "each enter must mint a fresh handle_id", handle_ids,
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
