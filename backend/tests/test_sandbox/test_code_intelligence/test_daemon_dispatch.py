"""Phase 3 dispatch + bypass-guard unit tests for the CI daemon.

These tests do not require Daytona — they exercise the daemon's request
lifecycle directly by calling ``_dispatch_request`` against a populated
``_DAEMON_STATE`` rooted at ``tmp_path``.
"""

from __future__ import annotations

import time
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from sandbox.code_intelligence.daemon import server as daemon_server
from sandbox.code_intelligence.daemon.server import (
    DISPATCH,
    _dispatch_request,
    _populate_state,
    _reset_daemon_state_for_tests,
)
from sandbox.code_intelligence.daemon.storage import LedgerStore
from sandbox.code_intelligence.service import CodeIntelligenceService


@pytest.fixture()
def daemon_state(tmp_path: Path) -> Path:
    """Populate the daemon state with a real CodeIntelligenceService rooted at tmp_path."""
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    state = tmp_path / "state"
    state.mkdir()

    ledger = LedgerStore(state_dir_path=state)
    svc = CodeIntelligenceService(
        sandbox_id="local",
        workspace_root=str(workspace),
        sandbox=None,
        transport=None,
        edit_history=ledger,
    )
    _populate_state(
        state=state,
        workspace_root=str(workspace),
        svc=svc,
        ledger=ledger,
    )
    yield workspace
    try:
        svc.dispose()
    except Exception:  # pragma: no cover - defensive
        pass
    ledger.close()
    _reset_daemon_state_for_tests()


def _make_request(op: str, args: dict[str, Any] | None = None) -> dict[str, Any]:
    return {"v": 1, "id": "req-1", "op": op, "args": args or {}}


# ---------------------------------------------------------------------------
# Dispatch table coverage
# ---------------------------------------------------------------------------


def test_dispatch_table_includes_phase3_ops() -> None:
    expected = {
        "ping",
        "shutdown",
        "version",
        "query_symbols",
        "find_definitions",
        "find_references",
        "hover",
        "diagnostics",
        "list_folder_files",
        "status",
        "get_telemetry",
        "svc_cmd",
        "apply_edit",
        "commit_operation_against_base",
        "commit_specs_many",
        "write_file",
        "edit_file",
        "delete_file",
        "move_file",
        "undo_last_edit",
        "index_refresh",
        "lsp_invalidate",
        "index_ready",
        "_set_guard_mode",
    }
    missing = expected - DISPATCH.keys()
    assert missing == set(), f"DISPATCH missing ops: {missing}"


# ---------------------------------------------------------------------------
# Query handlers
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_status_returns_initialized_field(daemon_state: Path) -> None:
    response = await _dispatch_request(_make_request("status"))
    assert response["ok"] is True
    assert "initialized" in response["result"]


@pytest.mark.asyncio
async def test_query_symbols_returns_list(daemon_state: Path) -> None:
    response = await _dispatch_request(
        _make_request("query_symbols", {"query": "nonexistent"})
    )
    assert response["ok"] is True
    assert isinstance(response["result"], list)


@pytest.mark.asyncio
async def test_index_ready_responds(daemon_state: Path) -> None:
    response = await _dispatch_request(_make_request("index_ready"))
    assert response["ok"] is True
    assert "ready" in response["result"]


@pytest.mark.asyncio
async def test_unknown_op_returns_unsupported_envelope(daemon_state: Path) -> None:
    response = await _dispatch_request(_make_request("does_not_exist"))
    assert response["ok"] is False
    assert response["error"]["kind"] == "UnsupportedOp"


# ---------------------------------------------------------------------------
# Mutation handlers
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_write_file_routes_through_service(daemon_state: Path) -> None:
    target = str(daemon_state / "hello.py")
    response = await _dispatch_request(
        _make_request(
            "write_file",
            {
                "specs": [
                    {
                        "file_path": target,
                        "content": "x = 1\n",
                        "overwrite": True,
                    }
                ],
                "agent_id": "agent-a",
                "description": "create hello.py",
            },
        )
    )
    assert response["ok"] is True, response
    result = response["result"]
    assert result["success"] is True
    assert Path(target).read_text(encoding="utf-8") == "x = 1\n"


