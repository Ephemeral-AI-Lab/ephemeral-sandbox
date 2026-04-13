# ruff: noqa
"""E2E: ci_hover and ci_query_references with character=0 on indented lines.

Exercises the bug where ci_hover and ci_query_references return empty when
called with character=0 on indented lines. Jedi's help()/get_references()
receive column=0 (whitespace) and find nothing.

The fix adds _resolve_column in LspClient which auto-detects the first
non-whitespace column when character=0.

Run with: pytest tests/test_e2e/test_ci_hover_resolve_column_live.py -v -m live
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from tools.core.base import ToolExecutionContext
from tools.ci_toolkit.lsp_tools import ci_hover
from tools.ci_toolkit.query_tools import ci_query_references

pytestmark = [pytest.mark.e2e]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _ctx(metadata: dict | None = None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _svc_stub(*, workspace_root: str = "/testbed", initialized: bool = True) -> MagicMock:
    svc = MagicMock()
    svc.is_initialized = initialized
    svc.workspace_root = workspace_root
    svc.find_references.return_value = []
    svc.lsp_client.connected = True
    return svc


# ---------------------------------------------------------------------------
# Unit: mock-based tests to verify the fix wiring
# ---------------------------------------------------------------------------


class TestHoverResolveColumnUnit:
    """Verify ci_hover passes resolved character to svc.hover()."""

    async def test_hover_passes_character_unchanged_when_nonzero(self):
        """Non-zero character is forwarded as-is to svc.hover()."""
        hover_result = MagicMock(content="def foo()", language="python")
        svc = MagicMock()
        svc.hover.return_value = hover_result
        ctx = _ctx({"ci_service": svc})

        await ci_hover.execute(
            ci_hover.input_model(file_path="/f.py", line=10, character=5),
            ctx,
        )
        svc.hover.assert_called_once_with("/f.py", 10, 5)

    async def test_hover_character_zero_forwarded(self):
        """character=0 is forwarded to svc.hover (resolution happens in LspClient)."""
        svc = MagicMock()
        svc.hover.return_value = None
        ctx = _ctx({"ci_service": svc})

        await ci_hover.execute(
            ci_hover.input_model(file_path="/f.py", line=10, character=0),
            ctx,
        )
        # ci_hover tool passes character=0 through; LspClient._resolve_column handles it
        svc.hover.assert_called_once_with("/f.py", 10, 0)


class TestReferencesResolveColumnUnit:
    """Verify ci_query_references passes character through correctly."""

    async def test_references_character_zero_forwarded(self):
        """character=0 is passed to svc.find_references (LspClient resolves it)."""
        svc = _svc_stub()
        ctx = _ctx({"ci_service": svc})

        with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
            await ci_query_references.execute(
                ci_query_references.input_model(
                    file_path="/testbed/src/engine.py",
                    symbol="Engine",
                    line=10,
                    character=0,
                ),
                ctx,
            )
        svc.find_references.assert_called_once_with(
            "/testbed/src/engine.py", "Engine", 10, 0,
        )


# ---------------------------------------------------------------------------
# Live sandbox tests (requires Daytona credentials + Jedi in sandbox)
# ---------------------------------------------------------------------------


from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import (
    create_eval_agent,
    create_test_sandbox,
    delete_test_sandbox,
    populate_sandbox_files,
)


# Custom sandbox files with indented class methods to test column resolution
_RESOLVE_COLUMN_FILES: dict[str, str] = {
    "src/__init__.py": "",
    "src/main.py": '''"""Main module with indented methods for column resolve testing."""
import os
from typing import Optional


DEBUG = False
VERSION = "1.0.0"


def get_config() -> dict:
    """Get application configuration."""
    return {
        "debug": DEBUG,
        "version": VERSION,
        "env": os.getenv("APP_ENV", "development"),
    }


class App:
    """Main application class."""

    def __init__(self, name: str):
        self.name = name
        self.running = False

    def start(self) -> None:
        """Start the application."""
        self.running = True

    def stop(self) -> None:
        """Stop the application."""
        self.running = False

    def restart(self) -> None:
        """Restart the application."""
        self.stop()
        self.start()


def main() -> None:
    """Entry point."""
    app = App("MyApp")
    app.start()
    print(f"Started {app.name}")
''',
    "src/utils.py": '''"""Utility functions."""
import json
import hashlib
from typing import Any


def sha256(data: str) -> str:
    """Compute SHA-256 hash."""
    return hashlib.sha256(data.encode()).hexdigest()


def format_json(data: Any) -> str:
    """Format data as JSON."""
    return json.dumps(data, indent=2)
''',
}


@pytest.fixture(scope="module")
def live_sandbox_id():
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona credentials required")
    sb = create_test_sandbox("ci-col-resolve")
    populate_sandbox_files(sb["id"], files=_RESOLVE_COLUMN_FILES)
    yield sb["id"]
    delete_test_sandbox(sb["id"])


def _build_ci_context(sandbox_id: str) -> tuple[Any, ToolExecutionContext]:
    """Build a CI service and tool context for a live sandbox."""
    from sandbox.service import SandboxService
    from sandbox.workspace import discover_workspace, inject_code_intelligence

    svc_client = SandboxService()
    sandbox = svc_client.get_sandbox_object(sandbox_id)
    workspace_root = discover_workspace(sandbox) or "/home/daytona"

    context = MagicMock()
    context.metadata = {}
    inject_code_intelligence(context, sandbox_id, sandbox, workspace_root)

    ci_svc = context.metadata.get("ci_service")
    assert ci_svc is not None
    ci_svc.symbol_index.ensure_built(wait=True, timeout=60.0)

    # Ensure jedi is available in the sandbox
    ci_svc.lsp_client.ensure_ready(install_missing=True)

    tool_ctx = _ctx({
        "ci_service": ci_svc,
        "sandbox_id": sandbox_id,
        "daytona_cwd": workspace_root,
        "daytona_sandbox": sandbox,
    })
    return ci_svc, tool_ctx


@pytest.mark.live
@pytest.mark.asyncio
class TestLiveHoverResolveColumn:
    """Live: ci_hover returns results for indented symbols with character=0."""

    async def test_hover_indented_method_character_zero(self, live_sandbox_id):
        """Hover on 'def start(self)' at character=0 should return docstring."""
        ci_svc, ctx = _build_ci_context(live_sandbox_id)
        workspace_root = ci_svc.workspace_root

        # Line 29 in our file: "    def start(self) -> None:"
        # character=0 lands on whitespace — _resolve_column should fix this
        result = await ci_hover.execute(
            ci_hover.input_model(
                file_path=f"{workspace_root}/src/main.py",
                line=29,
                character=0,
            ),
            ctx,
        )

        assert not result.is_error, f"ci_hover error: {result.output}"
        # Should NOT say "No hover information" — the fix should resolve the column
        assert "No hover information" not in result.output, (
            f"Hover returned empty for indented method with character=0: {result.output}"
        )
        data = json.loads(result.output)
        assert data.get("content"), f"Empty hover content: {data}"

    async def test_hover_top_level_function_character_zero(self, live_sandbox_id):
        """Hover on top-level 'def get_config()' at character=0 should work."""
        ci_svc, ctx = _build_ci_context(live_sandbox_id)
        workspace_root = ci_svc.workspace_root

        # Line 10: "def get_config() -> dict:"
        result = await ci_hover.execute(
            ci_hover.input_model(
                file_path=f"{workspace_root}/src/main.py",
                line=10,
                character=0,
            ),
            ctx,
        )

        assert not result.is_error
        # Top-level function — character=0 is already on 'def', should still work
        assert "No hover information" not in result.output, (
            f"Hover failed for top-level function: {result.output}"
        )

    async def test_hover_class_character_zero(self, live_sandbox_id):
        """Hover on 'class App' at character=0 should return class info."""
        ci_svc, ctx = _build_ci_context(live_sandbox_id)
        workspace_root = ci_svc.workspace_root

        # Line 20: "class App:"
        result = await ci_hover.execute(
            ci_hover.input_model(
                file_path=f"{workspace_root}/src/main.py",
                line=20,
                character=0,
            ),
            ctx,
        )

        assert not result.is_error
        assert "No hover information" not in result.output


@pytest.mark.live
@pytest.mark.asyncio
class TestLiveReferencesResolveColumn:
    """Live: ci_query_references finds refs for indented symbols with character=0."""

    async def test_references_indented_method_character_zero(self, live_sandbox_id):
        """References for 'start' method at character=0 should find self.start() calls."""
        ci_svc, ctx = _build_ci_context(live_sandbox_id)
        workspace_root = ci_svc.workspace_root

        with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=ci_svc):
            result = await ci_query_references.execute(
                ci_query_references.input_model(
                    file_path=f"{workspace_root}/src/main.py",
                    symbol="start",
                    line=29,
                    character=0,
                ),
                ctx,
            )

        assert not result.is_error
        data = json.loads(result.output)
        # 'start' is called in restart() as self.start() and in main() as app.start()
        assert data["total_references"] >= 1, (
            f"Expected references for 'start', got: {result.output}"
        )

    async def test_references_class_character_zero(self, live_sandbox_id):
        """References for 'App' class at character=0 should find usages."""
        ci_svc, ctx = _build_ci_context(live_sandbox_id)
        workspace_root = ci_svc.workspace_root

        with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=ci_svc):
            result = await ci_query_references.execute(
                ci_query_references.input_model(
                    file_path=f"{workspace_root}/src/main.py",
                    symbol="App",
                    line=20,
                    character=0,
                ),
                ctx,
            )

        assert not result.is_error
        data = json.loads(result.output)
        # App is used in main() as App("MyApp")
        assert data["total_references"] >= 1, (
            f"Expected references for 'App', got: {result.output}"
        )


@pytest.mark.live
@pytest.mark.asyncio
class TestLiveAgentHoverResolveColumn:
    """Live agent test: agent uses ci_hover on indented symbols."""

    async def test_agent_hover_indented_method(self, live_sandbox_id):
        """Agent calling ci_hover on an indented method should get results."""
        if not EvalAgent.has_all():
            pytest.skip("LLM + Daytona credentials required")

        agent = create_eval_agent(
            sandbox_id=live_sandbox_id,
            toolkits=["sandbox_operations", "code_intelligence"],
            system_prompt=(
                "You have a remote sandbox with Python files in src/. "
                "When asked to get hover info, ONLY use ci_hover. "
                "Be concise."
            ),
        )

        result = await agent.invoke(
            "Use ci_hover to get hover info for file_path='src/main.py' "
            "line=29 character=0. This is the 'start' method of the App class."
        )

        completed = result.tools_completed()
        hover_calls = [e for e in completed if e.tool_name == "ci_hover"]
        assert hover_calls, (
            f"Expected ci_hover to be called, but agent used: "
            f"{[t.name for t in result.tool_calls]}"
        )
        for call in hover_calls:
            assert "No hover information" not in (call.output or ""), (
                f"ci_hover returned empty for indented method: {call.output}"
            )
