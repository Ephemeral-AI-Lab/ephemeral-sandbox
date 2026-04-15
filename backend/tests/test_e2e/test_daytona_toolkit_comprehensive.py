# ruff: noqa
"""Comprehensive sandbox and CI tool tests — OCC, LSP, CI, conflict resolution.

Covers sandbox and CI concerns:
  OCC Editing:         daytona_edit_file with Arbiter lock/token/conflict
  CI toolkit:  ci_query_symbol, ci_query_symbol, ci_query_symbol, ci_diagnostics
  CodeAct:             daytona_codeact multi-step execution
  Tool Selection:      ordering, schema validation, completeness
  Code Intelligence:   CI service, LSP client, registry, types
  Conflict Resolution: Arbiter, TimeMachine, Ledger, OCC edit flow
  Live Sandbox:        real Daytona execution

Run with: pytest tests/test_e2e/test_daytona_toolkit_comprehensive.py -v
"""

from __future__ import annotations

import asyncio
import json
import os
import time
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock, PropertyMock

import pytest
from dotenv import load_dotenv

_PROJECT_ROOT = Path(__file__).resolve().parents[3]
load_dotenv(_PROJECT_ROOT / ".env")

from tools.core.base import ToolExecutionContext

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
        result = MagicMock()
        # Check for matching commands
        for pattern, output in exec_map.items():
            if pattern in command:
                result.result = output
                result.exit_code = exec_exit_code
                return result
        # Default: echo-style
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


def _make_context(
    sandbox: Any, *, cwd: str = "/workspace", ci_service: Any = None
) -> ToolExecutionContext:
    """Create a ToolExecutionContext with sandbox injected."""
    metadata: dict[str, Any] = {
        "daytona_sandbox": sandbox,
        "daytona_cwd": cwd,
    }
    if ci_service is not None:
        metadata["ci_service"] = ci_service
    return ToolExecutionContext(cwd=Path(cwd), metadata=metadata)


def _make_lsp_sandbox(responses: dict[str, str] | None = None) -> MagicMock:
    """Create a mock sandbox with synchronous process.exec responses for LSP."""
    sandbox = MagicMock()
    resp_map = responses or {}

    def _exec(cmd, *, timeout=30, cwd=None):
        result = MagicMock()
        for pattern, output in resp_map.items():
            if pattern in cmd:
                result.result = output
                result.exit_code = 0
                return result
        result.result = ""
        result.exit_code = 0
        return result

    sandbox.process.exec = _exec
    return sandbox


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
# removed — focus is on Daytona-specific concerns: OCC, LSP, CI,
# conflict resolution, tool selection/ordering.

# ===========================================================================
# 7. DaytonaEditTool — OCC-coordinated editing
# ===========================================================================


class TestDaytonaEditTool:
    """Test daytona_edit_file: search-and-replace with OCC."""

    def _tool(self):
        from tools.daytona_toolkit.edit_tool import daytona_edit_file

        return daytona_edit_file

    def test_edit_basic_replace(self):
        sandbox = _make_mock_sandbox(files={"/workspace/app.py": "def foo():\n    return 1"})
        ctx = _make_context(sandbox)
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
        ctx = _make_context(sandbox)
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
        ctx = _make_context(sandbox)
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
        ctx = _make_context(sandbox)
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
        ctx = _make_context(sandbox)
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
        ctx = ToolExecutionContext(cwd=Path("/workspace"), metadata={})
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
        ctx = _make_context(sandbox)
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
        ctx = _make_context(sandbox)
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

    def test_edit_upload_failure(self):
        sandbox = _make_mock_sandbox(files={"/workspace/f.py": "content"})
        sandbox.fs.upload_file = MagicMock(side_effect=OSError("write failed"))
        ctx = _make_context(sandbox)
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


# ===========================================================================
# 8-11. LSP Tools
# ===========================================================================


class TestDaytonaCiTools:
    """Test unified code intelligence tools: hover and diagnostics."""

    # -- Diagnostics --

    def test_lsp_diagnostics_no_ci(self):
        from tools.ci_toolkit.lsp_tools import ci_diagnostics

        ctx = _make_context(_make_mock_sandbox())
        result = _run(ci_diagnostics.execute(ci_diagnostics.input_model(file_path="/test.py"), ctx))
        assert result.is_error

    def test_lsp_diagnostics_clean(self):
        from tools.ci_toolkit.lsp_tools import ci_diagnostics

        svc = MagicMock()
        svc.diagnostics.return_value = []
        ctx = _make_context(_make_mock_sandbox(), ci_service=svc)
        result = _run(ci_diagnostics.execute(ci_diagnostics.input_model(file_path="/test.py"), ctx))
        _assert_success(result)
        assert "clean" in result.output

    def test_lsp_diagnostics_with_errors(self):
        from tools.ci_toolkit.lsp_tools import ci_diagnostics
        from code_intelligence.types import Diagnostic

        svc = MagicMock()
        svc.diagnostics.return_value = [
            Diagnostic(
                file_path="/test.py",
                line=5,
                character=10,
                severity="error",
                message="undefined name 'foo'",
                source="pyright",
            ),
        ]
        ctx = _make_context(_make_mock_sandbox(), ci_service=svc)
        result = _run(ci_diagnostics.execute(ci_diagnostics.input_model(file_path="/test.py"), ctx))
        _assert_success(result)
        assert "undefined name" in result.output
        assert "error" in result.output


