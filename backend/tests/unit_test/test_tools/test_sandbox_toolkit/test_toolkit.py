"""Tests for sandbox tool exports."""

from __future__ import annotations

from pathlib import Path

from tools._framework.core.base import ToolExecutionContextService
from tools._framework.core.registry import ToolRegistry
from tools.sandbox import make_sandbox_tools


# pytest-asyncio runs in auto mode (configured in pyproject.toml) — async
# test functions are handled automatically, so no module-level marker is
# needed. Leaving `pytestmark = pytest.mark.asyncio` in place here would
# emit a warning for every *sync* test in the file.


def _ctx(services=None) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"), services=services or {})


def test_sandbox_exports_expected_tools():
    names = {tool.name for tool in make_sandbox_tools()}
    expected = {
        "shell",
        "read_file",
        "write_file",
        "edit_file",
        "glob",
        "grep",
    }
    assert names == expected
    assert not any(name.startswith("daytona_") for name in names)


async def test_registered_api_backed_tools_require_sandbox_id():
    registry = ToolRegistry()
    registry.register_many(make_sandbox_tools())
    tools_by_name = {tool.name: tool for tool in registry.list_tools()}
    api_inputs = {
        "write_file": {"file_path": "/repo/new.txt", "content": "hello"},
        "edit_file": {
            "file_path": "/repo/app.py",
            "old_text": "old",
            "new_text": "new",
        },
        "shell": {"command": "echo hi"},
        "glob": {"pattern": "*.py"},
        "grep": {"pattern": "needle"},
    }

    assert set(api_inputs).issubset(tools_by_name)
    assert set(tools_by_name) - set(api_inputs) == {"read_file"}

    for tool_name, tool_input in api_inputs.items():
        ctx = _ctx({"repo_root": "/repo"})
        tool = tools_by_name[tool_name]
        result = await tool.execute(tool.input_model(**tool_input), ctx)

        assert result.is_error, tool_name
        assert result.metadata.get("sandbox_required") is True, tool_name


def test_make_sandbox_tools_includes_shell():
    names = {tool.name for tool in make_sandbox_tools()}

    assert "shell" in names
    assert "edit_file" in names
    assert "daytona_list_files" not in names


def test_get_sandbox_tool_by_name():
    tools = {tool.name: tool for tool in make_sandbox_tools()}
    tool = tools.get("shell")
    assert tool is not None
    assert tool.name == "shell"


def test_shell_schema_describes_command():
    tools = {tool.name: tool for tool in make_sandbox_tools()}
    tool = tools.get("shell")
    assert tool is not None

    schema = tool.to_api_schema()["input_schema"]
    command_description = schema["properties"]["command"]["description"]
    assert command_description == "Shell command to run for tests, builds, or verification."

    assert tool.short_description == "Run a shell command from the repo root."


def test_shell_exposes_optional_background_execution():
    """``shell`` is the only sandbox tool that supports daemon-native background.

    The daemon's ``shell.launch / poll / cancel / reap`` ops (plan
    ``docs/plans/2026-05-22-shell-background-mode.md``) let the agent fire
    long-running shells without blocking the conversation. The engine's
    background dispatcher injects ``background_task_id`` on opt-in calls,
    and the tool branches into the daemon's job-control surface when set.
    """
    tools = {tool.name: tool for tool in make_sandbox_tools()}
    tool = tools.get("shell")
    assert tool is not None

    schema = tool.to_api_schema()["input_schema"]
    assert tool.background == "optional"
    # The ``background`` flag is injected by the registry's
    # ``decorate_schemas_for_background`` pass at tool registration, not by
    # ``to_api_schema`` directly. Verify the per-tool schema is otherwise
    # unchanged: the LLM-facing parameter list still excludes the runtime
    # control field, but the BaseTool attribute opts the tool in.
    assert "background" not in schema["properties"]


def test_missing_sandbox_tool_absent():
    tools = {tool.name: tool for tool in make_sandbox_tools()}
    assert tools.get("nonexistent_tool") is None


def test_sandbox_tool_count():
    tools = make_sandbox_tools()
    assert len(tools) == 6


def test_sandbox_tools_omit_instruction_block():
    assert all(not hasattr(tool, "instructions") for tool in make_sandbox_tools())
