"""Tests for pure helpers in tools.daytona_toolkit.tools."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.tools import (
    _truncate,
    _get_sandbox,
    _path_error,
    _get_cwd,
    _resolve_path,
)


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


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


# ---------------------------------------------------------------------------
# _get_sandbox
# ---------------------------------------------------------------------------

def test_get_sandbox_returns_sandbox():
    sb = MagicMock()
    ctx = _ctx({"daytona_sandbox": sb})
    assert _get_sandbox(ctx) is sb


def test_get_sandbox_raises_when_missing():
    ctx = _ctx()
    with pytest.raises(RuntimeError, match="No Daytona sandbox"):
        _get_sandbox(ctx)


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
# _get_cwd
# ---------------------------------------------------------------------------

def test_get_cwd_returns_value():
    ctx = _ctx({"daytona_cwd": "/workspace/project"})
    assert _get_cwd(ctx) == "/workspace/project"


def test_get_cwd_returns_none_when_missing():
    assert _get_cwd(_ctx()) is None


# ---------------------------------------------------------------------------
# _resolve_path
# ---------------------------------------------------------------------------

def test_resolve_path_absolute_unchanged():
    ctx = _ctx({"daytona_cwd": "/workspace"})
    assert _resolve_path("/abs/path", ctx) == "/abs/path"


def test_resolve_path_relative_joins_cwd():
    ctx = _ctx({"daytona_cwd": "/workspace"})
    assert _resolve_path("relative/file.py", ctx) == "/workspace/relative/file.py"


def test_resolve_path_relative_no_cwd_unchanged():
    assert _resolve_path("bare_file.py", _ctx()) == "bare_file.py"