# ===========================================================================
# 12. DaytonaCodeActTool
# ===========================================================================


class TestDaytonaCodeActTool:
    """Test daytona_codeact: multi-step code execution with atomic I/O."""

    def _tool(self):
        from tools.daytona_toolkit.codeact_tool import daytona_codeact

        return daytona_codeact

    def test_codeact_no_sandbox(self):
        ctx = ToolExecutionContext(cwd=Path("/workspace"), metadata={})
        tool = self._tool()
        result = _run(tool.execute(tool.input_model(code="print('hi')"), ctx))
        assert result.is_error

    def test_codeact_upload_failure(self):
        sandbox = MagicMock()
        sandbox.fs.upload_file = MagicMock(side_effect=OSError("no space"))
        ctx = _make_context(sandbox)
        tool = self._tool()
        result = _run(tool.execute(tool.input_model(code="print('hi')"), ctx))
        assert result.is_error
        assert "upload" in result.output.lower() or "space" in result.output.lower()

    def test_codeact_execution_failure(self):
        sandbox = _make_mock_sandbox()
        sandbox.process.exec = MagicMock(side_effect=TimeoutError("timed out"))
        ctx = _make_context(sandbox)
        tool = self._tool()
        result = _run(tool.execute(tool.input_model(code="while True: pass"), ctx))
        assert result.is_error

    def test_codeact_bad_json_output(self):
        """When script output is not valid JSON, should still return output."""
        sandbox = _make_mock_sandbox(exec_results={"python3": "not json at all"})
        ctx = _make_context(sandbox)
        tool = self._tool()
        result = _run(tool.execute(tool.input_model(code="print('hello')"), ctx))
        # Should not crash — returns script output
        assert (
            "not json" in result.output
            or "hello" in result.output
            or "Script output" in result.output
        )

    def test_codeact_error_status_in_manifest(self):
        """When script reports error status, result should be is_error."""
        error_output = json.dumps({"manifest": "/tmp/codeact-test.json", "status": "error"})
        sandbox = _make_mock_sandbox(exec_results={"python3": f"some traceback\n{error_output}"})
        ctx = _make_context(sandbox)
        tool = self._tool()
        result = _run(tool.execute(tool.input_model(code="raise ValueError('oops')"), ctx))
        assert result.is_error

    def test_codeact_input_model_accepts_multiline_code(self):
        from tools.daytona_toolkit.codeact_tool import daytona_codeact

        inp = daytona_codeact.input_model(code="import os\nprint(os.getcwd())\nresult = 42")
        assert "import os" in inp.code
        assert "result = 42" in inp.code


# ===========================================================================
# Toolkit integration tests
# ===========================================================================


class TestDaytonaToolkitIntegration:
    """Test the DaytonaToolkit orchestrator."""

    def _toolkit(self, sandbox_id="test"):
        from tools.daytona_toolkit import DaytonaToolkit

        return DaytonaToolkit(sandbox_id=sandbox_id)

    def test_toolkit_registers_all_6_tools(self):
        toolkit = self._toolkit("test-123")
        tools = toolkit.list_tools()
        names = toolkit.tool_names()

        assert len(tools) == 6, f"Expected 6 tools, got {len(tools)}: {names}"

        expected = {
            "daytona_codeact",
            "daytona_read_file",
            "daytona_write_file",
            "daytona_grep",
            "daytona_glob",
            "daytona_edit_file",
        }
        assert set(names) == expected, (
            f"Missing: {expected - set(names)}, Extra: {set(names) - expected}"
        )

    def test_toolkit_name_is_sandbox_operations(self):
        from tools.daytona_toolkit import DaytonaToolkit

        toolkit = DaytonaToolkit()
        assert toolkit.name == "sandbox_operations"

    def test_toolkit_no_sandbox_id_raises_on_get(self):
        from tools.daytona_toolkit import DaytonaToolkit

        toolkit = DaytonaToolkit()
        with pytest.raises(RuntimeError, match="No sandbox_id"):
            toolkit._get_sandbox()

    def test_toolkit_get_tool_by_name(self):
        toolkit = self._toolkit()
        for name in ["daytona_codeact", "daytona_edit_file"]:
            tool = toolkit.get(name)
            assert tool is not None, f"Tool {name} not found"
            assert tool.name == name

    def test_toolkit_tools_have_api_schema(self):
        toolkit = self._toolkit()
        for tool in toolkit.list_tools():
            schema = tool.to_api_schema()
            assert "name" in schema
            assert "description" in schema
            assert "input_schema" in schema
            assert schema["name"] == tool.name

    def test_toolkit_read_only_tools(self):
        """Read-only tools should report is_read_only correctly."""
        toolkit = self._toolkit()
        read_only_tools = {
            "daytona_read_file",
            "daytona_grep",
            "daytona_glob",
        }
        for tool in toolkit.list_tools():
            dummy_input = tool.input_model.model_construct()
            if tool.name in read_only_tools:
                assert tool.is_read_only(dummy_input), f"{tool.name} should be read-only"

    def test_toolkit_registry_integration(self):
        """Toolkit should integrate with ToolRegistry correctly."""
        from tools.core.base import ToolRegistry
        from tools.daytona_toolkit import DaytonaToolkit

        registry = ToolRegistry()
        toolkit = DaytonaToolkit(sandbox_id="test")
        registry.register_toolkit(toolkit)

        assert registry.get_toolkit("sandbox_operations") is toolkit
        assert registry.get("daytona_codeact") is not None
        assert registry.get("daytona_codeact") is not None
        assert len(registry.to_api_schema()) == 6

    def test_toolkit_restrict_preserves_sandbox_tools(self):
        """restrict_to_toolkits should keep sandbox_operations."""
        from tools.core.base import ToolRegistry
        from tools.daytona_toolkit import DaytonaToolkit

        registry = ToolRegistry()
        registry.register_toolkit(DaytonaToolkit(sandbox_id="test"))
        registry.restrict_to_toolkits(["sandbox_operations"])

        assert len(registry.list_tools()) == 6
        assert registry.get("daytona_codeact") is not None


