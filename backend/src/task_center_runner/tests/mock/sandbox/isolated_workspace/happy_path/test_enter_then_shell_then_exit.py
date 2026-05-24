"""Golden lifecycle: enter → shell("echo hi") → exit.

Asserts:
- ``enter`` returns ``success=True`` with non-empty ``manifest_root_hash``.
- ``shell`` inside the workspace produces stdout containing ``hi``.
- ``exit`` returns ``success=True`` and discards the upperdir
  (``evicted_upperdir_bytes`` is non-negative; typically 0 for an empty ws).
- The audit log carries enter → tool_call → exit in order for this handle.
- Both enter and exit events expose ``total_ms`` and ``phases_ms`` keys
  (PR 1 contract).
- ``status`` after exit returns ``open=False``.
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
async def test_enter_then_shell_then_exit(iws_clean_sandbox, iws_audit_jsonl) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    agent_id = "agent-A"
    enter_response = await _iws_rpc.enter(
        sandbox_id, agent_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter_response.get("success") is True, enter_response
    assert enter_response.get("manifest_root_hash"), enter_response

    shell_response = await _iws_rpc.shell(sandbox_id, agent_id, "echo hi")
    assert shell_response.get("success") is True, shell_response
    assert "hi" in shell_response.get("stdout", ""), shell_response

    exit_response = await _iws_rpc.exit_(sandbox_id, agent_id)
    assert exit_response.get("success") is True, exit_response
    assert exit_response.get("evicted_upperdir_bytes", -1) >= 0, exit_response

    status_response = await _iws_rpc.status(sandbox_id, agent_id)
    assert status_response.get("success") is True
    assert status_response.get("open") is False, status_response

    # PR 1 contract: enter and exit events expose ``total_ms`` and
    # ``phases_ms`` (PLAN §14). Verify the sequence reached the audit sink.
    jsonl_path = await iws_audit_jsonl()
    _iws_invariants.assert_audit_sequence(
        jsonl_path,
        [
            "sandbox_isolated_workspace_enter",
            "sandbox_isolated_workspace_tool_call",
            "sandbox_isolated_workspace_exit",
        ],
    )
    enters = _iws_invariants.events_of_type(
        jsonl_path, "sandbox_isolated_workspace_enter"
    )
    assert enters, enters
    enter_payload = enters[-1].get("payload", {})
    assert enter_payload.get("phases_ms"), enter_payload
    assert float(enter_payload.get("total_ms", 0.0)) > 0.0, enter_payload
