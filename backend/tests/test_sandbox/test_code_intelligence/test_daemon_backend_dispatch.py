"""Phase 3 — DaemonBackend routes business verbs through daemon commands.

These tests inject a fake daemon command handler whose async method returns
canned responses, then assert that each :class:`DaemonBackend` method
serializes args correctly and reconstructs the right dataclass on the way
out.
"""

from __future__ import annotations

from typing import Any

import pytest

from sandbox.code_intelligence.backends import DaemonBackend
from sandbox.code_intelligence.core.types import (
    DeleteSpec,
    EditRequest,
    EditSpec,
    MoveSpec,
    OperationChange,
    SymbolKind,
    WriteSpec,
)


class _FakeDaemon:
    """Minimal stand-in for daemon command handling used by these tests."""

    def __init__(self, response_map: dict[str, Any]) -> None:
        self._responses = response_map
        self.calls: list[tuple[str, dict[str, Any]]] = []

    async def _call_daemon_command(self, op: str, args: dict[str, Any] | None = None) -> Any:
        self.calls.append((op, args or {}))
        if op not in self._responses:
            raise AssertionError(f"unexpected op: {op}")
        return self._responses[op]


def _make_backend(response_map: dict[str, Any]) -> tuple[DaemonBackend, _FakeDaemon]:
    backend = DaemonBackend(
        sandbox_id="sb-test",
        workspace_root="/ws",
        transport=object(),  # type: ignore[arg-type]
    )
    daemon = _FakeDaemon(response_map)
    backend._call_daemon_command = daemon._call_daemon_command  # type: ignore[method-assign]
    backend.is_initialized = True  # short-circuit ensure_initialized
    return backend, daemon


# ---------------------------------------------------------------------------
# Queries
# ---------------------------------------------------------------------------


def test_find_definitions_round_trips() -> None:
    backend, daemon = _make_backend(
        {
            "find_definitions": [
                {
                    "name": "foo",
                    "kind": "function",
                    "file_path": "/ws/x.py",
                    "line": 10,
                    "character": 4,
                    "signature": "def foo()",
                    "docstring": "",
                    "container": "",
                    "end_line": 12,
                }
            ]
        }
    )
    rows = backend.find_definitions("/ws/x.py", "foo", line=10, character=4)
    assert len(rows) == 1
    assert rows[0].name == "foo"
    assert rows[0].kind == SymbolKind.FUNCTION
    op, args = daemon.calls[0]
    assert op == "find_definitions"
    assert args["file_path"] == "/ws/x.py"


def test_find_references_round_trips() -> None:
    backend, _ = _make_backend(
        {
            "find_references": [
                {"file_path": "/ws/y.py", "line": 3, "character": 0, "text": "foo()"}
            ]
        }
    )
    rows = backend.find_references("/ws/x.py", "foo", line=1, character=0)
    assert len(rows) == 1
    assert rows[0].file_path == "/ws/y.py"
    assert rows[0].text == "foo()"


def test_hover_returns_none_for_falsy_response() -> None:
    backend, _ = _make_backend({"hover": None})
    assert backend.hover("/ws/x.py", 1, 0) is None


def test_hover_returns_dataclass_when_populated() -> None:
    backend, _ = _make_backend(
        {"hover": {"content": "def foo()", "language": "python"}}
    )
    result = backend.hover("/ws/x.py", 1, 0)
    assert result is not None
    assert result.content == "def foo()"
    assert result.language == "python"


def test_diagnostics_round_trips() -> None:
    backend, _ = _make_backend(
        {
            "diagnostics": [
                {
                    "file_path": "/ws/x.py",
                    "line": 7,
                    "severity": "error",
                    "message": "boom",
                }
            ]
        }
    )
    rows = backend.diagnostics("/ws/x.py")
    assert len(rows) == 1
    assert rows[0].message == "boom"


def test_query_symbols_uses_daemon_when_initialized() -> None:
    backend, daemon = _make_backend(
        {
            "query_symbols": [
                {"name": "Bag", "kind": "class", "file_path": "/ws/x.py", "line": 5}
            ]
        }
    )
    rows = backend.query_symbols("Bag")
    assert [s.name for s in rows] == ["Bag"]
    assert daemon.calls[0][0] == "query_symbols"