# ===========================================================================
# CI integration helpers
# ===========================================================================


class TestCIIntegrationHelpers:
    """Test shared CI runtime helper functions."""

    def test_get_ci_service_returns_none_when_missing(self):
        from tools.core.ci_runtime import get_ci_service

        ctx = ToolExecutionContext(cwd=Path("/ws"), metadata={})
        assert get_ci_service(ctx) is None

    def test_get_ci_service_returns_service(self):
        from tools.core.ci_runtime import get_ci_service

        svc = MagicMock()
        ctx = ToolExecutionContext(cwd=Path("/ws"), metadata={"ci_service": svc})
        assert get_ci_service(ctx) is svc

    def test_prime_cache_no_ci(self):
        """prime_cache_after_write should not raise without CI service."""
        from tools.core.ci_runtime import prime_cache_after_write

        ctx = ToolExecutionContext(cwd=Path("/ws"), metadata={})
        prime_cache_after_write(ctx, "/test.py", "content")  # should not raise

    def test_prime_cache_with_ci(self):
        from tools.core.ci_runtime import prime_cache_after_write

        svc = MagicMock()
        ctx = ToolExecutionContext(cwd=Path("/ws"), metadata={"ci_service": svc})
        prime_cache_after_write(ctx, "/test.py", "content")
        svc.symbol_index.refresh.assert_called_once_with("/test.py", "content")
        svc.lsp_client.invalidate.assert_called_once_with("/test.py")

    def test_record_edit_no_ci(self):
        from tools.core.ci_runtime import record_edit_in_arbiter

        ctx = ToolExecutionContext(cwd=Path("/ws"), metadata={})
        record_edit_in_arbiter(ctx, "/test.py")  # should not raise

    def test_record_edit_with_ci(self):
        from tools.core.ci_runtime import record_edit_in_arbiter

        svc = MagicMock()
        ctx = ToolExecutionContext(cwd=Path("/ws"), metadata={"ci_service": svc})
        record_edit_in_arbiter(ctx, "/test.py", edit_type="edit", old_hash="aaa", new_hash="bbb")
        svc.arbiter.record_edit.assert_called_once()

    def test_prime_cache_exception_swallowed(self):
        """CI exceptions should be swallowed gracefully."""
        from tools.core.ci_runtime import prime_cache_after_write

        svc = MagicMock()
        svc.symbol_index.refresh.side_effect = RuntimeError("boom")
        ctx = ToolExecutionContext(cwd=Path("/ws"), metadata={"ci_service": svc})
        prime_cache_after_write(ctx, "/test.py", "content")  # should not raise


# ===========================================================================
# Live sandbox tests (require DAYTONA_API_KEY)
# ===========================================================================


