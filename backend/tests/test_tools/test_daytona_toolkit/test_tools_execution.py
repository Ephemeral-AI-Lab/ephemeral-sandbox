"""Tests for async @tool functions in tools.daytona_toolkit.tools."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest
from pydantic import BaseModel

from engine.core.query import QueryContext
from message.stream_events import StreamEvent, SystemNotification
from tools.core.base import (
    BaseTool,
    ToolExecutionContext,
    ToolRegistry,
    ToolResult,
    run_tool_safely,
)
from tools.core.hooks.execution import execute_tool_with_hooks
from tools.core.runtime import ExecutionMetadata
from tools.core.tool_execution import execute_tool_call_streaming
from tools.daytona_toolkit.shell_tool import daytona_shell
from tools.daytona_toolkit.tools import (
    daytona_read_file,
    daytona_write_file,
    daytona_grep,
    daytona_glob,
)

pytestmark = pytest.mark.asyncio


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _sb(*, exec_result=None, download=None, list_result=None, find_result=None):
    sb = MagicMock()
    sb.process.exec = AsyncMock(return_value=exec_result or _write_exec_result())
    sb.fs.download_file = AsyncMock(return_value=download if download is not None else b"")
    sb.fs.upload_file = AsyncMock()
    sb.fs.list_files = AsyncMock(return_value=list_result or [])
    sb.fs.find_files = AsyncMock(return_value=find_result or [])
    return sb


def _write_exec_result(*, base_existed: bool = False):
    del base_existed
    return MagicMock(
        result=json.dumps(
            {
                "ok": True,
                "bytes_written": 5,
            }
        ),
        exit_code=0,
    )


def _read_exec_result(content: str = "", *, exists: bool = True):
    return MagicMock(
        result=json.dumps(
            {
                "exists": exists,
                "content": content,
            }
        ),
        exit_code=0,
    )


def _ci_service_mock(*, file_path: str = "/ws/new.txt"):
    from code_intelligence.types import EditResult, OperationResult

    svc = MagicMock()
    svc.write_file = MagicMock(
        return_value=OperationResult(
            success=True,
            status="committed",
            files=(
                EditResult(success=True, file_path=file_path, message="Wrote file"),
            ),
            conflict_file=None,
            conflict_reason="",
            timings={},
        )
    )
    return svc


class _FakeEditInput(BaseModel):
    file_path: str


class _FakeEditTool(BaseTool):
    name = "daytona_edit_file"
    description = "Fake edit tool for hook pipeline tests."
    input_model = _FakeEditInput

    async def execute(
        self,
        arguments: BaseModel,
        context: ToolExecutionContext,
    ) -> ToolResult:
        del arguments, context
        return ToolResult(output="ok")


async def _capture_emit(events: list[StreamEvent], event: StreamEvent) -> None:
    events.append(event)


async def _run_with_events(tool, payload, ctx):
    events: list[StreamEvent] = []
    result = await execute_tool_with_hooks(
        tool,
        payload,
        ctx,
        emit=lambda event: _capture_emit(events, event),
        emit_started=False,
    )
    return result, events


def _notification_texts(events: list[StreamEvent]) -> list[str]:
    return [event.text for event in events if isinstance(event, SystemNotification)]


async def test_shell_schema_describes_command():
    schema = daytona_shell.to_api_schema()
    description = schema["description"]
    properties = schema["input_schema"]["properties"]
    command_desc = properties["command"]["description"]

    assert "code" not in properties
    assert "mode" not in properties
    assert "Run a shell command in the Daytona sandbox" in description
    assert "host paths like `/Users/...`" in description
    assert "Output is captured automatically" in description
    assert "Shell command to run from the repo root" in command_desc
    assert "Do not prefix with host paths like /Users/..." in command_desc
    assert "Output is captured automatically" in command_desc


# ---------------------------------------------------------------------------
# daytona_read_file
# ---------------------------------------------------------------------------

async def test_read_file_success():
    sb = _sb(exec_result=_read_exec_result("line one\nline two\nline three\n"))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="foo.txt"), ctx
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["total_lines"] == 3
    assert data["start_line"] == 1
    assert "line one" in data["content"]


async def test_read_file_with_line_range():
    lines = "\n".join(f"line{i}" for i in range(1, 11))
    sb = _sb(exec_result=_read_exec_result(lines))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="/abs/file.txt", start_line=3, end_line=5), ctx
    )
    data = json.loads(result.output)
    assert data["start_line"] == 3
    assert data["end_line"] == 5
    assert data["total_lines"] == 10


async def test_read_file_resolves_relative_path():
    sb = _sb(exec_result=_read_exec_result("hello"))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/workspace"})
    await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="relative.txt"), ctx
    )
    command = sb.process.exec.await_args.args[0]
    assert "/workspace/relative.txt" in command


async def test_read_file_not_found():
    sb = _sb(exec_result=_read_exec_result(exists=False))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="/missing.txt"), ctx
    )
    assert result.is_error
    assert "does not exist" in result.output


async def test_read_file_generic_exception():
    sb = _sb()
    sb.process.exec = AsyncMock(side_effect=RuntimeError("network error"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="/x.txt"), ctx
    )
    assert result.is_error
    assert "network error" in result.output


async def test_read_file_str_content():
    # SDK returns str instead of bytes
    sb = _sb(exec_result=_read_exec_result("plain string content"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="/str.txt"), ctx
    )
    assert not result.is_error


async def test_read_file_allows_benchmark_lane_before_first_repro():
    sb = _sb(exec_result=_read_exec_result("ok"))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "benchmark_test_ids": ["tests/unit/command/test_update.py::test_update"],
            "benchmark_test_files": ["tests/unit/command/test_update.py"],
        }
    )

    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="dvc/command/update.py"),
        ctx,
    )

    assert not result.is_error
    sb.process.exec.assert_awaited_once()


async def test_read_file_allows_benchmark_test_files_after_repro():
    sb = _sb(exec_result=_read_exec_result("ok"))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "benchmark_test_ids": ["tests/unit/command/test_update.py::test_update"],
            "benchmark_test_files": ["tests/unit/command/test_update.py"],
            "_daytona_shell_calls": 1,
        }
    )

    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="tests/unit/command/test_update.py"),
        ctx,
    )

    assert not result.is_error
    sb.process.exec.assert_awaited_once()


async def test_read_file_allows_team_lane_without_runtime_workflow_gate():
    sb = _sb(exec_result=_read_exec_result("ok"))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_run_id": "team-1",
            "work_item_id": "task-1",
        }
    )

    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="dvc/command/update.py"),
        ctx,
    )

    assert not result.is_error
    sb.process.exec.assert_awaited_once()


async def test_read_file_allows_team_lane_after_notes_and_ci_context():
    sb = _sb(exec_result=_read_exec_result("ok"))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "team_run_id": "team-1",
            "work_item_id": "task-1",
            "_read_file_note_calls": 1,
            "_ci_context_calls": 1,
        }
    )

    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="dvc/command/update.py"),
        ctx,
    )

    assert not result.is_error


async def test_read_file_allows_production_reads_after_repro():
    sb = _sb(exec_result=_read_exec_result("ok"))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "benchmark_test_ids": ["tests/unit/command/test_update.py::test_update"],
            "benchmark_test_files": ["tests/unit/command/test_update.py"],
            "_daytona_shell_calls": 1,
        }
    )

    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="dvc/command/update.py"),
        ctx,
    )

    assert not result.is_error


# ---------------------------------------------------------------------------
# daytona_write_file
# ---------------------------------------------------------------------------

async def test_write_file_requires_ci_service():
    sb = _sb()
    sb.process.exec = AsyncMock(return_value=_write_exec_result())
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    result = await daytona_write_file.execute(
        daytona_write_file.input_model(file_path="/ws/new.txt", content="hello"), ctx
    )
    assert result.is_error
    assert "Code intelligence service is unavailable" in result.output
    assert result.metadata["ci_required"] is True
    sb.fs.upload_file.assert_not_called()
    sb.process.exec.assert_not_called()


async def test_write_file_syncs_ci_state():
    sb = _sb()
    svc = _ci_service_mock(file_path="/ws/new.txt")
    ctx = _ctx({
        "daytona_sandbox": sb,
        "daytona_cwd": "/ws",
        "ci_service": svc,
    })

    result = await daytona_write_file.execute(
        daytona_write_file.input_model(file_path="/ws/new.txt", content="hello"), ctx
    )

    assert not result.is_error
    svc.write_file.assert_called_once()
    assert json.loads(result.output)["ci_sync"] is True


async def test_write_file_resolves_relative_path():
    sb = _sb()
    svc = _ci_service_mock(file_path="/workspace/subdir/file.txt")
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/workspace", "ci_service": svc})
    result = await daytona_write_file.execute(
        daytona_write_file.input_model(file_path="subdir/file.txt", content="data"), ctx
    )
    assert not result.is_error
    specs = svc.write_file.call_args.args[0]
    assert specs[0].file_path == "/workspace/subdir/file.txt"


async def test_write_file_extends_scope_without_advisory_for_outside_write_scope():
    """Successful write-file calls widen scope without pre-hook notifications."""
    sb = _sb()
    sb.process.exec = AsyncMock(return_value=_write_exec_result())
    svc = _ci_service_mock(file_path="/testbed/dask/_compatibility.py")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "write_scope": ["dask/config.py"],
            "ci_service": svc,
        }
    )

    result, events = await _run_with_events(
        daytona_write_file,
        {"file_path": "/testbed/dask/_compatibility.py", "content": "patched"},
        ctx,
    )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["warnings"] == []
    assert not any("outside write_scope" in text for text in _notification_texts(events))
    assert ctx.metadata["write_scope"] == ["dask/config.py", "dask/_compatibility.py"]


async def test_query_dispatch_persists_write_file_scope_extension():
    """The agent query path must keep post-hook write_scope expansions."""
    registry = ToolRegistry()
    registry.register(daytona_write_file)
    metadata = ExecutionMetadata(
        ci_service=_ci_service_mock(file_path="/testbed/dask/_compatibility.py"),
        daytona_cwd="/testbed",
        repo_root="/testbed",
        agent_name="developer",
    )
    metadata["write_scope"] = ["dask/config.py"]
    ctx = QueryContext(
        api_client=MagicMock(),
        tool_registry=registry,
        cwd=Path("/testbed"),
        model="m",
        system_prompt="p",
        max_tokens=1,
        tool_metadata=metadata,
    )

    result = await execute_tool_call_streaming(
        ctx,
        "daytona_write_file",
        "tool-1",
        {"file_path": "/testbed/dask/_compatibility.py", "content": "patched"},
        emit=lambda event: _capture_emit([], event),
        emit_started=False,
    )

    assert not result.is_error
    assert metadata["write_scope"] == ["dask/config.py", "dask/_compatibility.py"]


async def test_query_dispatch_persists_coordination_warnings_from_hooks():
    """Hook-recorded coordination warnings must survive per-call metadata copies."""
    registry = ToolRegistry()
    registry.register(_FakeEditTool())
    metadata = ExecutionMetadata(
        repo_root="/testbed",
        agent_name="developer",
    )
    metadata["write_scope"] = ["dask/config.py"]
    ctx = QueryContext(
        api_client=MagicMock(),
        tool_registry=registry,
        cwd=Path("/testbed"),
        model="m",
        system_prompt="p",
        max_tokens=1,
        tool_metadata=metadata,
    )
    events: list[StreamEvent] = []

    result = await execute_tool_call_streaming(
        ctx,
        "daytona_edit_file",
        "tool-1",
        {"file_path": "/testbed/dask/_compatibility.py"},
        emit=lambda event: _capture_emit(events, event),
        emit_started=False,
    )

    assert not result.is_error
    assert metadata["coordination_warning_present"] is True
    warnings = metadata["coordination_warnings"]
    assert len(warnings) == 1
    assert warnings[0]["category"] == "outside_write_scope"
    assert "dask/_compatibility.py" in warnings[0]["message"]


async def test_write_file_allows_write_inside_write_scope():
    sb = _sb()
    svc = _ci_service_mock(file_path="/testbed/dask/config.py")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "write_scope": ["dask/"],
            "ci_service": svc,
        }
    )

    result = await run_tool_safely(
        daytona_write_file,
        {"file_path": "/testbed/dask/config.py", "content": "patched"},
        ctx,
    )

    assert not result.is_error
    svc.write_file.assert_called_once()


async def test_write_file_blocks_test_file_with_policy_message():
    sb = _sb()
    sb.process.exec = AsyncMock(return_value=_write_exec_result())
    svc = _ci_service_mock(file_path="/testbed/dask/tests/test_cli.py")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "write_scope": ["dask/cli.py"],
            "owned_failures": ["dask/tests/test_cli.py"],
            "verify": ["pytest dask/tests/test_cli.py -q"],
            "ci_service": svc,
        }
    )

    result = await run_tool_safely(
        daytona_write_file,
        {"file_path": "/testbed/dask/tests/test_cli.py", "content": "patched"},
        ctx,
    )

    assert result.is_error
    assert "BLOCKED_TEST_FILE_EDIT" in result.output
    assert "dask/tests/test_cli.py" in result.output
    assert "read/verify-only" in result.output
    sb.process.exec.assert_not_awaited()


async def test_write_file_extends_scope_for_non_verify_surface_write_in_warn_mode():
    """Write-file no longer emits write-scope advisories in warn mode."""
    sb = _sb()
    sb.process.exec = AsyncMock(return_value=_write_exec_result())
    svc = _ci_service_mock(file_path="/testbed/dask/_compatibility.py")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "developer",
            "write_scope": ["dask/compatibility.py"],
            "verification_surface_write_enforcement": "warn",
            "owned_failures": ["dask/tests/test_cli.py"],
            "verify": ["pytest dask/tests/test_cli.py -q"],
            "ci_service": svc,
        }
    )

    result, events = await _run_with_events(
        daytona_write_file,
        {"file_path": "/testbed/dask/_compatibility.py", "content": "patched"},
        ctx,
    )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["warnings"] == []
    assert not any("outside write_scope" in text for text in _notification_texts(events))
    assert ctx.metadata["write_scope"] == [
        "dask/compatibility.py",
        "dask/_compatibility.py",
    ]


async def test_write_file_allows_repo_write_from_validator():
    sb = _sb()
    svc = _ci_service_mock(file_path="/testbed/dask/config.py")
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "daytona_cwd": "/testbed",
            "agent_name": "validator",
            "ci_service": svc,
        }
    )

    result = await daytona_write_file.execute(
        daytona_write_file.input_model(
            file_path="/testbed/dask/config.py",
            content="patched",
        ),
        ctx,
    )

    assert not result.is_error


async def test_write_file_no_raw_write_after_ci_unavailable():
    sb = _sb()
    sb.process.exec = AsyncMock(return_value=_write_exec_result())
    sb.fs.upload_file = AsyncMock(side_effect=RuntimeError("disk full"))
    ctx = _ctx(
        {
            "daytona_sandbox": sb,
            "agent_name": "developer",
        }
    )
    result = await daytona_write_file.execute(
        daytona_write_file.input_model(file_path="/x.txt", content="data"), ctx
    )
    assert result.is_error
    assert "Code intelligence service is unavailable" in result.output
    assert result.metadata["ci_required"] is True
    sb.fs.upload_file.assert_not_called()


# ---------------------------------------------------------------------------
# daytona_grep
# ---------------------------------------------------------------------------

async def test_grep_no_matches():
    sb = _sb(exec_result=MagicMock(result=json.dumps({"ok": True, "matches": []}), exit_code=0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    result = await daytona_grep.execute(daytona_grep.input_model(pattern="needle"), ctx)
    assert not result.is_error
    data = json.loads(result.output)
    assert data["total_matches"] == 0
    assert data["matches"] == []


async def test_grep_with_matches():
    sb = _sb(
        exec_result=MagicMock(
            result=json.dumps(
                {
                    "ok": True,
                    "matches": [
                        {"file": "/ws/a.py", "line": 10, "content": "  needle here  "},
                        {"file": "/ws/b.py", "line": 20, "content": "another needle"},
                    ],
                }
            ),
            exit_code=0,
        )
    )
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    result = await daytona_grep.execute(daytona_grep.input_model(pattern="needle"), ctx)
    data = json.loads(result.output)
    assert data["total_matches"] == 2
    assert len(data["matches"]) == 2
    assert data["matches"][0]["file"] == "/ws/a.py"
    assert data["matches"][0]["line"] == 10
    # rstripped
    assert data["matches"][0]["content"] == "  needle here"


async def test_grep_exception_returns_error():
    sb = _sb()
    sb.process.exec = AsyncMock(side_effect=RuntimeError("search error"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_grep.execute(
        daytona_grep.input_model(pattern="x", path="/somewhere"), ctx
    )
    assert result.is_error
    assert "search error" in result.output


async def test_grep_path_not_found():
    sb = _sb(
        exec_result=MagicMock(
            result=json.dumps({"ok": False, "error": "Path does not exist: /missing/dir"}),
            exit_code=1,
        )
    )
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_grep.execute(
        daytona_grep.input_model(pattern="x", path="/missing/dir"), ctx
    )
    assert result.is_error
    assert "does not exist" in result.output


async def test_grep_dot_path_uses_cwd():
    sb = _sb(exec_result=MagicMock(result=json.dumps({"ok": True, "matches": []}), exit_code=0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/workspace"})
    await daytona_grep.execute(daytona_grep.input_model(pattern="x"), ctx)
    executed = sb.process.exec.call_args.args[0]
    assert "/workspace" in executed
    assert "grep" in executed
    assert "root.rglob" not in executed


# ---------------------------------------------------------------------------
# daytona_glob
# ---------------------------------------------------------------------------

async def test_glob_success():
    sb = _sb(exec_result=MagicMock(result="/ws/a.py\n/ws/b.py\n", exit_code=0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    result = await daytona_glob.execute(daytona_glob.input_model(pattern="*.py"), ctx)
    assert not result.is_error
    data = json.loads(result.output)
    assert data["total_files"] == 2
    assert "/ws/a.py" in data["files"]


async def test_glob_no_results():
    sb = _sb(exec_result=MagicMock(result="", exit_code=0))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_glob.execute(
        daytona_glob.input_model(pattern="*.xyz", path="/no/match"), ctx
    )
    data = json.loads(result.output)
    assert data["total_files"] == 0
    assert data["files"] == []


async def test_glob_exception_returns_error():
    sb = _sb()
    sb.process.exec = AsyncMock(side_effect=RuntimeError("glob fail"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_glob.execute(
        daytona_glob.input_model(pattern="*.py", path="/ws"), ctx
    )
    assert result.is_error
    assert "glob fail" in result.output


async def test_glob_strips_double_star_prefix():
    sb = _sb(exec_result=MagicMock(result="/ws/test_a.py\n", exit_code=0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    await daytona_glob.execute(daytona_glob.input_model(pattern="**/*.py"), ctx)
    call_cmd = sb.process.exec.call_args[0][0]
    assert "**/*.py" in call_cmd
    assert "python3 -c" in call_cmd
    assert "find" in call_cmd
    assert "os.walk" not in call_cmd


async def test_glob_quotes_root_path_and_pattern_payload():
    sb = _sb(exec_result=MagicMock(result="", exit_code=0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws with space"})
    await daytona_glob.execute(
        daytona_glob.input_model(pattern="*.py; echo boom", path="."),
        ctx,
    )
    call_cmd = sb.process.exec.call_args[0][0]
    assert "/ws with space" in call_cmd
    assert "*.py; echo boom" in call_cmd
    assert "find /ws with space" not in call_cmd
