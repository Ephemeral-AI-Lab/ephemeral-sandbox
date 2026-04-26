# ruff: noqa
"""E2E: ci_query_symbol symbol-index-first approach for planner agents.

Tests that ci_query_symbol uses the symbol index to resolve definitions,
then queries LSP with correct coordinates — eliminating the old ripgrep
fallback chain that was unreliable in planner/sandbox contexts.

Run with: pytest tests/test_e2e/test_ci_reference_lazy_sandbox.py -v
"""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from tools.core.base import ToolExecutionContextService
from tools.ci_toolkit.ci_query_symbol import ci_query_symbol

pytestmark = [pytest.mark.e2e]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _ctx(metadata: dict | None = None) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"), services=metadata or {})


def _make_defn(name: str, file_path: str, line: int, kind_value: str = "function"):
    defn = MagicMock()
    defn.name = name
    defn.file_path = file_path
    defn.line = line
    defn.kind.value = kind_value
    defn.signature = f"{kind_value} {name}"
    return defn


def _svc_stub(
    *,
    workspace_root: str = "/testbed",
    initialized: bool = True,
    is_built: bool = True,
    symbols=None,
    refs=None,
) -> MagicMock:
    svc = MagicMock()
    svc.is_initialized = initialized
    svc.workspace_root = workspace_root
    svc.symbol_index.is_built = is_built
    svc.symbol_index.find.return_value = symbols or []
    svc.query_symbols.return_value = symbols or []
    svc.find_references.return_value = refs or []
    svc.tree_cache = None
    svc.lsp_client = MagicMock()
    svc.lsp_client._read_line = MagicMock(return_value=None)
    return svc


# ---------------------------------------------------------------------------
# Symbol-index-first: planner scenario
# ---------------------------------------------------------------------------


class TestCIQueryReferencesSymbolIndex:
    """Tests the symbol-index-first approach that replaces the old
    ripgrep fallback chain."""

    async def test_planner_context_finds_references_via_symbol_index(self):
        """Planner has sandbox_id but no daytona_sandbox — symbol index
        resolves definitions, LSP returns references."""
        defn = _make_defn("Engine", "/testbed/src/engine.py", 10, "class")
        ref1 = MagicMock(file_path="/testbed/src/runner.py", line=5, text="from engine import Engine")
        ref2 = MagicMock(file_path="/testbed/src/main.py", line=20, text="engine = Engine(config)")

        svc = _svc_stub(symbols=[defn], refs=[ref1, ref2])

        ctx = _ctx({
            "ci_service": svc,
            "sandbox_id": "sb-planner",
            "repo_root": "/testbed",
            "agent_name": "analysis_agent",
        })

        with patch("tools.ci_toolkit._query_runtime.get_ci_service", return_value=svc):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="Engine", references=True),
                ctx,
            )

        assert not result.is_error
        data = json.loads(result.output)
        assert data["confidence"] == "full"
        assert data["total_references"] == 2
        files = [r["file"] for r in data["references"]]
        assert "/testbed/src/runner.py" in files
        assert "/testbed/src/main.py" in files

    async def test_planner_lsp_cold_returns_definitions(self):
        """When LSP is cold, returns definitions with confidence=unavailable."""
        defn = _make_defn("fs_copy", "/testbed/dvc/utils/fs.py", 42, "function")

        svc = _svc_stub(symbols=[defn], refs=[])

        ctx = _ctx({
            "ci_service": svc,
            "sandbox_id": "sb-planner",
            "repo_root": "/testbed",
            "agent_name": "analysis_agent",
        })

        with patch("tools.ci_toolkit._query_runtime.get_ci_service", return_value=svc):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="fs_copy", references=True),
                ctx,
            )

        data = json.loads(result.output)
        assert data["confidence"] == "unavailable"
        assert data["total_references"] == 1
        assert data["references"][0]["file"] == "/testbed/dvc/utils/fs.py"
        assert "definition:" in data["references"][0]["text"]

    async def test_symbol_not_in_index_returns_clean_message(self):
        """When symbol doesn't exist in index, returns clear message."""
        svc = _svc_stub(symbols=[])

        ctx = _ctx({
            "ci_service": svc,
            "sandbox_id": "sb-planner",
            "repo_root": "/testbed",
        })

        with patch("tools.ci_toolkit._query_runtime.get_ci_service", return_value=svc):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="nonexistent", references=True),
                ctx,
            )

        assert "No symbols matching" in result.output

    async def test_no_service_returns_unavailable(self):
        """When CI service is not available, returns unavailable status."""
        ctx = _ctx({
            "sandbox_id": "sb-planner",
            "repo_root": "/testbed",
        })

        with patch("tools.ci_toolkit._query_runtime.get_ci_service", return_value=None):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="Engine", references=True),
                ctx,
            )

        data = json.loads(result.output)
        assert data["status"] == "unavailable"

    async def test_prefers_production_definitions_over_test(self):
        """Production files are queried before test files."""
        test_defn = _make_defn("CmdDiff", "/testbed/tests/test_diff.py", 5, "class")
        prod_defn = _make_defn("CmdDiff", "/testbed/dvc/command/diff.py", 10, "class")
        ref = MagicMock(file_path="/testbed/dvc/cli.py", line=1, text="from command.diff import CmdDiff")

        svc = _svc_stub(symbols=[test_defn, prod_defn], refs=[ref])

        ctx = _ctx({
            "ci_service": svc,
            "sandbox_id": "sb-planner",
            "repo_root": "/testbed",
        })

        with patch("tools.ci_toolkit._query_runtime.get_ci_service", return_value=svc):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="CmdDiff", references=True),
                ctx,
            )

        data = json.loads(result.output)
        assert data["confidence"] == "full"
        # LSP was called with production definition first
        call_args = svc.find_references.call_args_list[0]
        assert call_args[0][0] == "/testbed/dvc/command/diff.py"


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
    sb = create_test_sandbox("ci-ref-idx")
    populate_sandbox_files(sb["id"])
    yield sb["id"]
    delete_test_sandbox(sb["id"])


@pytest.mark.live
@pytest.mark.asyncio
class TestLiveSymbolIndexReferences:
    """Live sandbox tests for symbol-index-first ci_query_symbol."""

    async def test_live_references_via_symbol_index(self, live_sandbox_id):
        """Live: ci_query_symbol finds references using symbol index + LSP."""
        from sandbox.service import SandboxService
        from sandbox.workspace import discover_workspace, inject_code_intelligence

        svc_client = SandboxService()
        sandbox = svc_client.get_sandbox_object(live_sandbox_id)
        workspace_root = discover_workspace(sandbox) or "/home/daytona"

        context = ToolExecutionContextService(cwd=Path("/tmp"))
        inject_code_intelligence(context, live_sandbox_id, sandbox, workspace_root)

        ci_svc = context.get("ci_service")
        assert ci_svc is not None
        ci_svc.symbol_index.ensure_built(wait=True, timeout=60.0)

        planner_ctx = _ctx({
            "ci_service": ci_svc,
            "sandbox_id": live_sandbox_id,
            "repo_root": workspace_root,
            "agent_name": "analysis_agent",
        })

        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="App", references=True),
            planner_ctx,
        )

        assert not result.is_error
        data = json.loads(result.output)
        assert data["total_references"] >= 1, (
            f"Expected references for 'App', got: {result.output}"
        )
