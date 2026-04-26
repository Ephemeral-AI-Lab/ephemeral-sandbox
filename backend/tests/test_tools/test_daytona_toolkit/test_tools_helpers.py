"""Tests for pure helpers in tools.daytona_toolkit._daytona_utils."""

from __future__ import annotations

from pathlib import Path

from tools.core.base import ToolExecutionContextService
from tools.daytona_toolkit._daytona_utils import (
    _format_shell_stdout,
    _get_repo_root,
    _path_error,
    _resolve_path,
    _truncate,
    _truncate_tail,
)


def _ctx(services=None) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"), services=services or {})


# ---------------------------------------------------------------------------
# _truncate
# ---------------------------------------------------------------------------


def test_truncate_short_passthrough():
    assert _truncate("hello") == "hello"


def test_truncate_exact_boundary():
    text = "x" * 8000
    assert _truncate(text) == text


def test_truncate_long_text():
    text = "a" * 10_000
    result = _truncate(text)
    assert "truncated" in result
    assert len(result) < len(text)
    assert result.startswith("a" * 4000)
    assert result.endswith("a" * 4000)


def test_truncate_custom_max():
    text = "ab" * 100
    result = _truncate(text, max_chars=10)
    assert "truncated" in result


def test_truncate_tail_keeps_end_only():
    text = "prefix-" + ("x" * 50) + "-suffix"
    result = _truncate_tail(text, max_chars=20)
    assert "truncated" in result
    assert "prefix-" not in result
    assert result.endswith(text[-20:])


def test_format_shell_stdout_prefers_tail_for_errors():
    text = "header-" + ("m" * 50) + "-failure-tail"
    result = _format_shell_stdout(text, exit_code=1, max_chars=25)
    assert "truncated" in result
    assert "header-" not in result
    assert result.endswith(text[-25:])


def test_format_shell_stdout_keeps_head_and_tail_for_success():
    text = "header-" + ("m" * 50) + "-success-tail"
    result = _format_shell_stdout(text, exit_code=0, max_chars=24)
    assert "truncated" in result
    assert result.startswith(text[:12])
    assert result.endswith(text[-12:])


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
