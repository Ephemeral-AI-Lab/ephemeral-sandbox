"""Tests for tools.daytona_toolkit.toolkit.DaytonaToolkit."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.toolkit import DaytonaToolkit


pytestmark = pytest.mark.asyncio


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


# ---------------------------------------------------------------------------
# __init__ and module-level import
# ---------------------------------------------------------------------------

def test_init_import():
    from tools.daytona_toolkit import DaytonaToolkit as DT
    assert DT is DaytonaToolkit


def test_toolkit_instantiation():
    tk = DaytonaToolkit(sandbox_id="sb-test123")
    assert tk.sandbox_id == "sb-test123"
    assert tk.name == "sandbox_operations"


def test_toolkit_no_sandbox_id():
    tk = DaytonaToolkit()
    assert tk.sandbox_id is None


# ---------------------------------------------------------------------------
# Tool registration
# ---------------------------------------------------------------------------

def test_toolkit_registers_expected_tools():
    tk = DaytonaToolkit()
    names = set(tk.tool_names())
    expected = {
        "daytona_bash",
        "daytona_read_file",
        "daytona_write_file",
        "daytona_list_files",
        "daytona_grep",
        "daytona_glob",
        "daytona_edit_file",
        "daytona_lsp_hover",
        "daytona_lsp_definition",
        "daytona_lsp_references",
        "daytona_lsp_diagnostics",
        "daytona_codeact",
    }
    assert expected.issubset(names)


def test_toolkit_get_tool():
    tk = DaytonaToolkit()
    tool = tk.get("daytona_bash")
    assert tool is not None
    assert tool.name == "daytona_bash"


def test_toolkit_get_missing_tool():
    tk = DaytonaToolkit()
    assert tk.get("nonexistent_tool") is None


def test_toolkit_list_tools_length():
    tk = DaytonaToolkit()
    tools = tk.list_tools()
    assert len(tools) == 12


# ---------------------------------------------------------------------------
# _get_sandbox (sync)
# ---------------------------------------------------------------------------

def test_get_sandbox_no_id_raises():
    tk = DaytonaToolkit()
    with pytest.raises(RuntimeError, match="No sandbox_id"):
        tk._get_sandbox()


def test_get_sandbox_caches_instance():
    tk = DaytonaToolkit(sandbox_id="sb-abc")
    fake_sb = MagicMock()
    with patch("tools.daytona_toolkit.toolkit.DaytonaToolkit._get_sandbox") as mock_get:
        mock_get.return_value = fake_sb
        result = tk._get_sandbox()
        assert result is fake_sb


def test_get_sandbox_uses_cached():
    tk = DaytonaToolkit(sandbox_id="sb-abc")
    fake_sb = MagicMock()
    tk._sandbox = fake_sb
    # Should return cached without importing sandbox module
    result = tk._get_sandbox()
    assert result is fake_sb


# ---------------------------------------------------------------------------
# _get_sandbox_async
# ---------------------------------------------------------------------------

async def test_get_sandbox_async_no_id_raises():
    tk = DaytonaToolkit()
    with pytest.raises(RuntimeError, match="No sandbox_id"):
        await tk._get_sandbox_async()


async def test_get_sandbox_async_caches_per_loop():
    tk = DaytonaToolkit(sandbox_id="sb-xyz")
    fake_sb = MagicMock()

    async def fake_get_async(sandbox_id):
        return fake_sb

    with patch("sandbox.async_client.get_async_sandbox", new=fake_get_async, create=True):
        # Patch the import inside the method
        import sys
        mock_module = MagicMock()
        mock_module.get_async_sandbox = fake_get_async
        with patch.dict("sys.modules", {"sandbox.async_client": mock_module}):
            result = await tk._get_sandbox_async()
            assert result is fake_sb
            # Second call with same loop → should use cache
            result2 = await tk._get_sandbox_async()
            assert result2 is fake_sb


async def test_get_sandbox_async_invalidates_on_new_loop():
    """Stale sandbox from different loop ID should be discarded."""
    tk = DaytonaToolkit(sandbox_id="sb-xyz")
    old_sb = MagicMock()
    tk._sandbox = old_sb
    tk._sandbox_loop_id = 999999  # fake old loop id

    new_sb = MagicMock()
    async def fake_get_async(sandbox_id):
        return new_sb

    mock_module = MagicMock()
    mock_module.get_async_sandbox = fake_get_async
    import sys
    with patch.dict("sys.modules", {"sandbox.async_client": mock_module}):
        result = await tk._get_sandbox_async()
        assert result is new_sb


# ---------------------------------------------------------------------------
# prepare_context (sync)
# ---------------------------------------------------------------------------

def test_prepare_context_injects_sandbox_and_cwd():
    tk = DaytonaToolkit(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx()

    with patch.object(tk, "_get_sandbox", return_value=fake_sb), \
         patch.object(DaytonaToolkit, "_resolve_cwd_sync", return_value="/workspace"), \
         patch.object(tk, "_inject_ci"):
        tk.prepare_context(ctx)

    assert ctx.metadata["daytona_sandbox"] is fake_sb
    assert ctx.metadata["daytona_cwd"] == "/workspace"


def test_prepare_context_no_cwd_skips_metadata_key():
    tk = DaytonaToolkit(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx()

    with patch.object(tk, "_get_sandbox", return_value=fake_sb), \
         patch.object(DaytonaToolkit, "_resolve_cwd_sync", return_value=None), \
         patch.object(tk, "_inject_ci"):
        tk.prepare_context(ctx)

    assert ctx.metadata["daytona_sandbox"] is fake_sb
    assert "daytona_cwd" not in ctx.metadata


# ---------------------------------------------------------------------------
# prepare_context_async
# ---------------------------------------------------------------------------

async def test_prepare_context_async_injects_sandbox_and_cwd():
    tk = DaytonaToolkit(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx()

    async def fake_get_async():
        return fake_sb

    async def fake_resolve_cwd(sb):
        return "/async/workspace"

    with patch.object(tk, "_get_sandbox_async", new=AsyncMock(return_value=fake_sb)), \
         patch.object(DaytonaToolkit, "_resolve_cwd_async", new=AsyncMock(return_value="/async/workspace")), \
         patch.object(tk, "_inject_ci"):
        await tk.prepare_context_async(ctx)

    assert ctx.metadata["daytona_sandbox"] is fake_sb
    assert ctx.metadata["daytona_cwd"] == "/async/workspace"


async def test_prepare_context_async_no_cwd():
    tk = DaytonaToolkit(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx()

    with patch.object(tk, "_get_sandbox_async", new=AsyncMock(return_value=fake_sb)), \
         patch.object(DaytonaToolkit, "_resolve_cwd_async", new=AsyncMock(return_value=None)), \
         patch.object(tk, "_inject_ci"):
        await tk.prepare_context_async(ctx)

    assert ctx.metadata["daytona_sandbox"] is fake_sb
    assert "daytona_cwd" not in ctx.metadata


# ---------------------------------------------------------------------------
# _resolve_cwd_sync / _resolve_cwd_async (static methods via patch)
# ---------------------------------------------------------------------------

def test_resolve_cwd_sync_calls_discover_workspace():
    fake_sb = MagicMock()
    mock_module = MagicMock()
    mock_module.discover_workspace.return_value = "/found/workspace"
    import sys
    with patch.dict("sys.modules", {"sandbox.workspace": mock_module}):
        result = DaytonaToolkit._resolve_cwd_sync(fake_sb)
        assert result == "/found/workspace"
        mock_module.discover_workspace.assert_called_once_with(fake_sb)


async def test_resolve_cwd_async_calls_discover_workspace_async():
    fake_sb = MagicMock()
    mock_module = MagicMock()
    mock_module.discover_workspace_async = AsyncMock(return_value="/async/found")
    import sys
    with patch.dict("sys.modules", {"sandbox.workspace": mock_module}):
        result = await DaytonaToolkit._resolve_cwd_async(fake_sb)
        assert result == "/async/found"


# ---------------------------------------------------------------------------
# Toolkit description/instructions present
# ---------------------------------------------------------------------------

def test_toolkit_has_description():
    tk = DaytonaToolkit()
    assert tk.description
    assert "sandbox" in tk.description.lower()


def test_toolkit_has_instructions():
    tk = DaytonaToolkit()
    assert tk.instructions
    assert "daytona_bash" in tk.instructions


# ---------------------------------------------------------------------------
# _inject_ci
# ---------------------------------------------------------------------------

def test_inject_ci_calls_inject_code_intelligence():
    tk = DaytonaToolkit(sandbox_id="sb-ci")
    fake_sb = MagicMock()
    ctx = _ctx()
    mock_module = MagicMock()
    import sys
    with patch.dict("sys.modules", {"sandbox.workspace": mock_module}):
        tk._inject_ci(ctx, fake_sb, "/workspace")
    mock_module.inject_code_intelligence.assert_called_once_with(
        ctx, "sb-ci", fake_sb, "/workspace"
    )
