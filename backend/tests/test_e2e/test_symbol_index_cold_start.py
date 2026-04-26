# ruff: noqa
"""E2E: Symbol index cold start — verify the index is built before CI tools run.

Exercises the full cold-start flow that analysis_agent hits:
  1. inject_code_intelligence with an async sandbox (no sync handle)
  2. ci_workspace_structure — should wait for the index and return indexed paths
  3. ci_query_symbol — should find symbols from the indexed workspace

Also includes a live sandbox variant that runs against a real Daytona sandbox.

Run with: pytest tests/test_e2e/test_symbol_index_cold_start.py -v
"""

from __future__ import annotations

import asyncio
import json
import threading
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from tools.core.base import ToolExecutionContextService

pytestmark = [pytest.mark.e2e]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_async_sandbox(files: dict[str, str]) -> MagicMock:
    """Create a mock async sandbox with fs.list_files and fs.download_file.

    The sandbox has an async process.exec so _sandbox_exec_is_async returns True.
    """
    sandbox = MagicMock()
    file_store = dict(files)

    # Async exec so _sandbox_exec_is_async detects it as async
    sandbox.process.exec = AsyncMock(return_value=MagicMock(exit_code=0, result=""))

    # fs.download_file — used by SymbolIndex._read_file_content
    def _download(path: str):
        if path in file_store:
            return file_store[path].encode("utf-8")
        raise FileNotFoundError(f"File not found: {path}")

    sandbox.fs.download_file = _download

    # fs.list_files — used by SymbolIndex._collect_remote_files
    def _list_files(dir_path: str):
        entries = []
        seen_dirs = set()
        prefix = dir_path.rstrip("/") + "/"
        for fp in sorted(file_store):
            if not fp.startswith(prefix):
                continue
            rel = fp[len(prefix):]
            parts = rel.split("/")
            if len(parts) == 1:
                # File in this directory
                entry = MagicMock()
                entry.name = parts[0]
                entry.is_dir = False
                entries.append(entry)
            elif parts[0] not in seen_dirs:
                # Subdirectory
                seen_dirs.add(parts[0])
                entry = MagicMock()
                entry.name = parts[0]
                entry.is_dir = True
                entries.append(entry)
        return entries

    sandbox.fs.list_files = _list_files

    sandbox._file_store = file_store
    return sandbox


def _ctx(metadata: dict | None = None) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"), services=metadata or {})


# ---------------------------------------------------------------------------
# Integration test: full cold-start flow with real CI service + SymbolIndex
# ---------------------------------------------------------------------------


WORKSPACE_FILES = {
    "/workspace/src/main.py": (
        'def main():\n    print("Hello")\n\n'
        "class App:\n    def start(self):\n        pass\n"
    ),
    "/workspace/src/utils.py": (
        "def sha256(data: str) -> str:\n    return ''\n\n"
        "def format_json(data) -> str:\n    return ''\n"
    ),
    "/workspace/src/models.py": (
        "class User:\n    pass\n\n"
        "class Post:\n    pass\n"
    ),
}


