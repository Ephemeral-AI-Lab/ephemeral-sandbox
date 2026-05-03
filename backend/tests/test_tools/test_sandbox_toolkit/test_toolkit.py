"""Tests for sandbox tool exports."""

from __future__ import annotations

from pathlib import Path

from tools.core.base import ToolExecutionContextService, ToolRegistry
from tools.sandbox_toolkit import make_sandbox_tools


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


def test_missing_sandbox_tool_absent():
    tools = {tool.name: tool for tool in make_sandbox_tools()}
    assert tools.get("nonexistent_tool") is None


def test_sandbox_tool_count():
    tools = make_sandbox_tools()
    assert len(tools) == 4


def test_sandbox_tools_omit_instruction_block():
    assert all(not hasattr(tool, "instructions") for tool in make_sandbox_tools())