@pytest.mark.asyncio
async def test_svc_cmd_routes_through_service_and_preserves_shape(
    daemon_state: Path,
) -> None:
    calls: list[dict[str, Any]] = []

    async def _fake_cmd(_sandbox: Any, command: str, **kwargs: Any) -> SimpleNamespace:
        calls.append({"command": command, **kwargs})
        return SimpleNamespace(
            result="ok\n",
            exit_code=0,
            changed_paths=[str(daemon_state / "a.py")],
            ambient_changed_paths=[],
            files_written=1,
            git_commit_status="committed",
            git_conflict_file=None,
            git_conflict_reason=None,
            gitinclude_changed_paths=[str(daemon_state / "a.py")],
            gitignore_direct_merged_paths=[],
            gitignore_direct_merged_count=0,
            mixed_gitinclude_gitignore=False,
            mixed_partial_apply=False,
            warnings=[],
            overlay_run_timings={"total": 0.02},
            overlay_stage_timings={"total": 0.03},
        )

    daemon_server._DAEMON_STATE.svc.cmd = _fake_cmd

    response = await _dispatch_request(
        _make_request(
            "svc_cmd",
            {
                "command": "echo ok",
                "timeout": 5,
                "description": "smoke",
                "agent_id": "agent-a",
                "run_id": "run-a",
                "agent_run_id": "agent-run-a",
                "task_id": "task-a",
                "stdin": "payload",
                "attribute_changes": False,
            },
        )
    )

    assert response["ok"] is True, response
    assert calls == [
        {
            "command": "echo ok",
            "timeout": 5,
            "description": "smoke",
            "agent_id": "agent-a",
            "run_id": "run-a",
            "agent_run_id": "agent-run-a",
            "task_id": "task-a",
            "stdin": "payload",
            "attribute_changes": False,
        }
    ]
    assert response["result"] == {
        "result": "ok\n",
        "exit_code": 0,
        "changed_paths": [str(daemon_state / "a.py")],
        "ambient_changed_paths": [],
        "files_written": 1,
        "git_commit_status": "committed",
        "git_conflict_file": None,
        "git_conflict_reason": None,
        "gitinclude_changed_paths": [str(daemon_state / "a.py")],
        "gitignore_direct_merged_paths": [],
        "gitignore_direct_merged_count": 0,
        "mixed_gitinclude_gitignore": False,
        "mixed_partial_apply": False,
        "warnings": [],
        "overlay_run_timings": {"total": 0.02},
        "overlay_stage_timings": {"total": 0.03},
    }


@pytest.mark.asyncio
async def test_undo_last_edit_routes_through_service(daemon_state: Path) -> None:
    target = str(daemon_state / "u.py")
    Path(target).write_text("old\n", encoding="utf-8")

    write_resp = await _dispatch_request(
        _make_request(
            "write_file",
            {
                "specs": [{"file_path": target, "content": "new\n", "overwrite": True}],
                "agent_id": "agent-a",
            },
        )
    )
    assert write_resp["ok"] is True
    assert Path(target).read_text(encoding="utf-8") == "new\n"

    undo_resp = await _dispatch_request(
        _make_request("undo_last_edit", {"file_path": target})
    )
    assert undo_resp["ok"] is True
    assert undo_resp["result"]["success"] is True


# ---------------------------------------------------------------------------
# Bypass guard
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_guard_strict_mode_surfaces_workspace_bypass(
    daemon_state: Path,
) -> None:
    """A handler that mutates the workspace WITHOUT going through ``svc``
    should be flagged as ``WorkspaceBypass`` once strict mode is on."""

    bypass_target = daemon_state / "__bypass__.txt"

    async def _bad_handler(args: dict[str, Any]) -> dict[str, Any]:
        # Simulate a malicious bypass — write directly to the workspace.
        bypass_target.write_text("planted\n", encoding="utf-8")
        return {"wrote": str(bypass_target)}

    # Register in extra_dispatch and enable strict mode.
    daemon_server._DAEMON_STATE.extra_dispatch["_test_bypass"] = _bad_handler
    daemon_server._DAEMON_STATE.guard_strict = True

    response = await _dispatch_request(_make_request("_test_bypass"))
    # Detection, not prevention: the file IS written.
    assert bypass_target.exists(), "guard should not have prevented the write"
    assert response["ok"] is False, response
    assert response["error"]["kind"] == "WorkspaceBypass"
    assert str(bypass_target) in response["error"]["message"]