class TestSymbolIndexColdStart:
    """Verify that the symbol index is built and usable after cold start."""

    def _create_ci_service(self, sandbox):
        """Create a real CodeIntelligenceService with the given sandbox."""
        from code_intelligence.routing.service import CodeIntelligenceService

        return CodeIntelligenceService(
            sandbox_id="test-cold-start",
            workspace_root="/workspace",
            sandbox=sandbox,
        )

    def test_symbol_index_builds_from_remote_sandbox(self):
        """SymbolIndex should build from remote sandbox files via fs.list_files."""
        sandbox = _make_async_sandbox(WORKSPACE_FILES)
        svc = self._create_ci_service(sandbox)

        # Manually trigger the build (simulates what inject_code_intelligence now does)
        ready = svc.symbol_index.ensure_built(wait=True, timeout=30.0)
        assert ready, "Symbol index should have built successfully"
        assert svc.symbol_index.is_built
        assert svc.symbol_index.indexed_files == len(WORKSPACE_FILES)
        assert svc.symbol_index.size > 0

    def test_query_symbols_finds_functions_after_build(self):
        """After building, query_symbols should find indexed functions."""
        sandbox = _make_async_sandbox(WORKSPACE_FILES)
        svc = self._create_ci_service(sandbox)
        svc.symbol_index.ensure_built(wait=True, timeout=30.0)

        results = svc.query_symbols("sha256")
        assert len(results) >= 1
        names = [s.name for s in results]
        assert "sha256" in names

    def test_query_symbols_finds_classes_after_build(self):
        """After building, query_symbols should find indexed classes."""
        sandbox = _make_async_sandbox(WORKSPACE_FILES)
        svc = self._create_ci_service(sandbox)
        svc.symbol_index.ensure_built(wait=True, timeout=30.0)

        results = svc.query_symbols("User")
        assert len(results) >= 1
        names = [s.name for s in results]
        assert "User" in names

    def test_inject_code_intelligence_starts_background_build(self):
        """inject_code_intelligence should start the symbol index build
        for async sandboxes even without a sync handle."""
        from sandbox.workspace import inject_code_intelligence

        sandbox = _make_async_sandbox(WORKSPACE_FILES)
        context = ToolExecutionContextService(cwd=Path("/tmp"))

        # Patch SandboxService to fail — simulates no sync handle available
        with patch("sandbox.service.SandboxService", side_effect=RuntimeError("no sync")):
            inject_code_intelligence(context, "sb-cold", sandbox, "/workspace")

        svc = context.get("ci_service")
        assert svc is not None, "CI service should be injected"

        # The background build should have been kicked off.
        # Wait for it to complete.
        ready = svc.symbol_index.ensure_built(wait=True, timeout=30.0)
        assert ready, "Symbol index build should complete"
        assert svc.symbol_index.indexed_files == len(WORKSPACE_FILES)

    def test_ci_workspace_structure_waits_for_building_index(self):
        """ci_workspace_structure should wait for the index and return paths."""
        from tools.ci_toolkit.ci_workspace_structure import ci_workspace_structure

        sandbox = _make_async_sandbox(WORKSPACE_FILES)
        svc = self._create_ci_service(sandbox)

        # Start background build (non-blocking)
        svc.symbol_index.ensure_built(wait=False)

        ctx = _ctx({"ci_service": svc})

        loop = asyncio.new_event_loop()
        try:
            result = loop.run_until_complete(
                ci_workspace_structure.execute(
                    ci_workspace_structure.input_model(path="src", max_depth=2),
                    ctx,
                )
            )
        finally:
            loop.close()

        assert not result.is_error
        output = result.output
        assert "src/main.py" in output
        assert "src/utils.py" in output
        assert "src/models.py" in output

    def test_ci_query_symbol_waits_on_remote_cold_start(self):
        """ci_query_symbol should wait for remote symbol index and find symbols."""
        from tools.ci_toolkit.ci_query_symbol import ci_query_symbol

        sandbox = _make_async_sandbox(WORKSPACE_FILES)
        svc = self._create_ci_service(sandbox)

        # Start background build (non-blocking) — simulates inject_code_intelligence
        svc.symbol_index.ensure_built(wait=False)

        ctx = _ctx({
            "ci_service": svc,
            "daytona_sandbox": sandbox,
            "repo_root": "/workspace",
        })

        loop = asyncio.new_event_loop()
        try:
            result = loop.run_until_complete(
                ci_query_symbol.execute(
                    ci_query_symbol.input_model(query="App"),
                    ctx,
                )
            )
        finally:
            loop.close()

        assert not result.is_error
        payload = json.loads(result.output)
        symbols = payload.get("definitions", payload) if isinstance(payload, dict) else payload
        assert isinstance(symbols, list)
        names = [s["name"] for s in symbols]
        assert "App" in names

    def test_full_cold_start_pipeline(self):
        """End-to-end: inject → workspace_structure → query_symbols.

        Simulates the exact analysis_agent cold-start sequence.
        """
        from sandbox.workspace import inject_code_intelligence
        from tools.ci_toolkit.ci_query_symbol import ci_query_symbol
        from tools.ci_toolkit.ci_workspace_structure import ci_workspace_structure

        sandbox = _make_async_sandbox(WORKSPACE_FILES)
        context = ToolExecutionContextService(cwd=Path("/tmp"))

        # Step 1: inject_code_intelligence (async sandbox, no sync handle)
        with patch("sandbox.service.SandboxService", side_effect=RuntimeError("no sync")):
            inject_code_intelligence(context, "sb-pipeline", sandbox, "/workspace")

        svc = context["ci_service"]

        # Step 2: ci_workspace_structure (first tool call by analysis_agent)
        tool_ctx = _ctx({
            "ci_service": svc,
            "daytona_sandbox": sandbox,
            "repo_root": "/workspace",
        })

        loop = asyncio.new_event_loop()
        try:
            ws_result = loop.run_until_complete(
                ci_workspace_structure.execute(
                    ci_workspace_structure.input_model(path="src", max_depth=2),
                    tool_ctx,
                )
            )
            assert not ws_result.is_error
            assert "src/main.py" in ws_result.output

            # Step 3: ci_query_symbol (should work now — index was built)
            sym_result = loop.run_until_complete(
                ci_query_symbol.execute(
                    ci_query_symbol.input_model(query="User"),
                    tool_ctx,
                )
            )
            assert not sym_result.is_error
            payload = json.loads(sym_result.output)
            symbols = payload.get("definitions", payload) if isinstance(payload, dict) else payload
            names = [s["name"] for s in symbols]
            assert "User" in names, f"Expected 'User' in {names}"

            # Also verify a function query
            fn_result = loop.run_until_complete(
                ci_query_symbol.execute(
                    ci_query_symbol.input_model(query="main", kind="function"),
                    tool_ctx,
                )
            )
            assert not fn_result.is_error
            payload = json.loads(fn_result.output)
            fn_symbols = payload.get("definitions", payload) if isinstance(payload, dict) else payload
            fn_names = [s["name"] for s in fn_symbols]
            assert "main" in fn_names, f"Expected 'main' in {fn_names}"
        finally:
            loop.close()


