"""R1 behavioral counterpart to the C2 source-scan fence.

Drives a full ``enter → tool_call → exit`` cycle and asserts that the
upperdir was discarded (no commit to OCC) AND that the layerstack tip
did NOT advance during the isolated cycle.

The strongest form of "OCC primitives never called" would monkeypatch
``CommitQueue.apply`` and count call sites; that needs a test-only hook
in the daemon process which is out of scope for this PR. The form we
ship here is the closest live-observable proxy:

- The iws audit log contains the full enter→tool_call→exit sequence.
- The default-mode ``api.overlay.flush`` BEFORE and AFTER the isolated
  cycle returns the SAME ``manifest_version`` — proving no isolated
  write reached the layerstack.

If a regression wired the iws exit path into OCC commit, the post-cycle
flush manifest would advance.
"""

from __future__ import annotations

import pytest

from sandbox.host.daemon_client import call_daemon_api
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
async def test_full_cycle_never_calls_occ(
    iws_clean_sandbox, iws_audit_jsonl
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])

    # Snapshot the layerstack tip BEFORE the isolated cycle.
    pre = await call_daemon_api(
        sandbox_id, "api.overlay.flush", {}, timeout=30,
    )
    pre_version = (pre or {}).get("manifest_version")

    agent_id = "agent-A"
    enter_resp = await _iws_rpc.enter(
        sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter_resp.get("success") is True, enter_resp
    try:
        # Write something visible inside the isolated ws but it must NOT
        # leak to the layerstack tip.
        shell_resp = await _iws_rpc.shell(
            sandbox_id, agent_id, "echo ok > /testbed/iws_only.txt",
        )
        assert shell_resp.get("success") is True, shell_resp
    finally:
        await _iws_rpc.exit_(sandbox_id, agent_id)

    # Snapshot the layerstack tip AFTER. The isolated cycle must NOT have
    # advanced it — that is the runtime evidence that no OCC commit
    # primitive was reached.
    post = await call_daemon_api(
        sandbox_id, "api.overlay.flush", {}, timeout=30,
    )
    post_version = (post or {}).get("manifest_version")
    assert pre_version == post_version, (
        "layerstack tip advanced during isolated cycle — OCC commit suspected",
        pre_version, post_version,
    )

    jsonl = await iws_audit_jsonl()
    _iws_invariants.assert_audit_sequence(
        jsonl,
        [
            "sandbox_isolated_workspace_enter",
            "sandbox_isolated_workspace_tool_call",
            "sandbox_isolated_workspace_exit",
        ],
    )

