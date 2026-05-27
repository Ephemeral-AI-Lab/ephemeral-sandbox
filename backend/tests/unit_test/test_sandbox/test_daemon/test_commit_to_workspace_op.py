"""``api.commit_to_workspace`` daemon RPC integration test.

Asserts dispatcher routes the op and that ``layer_stack_runtime.commit_to_workspace``
is called with the workspace_root from the args. Uses a stub manifest to
avoid spinning up real layer storage.
"""

from __future__ import annotations

from types import SimpleNamespace

import pytest

from sandbox.daemon import builtin_operations
from sandbox.daemon.rpc.dispatcher import OP_TABLE, dispatch_envelope_async


def test_commit_to_workspace_op_registered_in_dispatcher() -> None:
    assert "api.commit_to_workspace" in OP_TABLE
    assert OP_TABLE["api.commit_to_workspace"] is builtin_operations.commit_to_workspace


@pytest.mark.asyncio
async def test_commit_to_workspace_invokes_runtime_with_workspace_root(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, object] = {}

    def fake_commit(layer_stack_root, *, workspace_root, timings=None):
        captured["layer_stack_root"] = layer_stack_root
        captured["workspace_root"] = workspace_root
        if timings is not None:
            timings["layer_stack.commit_to_workspace.total_s"] = 0.012
        return SimpleNamespace(version=42)

    monkeypatch.setattr(
        "sandbox.daemon.builtin_operations.layer_stack_runtime.commit_to_workspace",
        fake_commit,
    )

    envelope = {
        "op": "api.commit_to_workspace",
        "invocation_id": "test-invocation",
        "args": {
            "layer_stack_root": "/tmp/some-root",
            "workspace_root": "/testbed",
        },
    }
    response = await dispatch_envelope_async(envelope)

    assert response["success"] is True
    assert response["manifest_version"] == 42
    assert captured["workspace_root"] == "/testbed"
    assert str(captured["layer_stack_root"]).endswith("/tmp/some-root") or (
        captured["layer_stack_root"] == "/tmp/some-root"
    )
    timings = response.get("timings")
    assert isinstance(timings, dict)
    assert "api.commit_to_workspace.total_s" in timings


@pytest.mark.asyncio
async def test_commit_to_workspace_requires_workspace_root(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def fake_commit(layer_stack_root, *, workspace_root, timings=None):
        raise AssertionError("runtime must not be called without workspace_root")

    monkeypatch.setattr(
        "sandbox.daemon.builtin_operations.layer_stack_runtime.commit_to_workspace",
        fake_commit,
    )

    envelope = {
        "op": "api.commit_to_workspace",
        "invocation_id": "test-invocation",
        "args": {
            "layer_stack_root": "/tmp/some-root",
        },
    }
    response = await dispatch_envelope_async(envelope)
    assert response["success"] is False
    assert response["error"]["kind"] == "internal_error"
