# ruff: noqa
"""E2E: ci_query_symbol with symbol-index-first approach.

Verifies that ci_query_symbol resolves symbols via the index,
then calls LSP with correct coordinates.

Run with: pytest tests/test_e2e/test_ci_hover_resolve_column_live.py -v -m live
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from tools.core.base import ToolExecutionContextService
from tools.ci_toolkit.ci_query_symbol import ci_query_symbol

pytestmark = [pytest.mark.e2e]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _ctx(metadata: dict | None = None) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"), services=metadata or {})


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


class TestReferencesResolveColumnUnit:
    """Verify ci_query_symbol resolves symbol via index then calls LSP."""

    async def test_references_resolves_via_symbol_index(self):
        """Symbol index finds the definition, then LSP is called with resolved coords."""
        defn = MagicMock()
        defn.name = "Engine"
        defn.file_path = "/testbed/src/engine.py"
        defn.line = 10
        defn.kind.value = "class"
        defn.signature = "class Engine"

        svc = _svc_stub()
        svc.symbol_index.is_built = True
        svc.symbol_index.find.return_value = [defn]
        svc.query_symbols.return_value = [defn]
        svc.tree_cache = None
        svc.lsp_client._read_line = MagicMock(return_value=None)
        ctx = _ctx({"ci_service": svc})

        with patch("tools.ci_toolkit._query_runtime.get_ci_service", return_value=svc):
            await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="Engine", references=True),
                ctx,
            )
        svc.find_references.assert_called_once()
        call_args = svc.find_references.call_args[0]
        assert call_args[0] == "/testbed/src/engine.py"
        assert call_args[1] == "Engine"
        assert call_args[2] == 10


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


# Uses the default EVAL_SANDBOX_FILES which include src/main.py with:
#   - class App (line ~38) with indented methods start/stop
#   - def get_config() (line ~19) top-level function
#   - def main() (line ~54) which calls App("MyApp").start()
# These are sufficient to test column resolution on indented symbols.


@pytest.fixture(scope="module")
def live_sandbox_id():
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona credentials required")
    sb = create_test_sandbox("ci-col-resolve")
    populate_sandbox_files(sb["id"])
    yield sb["id"]
    delete_test_sandbox(sb["id"])


def _build_ci_context(sandbox_id: str) -> tuple[Any, ToolExecutionContextService]:
    """Build a CI service and tool context for a live sandbox."""
    from sandbox.service import SandboxService
    from sandbox.workspace import discover_workspace, inject_code_intelligence

    svc_client = SandboxService()
    sandbox = svc_client.get_sandbox_object(sandbox_id)
    workspace_root = discover_workspace(sandbox) or "/home/daytona"

    context = ToolExecutionContextService(cwd=Path("/tmp"))
    inject_code_intelligence(context, sandbox_id, sandbox, workspace_root)

    ci_svc = context.get("ci_service")
    assert ci_svc is not None
    ci_svc.symbol_index.ensure_built(wait=True, timeout=60.0)

    # Ensure jedi is available in the sandbox
    ci_svc.lsp_client.ensure_ready(install_missing=True)

    tool_ctx = _ctx({
        "ci_service": ci_svc,
        "sandbox_id": sandbox_id,
        "repo_root": workspace_root,
        "daytona_sandbox": sandbox,
    })
    return ci_svc, tool_ctx


@pytest.mark.live
@pytest.mark.asyncio
class TestLiveReferencesResolveColumn:
    """Live: ci_query_symbol finds refs for indented symbols with character=0."""

    async def test_references_indented_method(self, live_sandbox_id):
        """References for 'start' method should find app.start() calls via symbol index."""
        ci_svc, ctx = _build_ci_context(live_sandbox_id)

        with patch("tools.ci_toolkit._query_runtime.get_ci_service", return_value=ci_svc):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="start", references=True),
                ctx,
            )

        assert not result.is_error
        data = json.loads(result.output)
        # 'start' is called in main() as app.start()
        assert data["total_references"] >= 1, (
            f"Expected references for 'start', got: {result.output}"
        )

    async def test_references_class(self, live_sandbox_id):
        """References for 'App' class should find usages via symbol index."""
        ci_svc, ctx = _build_ci_context(live_sandbox_id)

        with patch("tools.ci_toolkit._query_runtime.get_ci_service", return_value=ci_svc):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="App", references=True),
                ctx,
            )

        assert not result.is_error
        data = json.loads(result.output)
        # App is used in main() as App("MyApp")
        assert data["total_references"] >= 1, (
            f"Expected references for 'App', got: {result.output}"
        )


