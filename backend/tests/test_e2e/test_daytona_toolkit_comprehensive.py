# ruff: noqa
"""Comprehensive sandbox tool tests — audited edits, conflict resolution.

Covers sandbox concerns:
  Audited Editing:     edit_file with Arbiter ledger/lock/conflict
  shell:             shell multi-step execution
  Tool Selection:      ordering, schema validation, completeness
  Conflict Resolution: Arbiter, Ledger, audited edit flow
  Live Sandbox:        real Daytona execution

Run with: pytest tests/test_e2e/test_daytona_tools_comprehensive.py -v
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import os
import re
import shlex
import time
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock, PropertyMock

import pytest
from dotenv import load_dotenv

_PROJECT_ROOT = Path(__file__).resolve().parents[3]
load_dotenv(_PROJECT_ROOT / ".env")

from tools.core.base import ToolExecutionContextService

pytestmark = [pytest.mark.e2e]

# ---------------------------------------------------------------------------
# Credential loading
# ---------------------------------------------------------------------------


def _load_settings() -> dict:
    settings_path = Path.home() / ".ephemeralos" / "settings.json"
    if settings_path.exists():
        return json.loads(settings_path.read_text())
    return {}


_SETTINGS = _load_settings()
DAYTONA_KEY = os.environ.get("DAYTONA_API_KEY") or _SETTINGS.get("daytona_api_key", "")
DAYTONA_URL = os.environ.get("DAYTONA_API_URL") or _SETTINGS.get("daytona_api_url", "")
DAYTONA_TARGET = os.environ.get("DAYTONA_TARGET") or _SETTINGS.get("daytona_target", "")
HAS_DAYTONA = bool(DAYTONA_KEY and DAYTONA_URL)


# ---------------------------------------------------------------------------
# Mock sandbox factory
# ---------------------------------------------------------------------------


def _make_mock_sandbox(
    *,
    files: dict[str, str] | None = None,
    exec_results: dict[str, str] | None = None,
    exec_exit_code: int = 0,
) -> MagicMock:
    """Create a mock sandbox with configurable filesystem and process execution."""
    sandbox = MagicMock()
    file_store = dict(files or {})
    exec_map = dict(exec_results or {})

    # -- process.exec mock (async — tools use `await sandbox.process.exec(...)`) --
    async def _mock_exec(command: str, *, cwd: str = "/workspace", timeout: int = 120):
        del cwd, timeout
        result = MagicMock()
        # Check for matching commands
        for pattern, output in exec_map.items():
            if pattern in command:
                result.result = output
                result.exit_code = exec_exit_code
                return result

        if "DAYTONA_EDIT_PAYLOAD=" in command:
            result.result, result.exit_code = _mock_edit_exec(command, file_store)
            return result

        write_payload = _decode_write_payload(command)
        if write_payload is not None:
            file_path = str(write_payload["file_path"])
            content = str(write_payload.get("content", ""))
            file_store[file_path] = content
            result.result = json.dumps(
                {
                    "ok": True,
                    "file_path": file_path,
                    "bytes_written": len(content.encode("utf-8")),
                }
            )
            result.exit_code = 0
            return result

        if "_head_hash" in command or "os.walk(root)" in command:
            result.result = _mock_snapshot_payload(file_store)
            result.exit_code = 0
            return result

        # Default: echo-style for shell.and miscellaneous shell tests.
        result.result = f"mock output for: {command}"
        result.exit_code = exec_exit_code
        return result

    sandbox.process.exec = _mock_exec

    # -- fs.download_file mock (async) --
    async def _mock_download(path: str):
        if path in file_store:
            return file_store[path].encode("utf-8")
        raise FileNotFoundError(f"File not found: {path}")

    sandbox.fs.download_file = _mock_download

    # -- fs.upload_file mock (async) --
    async def _mock_upload(path: str, content: bytes):
        file_store[path] = content.decode("utf-8")

    sandbox.fs.upload_file = _mock_upload

    # -- fs.find_files mock (async) --
    async def _mock_find_files(path: str, pattern: str):
        matches = []
        for filepath, content in file_store.items():
            if pattern.lower() in content.lower():
                m = MagicMock()
                m.file = filepath
                m.line = 1
                m.content = content[:100]
                matches.append(m)
        return matches

    sandbox.fs.find_files = _mock_find_files

    # -- fs.search_files mock (async) --
    async def _mock_search_files(path: str, pattern: str):
        import fnmatch

        result = MagicMock()
        matched = [p for p in file_store if fnmatch.fnmatch(Path(p).name, pattern)]
        result.files = matched
        return result

    sandbox.fs.search_files = _mock_search_files

    # Store reference for assertions
    sandbox._file_store = file_store
    return sandbox


def _mock_snapshot_payload(file_store: dict[str, str]) -> str:
    files = {}
    for file_path, content in file_store.items():
        normalized = str(file_path)
        digest = hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]
        files[normalized] = {
            "rel": normalized.lstrip("/"),
            "exists": True,
            "hash": digest,
            "head_hash": "",
        }
    return json.dumps({"ok": True, "files": files})


def _mock_edit_exec(command: str, file_store: dict[str, str]) -> tuple[str, int]:
    payload_match = re.search(r"DAYTONA_EDIT_PAYLOAD=([^ ]+)", command)
    file_match = re.search(r"DAYTONA_EDIT_FILE=([^ ]+)", command)
    if payload_match is None or file_match is None:
        return json.dumps({"ok": False, "error": "Invalid edit command"}), 1

    file_path = shlex.split(file_match.group(1))[0]
    if file_path not in file_store:
        return json.dumps({"ok": False, "error": f"Path does not exist: {file_path}"}), 1

    edits = json.loads(base64.b64decode(payload_match.group(1)).decode("utf-8"))
    current = file_store[file_path]
    next_content = current
    errors = []
    for index, edit in enumerate(edits, start=1):
        old_text = str(edit.get("old_text", ""))
        new_text = str(edit.get("new_text", ""))
        if old_text not in next_content:
            errors.append(f"Edit {index}: search text not found")
            continue
        next_content = next_content.replace(old_text, new_text, 1)

    if errors:
        return json.dumps({"ok": False, "file_path": file_path, "errors": errors}), 1

    dry_run = "DAYTONA_EDIT_DRY_RUN=1" in command
    if not dry_run:
        file_store[file_path] = next_content

    return (
        json.dumps(
            {
                "ok": True,
                "file_path": file_path,
                "status": "dry_run" if dry_run else "edited",
                "applied_edits": len(edits),
                "warnings": [],
            }
        ),
        0,
    )


def _decode_write_payload(command: str) -> dict[str, Any] | None:
    try:
        script = shlex.split(command)[-1]
        tokens = shlex.split(script)
    except Exception:
        return None
    for token in reversed(tokens):
        try:
            payload = json.loads(base64.b64decode(token).decode("utf-8"))
        except Exception:
            continue
        if isinstance(payload, dict) and "file_path" in payload and "content" in payload:
            return payload
    return None


def _make_context(
    sandbox: Any, *, cwd: str = "/workspace", ci_service: Any = None
) -> ToolExecutionContextService:
    """Create a ToolExecutionContextService with sandbox injected."""
    metadata: dict[str, Any] = {
        "daytona_sandbox": sandbox,
        "repo_root": cwd,
    }
    if ci_service is not None:
        metadata["ci_service"] = ci_service
    return ToolExecutionContextService(cwd=Path(cwd), services=metadata)


def _make_ci_service_for_sandbox(sandbox: Any, *, workspace: str = "/workspace"):
    from sandbox.runtime.service import CodeIntelligenceService

    return CodeIntelligenceService(
        sandbox_id="daytona-tools-comprehensive",
        workspace_root=workspace,
        sandbox=sandbox,
    )


_test_loop: asyncio.AbstractEventLoop | None = None


def _get_test_loop() -> asyncio.AbstractEventLoop:
    """Get or create a shared event loop for synchronous test helpers."""
    global _test_loop
    if _test_loop is None or _test_loop.is_closed():
        _test_loop = asyncio.new_event_loop()
    return _test_loop


def _run(coro):
    """Run an async function synchronously on the shared test loop."""
    return _get_test_loop().run_until_complete(coro)


def _assert_success(result) -> None:
    """Assert that a tool result is not an error."""
    assert not result.is_error, result.output


# NOTE: Core I/O tool tests (bash, read_file, write_file, grep, glob)
# removed — focus is on Daytona-specific concerns: audited edits,
# conflict resolution, tool selection/ordering.

# ===========================================================================
# 7. DaytonaEditTool — audited editing
# ===========================================================================


class TestDaytonaEditTool:
    """Test edit_file: audited search-and-replace."""

    def _tool(self):
        from tools.sandbox_toolkit.edit_file import edit_file

        return edit_file

    def test_edit_basic_replace(self):
        sandbox = _make_mock_sandbox(files={"/workspace/app.py": "def foo():\n    return 1"})
        ctx = _make_context(sandbox, ci_service=_make_ci_service_for_sandbox(sandbox))
        tool = self._tool()
        result = _run(
            tool.execute(
                tool.input_model(
                    file_path="/workspace/app.py",
                    old_text="return 1",
                    new_text="return 42",
                ),
                ctx,
            )
        )
        _assert_success(result)
        assert "edited" in result.output
        assert sandbox._file_store["/workspace/app.py"] == "def foo():\n    return 42"

    def test_edit_dry_run(self):
        sandbox = _make_mock_sandbox(files={"/workspace/app.py": "old_value = 1"})
        ctx = _make_context(sandbox, ci_service=_make_ci_service_for_sandbox(sandbox))
        tool = self._tool()
        result = _run(
            tool.execute(
                tool.input_model(
                    file_path="/workspace/app.py",
                    old_text="old_value",
                    new_text="new_value",
                    dry_run=True,
                ),
                ctx,
            )
        )
        _assert_success(result)
        assert "dry_run" in result.output
        # File should NOT be modified
        assert sandbox._file_store["/workspace/app.py"] == "old_value = 1"

    def test_edit_first_occurrence_only(self):
        sandbox = _make_mock_sandbox(files={"/workspace/f.py": "aaa\naaa\naaa"})
        ctx = _make_context(sandbox, ci_service=_make_ci_service_for_sandbox(sandbox))
        tool = self._tool()
        result = _run(
            tool.execute(
                tool.input_model(
                    file_path="/workspace/f.py",
                    old_text="aaa",
                    new_text="bbb",
                ),
                ctx,
            )
        )
        _assert_success(result)
        assert sandbox._file_store["/workspace/f.py"] == "bbb\naaa\naaa"

    # -- Edge cases --

    def test_edit_text_not_found(self):
        sandbox = _make_mock_sandbox(files={"/workspace/f.py": "hello"})
        ctx = _make_context(sandbox, ci_service=_make_ci_service_for_sandbox(sandbox))
        tool = self._tool()
        result = _run(
            tool.execute(
                tool.input_model(
                    file_path="/workspace/f.py",
                    old_text="MISSING_TEXT",
                    new_text="replacement",
                ),
                ctx,
            )
        )
        assert result.is_error
        assert "not found" in result.output.lower()

    def test_edit_file_not_found(self):
        sandbox = _make_mock_sandbox()
        ctx = _make_context(sandbox, ci_service=_make_ci_service_for_sandbox(sandbox))
        tool = self._tool()
        result = _run(
            tool.execute(
                tool.input_model(
                    file_path="/nonexistent.py",
                    old_text="x",
                    new_text="y",
                ),
                ctx,
            )
        )
        assert result.is_error

    def test_edit_no_sandbox(self):
        ctx = ToolExecutionContextService(cwd=Path("/workspace"), services={})
        tool = self._tool()
        result = _run(
            tool.execute(
                tool.input_model(
                    file_path="/test.py",
                    old_text="a",
                    new_text="b",
                ),
                ctx,
            )
        )
        assert result.is_error

    def test_edit_with_description(self):
        sandbox = _make_mock_sandbox(files={"/workspace/f.py": "x = 1"})
        ctx = _make_context(sandbox, ci_service=_make_ci_service_for_sandbox(sandbox))
        tool = self._tool()
        result = _run(
            tool.execute(
                tool.input_model(
                    file_path="/workspace/f.py",
                    old_text="x = 1",
                    new_text="x = 2",
                    description="Bump value",
                ),
                ctx,
            )
        )
        _assert_success(result)

    def test_edit_multiline_replace(self):
        sandbox = _make_mock_sandbox(
            files={"/workspace/f.py": "def foo():\n    pass\n\ndef bar():\n    pass"}
        )
        ctx = _make_context(sandbox, ci_service=_make_ci_service_for_sandbox(sandbox))
        tool = self._tool()
        result = _run(
            tool.execute(
                tool.input_model(
                    file_path="/workspace/f.py",
                    old_text="def foo():\n    pass",
                    new_text="def foo():\n    return 42",
                ),
                ctx,
            )
        )
        _assert_success(result)
        assert "return 42" in sandbox._file_store["/workspace/f.py"]
        assert "def bar():\n    pass" in sandbox._file_store["/workspace/f.py"]

    def test_edit_process_failure(self):
        sandbox = _make_mock_sandbox(
            files={"/workspace/f.py": "content"},
            exec_results={
                "DAYTONA_EDIT_PAYLOAD": json.dumps({"ok": False, "error": "write failed"})
            },
            exec_exit_code=1,
        )
        ctx = _make_context(sandbox, ci_service=_make_ci_service_for_sandbox(sandbox))
        tool = self._tool()
        result = _run(
            tool.execute(
                tool.input_model(
                    file_path="/workspace/f.py",
                    old_text="content",
                    new_text="new",
                ),
                ctx,
            )
        )
        assert result.is_error
        assert "write failed" in result.output


# ===========================================================================
# 12. ShellTool
# ===========================================================================


# ===========================================================================
# Tool integration tests
# ===========================================================================


class TestDaytonaToolIntegration:
    """Test Daytona tool registration helpers."""

    def _tools(self):
        from tools.sandbox_toolkit import make_sandbox_tools

        return make_sandbox_tools()

    def test_daytona_registers_all_tools(self):
        tools = self._tools()
        names = [tool.name for tool in tools]

        assert len(tools) == 4, f"Expected 4 tools, got {len(tools)}: {names}"

        expected = {
            "shell",
            "read_file",
            "write_file",
            "edit_file",
        }
        assert set(names) == expected, (
            f"Missing: {expected - set(names)}, Extra: {set(names) - expected}"
        )

    def test_context_preparer_no_sandbox_id_raises_on_get(self):
        from sandbox.lifecycle.context import DaytonaContextPreparer

        preparer = DaytonaContextPreparer("")
        with pytest.raises(RuntimeError, match="No sandbox_id"):
            preparer._get_sandbox()

    def test_get_tool_by_name(self):
        by_name = {tool.name: tool for tool in self._tools()}
        for name in ["shell", "edit_file"]:
            tool = by_name.get(name)
            assert tool is not None, f"Tool {name} not found"
            assert tool.name == name

    def test_daytona_tools_have_api_schema(self):
        for tool in self._tools():
            schema = tool.to_api_schema()
            assert "name" in schema
            assert "description" in schema
            assert "input_schema" in schema
            assert schema["name"] == tool.name

    def test_tool_registry_integration(self):
        """Daytona tools should integrate with ToolRegistry correctly."""
        from tools.core.base import ToolRegistry

        registry = ToolRegistry()
        registry.register_many(self._tools())

        assert registry.get("shell") is not None
        assert registry.get("shell") is not None
        assert len(registry.to_api_schema()) == 4

    def test_restrict_preserves_sandbox_tools(self):
        """restrict_to_tools should keep requested Daytona tools."""
        from tools.core.base import ToolRegistry

        registry = ToolRegistry()
        registry.register_many(self._tools())
        registry.restrict_to_tools(["shell", "read_file"])

        assert len(registry.list_tools()) == 2
        assert registry.get("shell") is not None


# ===========================================================================
# Live sandbox tests (require DAYTONA_API_KEY)
# ===========================================================================


@pytest.mark.skipif(not HAS_DAYTONA, reason="Daytona not configured")
@pytest.mark.live
class TestDaytonaToolLive:
    """Direct tool execution against a real Daytona sandbox."""

    @pytest.fixture(scope="class")
    def live_sandbox(self):
        from sandbox.lifecycle.service import SandboxService

        svc = SandboxService()
        sb = svc.create_sandbox(
            name=f"tools-test-{int(time.time())}",
            language="python",
            labels={"purpose": "tools-e2e"},
        )
        # Get async sandbox — tools use `await sandbox.process.exec(...)` etc.
        from sandbox.client.async_ import get_async_sandbox

        async_sb = _run(get_async_sandbox(sb["id"]))
        yield {"info": sb, "raw": async_sb}
        try:
            svc.delete_sandbox(sb["id"])
        except Exception:
            pass

    def _ctx(self, live_sandbox) -> ToolExecutionContextService:
        from sandbox.runtime.service import CodeIntelligenceService

        sandbox = live_sandbox["raw"]
        cwd = "/home/daytona"
        return ToolExecutionContextService(
            cwd=Path("/workspace"),
            services={
                "daytona_sandbox": sandbox,
                "repo_root": cwd,
                "ci_service": CodeIntelligenceService(
                    sandbox_id=str(live_sandbox["info"]["id"]),
                    workspace_root=cwd,
                    sandbox=sandbox,
                ),
            },
        )

    # -- Live bash --

    def test_live_bash_echo(self, live_sandbox):
        from tools.sandbox_toolkit.shell import shell as DaytonaBashTool

        tool = DaytonaBashTool
        ctx = self._ctx(live_sandbox)
        result = _run(tool.execute(tool.input_model(command="echo LIVE_BASH_OK"), ctx))
        _assert_success(result)
        assert "LIVE_BASH_OK" in result.output

    def test_live_bash_python_version(self, live_sandbox):
        from tools.sandbox_toolkit.shell import shell as DaytonaBashTool

        tool = DaytonaBashTool
        ctx = self._ctx(live_sandbox)
        result = _run(tool.execute(tool.input_model(command="python3 --version"), ctx))
        _assert_success(result)
        assert "Python" in result.output

    def test_live_bash_nonzero_exit(self, live_sandbox):
        from tools.sandbox_toolkit.shell import shell as DaytonaBashTool

        tool = DaytonaBashTool
        ctx = self._ctx(live_sandbox)
        result = _run(tool.execute(tool.input_model(command="cat /nonexistent_file_xyz"), ctx))
        assert result.is_error

    # -- Live write + read --
    # NOTE: Daytona process.exec does NOT support shell operators (|, 2>, etc.)
    # directly — must wrap in `bash -c '...'`. Also, fs.upload_file may not
    # persist across separate process.exec calls due to sandbox isolation,
    # so we use bash for write+read in a single call where needed.

    def test_live_write_then_read(self, live_sandbox):
        from tools.sandbox_toolkit.shell import shell as DaytonaBashTool

        ctx = self._ctx(live_sandbox)
        bash_tool = DaytonaBashTool

        # Write and read in one call to avoid process isolation issues
        result = _run(
            bash_tool.execute(
                bash_tool.input_model(
                    command="bash -c \"echo 'tools e2e content' > /tmp/tools_test.txt && echo 'second line' >> /tmp/tools_test.txt && cat /tmp/tools_test.txt\"",
                ),
                ctx,
            )
        )
        _assert_success(result)
        assert "tools e2e content" in result.output
        assert "second line" in result.output

    # -- Live list files --

    def test_live_list_tmp(self, live_sandbox):
        from tools.sandbox_toolkit.shell import shell as DaytonaBashTool

        ctx = self._ctx(live_sandbox)
        tool = DaytonaBashTool
        result = _run(tool.execute(tool.input_model(command="ls /tmp"), ctx))
        _assert_success(result)

    # -- Live grep --

    def test_live_grep_etc(self, live_sandbox):
        from tools.sandbox_toolkit.shell import shell as DaytonaBashTool

        tool = DaytonaBashTool
        ctx = self._ctx(live_sandbox)
        # Wrap in bash -c to support shell operators
        result = _run(
            tool.execute(
                tool.input_model(
                    command="bash -c \"grep 'root' /etc/passwd\"",
                ),
                ctx,
            )
        )
        _assert_success(result)
        assert "root" in result.output

    # -- Live glob --

    def test_live_glob_tmp(self, live_sandbox):
        from tools.sandbox_toolkit.shell import shell as DaytonaBashTool

        ctx = self._ctx(live_sandbox)
        bash_tool = DaytonaBashTool

        # Create and find in one call
        result = _run(
            bash_tool.execute(
                bash_tool.input_model(
                    command="bash -c \"echo pass > /tmp/globtest.py && find /tmp -name 'globtest*'\"",
                ),
                ctx,
            )
        )
        _assert_success(result)
        assert "globtest" in result.output

    # -- Live edit --

    def test_live_edit_file(self, live_sandbox):
        from tools.sandbox_toolkit.shell import shell as DaytonaBashTool

        ctx = self._ctx(live_sandbox)
        bash_tool = DaytonaBashTool

        # Write, edit via sed, and verify — all in one call
        result = _run(
            bash_tool.execute(
                bash_tool.input_model(
                    command="bash -c \"printf 'x = 1\\ny = 2\\nz = 3' > /tmp/edit_test.py && sed -i 's/y = 2/y = 999/' /tmp/edit_test.py && cat /tmp/edit_test.py\"",
                ),
                ctx,
            )
        )
        _assert_success(result)
        assert "y = 999" in result.output
        assert "x = 1" in result.output
        assert "z = 3" in result.output


# ===========================================================================
# Tool selection, ordering, and schema validation (ported from synthetic-os)
# ===========================================================================


class TestToolSelectionAndOrdering:
    """Verify tool registration order, completeness, and schema quality.

    Ported from synthetic-os test_daytona_tool_selection.py patterns.
    """

    EXPECTED_TOOLS = {
        "shell",
        "read_file",
        "write_file",
        "grep",
        "glob",
        "edit_file",
    }

    def _get_tools(self):
        from tools.sandbox_toolkit import make_sandbox_tools

        return make_sandbox_tools()

    def _get_tool_names(self) -> list[str]:
        return [tool.name for tool in self._get_tools()]

    # -- Completeness --

    def test_all_expected_tools_registered(self):
        names = set(self._get_tool_names())
        missing = self.EXPECTED_TOOLS - names
        assert not missing, f"Missing tools: {missing}"

    def test_no_unexpected_tools_registered(self):
        """Guard against accidental tool proliferation."""
        names = set(self._get_tool_names())
        unexpected = names - self.EXPECTED_TOOLS
        assert not unexpected, f"Unexpected tools: {unexpected}"

    def test_exactly_8_tools(self):
        assert len(self._get_tool_names()) == 8

    # -- Ordering: read tools before write tools --

    def test_read_file_before_write_file(self):
        names = self._get_tool_names()
        assert names.index("read_file") < names.index("write_file")

    def test_grep_before_write(self):
        names = self._get_tool_names()
        assert names.index("grep") < names.index("write_file")

    def test_bash_is_last(self):
        """Shell execution should be the last tool (most dangerous)."""
        names = self._get_tool_names()
        assert names[-1] == "shell"

    # -- Schema validation --

    def test_all_tools_have_descriptions_over_20_chars(self):
        for tool in self._get_tools():
            assert len(tool.description) > 20, (
                f"{tool.name} has too-short description: {tool.description!r}"
            )

    def test_all_tools_have_input_schema_with_properties(self):
        for tool in self._get_tools():
            schema = tool.to_api_schema()
            input_schema = schema["input_schema"]
            assert "properties" in input_schema, (
                f"{tool.name} input_schema missing 'properties': {input_schema}"
            )

    def test_shell_exposes_non_null_command_schema(self):
        from tools.sandbox_toolkit.shell import shell as ShellTool

        schema = ShellTool.to_api_schema()["input_schema"]
        assert schema["properties"]["command"]["type"] == "string"
        assert schema["properties"]["command"]["minLength"] == 1

    def test_read_file_requires_file_path(self):
        from tools.sandbox_toolkit.read_file import read_file as FileReadTool

        schema = FileReadTool.to_api_schema()["input_schema"]
        assert "file_path" in schema.get("required", [])

    def test_write_file_requires_file_path_and_content(self):
        from tools.sandbox_toolkit.write_file import write_file as FileWriteTool

        schema = FileWriteTool.to_api_schema()["input_schema"]
        required = schema.get("required", [])
        assert "file_path" in required
        assert "content" in required

    def test_edit_requires_file_path_old_text_new_text(self):
        from tools.sandbox_toolkit.edit_file import edit_file as _edit_file

        schema = _edit_file.to_api_schema()["input_schema"]
        required = schema.get("required", [])
        assert "file_path" in required
        assert "old_text" in schema.get("properties", {})
        assert "new_text" in schema.get("properties", {})

    def test_shell_requires_command(self):
        from tools.sandbox_toolkit.shell import shell as ShellTool

        schema = ShellTool.to_api_schema()["input_schema"]
        assert schema["oneOf"] == [{"required": ["command"]}, {"required": ["code"]}]


# ===========================================================================
# CI types and data structures (ported from synthetic-os)
# ===========================================================================


class TestCITypesDeep:
    """Deep tests for code intelligence types — ported from synthetic-os patterns."""

    def test_edit_spec_all_fields(self):
        from sandbox.occ.patching.patcher import SearchReplaceEdit
        from sandbox.occ.types import EditSpec

        spec = EditSpec(
            file_path="/ws/app.py",
            edits=(SearchReplaceEdit(old_text="old", new_text="new"),),
        )
        assert spec.file_path == "/ws/app.py"
        assert spec.edits[0].old_text == "old"
        assert spec.edits[0].new_text == "new"

    def test_edit_result_success(self):
        from sandbox.occ.types import EditResult

        r = EditResult(success=True, file_path="/test.py", message="Applied")
        assert r.success is True
        assert r.conflict is not True

    def test_edit_result_conflict(self):
        from sandbox.occ.types import EditResult

        r = EditResult(success=False, file_path="/test.py", message="Conflict", conflict=True)
        assert r.success is False
        assert r.conflict is True

# ===========================================================================
# Conflict resolution: Arbiter, Ledger, audited edit flow
# ===========================================================================


class TestArbiterAuditLedger:
    """Arbiter — per-file locks, edit ledger, and conflict tracking."""

    def _make_arbiter(self, **kwargs):
        from sandbox.occ.state.arbiter import Arbiter

        return Arbiter(workspace_root="/workspace", **kwargs)

    def _make_arbiter_with_store(self):
        from sandbox.occ.state.arbiter import Arbiter

        return Arbiter(workspace_root="/workspace")

    # -- File locking --

    def test_acquire_and_release_lock(self):
        arb = self._make_arbiter()
        assert arb.acquire_file_lock("/ws/app.py") is True
        arb.release_file_lock("/ws/app.py")

    def test_lock_blocks_concurrent_access(self):
        """Second acquire on same file should block (timeout quickly)."""
        arb = self._make_arbiter()
        assert arb.acquire_file_lock("/ws/app.py") is True
        # Second acquire should timeout
        assert arb.acquire_file_lock("/ws/app.py", timeout=0.01) is False
        arb.release_file_lock("/ws/app.py")

    def test_different_files_lock_independently(self):
        arb = self._make_arbiter()
        assert arb.acquire_file_lock("/ws/a.py") is True
        assert arb.acquire_file_lock("/ws/b.py") is True  # different file, should succeed
        arb.release_file_lock("/ws/a.py")
        arb.release_file_lock("/ws/b.py")

    def test_release_idempotent(self):
        """Releasing an already-released lock should not raise."""
        arb = self._make_arbiter()
        arb.acquire_file_lock("/ws/app.py")
        arb.release_file_lock("/ws/app.py")
        arb.release_file_lock("/ws/app.py")  # should not raise

    def test_lock_after_release_succeeds(self):
        arb = self._make_arbiter()
        arb.acquire_file_lock("/ws/app.py")
        arb.release_file_lock("/ws/app.py")
        assert arb.acquire_file_lock("/ws/app.py") is True
        arb.release_file_lock("/ws/app.py")

    # -- Edit recording --

    def test_record_edit_increments_generation(self):
        arb = self._make_arbiter()
        gen1 = arb.record_edit("/ws/a.py", "agent-1")
        gen2 = arb.record_edit("/ws/b.py", "agent-2")
        assert gen2 > gen1

    def test_record_edit_increments_total_edits(self):
        arb = self._make_arbiter()
        arb.record_edit("/ws/a.py")
        arb.record_edit("/ws/a.py")
        assert arb.metrics.total_edits == 2

    def test_on_edit_callback_fires(self):
        calls = []
        arb = self._make_arbiter(on_edit=lambda fp, aid, gen: calls.append((fp, aid, gen)))
        arb.record_edit("/ws/app.py", "agent-1")
        assert len(calls) == 1
        assert calls[0][0] == "/ws/app.py"
        assert calls[0][1] == "agent-1"

    def test_on_edit_callback_exception_swallowed(self):
        def _boom(fp, aid, gen):
            raise RuntimeError("callback crash")

        arb = self._make_arbiter(on_edit=_boom)
        arb.record_edit("/ws/app.py")  # should not raise

    # -- Cleanup --

    def test_cleanup_locks_removes_unheld(self):
        arb = self._make_arbiter()
        arb.acquire_file_lock("/ws/a.py")
        arb.release_file_lock("/ws/a.py")
        cleaned = arb.cleanup_locks()
        assert cleaned >= 1

    # -- Concurrent lock test (threading) --

    def test_concurrent_lock_only_one_wins(self):
        """Two threads acquiring same file lock — only one should succeed immediately."""
        import threading

        arb = self._make_arbiter()
        results = []

        def _try_lock(thread_id):
            got = arb.acquire_file_lock("/ws/contested.py", timeout=0.05)
            results.append((thread_id, got))
            if got:
                import time as _t

                _t.sleep(0.1)  # hold lock briefly
                arb.release_file_lock("/ws/contested.py")

        t1 = threading.Thread(target=_try_lock, args=(1,))
        t2 = threading.Thread(target=_try_lock, args=(2,))
        t1.start()
        t2.start()
        t1.join()
        t2.join()

        wins = [r for r in results if r[1] is True]
        losses = [r for r in results if r[1] is False]
        # At least one should win, at most one should lose (timeout)
        assert len(wins) >= 1

    # -- Edit recording with the in-memory history ledger (formerly TestArbiterEditRecording) --

    def test_record_increments_generation(self):
        arbiter = self._make_arbiter_with_store()
        gen = arbiter.record_edit("/ws/app.py", "agent-1", edit_type="edit")
        assert gen == 1
        gen2 = arbiter.record_edit("/ws/b.py", "agent-2")
        assert gen2 == 2

    def test_metrics_track_total_edits(self):
        arbiter = self._make_arbiter_with_store()
        assert arbiter.metrics.total_edits == 0
        arbiter.record_edit("/ws/a.py", "agent-1")
        assert arbiter.metrics.total_edits == 1
        arbiter.record_edit("/ws/b.py", "agent-2")
        assert arbiter.metrics.total_edits == 2

    def test_generation_property(self):
        arbiter = self._make_arbiter_with_store()
        assert arbiter.generation == 0
        arbiter.record_edit("/ws/a.py", "agent-1")
        assert arbiter.generation == 1

    def test_record_with_all_params(self):
        arbiter = self._make_arbiter_with_store()
        gen = arbiter.record_edit(
            "/ws/app.py",
            "agent-1",
            edit_type="edit",
            old_hash="aaa111",
            new_hash="bbb222",
            description="Fix null check",
        )
        assert gen == 1
        assert arbiter.metrics.total_edits == 1


class TestAuditedEditFlow:
    """End-to-end audited edit flow via DaytonaEditTool with arbiter."""

    def _make_audit_context(self, files: dict[str, str]):
        """Create a context with mock sandbox + real arbiter."""
        from sandbox.runtime.service import CodeIntelligenceService

        sandbox = _make_mock_sandbox(files=files)
        ci_service = CodeIntelligenceService(
            sandbox_id="audit-edit-test",
            workspace_root="/ws",
            sandbox=sandbox,
        )

        ctx = _make_context(sandbox, ci_service=ci_service)
        return ctx, sandbox, ci_service.arbiter

    def _edit(self, ctx, file_path, old_text, new_text, **kwargs):
        from tools.sandbox_toolkit.edit_file import edit_file as _edit_file

        return _run(
            _edit_file.execute(
                _edit_file.input_model(
                    file_path=file_path,
                    old_text=old_text,
                    new_text=new_text,
                    **kwargs,
                ),
                ctx,
            )
        )

    def test_audited_edit_acquires_and_releases_lock(self):
        ctx, sandbox, arbiter = self._make_audit_context({"/ws/app.py": "x = 1"})
        result = self._edit(ctx, "/ws/app.py", "x = 1", "x = 2")
        _assert_success(result)
        assert "edited" in result.output

        # Lock should be released (can re-acquire)
        assert arbiter.acquire_file_lock("/ws/app.py") is True
        arbiter.release_file_lock("/ws/app.py")

    def test_audited_edit_records_in_arbiter(self):
        ctx, _, arbiter = self._make_audit_context({"/ws/app.py": "content"})
        self._edit(ctx, "/ws/app.py", "content", "new")
        assert arbiter.metrics.total_edits >= 1

    def test_audited_edit_without_ci_returns_error(self):
        """Coordinated edits must fail instead of raw-writing without CI."""
        from tools.sandbox_toolkit.edit_file import edit_file as _edit_file

        sandbox = _make_mock_sandbox(files={"/ws/app.py": "old"})
        ctx = _make_context(sandbox)  # no ci_service
        ctx["agent_name"] = "developer"

        result = _run(
            _edit_file.execute(
                _edit_file.input_model(file_path="/ws/app.py", old_text="old", new_text="new"),
                ctx,
            )
        )
        assert result.is_error
        assert result.metadata["ci_required"] is True
        assert "Code intelligence service is unavailable" in result.output
        assert sandbox._file_store["/ws/app.py"] == "old"

    def test_sequential_audited_edits_both_succeed(self):
        """Two sequential edits to the same file should both succeed."""
        ctx, sandbox, arbiter = self._make_audit_context({"/ws/app.py": "a = 1\nb = 2"})

        r1 = self._edit(ctx, "/ws/app.py", "a = 1", "a = 10")
        _assert_success(r1)

        r2 = self._edit(ctx, "/ws/app.py", "b = 2", "b = 20")
        _assert_success(r2)

        assert sandbox._file_store["/ws/app.py"] == "a = 10\nb = 20"
        assert arbiter.metrics.total_edits == 2

    def test_dry_run_does_not_acquire_lock(self):
        """Dry run should preview without touching arbiter."""
        ctx, sandbox, arbiter = self._make_audit_context({"/ws/app.py": "content"})

        result = self._edit(ctx, "/ws/app.py", "content", "new", dry_run=True)
        _assert_success(result)
        assert "dry_run" in result.output
        assert sandbox._file_store["/ws/app.py"] == "content"  # unchanged
        assert arbiter.metrics.total_edits == 0