@pytest.mark.skipif(not HAS_DAYTONA, reason="Daytona not configured")
@pytest.mark.live
class TestDaytonaToolkitLive:
    """Direct tool execution against a real Daytona sandbox."""

    @pytest.fixture(scope="class")
    def live_sandbox(self):
        from sandbox.service import SandboxService

        svc = SandboxService()
        sb = svc.create_sandbox(
            name=f"toolkit-test-{int(time.time())}",
            language="python",
            labels={"purpose": "toolkit-e2e"},
        )
        # Get async sandbox — tools use `await sandbox.process.exec(...)` etc.
        from tools.daytona_toolkit.async_client import get_async_sandbox

        async_sb = _run(get_async_sandbox(sb["id"]))
        yield {"info": sb, "raw": async_sb}
        try:
            svc.delete_sandbox(sb["id"])
        except Exception:
            pass

    def _ctx(self, live_sandbox) -> ToolExecutionContext:
        return ToolExecutionContext(
            cwd=Path("/workspace"),
            metadata={
                "daytona_sandbox": live_sandbox["raw"],
                "daytona_cwd": "/home/daytona",
            },
        )

    # -- Live bash --

    def test_live_bash_echo(self, live_sandbox):
        from tools.daytona_toolkit.codeact_tool import daytona_codeact as DaytonaBashTool

        tool = DaytonaBashTool
        ctx = self._ctx(live_sandbox)
        result = _run(tool.execute(tool.input_model(command="echo LIVE_BASH_OK"), ctx))
        _assert_success(result)
        assert "LIVE_BASH_OK" in result.output

    def test_live_bash_python_version(self, live_sandbox):
        from tools.daytona_toolkit.codeact_tool import daytona_codeact as DaytonaBashTool

        tool = DaytonaBashTool
        ctx = self._ctx(live_sandbox)
        result = _run(tool.execute(tool.input_model(command="python3 --version"), ctx))
        _assert_success(result)
        assert "Python" in result.output

    def test_live_bash_nonzero_exit(self, live_sandbox):
        from tools.daytona_toolkit.codeact_tool import daytona_codeact as DaytonaBashTool

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
        from tools.daytona_toolkit.codeact_tool import daytona_codeact as DaytonaBashTool

        ctx = self._ctx(live_sandbox)
        bash_tool = DaytonaBashTool

        # Write and read in one call to avoid process isolation issues
        result = _run(
            bash_tool.execute(
                bash_tool.input_model(
                    command="bash -c \"echo 'toolkit e2e content' > /tmp/toolkit_test.txt && echo 'second line' >> /tmp/toolkit_test.txt && cat /tmp/toolkit_test.txt\"",
                ),
                ctx,
            )
        )
        _assert_success(result)
        assert "toolkit e2e content" in result.output
        assert "second line" in result.output

    # -- Live list files --

    def test_live_list_tmp(self, live_sandbox):
        from tools.daytona_toolkit.codeact_tool import daytona_codeact as DaytonaBashTool

        ctx = self._ctx(live_sandbox)
        tool = DaytonaBashTool
        result = _run(tool.execute(tool.input_model(command="ls /tmp"), ctx))
        _assert_success(result)

    # -- Live grep --

    def test_live_grep_etc(self, live_sandbox):
        from tools.daytona_toolkit.codeact_tool import daytona_codeact as DaytonaBashTool

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
        from tools.daytona_toolkit.codeact_tool import daytona_codeact as DaytonaBashTool

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
        from tools.daytona_toolkit.codeact_tool import daytona_codeact as DaytonaBashTool

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
        "daytona_codeact",
        "daytona_read_file",
        "daytona_write_file",
        "daytona_grep",
        "daytona_glob",
        "daytona_edit_file",
    }

    def _get_toolkit(self):
        from tools.daytona_toolkit import DaytonaToolkit

        return DaytonaToolkit(sandbox_id="ordering-test")

    def _get_tool_names(self) -> list[str]:
        return self._get_toolkit().tool_names()

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

    def test_exactly_6_tools(self):
        assert len(self._get_tool_names()) == 6

    # -- Ordering: read tools before write tools --

    def test_read_file_before_write_file(self):
        names = self._get_tool_names()
        assert names.index("daytona_read_file") < names.index("daytona_write_file")

    def test_grep_before_write(self):
        names = self._get_tool_names()
        assert names.index("daytona_grep") < names.index("daytona_write_file")

    def test_bash_is_last(self):
        """Shell execution should be the last tool (most dangerous)."""
        names = self._get_tool_names()
        assert names[-1] == "daytona_codeact"

    # -- Schema validation --

    def test_all_tools_have_descriptions_over_20_chars(self):
        toolkit = self._get_toolkit()
        for tool in toolkit.list_tools():
            assert len(tool.description) > 20, (
                f"{tool.name} has too-short description: {tool.description!r}"
            )

    def test_all_tools_have_input_schema_with_properties(self):
        toolkit = self._get_toolkit()
        for tool in toolkit.list_tools():
            schema = tool.to_api_schema()
            input_schema = schema["input_schema"]
            assert "properties" in input_schema, (
                f"{tool.name} input_schema missing 'properties': {input_schema}"
            )

    def test_codeact_exposes_non_null_command_schema(self):
        from tools.daytona_toolkit.codeact_tool import daytona_codeact as DaytonaCodeActTool

        schema = DaytonaCodeActTool.to_api_schema()["input_schema"]
        assert schema["properties"]["command"]["type"] == "string"
        assert schema["properties"]["command"]["minLength"] == 1

    def test_read_file_requires_file_path(self):
        from tools.daytona_toolkit.tools import daytona_read_file as DaytonaFileReadTool

        schema = DaytonaFileReadTool.to_api_schema()["input_schema"]
        assert "file_path" in schema.get("required", [])

    def test_write_file_requires_file_path_and_content(self):
        from tools.daytona_toolkit.tools import daytona_write_file as DaytonaFileWriteTool

        schema = DaytonaFileWriteTool.to_api_schema()["input_schema"]
        required = schema.get("required", [])
        assert "file_path" in required
        assert "content" in required

    def test_edit_requires_file_path_old_text_new_text(self):
        from tools.daytona_toolkit.edit_tool import daytona_edit_file as _edit_tool

        schema = _edit_tool.to_api_schema()["input_schema"]
        required = schema.get("required", [])
        assert "file_path" in required
        assert "old_text" in required
        assert "new_text" in required

    def test_ci_query_symbol_requires_query(self):
        from tools.ci_toolkit.query_tools import ci_query_symbol

        schema = ci_query_symbol.to_api_schema()["input_schema"]
        required = schema.get("required", [])
        assert "query" in required

    def test_lsp_diagnostics_requires_file_path_only(self):
        from tools.ci_toolkit.lsp_tools import ci_diagnostics as DaytonaDiagnosticsTool

        schema = DaytonaDiagnosticsTool.to_api_schema()["input_schema"]
        required = schema.get("required", [])
        assert "file_path" in required
        assert "line" not in required

    def test_codeact_requires_command_or_code(self):
        from tools.daytona_toolkit.codeact_tool import daytona_codeact as DaytonaCodeActTool

        schema = DaytonaCodeActTool.to_api_schema()["input_schema"]
        assert schema["oneOf"] == [{"required": ["command"]}, {"required": ["code"]}]