def test_query_symbols_propagates_daemon_error() -> None:
    """Phase 3.5 retired the orchestrator-side snapshot cache fallback.
    A daemon error MUST surface to the caller — no silent stale data."""
    backend = DaemonBackend(
        sandbox_id="sb-test",
        workspace_root="/ws",
        transport=object(),  # type: ignore[arg-type]
    )

    class _BrokenDaemon:
        async def _call_daemon_command(self, op: str, args: Any | None = None) -> Any:
            del op, args
            raise RuntimeError("daemon down")

    backend._call_daemon_command = _BrokenDaemon()._call_daemon_command  # type: ignore[method-assign]
    backend.is_initialized = True

    import pytest

    with pytest.raises(RuntimeError, match="daemon down"):
        backend.query_symbols("Bag")


def test_list_folder_files_returns_list() -> None:
    backend, _ = _make_backend({"list_folder_files": ["/a.py", "/b.py"]})
    assert backend.list_folder_files("/ws") == ["/a.py", "/b.py"]


def test_status_returns_dict() -> None:
    backend, _ = _make_backend({"status": {"initialized": True, "workspace": "/ws"}})
    assert backend.status() == {"initialized": True, "workspace": "/ws"}


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
    backend, daemon = _make_backend({"write_file": _operation_result_payload()})
    spec = WriteSpec(file_path="/ws/x.py", content="x = 1\n", overwrite=True)
    result = backend.write_file(spec, agent_id="agent-a")
    assert result.success is True
    assert result.status == "committed"

    op, args = daemon.calls[0]
    assert op == "write_file"
    assert args["specs"][0] == {
        "file_path": "/ws/x.py",
        "content": "x = 1\n",
        "overwrite": True,
    }
    assert args["agent_id"] == "agent-a"


def test_edit_file_serializes_specs() -> None:
    backend, daemon = _make_backend({"edit_file": _operation_result_payload()})
    spec = EditSpec(file_path="/ws/x.py", edits=())
    backend.edit_file(spec)
    args = daemon.calls[0][1]
    assert args["specs"][0]["file_path"] == "/ws/x.py"


def test_delete_file_supports_str_or_spec() -> None:
    backend, daemon = _make_backend({"delete_file": _operation_result_payload()})
    backend.delete_file([DeleteSpec(path="/ws/a.py"), "/ws/b.py"])
    args = daemon.calls[0][1]
    assert args["paths"][0] == {"path": "/ws/a.py", "is_folder": False}
    assert args["paths"][1] == "/ws/b.py"


def test_move_file_serializes_specs() -> None:
    backend, daemon = _make_backend({"move_file": _operation_result_payload()})
    backend.move_file([MoveSpec(src_path="/ws/a.py", dst_path="/ws/b.py")])
    args = daemon.calls[0][1]
    assert args["specs"][0] == {
        "src_path": "/ws/a.py",
        "dst_path": "/ws/b.py",
        "overwrite": False,
        "is_folder": False,
    }


def test_apply_edit_serializes_request() -> None:
    backend, daemon = _make_backend(
        {
            "apply_edit": {
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
    result = backend.apply_edit(request)
    assert result.success is True
    args = daemon.calls[0][1]
    assert args["request"]["file_path"] == "/ws/x.py"
    assert args["request"]["old_text"] == "a"
    assert args["request"]["new_text"] == "b"


def test_commit_operation_against_base_serializes_changes() -> None:
    backend, daemon = _make_backend(
        {"commit_operation_against_base": _operation_result_payload()}
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
    args = daemon.calls[0][1]
    assert args["edit_type"] == "write_file"
    assert args["changes"][0]["file_path"] == "/ws/x.py"


def test_commit_specs_many_round_trips() -> None:
    backend, daemon = _make_backend(
        {"commit_specs_many": [_operation_result_payload()]}
    )
    rows = backend.commit_specs_many([{"foo": "bar"}])
    assert len(rows) == 1
    args = daemon.calls[0][1]
    assert args["requests"] == [{"foo": "bar"}]


def test_undo_last_edit_round_trips() -> None:
    backend, _ = _make_backend(
        {
            "undo_last_edit": {
                "success": True,
                "file_path": "/ws/x.py",
                "message": "undone",
                "conflict": False,
                "conflict_reason": "",
                "snapshot_id": "",
                "timings": {},
            }
        }
    )
    result = backend.undo_last_edit("/ws/x.py")
    assert result.success is True
    assert result.message == "undone"


# ---------------------------------------------------------------------------
# Telemetry / dispose / warmup contracts
# ---------------------------------------------------------------------------


def test_get_telemetry_round_trips_to_dataclass() -> None:
    backend, _ = _make_backend({"get_telemetry": {}})
    telemetry = backend.get_telemetry()
    # Telemetry dataclass exists and is constructed without crashing.
    from sandbox.code_intelligence.core.types import CITelemetry

    assert isinstance(telemetry, CITelemetry)


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
