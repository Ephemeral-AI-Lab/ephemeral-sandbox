"""Host-side ``@tool(name='glob')`` and ``@tool(name='grep')`` wiring."""

from __future__ import annotations

import importlib
import json
from pathlib import Path

import pytest

from sandbox._shared.models import GlobResult, GrepResult
from tools._framework.core.base import ToolExecutionContextService

glob_tool_module = importlib.import_module("tools.sandbox.glob.glob")
grep_tool_module = importlib.import_module("tools.sandbox.grep.grep")


def _ctx(services: dict[str, object] | None = None) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"), services=services or {})


def _sandbox_ctx() -> ToolExecutionContextService:
    return _ctx({"sandbox_id": "sb-1", "repo_root": "/repo"})


async def test_glob_tool_resolves_repo_root_and_returns_filenames(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    received: dict[str, object] = {}

    async def fake_glob(sandbox_id, request, **kwargs):
        received["sandbox_id"] = sandbox_id
        received["pattern"] = request.pattern
        received["path"] = request.path
        return GlobResult(
            success=True,
            filenames=("a.py", "pkg/b.py"),
            num_files=2,
            truncated=False,
            timings={"api.glob.total_s": 0.05},
        )

    monkeypatch.setattr(glob_tool_module.sandbox_api, "glob", fake_glob)

    tool = glob_tool_module.glob
    result = await tool.execute(
        tool.input_model(pattern="*.py", path="src"),
        _sandbox_ctx(),
    )

    assert not result.is_error
    payload = json.loads(result.output)
    assert payload["filenames"] == ["a.py", "pkg/b.py"]
    assert payload["num_files"] == 2
    assert payload["truncated"] is False
    assert payload["pattern"] == "*.py"
    assert payload["cwd"] == "/repo"
    assert received["sandbox_id"] == "sb-1"
    assert received["pattern"] == "*.py"
    # path arg gets resolved against /repo
    assert received["path"] == "/repo/src"


async def test_glob_tool_returns_error_without_sandbox_id() -> None:
    tool = glob_tool_module.glob
    result = await tool.execute(
        tool.input_model(pattern="*.py"),
        _ctx({"repo_root": "/repo"}),
    )

    assert result.is_error
    assert result.metadata.get("sandbox_required") is True


async def test_glob_tool_surfaces_truncated_flag(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def fake_glob(sandbox_id, request, **kwargs):
        return GlobResult(
            success=True,
            filenames=tuple(f"f{i}.py" for i in range(100)),
            num_files=100,
            truncated=True,
            timings={},
        )

    monkeypatch.setattr(glob_tool_module.sandbox_api, "glob", fake_glob)

    tool = glob_tool_module.glob
    result = await tool.execute(
        tool.input_model(pattern="*.py"),
        _sandbox_ctx(),
    )

    payload = json.loads(result.output)
    assert payload["truncated"] is True
    assert payload["num_files"] == 100


async def test_glob_tool_handles_exception(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def fake_glob(sandbox_id, request, **kwargs):
        raise RuntimeError("daemon unreachable")

    monkeypatch.setattr(glob_tool_module.sandbox_api, "glob", fake_glob)

    tool = glob_tool_module.glob
    result = await tool.execute(
        tool.input_model(pattern="*.py"),
        _sandbox_ctx(),
    )

    assert result.is_error
    assert "daemon unreachable" in result.output


async def test_grep_tool_dispatches_with_full_options(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    received: dict[str, object] = {}

    async def fake_grep(sandbox_id, request, **kwargs):
        received["sandbox_id"] = sandbox_id
        received["pattern"] = request.pattern
        received["path"] = request.path
        received["glob_filter"] = request.glob_filter
        received["output_mode"] = request.output_mode
        received["head_limit"] = request.head_limit
        received["case_insensitive"] = request.case_insensitive
        received["line_numbers"] = request.line_numbers
        received["multiline"] = request.multiline
        return GrepResult(
            success=True,
            output_mode="content",
            filenames=("a.py",),
            content="a.py:2:hit\n",
            num_files=1,
            num_lines=1,
            num_matches=1,
            applied_limit=10,
            applied_offset=0,
            truncated=False,
            timings={"api.grep.total_s": 0.1},
        )

    monkeypatch.setattr(
        grep_tool_module.sandbox_api, "grep", fake_grep
    )

    tool = grep_tool_module.grep
    result = await tool.execute(
        tool.input_model(
            pattern="hit",
            path="pkg",
            glob_filter="*.py",
            output_mode="content",
            head_limit=10,
            line_numbers=True,
            multiline=True,
            case_insensitive=True,
        ),
        _sandbox_ctx(),
    )

    assert not result.is_error
    payload = json.loads(result.output)
    assert payload["mode"] == "content"
    assert payload["filenames"] == ["a.py"]
    assert payload["content"] == "a.py:2:hit\n"
    assert payload["num_matches"] == 1
    assert payload["applied_limit"] == 10
    assert received["pattern"] == "hit"
    assert received["path"] == "/repo/pkg"
    assert received["glob_filter"] == "*.py"
    assert received["output_mode"] == "content"
    assert received["head_limit"] == 10
    assert received["case_insensitive"] is True
    assert received["line_numbers"] is True
    assert received["multiline"] is True


async def test_grep_tool_zero_head_limit_propagates_as_unlimited(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """``head_limit=0`` is the documented "unlimited" sentinel — it must reach
    the daemon as ``0`` (not ``None``), or the daemon will silently fall back
    to its 250-entry default. Regression for the host→API→daemon translation
    bug caught by the architect during ralph verification.
    """
    received: dict[str, object] = {}

    async def fake_grep(sandbox_id, request, **kwargs):
        received["head_limit"] = request.head_limit
        return GrepResult(
            success=True,
            output_mode="files_with_matches",
            filenames=(),
            content="",
            num_files=0,
            num_lines=0,
            num_matches=0,
            applied_limit=None,
            applied_offset=0,
            truncated=False,
            timings={},
        )

    monkeypatch.setattr(
        grep_tool_module.sandbox_api, "grep", fake_grep
    )

    tool = grep_tool_module.grep
    result = await tool.execute(
        tool.input_model(pattern="needle", head_limit=0),
        _sandbox_ctx(),
    )

    # The 0 sentinel must propagate verbatim — not become None — so the
    # daemon's unlimited branch fires.
    assert received["head_limit"] == 0
    # And the tool surfaces the daemon's "unlimited applied" verdict.
    payload = json.loads(result.output)
    assert payload["applied_limit"] is None


async def test_grep_tool_rejects_invalid_output_mode() -> None:
    tool = grep_tool_module.grep
    with pytest.raises(Exception):
        # Pydantic validates Literal["content","files_with_matches","count"]
        tool.input_model(pattern="x", output_mode="bogus")


async def test_grep_tool_returns_error_without_sandbox_id() -> None:
    tool = grep_tool_module.grep
    result = await tool.execute(
        tool.input_model(pattern="needle"),
        _ctx({"repo_root": "/repo"}),
    )

    assert result.is_error
    assert result.metadata.get("sandbox_required") is True


async def test_grep_tool_handles_exception(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def fake_grep(sandbox_id, request, **kwargs):
        raise RuntimeError("daemon unreachable")

    monkeypatch.setattr(
        grep_tool_module.sandbox_api, "grep", fake_grep
    )

    tool = grep_tool_module.grep
    result = await tool.execute(
        tool.input_model(pattern="needle"),
        _sandbox_ctx(),
    )

    assert result.is_error
    assert "daemon unreachable" in result.output