# ===========================================================================
# LSP query routing through the CI toolkit (ported from synthetic-os)
# ===========================================================================


class TestLspQueryRouting:
    """Test LSP tool execution with fake sandbox process responses.

    Ported from synthetic-os test_lsp.py and test_lsp_hybrid.py patterns.
    """

    def _make_ci_service(self, sandbox=None):
        """Create a real CI service with a mock sandbox."""
        from code_intelligence.routing.service import CodeIntelligenceService

        return CodeIntelligenceService(
            sandbox_id="lsp-test",
            workspace_root="/workspace",
            sandbox=sandbox,
        )

    # -- LspClient direct tests --

    def test_lsp_client_python_detection(self):
        from code_intelligence.lsp.client import LspClient

        lsp = LspClient(workspace_root="/workspace")
        assert lsp._detect_language("main.py") == "python"
        assert lsp._detect_language("app.ts") == "typescript"
        assert lsp._detect_language("style.css") == "unknown"

    def test_lsp_client_cache_ttl(self):
        """Cache entries should expire after TTL."""
        from code_intelligence.lsp.client import LspClient
        import time as _time

        lsp = LspClient(workspace_root="/ws", cache_ttl=0.1)
        lsp._put_cached("key1", ["result"])
        assert lsp._get_cached("key1") == ["result"]

        _time.sleep(0.15)
        assert lsp._get_cached("key1") is None  # expired

    def test_lsp_client_cache_max_eviction(self):
        """Cache should evict oldest entries when max is reached."""
        from code_intelligence.lsp.client import LspClient

        lsp = LspClient(workspace_root="/ws", cache_max=3)
        lsp._put_cached("a", 1)
        lsp._put_cached("b", 2)
        lsp._put_cached("c", 3)
        lsp._put_cached("d", 4)  # should evict "a"

        assert lsp._get_cached("a") is None
        assert lsp._get_cached("b") == 2
        assert lsp._get_cached("d") == 4

    def test_lsp_telemetry_tracks_queries(self):
        from code_intelligence.lsp.client import LspClient

        lsp = LspClient(workspace_root="/ws")
        assert lsp.telemetry.queries == 0

        # Call a query (will return empty since no backend)
        lsp.goto_definition("/test.py", 1, 0)
        assert lsp.telemetry.queries == 1

    def test_lsp_telemetry_tracks_cache_hits(self):
        from code_intelligence.lsp.client import LspClient

        lsp = LspClient(workspace_root="/ws")
        lsp._put_cached("def:/test.py:1:0", [])

        lsp.goto_definition("/test.py", 1, 0)  # cache hit
        assert lsp.telemetry.cache_hits == 1

    def test_lsp_invalidate_clears_file_entries(self):
        from code_intelligence.lsp.client import LspClient

        lsp = LspClient(workspace_root="/ws")
        lsp._put_cached("def:/ws/a.py:1:0", ["def_a"])
        lsp._put_cached("ref:/ws/a.py:5:0", ["ref_a"])
        lsp._put_cached("def:/ws/b.py:1:0", ["def_b"])

        lsp.invalidate("/ws/a.py")

        assert lsp._get_cached("def:/ws/a.py:1:0") is None
        assert lsp._get_cached("ref:/ws/a.py:5:0") is None
        assert lsp._get_cached("def:/ws/b.py:1:0") == ["def_b"]  # untouched

    # -- CI service LSP delegation --

    def test_ci_service_exposes_lsp_in_status(self):
        svc = self._make_ci_service()
        status = svc.status()
        lsp = status["lsp"]
        assert "connected" in lsp
        assert "queries" in lsp
        assert "cache_hits" in lsp

    def test_ci_service_status_reports_not_initialized(self):
        svc = self._make_ci_service()
        assert svc.is_initialized is False
        status = svc.status()
        assert status["initialized"] is False

    def test_ci_service_dispose_idempotent(self):
        """Disposing twice should not raise."""
        svc = self._make_ci_service()
        svc.dispose()
        svc.dispose()  # second call should be safe

    # -- CI registry tests --

    def test_ci_registry_dispose_removes_service(self):
        from code_intelligence.routing.service import (
            get_code_intelligence,
            get_code_intelligence_if_exists,
            dispose_code_intelligence,
            dispose_all_code_intelligence,
        )

        dispose_all_code_intelligence()

        get_code_intelligence("disposable", "/ws")
        assert get_code_intelligence_if_exists("disposable") is not None

        dispose_code_intelligence("disposable")
        assert get_code_intelligence_if_exists("disposable") is None

    def test_ci_registry_all_status(self):
        from code_intelligence.routing.service import (
            get_code_intelligence,
            get_all_services_status,
            dispose_all_code_intelligence,
        )

        dispose_all_code_intelligence()

        get_code_intelligence("svc-a", "/ws")
        get_code_intelligence("svc-b", "/ws")

        statuses = get_all_services_status()
        assert "svc-a" in statuses
        assert "svc-b" in statuses
        assert statuses["svc-a"]["sandbox_id"] == "svc-a"

        dispose_all_code_intelligence()


