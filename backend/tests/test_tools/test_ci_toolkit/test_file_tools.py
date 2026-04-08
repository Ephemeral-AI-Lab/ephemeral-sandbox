"""Tests for tools.ci_toolkit.file_tools."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from tools.core.base import ToolExecutionContext
from tools.ci_toolkit.file_tools import ci_read_file


pytestmark = pytest.mark.asyncio


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _ctx_with_svc(svc) -> ToolExecutionContext:
    return _ctx({"ci_service": svc})


# ---------------------------------------------------------------------------
# No CI service, fallback to direct file read
# ---------------------------------------------------------------------------

async def test_read_file_no_service_reads_directly(tmp_path):
    """Without a CI service, falls back to reading the file from disk."""
    f = tmp_path / "hello.py"
    f.write_text("line one\nline two\nline three\n")

    with patch("tools.ci_toolkit.file_tools.get_ci_service", return_value=None):
        ctx = _ctx()
        result = await ci_read_file.execute(
            ci_read_file.input_model(path=str(f)),
            ctx,
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["file_path"] == str(f)
    assert data["total_lines"] == 3
    assert data["start_line"] == 1
    assert "line one" in data["content"]


async def test_read_file_not_found_returns_error(tmp_path):
    """Missing file returns is_error=True."""
    with patch("tools.ci_toolkit.file_tools.get_ci_service", return_value=None):
        ctx = _ctx()
        result = await ci_read_file.execute(
            ci_read_file.input_model(path=str(tmp_path / "missing.py")),
            ctx,
        )

    assert result.is_error
    assert "not found" in result.output.lower() or "File not found" in result.output


async def test_read_file_binary_returns_error(tmp_path):
    """Binary file returns is_error=True with 'Binary file' message."""
    f = tmp_path / "bin.dat"
    f.write_bytes(b"\x00\x01\x02\xff\xfe")

    with patch("tools.ci_toolkit.file_tools.get_ci_service", return_value=None):
        ctx = _ctx()
        result = await ci_read_file.execute(
            ci_read_file.input_model(path=str(f)),
            ctx,
        )

    assert result.is_error
    assert "Binary" in result.output


async def test_read_file_generic_exception_returns_error(tmp_path):
    """An unexpected exception during read returns is_error=True."""
    f = tmp_path / "file.py"
    f.write_text("content")

    # Path is a lazy import inside the function body; patch at pathlib level
    with patch("tools.ci_toolkit.file_tools.get_ci_service", return_value=None):
        with patch("pathlib.Path.read_text", side_effect=OSError("permission denied")):
            ctx = _ctx()
            result = await ci_read_file.execute(
                ci_read_file.input_model(path=str(f)),
                ctx,
            )

    assert result.is_error
    assert "permission denied" in result.output


# ---------------------------------------------------------------------------
# CI service tree cache hit
# ---------------------------------------------------------------------------

async def test_read_file_uses_tree_cache_content():
    """When CI service has a cached tree entry, uses that content."""
    entry = MagicMock()
    entry.content = "cached line 1\ncached line 2\n"

    svc = MagicMock()
    svc.tree_cache.get_tree.return_value = entry

    with patch("tools.ci_toolkit.file_tools.get_ci_service", return_value=svc):
        ctx = _ctx_with_svc(svc)
        result = await ci_read_file.execute(
            ci_read_file.input_model(path="/some/file.py"),
            ctx,
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["total_lines"] == 2
    assert "cached line 1" in data["content"]


async def test_read_file_tree_cache_miss_falls_back_to_disk(tmp_path):
    """Cache miss on CI service falls back to direct file read."""
    f = tmp_path / "fallback.py"
    f.write_text("fallback content\n")

    svc = MagicMock()
    svc.tree_cache.get_tree.return_value = None  # cache miss

    with patch("tools.ci_toolkit.file_tools.get_ci_service", return_value=svc):
        ctx = _ctx_with_svc(svc)
        result = await ci_read_file.execute(
            ci_read_file.input_model(path=str(f)),
            ctx,
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert "fallback content" in data["content"]


# ---------------------------------------------------------------------------
# Line range / pagination
# ---------------------------------------------------------------------------

async def test_read_file_start_line_offset(tmp_path):
    """start_line parameter returns lines starting at that offset."""
    f = tmp_path / "multi.py"
    lines = [f"line{i}" for i in range(1, 11)]
    f.write_text("\n".join(lines))

    with patch("tools.ci_toolkit.file_tools.get_ci_service", return_value=None):
        ctx = _ctx()
        result = await ci_read_file.execute(
            ci_read_file.input_model(path=str(f), start_line=5, max_lines=3),
            ctx,
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["start_line"] == 5
    assert data["end_line"] == 7
    assert "line5" in data["content"]
    assert "line8" not in data["content"]


async def test_read_file_max_lines_limits_output(tmp_path):
    """max_lines parameter caps the number of returned lines."""
    f = tmp_path / "long.py"
    f.write_text("\n".join(f"l{i}" for i in range(50)))

    with patch("tools.ci_toolkit.file_tools.get_ci_service", return_value=None):
        ctx = _ctx()
        result = await ci_read_file.execute(
            ci_read_file.input_model(path=str(f), max_lines=5),
            ctx,
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["end_line"] - data["start_line"] + 1 <= 5


async def test_read_file_truncated_flag_set_for_large_content(tmp_path):
    """Files exceeding _MAX_CHARS get truncated=True in the result."""
    f = tmp_path / "big.py"
    # Write more than 32_000 chars
    f.write_text("x" * 33_000)

    with patch("tools.ci_toolkit.file_tools.get_ci_service", return_value=None):
        ctx = _ctx()
        result = await ci_read_file.execute(
            ci_read_file.input_model(path=str(f)),
            ctx,
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["truncated"] is True


async def test_read_file_keeps_true_total_lines_for_large_file_tail_reads(tmp_path):
    """Large files should keep their real line count when reading from a high
    start_line instead of pretending the file ends at the char cap."""
    f = tmp_path / "huge.py"
    lines = [f"line{i}" for i in range(1, 2001)]
    f.write_text("\n".join(lines))

    with patch("tools.ci_toolkit.file_tools.get_ci_service", return_value=None):
        ctx = _ctx()
        result = await ci_read_file.execute(
            ci_read_file.input_model(path=str(f), start_line=1700, max_lines=5),
            ctx,
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["total_lines"] == 2000
    assert data["start_line"] == 1700
    assert data["end_line"] == 1704
    assert "line1700" in data["content"]
    assert "line1704" in data["content"]


async def test_read_file_no_truncation_for_small_content(tmp_path):
    """Small files do not get truncated=True."""
    f = tmp_path / "small.py"
    f.write_text("small content\n")

    with patch("tools.ci_toolkit.file_tools.get_ci_service", return_value=None):
        ctx = _ctx()
        result = await ci_read_file.execute(
            ci_read_file.input_model(path=str(f)),
            ctx,
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["truncated"] is False


# ---------------------------------------------------------------------------
# Result structure
# ---------------------------------------------------------------------------

async def test_read_file_result_has_expected_keys(tmp_path):
    """Result JSON contains all expected keys."""
    f = tmp_path / "check.py"
    f.write_text("a\nb\n")

    with patch("tools.ci_toolkit.file_tools.get_ci_service", return_value=None):
        ctx = _ctx()
        result = await ci_read_file.execute(
            ci_read_file.input_model(path=str(f)),
            ctx,
        )

    data = json.loads(result.output)
    for key in ("file_path", "start_line", "end_line", "total_lines", "truncated", "content"):
        assert key in data, f"Missing key: {key}"