@pytest.mark.asyncio
async def test_guard_disabled_when_query_op(daemon_state: Path) -> None:
    """Pure query ops should not trigger the bypass guard at all, even if a
    malicious mtime sweep would otherwise flag a file."""
    # Touch an un-ledgered file ahead of the query so its mtime is fresh.
    (daemon_state / "untracked.txt").write_text("x", encoding="utf-8")
    time.sleep(0.01)
    daemon_server._DAEMON_STATE.guard_strict = True
    response = await _dispatch_request(
        _make_request("query_symbols", {"query": "anything"})
    )
    assert response["ok"] is True, response


@pytest.mark.asyncio
async def test_guard_lenient_mode_logs_but_passes_through(
    daemon_state: Path,
    caplog: pytest.LogCaptureFixture,
) -> None:
    """In production (strict=False) the guard logs ERROR but still returns
    the original result envelope."""
    bypass_target = daemon_state / "__lenient_bypass__.txt"

    async def _bad_handler(args: dict[str, Any]) -> dict[str, Any]:
        bypass_target.write_text("planted\n", encoding="utf-8")
        return {"wrote": str(bypass_target)}

    daemon_server._DAEMON_STATE.extra_dispatch["_test_bypass_lenient"] = _bad_handler
    daemon_server._DAEMON_STATE.guard_strict = False

    with caplog.at_level("ERROR"):
        response = await _dispatch_request(_make_request("_test_bypass_lenient"))

    assert response["ok"] is True
    assert any(
        "WORKSPACE WRITE BYPASS" in record.message for record in caplog.records
    )


# ---------------------------------------------------------------------------
# Serializer round-trip sanity (called by handlers)
# ---------------------------------------------------------------------------


def test_writespec_from_dict_round_trips() -> None:
    from sandbox.code_intelligence.core.types import WriteSpec

    payload = {"file_path": "/x.py", "content": "y = 1\n", "overwrite": True}
    spec = daemon_server._writespec_from_dict(payload)
    assert isinstance(spec, WriteSpec)
    assert spec.file_path == "/x.py"
    assert spec.content == "y = 1\n"
    assert spec.overwrite is True


def test_movespec_accepts_legacy_aliases() -> None:
    from sandbox.code_intelligence.core.types import MoveSpec

    spec = daemon_server._movespec_from_dict({"source": "/a", "destination": "/b"})
    assert isinstance(spec, MoveSpec)
    assert spec.src_path == "/a"
    assert spec.dst_path == "/b"


def test_deletespec_accepts_legacy_field() -> None:
    from sandbox.code_intelligence.core.types import DeleteSpec

    spec = daemon_server._deletespec_from_dict({"file_path": "/x.py"})
    assert isinstance(spec, DeleteSpec)
    assert spec.path == "/x.py"


def test_operation_change_round_trip() -> None:
    from sandbox.code_intelligence.core.types import OperationChange

    payload = {
        "file_path": "/x.py",
        "base_content": "old",
        "base_hash": "h",
        "final_content": "new",
        "base_existed": True,
        "strict_base": True,
    }
    change = daemon_server._operation_change_from_dict(payload)
    assert isinstance(change, OperationChange)
    assert change.strict_base is True


def test_to_dict_handles_dataclass_and_nested_lists() -> None:
    from sandbox.code_intelligence.core.types import OperationChange

    change = OperationChange(
        file_path="/x.py",
        base_content="",
        base_hash="",
        final_content="x",
    )
    converted = daemon_server._to_dict([change, {"a": [change]}])
    assert isinstance(converted, list)
    assert converted[0]["file_path"] == "/x.py"
    assert converted[1]["a"][0]["file_path"] == "/x.py"


def test_to_dict_handles_simple_namespace() -> None:
    converted = daemon_server._to_dict(
        SimpleNamespace(
            result="ok",
            nested=SimpleNamespace(paths=["/x.py"]),
        )
    )
    assert converted == {"result": "ok", "nested": {"paths": ["/x.py"]}}
