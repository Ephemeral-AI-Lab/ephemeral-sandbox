"""Phase 3 — DaemonBackend routes business verbs through runtime commands.

These tests inject a fake runtime command handler whose async method returns
canned responses, then assert that each :class:`DaemonBackend` method
serializes args correctly and reconstructs the right dataclass on the way
out.
"""

from __future__ import annotations

from typing import Any

import pytest

from sandbox.runtime.backends import DaemonBackend
from sandbox.occ.types import (
    EditRequest,
    EditSpec,
    OperationChange,
    WriteSpec,
)


class _FakeRuntime:
    """Minimal stand-in for runtime command handling used by these tests."""

    def __init__(self, response_map: dict[str, Any]) -> None:
        self._responses = response_map
        self.calls: list[tuple[str, dict[str, Any]]] = []

    async def _call_runtime_command(
        self,
        op: str,
        args: dict[str, Any] | None = None,
        *,
        timeout: float = 30.0,
    ) -> Any:
        del timeout
        self.calls.append((op, args or {}))
        if op not in self._responses:
            raise AssertionError(f"unexpected op: {op}")
        return self._responses[op]


def _make_backend(response_map: dict[str, Any]) -> tuple[DaemonBackend, _FakeRuntime]:
    backend = DaemonBackend(
        sandbox_id="sb-test",
        workspace_root="/ws",
        transport=object(),  # type: ignore[arg-type]
    )
    runtime = _FakeRuntime(response_map)
    backend._call_runtime_command = runtime._call_runtime_command  # type: ignore[method-assign]
    backend.is_initialized = True  # short-circuit ensure_initialized
    return backend, runtime


# ---------------------------------------------------------------------------
# Mutations
# ---------------------------------------------------------------------------


def _operation_result_payload(success: bool = True) -> dict[str, Any]:
    return {
        "success": success,
        "status": "committed" if success else "failed",
        "files": [
            {
                "success": success,
                "file_path": "/ws/x.py",
                "message": "",
                "conflict": False,
                "conflict_reason": "",
                "snapshot_id": "",
                "timings": {},
            }
        ],
        "conflict_file": None,
        "conflict_reason": "",
        "timings": {"total": 0.001},
    }


def test_write_file_serializes_specs() -> None:
    backend, runtime = _make_backend({"occ.write": _operation_result_payload()})
    spec = WriteSpec(file_path="/ws/x.py", content="x = 1\n", overwrite=True)
    result = backend.write_file(spec, agent_id="agent-a")
    assert result.success is True
    assert result.status == "committed"

    op, args = runtime.calls[0]
    assert op == "occ.write"
    assert args["workspace_root"] == "/ws"
    assert args["specs"][0] == {
        "file_path": "/ws/x.py",
        "content": "x = 1\n",
        "overwrite": True,
    }
    assert args["agent_id"] == "agent-a"


def test_edit_file_serializes_specs() -> None:
    backend, runtime = _make_backend({"occ.edit": _operation_result_payload()})
    spec = EditSpec(file_path="/ws/x.py", edits=())
    backend.edit_file(spec)
    args = runtime.calls[0][1]
    assert args["specs"][0]["file_path"] == "/ws/x.py"


def test_apply_edit_serializes_request() -> None:
    backend, runtime = _make_backend(
        {
            "occ.apply": {
                "success": True,
                "file_path": "/ws/x.py",
                "message": "",
                "conflict": False,
                "conflict_reason": "",
                "snapshot_id": "",
                "timings": {},
            }
        }
    )
    request = EditRequest(
        file_path="/ws/x.py",
        old_text="a",
        new_text="b",
        agent_id="ag",
    )
    result = backend.apply(request)
    assert result.success is True
    args = runtime.calls[0][1]
    assert args["workspace_root"] == "/ws"
    assert args["request"]["file_path"] == "/ws/x.py"
    assert args["request"]["old_text"] == "a"
    assert args["request"]["new_text"] == "b"


def test_commit_operation_against_base_serializes_changes() -> None:
    backend, runtime = _make_backend(
        {"occ.commit_against_base": _operation_result_payload()}
    )
    change = OperationChange(
        file_path="/ws/x.py",
        base_content="",
        base_hash="",
        final_content="x = 1\n",
    )
    result = backend.commit_operation_against_base(
        [change], agent_id="agent-a", edit_type="write_file"
    )
    assert result.success is True
    args = runtime.calls[0][1]
    assert args["workspace_root"] == "/ws"
    assert args["edit_type"] == "write_file"
    assert args["changes"][0]["file_path"] == "/ws/x.py"


def test_commit_specs_many_round_trips() -> None:
    backend, runtime = _make_backend(
        {"occ.commit_many": [_operation_result_payload()]}
    )
    rows = backend.commit_specs_many([{"foo": "bar"}])
    assert len(rows) == 1
    args = runtime.calls[0][1]
    assert args["workspace_root"] == "/ws"
    assert args["requests"] == [{"foo": "bar"}]


# ---------------------------------------------------------------------------
# Dispose / warmup contracts
# ---------------------------------------------------------------------------


def test_warmup_calls_ensure_initialized(monkeypatch: pytest.MonkeyPatch) -> None:
    """Warmup should bridge to ensure_initialized — no separate daemon op."""
    called: list[bool] = []

    def fake_ensure(self: DaemonBackend, wait: bool = True) -> bool:
        called.append(True)
        return True

    monkeypatch.setattr(DaemonBackend, "ensure_initialized", fake_ensure)
    backend = DaemonBackend(
        sandbox_id="sb",
        workspace_root="/ws",
        transport=object(),  # type: ignore[arg-type]
    )
    backend.warmup()
    assert called == [True]
