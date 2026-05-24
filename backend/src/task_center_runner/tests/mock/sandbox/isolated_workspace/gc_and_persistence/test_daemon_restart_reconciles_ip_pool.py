"""IP-pool reconciliation: persisted IPs stay reserved across restart.

Persisting the IP allocation across daemon crashes is the only way to
guarantee a fresh ``enter`` won't double-allocate an IP that an
in-flight orphan (zombie holder process, half-torn veth) still owns.
After restart the pool MUST mark every persisted ``ns_ip`` as taken;
a fresh enter for a different agent therefore picks the next-available
slot rather than re-using the persisted one.
"""

from __future__ import annotations

import json

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
    daemon_kill_and_respawn,
    iws_scratch_root,
    read_manager_json,
    write_manager_json,
)


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(300)
async def test_daemon_restart_reconciles_ip_pool(
    iws_clean_sandbox, iws_audit_jsonl
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter_a = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter_a.get("success") is True, enter_a

    # The audit log records agent-A's ns_ip.
    jsonl = await iws_audit_jsonl()
    enters = _iws_invariants.events_of_type(
        jsonl, "sandbox_isolated_workspace_enter",
    )
    a_payload = next(
        (row.get("payload") or {}) for row in enters
        if (row.get("payload") or {}).get("agent_id") == "agent-A"
    )
    a_ip = a_payload.get("ns_ip")
    assert isinstance(a_ip, str) and a_ip, a_payload

    # Preserve agent-A in manager.json so the post-restart pool sees it.
    scratch = await iws_scratch_root(sandbox_id)
    persisted = json.loads(await read_manager_json(sandbox_id, scratch_root=scratch))

    await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

    # Restore the manager.json the bootstrap re-persisted — the bootstrap
    # call_daemon_api enters + exits a throwaway agent, which clears
    # agent-A's record. Re-inject it.
    await write_manager_json(
        sandbox_id, scratch_root=scratch, payload=json.dumps(persisted),
    )
    # Bounce the daemon again so startup_gc sees the re-injected record.
    await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

    enter_b = await _iws_rpc.enter(
        sandbox_id, "agent-B", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter_b.get("success") is True, enter_b
    try:
        jsonl2 = await iws_audit_jsonl()
        # ``iws_audit_jsonl`` truncates only at fixture entry, so all events
        # land in one stream; agent-B's enter comes after both bounces.
        b_enters = [
            row for row in _iws_invariants.events_of_type(
                jsonl2, "sandbox_isolated_workspace_enter"
            )
            if (row.get("payload") or {}).get("agent_id") == "agent-B"
        ]
        assert b_enters, b_enters
        b_ip = (b_enters[-1].get("payload") or {}).get("ns_ip")
        assert b_ip and b_ip != a_ip, (
            "post-restart enter must NOT re-use agent-A's persisted IP",
            a_ip, b_ip,
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-B")