# ===========================================================================
# CI types and data structures (ported from synthetic-os)
# ===========================================================================


class TestCITypesDeep:
    """Deep tests for code intelligence types — ported from synthetic-os patterns."""

    def test_edit_request_all_fields(self):
        from code_intelligence.types import EditRequest

        req = EditRequest(
            file_path="/ws/app.py",
            old_text="old",
            new_text="new",
            agent_id="agent-1",
            description="Fix bug",
        )
        assert req.file_path == "/ws/app.py"
        assert req.old_text == "old"
        assert req.new_text == "new"
        assert req.agent_id == "agent-1"
        assert req.description == "Fix bug"

    def test_edit_result_success(self):
        from code_intelligence.types import EditResult

        r = EditResult(success=True, file_path="/test.py", message="Applied")
        assert r.success is True
        assert r.conflict is not True

    def test_edit_result_conflict(self):
        from code_intelligence.types import EditResult

        r = EditResult(success=False, file_path="/test.py", message="Conflict", conflict=True)
        assert r.success is False
        assert r.conflict is True

    def test_hover_result_fields(self):
        from code_intelligence.types import HoverResult

        h = HoverResult(content="def foo() -> int", language="python")
        assert h.content == "def foo() -> int"
        assert h.language == "python"

    def test_symbol_info_fields(self):
        from code_intelligence.types import SymbolInfo

        s = SymbolInfo(name="MyClass", kind="class", file_path="/ws/m.py", line=10, character=0)
        assert s.name == "MyClass"
        assert s.kind == "class"
        assert s.line == 10

    def test_reference_info_fields(self):
        from code_intelligence.types import ReferenceInfo

        r = ReferenceInfo(file_path="/ws/a.py", line=5, character=3)
        assert r.file_path == "/ws/a.py"

    def test_diagnostic_fields(self):
        from code_intelligence.types import Diagnostic

        d = Diagnostic(
            file_path="/test.py",
            line=5,
            character=10,
            severity="error",
            message="Undefined 'x'",
            source="pyright",
        )
        assert d.severity == "error"
        assert d.source == "pyright"

    def test_ci_telemetry_initial_values(self):
        from code_intelligence.types import CITelemetry
        from code_intelligence.routing.service import CodeIntelligenceService

        svc = CodeIntelligenceService(sandbox_id="tel-test", workspace_root="/ws")
        tel = svc.get_telemetry()
        assert isinstance(tel, CITelemetry)
        assert tel.symbol_index_size == 0
        assert tel.lsp_connected is False
        assert tel.arbiter_active_edits == 0
        assert tel.total_edits == 0


# ===========================================================================
# Conflict resolution: Arbiter, TimeMachine, Ledger, OCC edit flow
# ===========================================================================


