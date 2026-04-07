"""Tests for async @tool functions in tools.daytona_toolkit.tools."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.tools import (
    daytona_bash,
    daytona_read_file,
    daytona_write_file,
    daytona_list_files,
    daytona_grep,
    daytona_glob,
)

pytestmark = pytest.mark.asyncio


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _sb(*, exec_result=None, download=None, list_result=None, find_result=None):
    sb = MagicMock()
    sb.process.exec = AsyncMock(return_value=exec_result or MagicMock(result="", exit_code=0))
    sb.fs.download_file = AsyncMock(return_value=download if download is not None else b"")
    sb.fs.upload_file = AsyncMock()
    sb.fs.list_files = AsyncMock(return_value=list_result or [])
    sb.fs.find_files = AsyncMock(return_value=find_result or [])
    return sb


# ---------------------------------------------------------------------------
# daytona_bash
# ---------------------------------------------------------------------------

async def test_bash_success():
    sb = _sb(exec_result=MagicMock(result="hello world", exit_code=0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/workspace"})
    result = await daytona_bash.execute(daytona_bash.input_model(command="echo hello"), ctx)
    assert not result.is_error
    data = json.loads(result.output)
    assert data["stdout"] == "hello world"
    assert data["exit_code"] == 0
    assert data["cwd"] == "/workspace"


async def test_bash_nonzero_exit_is_error():
    sb = _sb(exec_result=MagicMock(result="err", exit_code=1))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_bash.execute(daytona_bash.input_model(command="bad"), ctx)
    assert result.is_error
    assert json.loads(result.output)["exit_code"] == 1


async def test_bash_exception_returns_error():
    sb = MagicMock()
    sb.process.exec = AsyncMock(side_effect=RuntimeError("boom"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_bash.execute(daytona_bash.input_model(command="fail"), ctx)
    assert result.is_error
    assert "boom" in result.output


async def test_bash_no_sandbox_raises():
    with pytest.raises(RuntimeError, match="No Daytona sandbox"):
        await daytona_bash.execute(daytona_bash.input_model(command="echo"), _ctx())


async def test_bash_no_cwd_empty_string():
    sb = _sb(exec_result=MagicMock(result="ok", exit_code=0))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_bash.execute(daytona_bash.input_model(command="echo ok"), ctx)
    assert json.loads(result.output)["cwd"] == ""


async def test_bash_truncates_long_output():
    sb = _sb(exec_result=MagicMock(result="x" * 20_000, exit_code=0))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_bash.execute(daytona_bash.input_model(command="big"), ctx)
    assert "truncated" in json.loads(result.output)["stdout"]


async def test_bash_passes_cwd_to_exec():
    sb = _sb(exec_result=MagicMock(result="", exit_code=0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/proj"})
    await daytona_bash.execute(daytona_bash.input_model(command="ls"), ctx)
    call_kwargs = sb.process.exec.call_args[1]
    assert call_kwargs.get("cwd") == "/proj"


# ---------------------------------------------------------------------------
# daytona_read_file
# ---------------------------------------------------------------------------

async def test_read_file_success():
    sb = _sb(download=b"line one\nline two\nline three\n")
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
    sb = _sb(download=lines.encode())
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="/abs/file.txt", start_line=3, end_line=5), ctx
    )
    data = json.loads(result.output)
    assert data["start_line"] == 3
    assert data["end_line"] == 5
    assert data["total_lines"] == 10


async def test_read_file_resolves_relative_path():
    sb = _sb(download=b"hello")
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/workspace"})
    await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="relative.txt"), ctx
    )
    sb.fs.download_file.assert_called_once_with("/workspace/relative.txt")


async def test_read_file_not_found():
    sb = _sb()
    sb.fs.download_file = AsyncMock(side_effect=FileNotFoundError("gone"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="/missing.txt"), ctx
    )
    assert result.is_error
    assert "does not exist" in result.output


async def test_read_file_generic_exception():
    sb = _sb()
    sb.fs.download_file = AsyncMock(side_effect=RuntimeError("network error"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="/x.txt"), ctx
    )
    assert result.is_error
    assert "network error" in result.output


async def test_read_file_str_content():
    # SDK returns str instead of bytes
    sb = _sb(download="plain string content")
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_read_file.execute(
        daytona_read_file.input_model(file_path="/str.txt"), ctx
    )
    assert not result.is_error


# ---------------------------------------------------------------------------
# daytona_write_file
# ---------------------------------------------------------------------------

async def test_write_file_success():
    sb = _sb()
    sb.process.exec = AsyncMock(return_value=MagicMock(result="", exit_code=0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    result = await daytona_write_file.execute(
        daytona_write_file.input_model(file_path="/ws/new.txt", content="hello"), ctx
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["bytes_written"] == len(b"hello")
    assert data["file_path"] == "/ws/new.txt"


async def test_write_file_resolves_relative_path():
    sb = _sb()
    sb.process.exec = AsyncMock(return_value=MagicMock(result="", exit_code=0))
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/workspace"})
    await daytona_write_file.execute(
        daytona_write_file.input_model(file_path="subdir/file.txt", content="data"), ctx
    )
    call_args = sb.fs.upload_file.call_args[0]
    assert call_args[1] == "/workspace/subdir/file.txt"


async def test_write_file_exception_returns_error():
    sb = _sb()
    sb.process.exec = AsyncMock(return_value=MagicMock(result="", exit_code=0))
    sb.fs.upload_file = AsyncMock(side_effect=RuntimeError("disk full"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_write_file.execute(
        daytona_write_file.input_model(file_path="/x.txt", content="data"), ctx
    )
    assert result.is_error
    assert "disk full" in result.output


# ---------------------------------------------------------------------------
# daytona_list_files
# ---------------------------------------------------------------------------

async def test_list_files_success():
    e1, e2 = MagicMock(), MagicMock()
    e1.name = "a.py"
    e2.name = "b.py"
    sb = _sb(list_result=[e1, e2])
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/workspace"})
    result = await daytona_list_files.execute(
        daytona_list_files.input_model(directory="."), ctx
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert "a.py" in data["entries"]
    assert "b.py" in data["entries"]


async def test_list_files_empty():
    sb = _sb(list_result=[])
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_list_files.execute(
        daytona_list_files.input_model(directory="/empty"), ctx
    )
    assert not result.is_error
    assert json.loads(result.output)["entries"] == []


async def test_list_files_exception():
    sb = _sb()
    sb.fs.list_files = AsyncMock(side_effect=FileNotFoundError("gone"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_list_files.execute(
        daytona_list_files.input_model(directory="/missing"), ctx
    )
    assert result.is_error
    assert "does not exist" in result.output


async def test_list_files_dot_uses_cwd():
    sb = _sb(list_result=[])
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/workspace"})
    await daytona_list_files.execute(daytona_list_files.input_model(directory="."), ctx)
    sb.fs.list_files.assert_called_once_with("/workspace")


async def test_list_files_absolute_path():
    sb = _sb(list_result=[])
    ctx = _ctx({"daytona_sandbox": sb})
    await daytona_list_files.execute(
        daytona_list_files.input_model(directory="/abs/dir"), ctx
    )
    sb.fs.list_files.assert_called_once_with("/abs/dir")


# ---------------------------------------------------------------------------
# daytona_grep
# ---------------------------------------------------------------------------

async def test_grep_no_matches():
    sb = _sb(find_result=[])
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/ws"})
    result = await daytona_grep.execute(daytona_grep.input_model(pattern="needle"), ctx)
    assert not result.is_error
    data = json.loads(result.output)
    assert data["total_matches"] == 0
    assert data["matches"] == []


async def test_grep_with_matches():
    m1 = MagicMock(file="/ws/a.py", line=10, content="  needle here  ")
    m2 = MagicMock(file="/ws/b.py", line=20, content="another needle")
    sb = _sb(find_result=[m1, m2])
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
    sb.fs.find_files = AsyncMock(side_effect=RuntimeError("search error"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_grep.execute(
        daytona_grep.input_model(pattern="x", path="/somewhere"), ctx
    )
    assert result.is_error
    assert "search error" in result.output


async def test_grep_path_not_found():
    sb = _sb()
    sb.fs.find_files = AsyncMock(side_effect=FileNotFoundError("missing"))
    ctx = _ctx({"daytona_sandbox": sb})
    result = await daytona_grep.execute(
        daytona_grep.input_model(pattern="x", path="/missing/dir"), ctx
    )
    assert result.is_error
    assert "does not exist" in result.output


async def test_grep_dot_path_uses_cwd():
    sb = _sb(find_result=[])
    ctx = _ctx({"daytona_sandbox": sb, "daytona_cwd": "/workspace"})
    await daytona_grep.execute(daytona_grep.input_model(pattern="x"), ctx)
    sb.fs.find_files.assert_called_once_with("/workspace", "x")


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
    assert "**/" not in call_cmd
    assert "*.py" in call_cmd
