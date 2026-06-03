"""Phase 4 §D1 integration: dispatch_workspace_tool_call gate behavior."""

from __future__ import annotations


import pytest

from sandbox._shared.models import Intent
from sandbox.daemon.workspace_tool import dispatch as dispatch_mod
from sandbox.daemon.workspace_tool.dispatch import (
    _ensure_quiesce_state,
    dispatch_workspace_tool_call,
    reset_quiesce_states_for_test,
)


@pytest.fixture(autouse=True)
def _clean_states():
    reset_quiesce_states_for_test()
    yield
    reset_quiesce_states_for_test()


async def test_dispatch_workspace_tool_call_returns_lifecycle_in_progress_when_pending():
    agent_id = "agent-pending-dispatch"
    state = await _ensure_quiesce_state(agent_id)
    state.exit_pending = True
    response = await dispatch_workspace_tool_call(
        {"agent_id": agent_id, "path": "/testbed/x.txt"},
        verb="read_file",
        intent=Intent.READ_ONLY,
    )
    assert response.get("success") is False
    error = response.get("error", {})
    assert error.get("kind") == "lifecycle_in_progress"
    assert error.get("details", {}).get("agent_id") == agent_id
    # No inflight bump because the slot raised on entry.
    assert state.inflight == 0


async def test_dispatch_workspace_tool_call_dispatches_when_no_isolation(monkeypatch):
    """Smoke: the wrapper hands through to the existing dispatch path."""
    agent_id = "agent-dispatch-smoke"
    called = []

    async def _fake_dispatch(request, isolated_pipeline):
        called.append((request.verb, isolated_pipeline))
        return {"success": True, "workspace": "ephemeral"}

    async def _fake_layer_stack(_request):
        return None

    monkeypatch.setattr(dispatch_mod, "_active_isolated_pipeline_for", lambda _: None)
    monkeypatch.setattr(
        dispatch_mod, "_dispatch_via_workspace_pipeline", _fake_dispatch
    )
    monkeypatch.setattr(
        dispatch_mod, "_dispatch_layer_stack_file_request", _fake_layer_stack
    )

    response = await dispatch_workspace_tool_call(
        {"agent_id": agent_id, "path": "/testbed/x.txt"},
        verb="read_file",
        intent=Intent.READ_ONLY,
    )
    assert response.get("success") is True
    assert called == [("read_file", None)]


__all__ = ()
