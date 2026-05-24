"""External raw ICMP echo to the workspace ns_ip gets no reply."""

from __future__ import annotations

import pytest

from sandbox.api import raw_exec
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
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(180)
async def test_external_inbound_icmp_rejected(
    iws_clean_sandbox, iws_audit_jsonl
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    try:
        jsonl = await iws_audit_jsonl()
        ns_ip = next(
            (row.get("payload") or {}).get("ns_ip")
            for row in _iws_invariants.events_of_type(
                jsonl, "sandbox_isolated_workspace_enter"
            )
            if (row.get("payload") or {}).get("agent_id") == "agent-A"
        )
        assert ns_ip

        probe = await raw_exec(
            sandbox_id,
            f"unshare -n -- bash -c 'ping -c 1 -W 2 {ns_ip} >/dev/null 2>&1; echo $?'",
            cwd="/", timeout=15,
        )
        rc = (getattr(probe, "stdout", "") or "").strip()
        assert rc != "0", (
            "external netns must not get ICMP replies from ws", probe,
        )
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
