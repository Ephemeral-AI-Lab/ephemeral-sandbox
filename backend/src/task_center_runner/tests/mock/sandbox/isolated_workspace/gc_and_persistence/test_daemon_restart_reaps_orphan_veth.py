"""Orphan veth devices from a SIGKILLed daemon are reaped on restart.

Enter; SIGKILL the daemon mid-flight; restart. Host-side ``ip link show``
must not list any ``eos-iws-*`` veth afterwards. The audit log of the new
daemon must carry one ``sandbox_isolated_workspace_gc_orphan`` event with
``kind=veth`` (or ``kind=cgroup`` — the order matters only insofar as the
veth reach is visible after GC, not which event fires first).
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
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    daemon_kill_and_respawn,
    list_host_eos_iws_resources,
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
async def test_daemon_restart_reaps_orphan_veth(
    iws_clean_sandbox, iws_audit_jsonl
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    before = await list_host_eos_iws_resources(sandbox_id)
    assert before["veth"], ("expected veth before kill", before)

    await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

    after = await list_host_eos_iws_resources(sandbox_id)
    assert after["veth"] == [], (
        "daemon restart must reap orphan veth devices", after,
    )

    jsonl = await iws_audit_jsonl()
    gc_events = _iws_invariants.events_of_type(
        jsonl, "sandbox_isolated_workspace_gc_orphan",
    )
    kinds = {row.get("payload", {}).get("kind") for row in gc_events}
    assert "veth" in kinds or "cgroup" in kinds, (
        "expected at least one gc_orphan event from the restart sweep", gc_events,
    )