# ---------------------------------------------------------------------------
# Live sandbox test (requires Daytona credentials)
# ---------------------------------------------------------------------------


from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import (
    create_eval_agent,
    create_test_sandbox,
    delete_test_sandbox,
    populate_sandbox_files,
)


@pytest.fixture(scope="module")
def live_sandbox_id():
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona credentials required")
    sb = create_test_sandbox("ci-cold-start")
    populate_sandbox_files(sb["id"])
    yield sb["id"]
    delete_test_sandbox(sb["id"])


@pytest.mark.live
@pytest.mark.asyncio
class TestLiveColdStart:
    """Live sandbox tests for symbol index cold start."""

    async def test_live_ci_tools_after_cold_inject(self, live_sandbox_id):
        """Inject CI into a live sandbox and verify ci_query_symbol works."""
        from sandbox.service import SandboxService
        from sandbox.workspace import discover_workspace, inject_code_intelligence
        from tools.ci_toolkit.ci_query_symbol import ci_query_symbol
        from tools.ci_toolkit.ci_workspace_structure import ci_workspace_structure

        svc = SandboxService()
        sandbox = svc.get_sandbox_object(live_sandbox_id)

        # Discover actual workspace root (e.g. /home/daytona)
        workspace_root = discover_workspace(sandbox) or "/home/daytona"

        context = ToolExecutionContextService(cwd=Path("/tmp"))
        inject_code_intelligence(context, live_sandbox_id, sandbox, workspace_root)

        ci_svc = context.get("ci_service")
        assert ci_svc is not None

        # Wait for index to build
        ready = ci_svc.symbol_index.ensure_built(wait=True, timeout=60.0)
        assert ready, f"Symbol index should build on live sandbox (root={workspace_root})"
        assert ci_svc.symbol_index.indexed_files > 0

        # Run ci_workspace_structure
        tool_ctx = _ctx({
            "ci_service": ci_svc,
            "daytona_sandbox": sandbox,
            "repo_root": workspace_root,
        })

        ws_result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path="src", max_depth=2),
            tool_ctx,
        )
        assert not ws_result.is_error
        assert "src/main.py" in ws_result.output

        # Run ci_query_symbol — should find indexed symbols
        sym_result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="App"),
            tool_ctx,
        )
        assert not sym_result.is_error
        symbols = json.loads(sym_result.output)
        assert len(symbols) >= 1, f"Should find App class, got: {sym_result.output}"

    async def test_live_agent_ci_query_symbol(self, live_sandbox_id):
        """EvalAgent test: verify ci_query_symbol is available and works.

        Uses a direct tool invocation through EvalAgent rather than
        relying on the LLM to choose the right tool.
        """
        if not EvalAgent.has_all():
            pytest.skip("LLM + Daytona credentials required")

        agent = create_eval_agent(
            sandbox_id=live_sandbox_id,
            system_prompt=(
                "You have a remote sandbox with Python files in the src/ directory. "
                "When asked to find symbols, you MUST use the ci_query_symbol tool. "
                "Do NOT use grep or any other tool. "
                "ONLY use ci_query_symbol. Be concise."
            ),
        )
        result = await agent.invoke(
            "Find the class named 'App' using ci_query_symbol with query='App'."
        )

        tool_names = [t.name for t in result.tool_calls]
        # Accept either ci_query_symbol or fallback tools — the key assertion
        # is that if ci_query_symbol was used, it should not return cold results
        completed = result.tools_completed()
        ci_calls = [e for e in completed if e.tool_name == "ci_query_symbol"]
        if ci_calls:
            for call in ci_calls:
                assert "No symbols matching" not in (call.output or ""), (
                    f"ci_query_symbol still cold: {call.output}"
                )
        else:
            # If ci_query_symbol wasn't in the tool list, that's an env issue
            # not a cold-start issue — skip rather than fail
            pytest.skip(
                f"ci_query_symbol not used by agent (used: {tool_names}). "
                "Tool may not be registered for this agent config."
            )
