"""Tests for pure helpers in sandbox.daytona_utils."""

from __future__ import annotations

import json
from pathlib import Path

from tools.core.base import ToolExecutionContextService
from tools.daytona_toolkit._file_tool_helpers import (
    build_find_result,
    build_read_file_result,
)
from sandbox.daytona_utils import (
    _get_repo_root,
    _path_error,
    _resolve_path,
)
from tools.daytona_toolkit.shell import _build_tool_output


def _ctx(services=None) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"), services=services or {})


def test_build_tool_output_preserves_all_shell_outputs():
    long_command = "python -c " + repr("x" * 200)
    long_stdout = "start-" + ("x" * 9_000) + "-end"
    long_error = "error-" + ("y" * 1_000)
    shells = [
        {
            "command": f"{long_command}-{idx}",
            "exit_code": 0,
            "stdout": f"{long_stdout}-{idx}",
            "stderr": "",
        }
        for idx in range(4)
    ]

    result = _build_tool_output(
        context=_ctx(),
        status="ok",
        files_written=0,
        shells=shells,
        warnings=[],
        error=long_error,
    )
    payload = json.loads(result.output)

    assert payload["error"] == long_error
    assert payload["shell_summaries"][-1] == f"$ {long_command}-3 -> exit 0"
    assert payload["shell_outputs"] == shells


def test_build_read_file_result_preserves_full_selected_content():
    long_line = "x" * 9_000
    result = build_read_file_result(
        context=_ctx(),
        file_path="/tmp/example.txt",
        content=f"first\n{long_line}\nlast",
        start_line=1,
        end_line=None,
    )
    payload = json.loads(result.output)

    assert long_line in payload["content"]
    assert payload["content"].endswith("   3: last")


def test_build_find_result_preserves_all_matches_without_truncated_flag():
    matches = [
        {"file": f"/tmp/{idx}.py", "line": idx, "content": f"match {idx}"}
        for idx in range(600)
    ]

    result = build_find_result(
        cwd="/tmp",
        pattern="match",
        path="/tmp",
        matches=matches,
    )
    payload = json.loads(result.output)

    assert len(payload["matches"]) == len(matches)
    assert payload["total_matches"] == len(matches)
    assert "truncated" not in payload


# ---------------------------------------------------------------------------
# _path_error
# ---------------------------------------------------------------------------


def test_path_error_file_not_found():
    exc = FileNotFoundError("gone")
    assert _path_error(exc, "/some/path") == "Path does not exist: /some/path"


def test_path_error_message_contains_no_such_file():
    exc = RuntimeError("No such file or directory")
    result = _path_error(exc, "/x")
    assert result is not None
    assert "/x" in result


def test_path_error_sdk_prefix_colon_suffix():
    exc = RuntimeError("Failed to list files:")
    assert _path_error(exc, "/dir") == "Path does not exist: /dir"


def test_path_error_unrecognized_returns_none():
    assert _path_error(RuntimeError("something totally different"), "/p") is None


def test_path_error_sdk_prefix_without_trailing_colon():
    # SDK prefix but no trailing colon — should NOT match
    exc = RuntimeError("Failed to list files: details here")
    assert _path_error(exc, "/p") is None


# ---------------------------------------------------------------------------
# _get_repo_root
# ---------------------------------------------------------------------------


def test_get_repo_root_returns_value():
    ctx = _ctx({"repo_root": "/workspace/project"})
    assert _get_repo_root(ctx) == "/workspace/project"


def test_get_repo_root_returns_none_when_missing():
    assert _get_repo_root(_ctx()) is None


# ---------------------------------------------------------------------------
# _resolve_path
# ---------------------------------------------------------------------------


def test_resolve_path_absolute_unchanged():
    ctx = _ctx({"repo_root": "/workspace"})
    assert _resolve_path("/abs/path", ctx) == "/abs/path"


def test_resolve_path_relative_joins_cwd():
    ctx = _ctx({"repo_root": "/workspace"})
    assert _resolve_path("relative/file.py", ctx) == "/workspace/relative/file.py"


def test_resolve_path_relative_no_cwd_unchanged():
    assert _resolve_path("bare_file.py", _ctx()) == "bare_file.py"