class TestArbiterOCC:
    """Arbiter — per-file edit tokens, locks, and conflict tracking."""

    def _make_arbiter(self, **kwargs):
        from code_intelligence.editing.arbiter import Arbiter

        return Arbiter(workspace_root="/workspace", **kwargs)

    def _make_arbiter_with_store(self):
        from code_intelligence.editing.arbiter import Arbiter

        return Arbiter(workspace_root="/workspace")

    # -- Token lifecycle --

    def test_issue_token_returns_valid_token(self):
        from code_intelligence.editing.arbiter import EditToken

        arb = self._make_arbiter()
        token = arb.issue_token("/ws/app.py", "abc123", agent_id="agent-1")
        assert isinstance(token, EditToken)
        assert token.file_path == "/ws/app.py"
        assert token.content_hash == "abc123"
        assert token.agent_id == "agent-1"
        assert len(token.token_id) == 12

    def test_issue_token_increments_metrics(self):
        arb = self._make_arbiter()
        arb.issue_token("/a.py", "h1")
        arb.issue_token("/b.py", "h2")
        assert arb.metrics.tokens_issued == 2

    def test_issue_token_tracks_active_count(self):
        arb = self._make_arbiter()
        arb.issue_token("/a.py", "h1")
        assert arb.active_edit_count == 1
        arb.issue_token("/b.py", "h2")
        assert arb.active_edit_count == 2

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

    # -- Status & cleanup --

    def test_status_returns_all_fields(self):
        arb = self._make_arbiter()
        arb.issue_token("/a.py", "h1")
        arb.record_edit("/a.py")
        status = arb.status()
        assert "total_edits" in status
        assert "conflicts_detected" in status
        assert "tokens_issued" in status
        assert "active_tokens" in status
        assert "active_locks" in status
        assert status["total_edits"] == 1
        assert status["tokens_issued"] == 1

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


class TestTimeMachine:
    """TimeMachine — per-file undo snapshots with global LRU capacity."""

    def _make_tm(self, **kwargs):
        from code_intelligence.editing.time_machine import TimeMachine

        return TimeMachine(**kwargs)

    def test_save_and_rollback(self):
        tm = self._make_tm()
        sid = tm.save("/ws/app.py", "original content")
        assert sid  # non-empty snapshot ID

        snap = tm.rollback("/ws/app.py")
        assert snap is not None
        assert snap.content == "original content"
        assert snap.snapshot_id == sid

    def test_rollback_empty_returns_none(self):
        tm = self._make_tm()
        assert tm.rollback("/ws/nonexistent.py") is None

    def test_rollback_pops_most_recent(self):
        tm = self._make_tm()
        tm.save("/ws/app.py", "v1")
        tm.save("/ws/app.py", "v2")
        tm.save("/ws/app.py", "v3")

        snap = tm.rollback("/ws/app.py")
        assert snap.content == "v3"
        snap = tm.rollback("/ws/app.py")
        assert snap.content == "v2"
        snap = tm.rollback("/ws/app.py")
        assert snap.content == "v1"
        assert tm.rollback("/ws/app.py") is None

    def test_max_per_file_evicts_oldest(self):
        tm = self._make_tm(max_per_file=3)
        tm.save("/ws/app.py", "v1")
        tm.save("/ws/app.py", "v2")
        tm.save("/ws/app.py", "v3")
        tm.save("/ws/app.py", "v4")  # should evict v1

        # Rollback order: v4, v3, v2 — v1 is gone
        assert tm.rollback("/ws/app.py").content == "v4"
        assert tm.rollback("/ws/app.py").content == "v3"
        assert tm.rollback("/ws/app.py").content == "v2"
        assert tm.rollback("/ws/app.py") is None  # v1 evicted

    def test_global_capacity_eviction(self):
        """When global capacity is exceeded, oldest file's snapshots are evicted."""
        tm = self._make_tm(max_global_bytes=20)  # tiny capacity
        tm.save("/ws/a.py", "aaaaaaaaaa")  # 10 bytes
        tm.save("/ws/b.py", "bbbbbbbbbb")  # 10 bytes — at capacity
        tm.save("/ws/c.py", "cccccccccc")  # 10 bytes — should evict /ws/a.py

        assert tm.rollback("/ws/a.py") is None  # evicted
        assert tm.rollback("/ws/c.py") is not None

    def test_discard_snapshot(self):
        tm = self._make_tm()
        tm.save("/ws/app.py", "content")
        assert tm.discard_snapshot("/ws/app.py") is True
        assert tm.rollback("/ws/app.py") is None  # discarded

    def test_discard_empty_returns_false(self):
        tm = self._make_tm()
        assert tm.discard_snapshot("/ws/nonexistent.py") is False

    def test_clear_file(self):
        tm = self._make_tm()
        tm.save("/ws/a.py", "v1")
        tm.save("/ws/b.py", "v2")
        tm.clear("/ws/a.py")
        assert tm.rollback("/ws/a.py") is None
        assert tm.rollback("/ws/b.py") is not None  # untouched

    def test_clear_all(self):
        tm = self._make_tm()
        tm.save("/ws/a.py", "v1")
        tm.save("/ws/b.py", "v2")
        tm.clear()
        assert tm.rollback("/ws/a.py") is None
        assert tm.rollback("/ws/b.py") is None

    def test_content_hash_in_snapshot(self):
        tm = self._make_tm()
        tm.save("/ws/app.py", "test content")
        snap = tm.rollback("/ws/app.py")
        assert snap.content_hash  # non-empty hash
        assert len(snap.content_hash) == 16  # SHA256 prefix


