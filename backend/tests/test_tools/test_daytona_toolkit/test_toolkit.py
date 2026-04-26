"""Tests for Daytona tool exports and context preparation."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from tools.core.base import ToolExecutionContextService, ToolRegistry
from tools.daytona_toolkit import DaytonaContextPreparer, make_daytona_tools


# pytest-asyncio runs in auto mode (configured in pyproject.toml) — async
# test functions are handled automatically, so no module-level marker is
# needed. Leaving `pytestmark = pytest.mark.asyncio` in place here would
# emit a warning for every *sync* test in the file.


def _ctx(services=None) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"), services=services or {})


def _sandbox_with_noop_io():
    sandbox = MagicMock()
    sandbox.fs.download_file = AsyncMock(return_value=b"old")
    sandbox.fs.upload_file = AsyncMock()
    sandbox.process.exec = AsyncMock(return_value=MagicMock(result="", exit_code=0))
    return sandbox


# ---------------------------------------------------------------------------
# Context setup and module-level import
# ---------------------------------------------------------------------------


def test_init_import():
    from tools.daytona_toolkit import DaytonaContextPreparer as DCP

    assert DCP is DaytonaContextPreparer


def test_context_preparer_instantiation():
    preparer = DaytonaContextPreparer(sandbox_id="sb-test123")
    assert preparer.sandbox_id == "sb-test123"


def test_daytona_exports_expected_tools():
    names = {tool.name for tool in make_daytona_tools()}
    expected = {
        "shell",
        "read_file",
        "write_file",
        "grep",
        "glob",
        "edit_file",
        "delete_file",
        "move_file",
    }
    assert names == expected
    assert not any(name.startswith("daytona_lsp_") for name in names)


async def test_registered_write_capable_tools_require_ci_service():
    registry = ToolRegistry()
    registry.register_many(make_daytona_tools())
    tools_by_name = {tool.name: tool for tool in registry.list_tools()}
    write_inputs = {
        "write_file": {"file_path": "/repo/new.txt", "content": "hello"},
        "edit_file": {
            "file_path": "/repo/app.py",
            "old_text": "old",
            "new_text": "new",
        },
        "shell": {"command": "echo hi"},
        "delete_file": {"path": "/repo/app.py"},
        "move_file": {
            "src_path": "/repo/src.py",
            "target_path": "/repo/dst.py",
        },
    }

    assert set(write_inputs).issubset(tools_by_name)
    assert set(tools_by_name) - set(write_inputs) == {
        "read_file",
        "grep",
        "glob",
    }

    for tool_name, tool_input in write_inputs.items():
        ctx = _ctx({"daytona_sandbox": _sandbox_with_noop_io(), "repo_root": "/repo"})
        tool = tools_by_name[tool_name]
        result = await tool.execute(tool.input_model(**tool_input), ctx)

        assert result.is_error, tool_name
        assert "Code intelligence service is unavailable" in result.output
        assert result.metadata.get("ci_required") is True, tool_name


def test_make_daytona_tools_includes_shell():
    names = {tool.name for tool in make_daytona_tools()}

    assert "shell" in names
    assert "edit_file" in names
    assert "daytona_list_files" not in names


def test_get_daytona_tool_by_name():
    tools = {tool.name: tool for tool in make_daytona_tools()}
    tool = tools.get("shell")
    assert tool is not None
    assert tool.name == "shell"


def test_shell_schema_describes_command():
    tools = {tool.name: tool for tool in make_daytona_tools()}
    tool = tools.get("shell")
    assert tool is not None

    schema = tool.to_api_schema()["input_schema"]
    command_description = schema["properties"]["command"]["description"]
    assert command_description == "Shell command to run for tests, builds, or verification."

    assert tool.short_description == "Run a shell command from the repo root."


def test_missing_daytona_tool_absent():
    tools = {tool.name: tool for tool in make_daytona_tools()}
    assert tools.get("nonexistent_tool") is None


def test_daytona_tool_count():
    tools = make_daytona_tools()
    assert len(tools) == 8


def test_daytona_tools_omit_instruction_block():
    assert all(not hasattr(tool, "instructions") for tool in make_daytona_tools())


# ---------------------------------------------------------------------------
# _get_sandbox (sync)
# ---------------------------------------------------------------------------


def test_get_sandbox_no_id_raises():
    tk = DaytonaContextPreparer("")
    with pytest.raises(RuntimeError, match="No sandbox_id"):
        tk._get_sandbox()


def test_get_sandbox_caches_instance():
    tk = DaytonaContextPreparer(sandbox_id="sb-abc")
    fake_sb = MagicMock()
    with patch("tools.daytona_toolkit.context.DaytonaContextPreparer._get_sandbox") as mock_get:
        mock_get.return_value = fake_sb
        result = tk._get_sandbox()
        assert result is fake_sb


def test_get_sandbox_uses_cached():
    tk = DaytonaContextPreparer(sandbox_id="sb-abc")
    fake_sb = MagicMock()
    tk._sandbox = fake_sb
    # Should return cached without importing sandbox module
    result = tk._get_sandbox()
    assert result is fake_sb


# ---------------------------------------------------------------------------
# _get_sandbox_async
# ---------------------------------------------------------------------------


async def test_get_sandbox_async_no_id_raises():
    tk = DaytonaContextPreparer("")
    with pytest.raises(RuntimeError, match="No sandbox_id"):
        await tk._get_sandbox_async()


async def test_get_sandbox_async_caches_per_loop():
    tk = DaytonaContextPreparer(sandbox_id="sb-xyz")
    fake_sb = MagicMock()

    async def fake_get_async(sandbox_id):
        return fake_sb

    with patch("sandbox.async_client.get_async_sandbox", new=fake_get_async, create=True):
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
    tk = DaytonaContextPreparer(sandbox_id="sb-xyz")
    old_sb = MagicMock()
    tk._sandbox = old_sb
    tk._sandbox_loop_id = 999999  # fake old loop id

    new_sb = MagicMock()

    async def fake_get_async(sandbox_id):
        return new_sb

    mock_module = MagicMock()
    mock_module.get_async_sandbox = fake_get_async
    with patch.dict("sys.modules", {"sandbox.async_client": mock_module}):
        result = await tk._get_sandbox_async()
        assert result is new_sb


# ---------------------------------------------------------------------------
# prepare_context (sync)
# ---------------------------------------------------------------------------


def test_prepare_context_injects_sandbox_and_cwd():
    tk = DaytonaContextPreparer(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx()

    with (
        patch.object(tk, "_get_sandbox", return_value=fake_sb),
        patch.object(DaytonaContextPreparer, "_resolve_cwd_sync", return_value="/workspace"),
        patch("sandbox.workspace.inject_code_intelligence"),
    ):
        tk.prepare_context(ctx)

    assert ctx["daytona_sandbox"] is fake_sb
    assert ctx["repo_root"] == "/workspace"
    assert ctx["exec_cwd"] == "/workspace"


def test_prepare_context_no_cwd_skips_metadata_key():
    tk = DaytonaContextPreparer(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx()

    with (
        patch.object(tk, "_get_sandbox", return_value=fake_sb),
        patch.object(DaytonaContextPreparer, "_resolve_cwd_sync", return_value=None),
        patch("sandbox.workspace.inject_code_intelligence"),
    ):
        tk.prepare_context(ctx)

    assert ctx["daytona_sandbox"] is fake_sb
    assert "repo_root" not in ctx
    assert "exec_cwd" not in ctx


def test_prepare_context_respects_preseeded_workspace_root_override():
    tk = DaytonaContextPreparer(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx({"repo_root": "/testbed", "ci_workspace_root": "/testbed"})

    with (
        patch.object(tk, "_get_sandbox", return_value=fake_sb),
        patch.object(
            DaytonaContextPreparer, "_resolve_cwd_sync", return_value="/workspace"
        ) as resolve_mock,
        patch("sandbox.workspace.inject_code_intelligence") as inject_mock,
    ):
        tk.prepare_context(ctx)

    resolve_mock.assert_not_called()
    inject_mock.assert_called_once_with(ctx, "sb-test", fake_sb, "/testbed")
    assert ctx["daytona_sandbox"] is fake_sb
    assert ctx["repo_root"] == "/testbed"
    assert ctx["exec_cwd"] == "/testbed"


# ---------------------------------------------------------------------------
# prepare_context_async
# ---------------------------------------------------------------------------


async def test_prepare_context_async_injects_sandbox_and_cwd():
    tk = DaytonaContextPreparer(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx()

    async def fake_get_async():
        return fake_sb

    async def fake_resolve_cwd(sb):
        return "/async/workspace"

    with (
        patch.object(tk, "_get_sandbox_async", new=AsyncMock(return_value=fake_sb)),
        patch.object(
            DaytonaContextPreparer,
            "_resolve_cwd_async",
            new=AsyncMock(return_value="/async/workspace"),
        ),
        patch("sandbox.workspace.inject_code_intelligence"),
    ):
        await tk.prepare_context_async(ctx)

    assert ctx["daytona_sandbox"] is fake_sb
    assert ctx["repo_root"] == "/async/workspace"
    assert ctx["exec_cwd"] == "/async/workspace"


async def test_prepare_context_async_no_cwd():
    tk = DaytonaContextPreparer(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx()

    with (
        patch.object(tk, "_get_sandbox_async", new=AsyncMock(return_value=fake_sb)),
        patch.object(
            DaytonaContextPreparer,
            "_resolve_cwd_async",
            new=AsyncMock(return_value=None),
        ),
        patch("sandbox.workspace.inject_code_intelligence"),
    ):
        await tk.prepare_context_async(ctx)

    assert ctx["daytona_sandbox"] is fake_sb
    assert "repo_root" not in ctx
    assert "exec_cwd" not in ctx


async def test_prepare_context_async_respects_preseeded_workspace_root_override():
    tk = DaytonaContextPreparer(sandbox_id="sb-test")
    fake_sb = MagicMock()
    ctx = _ctx({"repo_root": "/testbed", "ci_workspace_root": "/testbed"})

    with (
        patch.object(tk, "_get_sandbox_async", new=AsyncMock(return_value=fake_sb)),
        patch.object(
            DaytonaContextPreparer, "_resolve_cwd_async", new=AsyncMock(return_value="/workspace")
        ) as resolve_mock,
        patch("sandbox.workspace.inject_code_intelligence") as inject_mock,
    ):
        await tk.prepare_context_async(ctx)

    resolve_mock.assert_not_called()
    inject_mock.assert_called_once_with(ctx, "sb-test", fake_sb, "/testbed")
    assert ctx["daytona_sandbox"] is fake_sb
    assert ctx["repo_root"] == "/testbed"
    assert ctx["exec_cwd"] == "/testbed"


# ---------------------------------------------------------------------------
# _resolve_cwd_sync / _resolve_cwd_async (static methods via patch)
# ---------------------------------------------------------------------------


def test_resolve_cwd_sync_calls_discover_workspace():
    fake_sb = MagicMock()
    mock_module = MagicMock()
    mock_module.discover_workspace.return_value = "/found/workspace"
    with patch.dict("sys.modules", {"sandbox.workspace": mock_module}):
        result = DaytonaContextPreparer._resolve_cwd_sync(fake_sb)
        assert result == "/found/workspace"
        mock_module.discover_workspace.assert_called_once_with(fake_sb)


async def test_resolve_cwd_async_calls_discover_workspace_async():
    fake_sb = MagicMock()
    mock_module = MagicMock()
    mock_module.discover_workspace_async = AsyncMock(return_value="/async/found")
    with patch.dict("sys.modules", {"sandbox.workspace": mock_module}):
        result = await DaytonaContextPreparer._resolve_cwd_async(fake_sb)
        assert result == "/async/found"


# ---------------------------------------------------------------------------
def test_daytona_context_preparer_has_no_instructions():
    tk = DaytonaContextPreparer("sb-test")
    assert not hasattr(tk, "instructions")