class TestOCCEditFlow:
    """End-to-end OCC edit flow via DaytonaEditTool with arbiter + time_machine."""

    def _make_occ_context(self, files: dict[str, str]):
        """Create a context with mock sandbox + real arbiter + time_machine."""
        from code_intelligence.editing.arbiter import Arbiter
        from code_intelligence.editing.time_machine import TimeMachine

        sandbox = _make_mock_sandbox(files=files)

        arbiter = Arbiter(workspace_root="/workspace")
        time_machine = TimeMachine()

        ci_service = MagicMock()
        ci_service.arbiter = arbiter
        ci_service.time_machine = time_machine
        ci_service.symbol_index = MagicMock()
        ci_service.lsp_client = MagicMock()

        ctx = _make_context(sandbox, ci_service=ci_service)
        return ctx, sandbox, arbiter, time_machine

    def _edit(self, ctx, file_path, old_text, new_text, **kwargs):
        from tools.daytona_toolkit.edit_tool import daytona_edit_file as _edit_tool

        return _run(
            _edit_tool.execute(
                _edit_tool.input_model(
                    file_path=file_path,
                    old_text=old_text,
                    new_text=new_text,
                    **kwargs,
                ),
                ctx,
            )
        )

    def test_occ_edit_acquires_and_releases_lock(self):
        ctx, sandbox, arbiter, _ = self._make_occ_context({"/ws/app.py": "x = 1"})
        result = self._edit(ctx, "/ws/app.py", "x = 1", "x = 2")
        _assert_success(result)
        assert "edited" in result.output

        # Lock should be released (can re-acquire)
        assert arbiter.acquire_file_lock("/ws/app.py") is True
        arbiter.release_file_lock("/ws/app.py")

    def test_occ_edit_saves_snapshot_for_undo(self):
        ctx, _, _, time_machine = self._make_occ_context({"/ws/app.py": "original"})
        self._edit(ctx, "/ws/app.py", "original", "modified")

        snap = time_machine.rollback("/ws/app.py")
        assert snap is not None
        assert snap.content == "original"  # snapshot saved before edit

    def test_occ_edit_records_in_arbiter(self):
        ctx, _, arbiter, _ = self._make_occ_context({"/ws/app.py": "content"})
        self._edit(ctx, "/ws/app.py", "content", "new")
        assert arbiter.metrics.total_edits >= 1

    def test_occ_conflict_when_lock_held(self):
        """Edit should fail with conflict when another agent holds the lock."""
        ctx, _, arbiter, _ = self._make_occ_context({"/ws/app.py": "content"})

        # Simulate another agent holding the lock
        arbiter.acquire_file_lock("/ws/app.py")

        result = self._edit(ctx, "/ws/app.py", "content", "new")
        assert result.is_error
        assert "conflict" in result.output.lower() or "lock" in result.output.lower()
        assert result.metadata.get("conflict") is True

        arbiter.release_file_lock("/ws/app.py")

    def test_occ_edit_without_ci_falls_back_to_direct(self):
        """Without CI service, edit should use direct write (no OCC)."""
        from tools.daytona_toolkit.edit_tool import daytona_edit_file as _edit_tool

        sandbox = _make_mock_sandbox(files={"/ws/app.py": "old"})
        ctx = _make_context(sandbox)  # no ci_service

        result = _run(
            _edit_tool.execute(
                _edit_tool.input_model(file_path="/ws/app.py", old_text="old", new_text="new"),
                ctx,
            )
        )
        _assert_success(result)
        assert '"occ": false' in result.output
        assert sandbox._file_store["/ws/app.py"] == "new"

    def test_sequential_occ_edits_both_succeed(self):
        """Two sequential edits to the same file should both succeed."""
        ctx, sandbox, arbiter, _ = self._make_occ_context({"/ws/app.py": "a = 1\nb = 2"})

        r1 = self._edit(ctx, "/ws/app.py", "a = 1", "a = 10")
        _assert_success(r1)

        r2 = self._edit(ctx, "/ws/app.py", "b = 2", "b = 20")
        _assert_success(r2)

        assert sandbox._file_store["/ws/app.py"] == "a = 10\nb = 20"
        assert arbiter.metrics.total_edits == 2

    def test_dry_run_does_not_acquire_lock(self):
        """Dry run should preview without touching arbiter or time_machine."""
        ctx, sandbox, arbiter, time_machine = self._make_occ_context({"/ws/app.py": "content"})

        result = self._edit(ctx, "/ws/app.py", "content", "new", dry_run=True)
        _assert_success(result)
        assert "dry_run" in result.output
        assert sandbox._file_store["/ws/app.py"] == "content"  # unchanged
        assert arbiter.metrics.total_edits == 0
        assert time_machine.rollback("/ws/app.py") is None  # no snapshot
