"""Tests for diagnostics tool in tools.ci_toolkit.ci_diagnostics."""

from __future__ import annotations

import asyncio
import json
import textwrap
from pathlib import Path
from unittest.mock import MagicMock

import pytest
from sandbox.async_bridge import current_sandbox_io_loop
from sandbox.code_intelligence.language_server.client import LspClient
from tools.ci_toolkit.ci_diagnostics import ci_diagnostics
from tools.core.base import ToolExecutionContextService


def _ctx(services=None) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"), services=services or {})


def _ctx_with_svc(svc) -> ToolExecutionContextService:
    return _ctx({"ci_service": svc})


def test_diagnostics_no_service_returns_error():
    ctx = _ctx()
    result = asyncio.run(
        ci_diagnostics.execute(ci_diagnostics.input_model(file_path="/f.py"), ctx)
    )
    assert result.is_error
    assert "LSP not available" in result.output


def test_diagnostics_clean():
    svc = MagicMock()
    svc.diagnostics.return_value = []
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws"})
    result = asyncio.run(
        ci_diagnostics.execute(ci_diagnostics.input_model(file_path="/f.py"), ctx)
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["clean"] is True
    assert data["diagnostics"] == []


def test_diagnostics_runs_service_with_sandbox_io_loop():
    class _Service:
        def diagnostics(self, file_path: str):
            assert file_path == "/f.py"
            assert current_sandbox_io_loop() is not None
            return []

    ctx = _ctx({"ci_service": _Service(), "repo_root": "/ws"})
    result = asyncio.run(
        ci_diagnostics.execute(ci_diagnostics.input_model(file_path="/f.py"), ctx)
    )

    assert not result.is_error
    assert json.loads(result.output)["clean"] is True


def test_diagnostics_backend_failure_returns_error_not_clean():
    svc = MagicMock()
    svc.diagnostics.side_effect = RuntimeError("transport unavailable")
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws"})

    result = asyncio.run(
        ci_diagnostics.execute(ci_diagnostics.input_model(file_path="/f.py"), ctx)
    )

    assert result.is_error
    assert "LSP diagnostics unavailable" in result.output
    assert "transport unavailable" in result.output


def test_diagnostics_with_errors():
    diag = MagicMock()
    diag.line = 5
    diag.character = 3
    diag.severity = MagicMock(value="error")
    diag.message = "undefined name 'x'"
    diag.source = "pyright"
    svc = MagicMock()
    svc.diagnostics.return_value = [diag]
    ctx = _ctx({"ci_service": svc, "repo_root": "/ws"})
    result = asyncio.run(
        ci_diagnostics.execute(ci_diagnostics.input_model(file_path="/f.py"), ctx)
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["clean"] is False
    assert len(data["diagnostics"]) == 1
    diagnostic = data["diagnostics"][0]
    assert diagnostic["line"] == 5
    assert diagnostic["severity"] == "error"
    assert diagnostic["message"] == "undefined name 'x'"
    assert diagnostic["source"] == "pyright"


def test_diagnostics_severity_without_value_attr():
    diag = MagicMock(spec=["line", "character", "severity", "message", "source"])
    diag.line = 1
    diag.character = 0
    diag.severity = "warning"
    diag.message = "unused import"
    diag.source = "flake8"
    svc = MagicMock()
    svc.diagnostics.return_value = [diag]
    ctx = _ctx_with_svc(svc)
    result = asyncio.run(
        ci_diagnostics.execute(ci_diagnostics.input_model(file_path="/f.py"), ctx)
    )
    data = json.loads(result.output)
    assert data["diagnostics"][0]["severity"] == "warning"


def test_lsp_diagnostics_cache_invalidates_relative_query_with_absolute_path(
    tmp_path: Path,
) -> None:
    source = tmp_path / "pkg" / "mod.py"
    source.parent.mkdir()
    source.write_text("value = 1\n", encoding="utf-8")
    client = LspClient(workspace_root=str(tmp_path))

    assert client.diagnostics("pkg/mod.py") == []

    source.write_text("def broken(:\n", encoding="utf-8")
    client.invalidate(str(source))

    diagnostics = client.diagnostics("pkg/mod.py")
    assert len(diagnostics) == 1
    assert diagnostics[0].source == "python"
    assert diagnostics[0].message == "invalid syntax"


# ---------------------------------------------------------------------------
# _resolve_column tests
# ---------------------------------------------------------------------------


class TestResolveColumn:
    """Verify _resolve_column auto-detects first non-whitespace column."""

    def _make_client(self, tmp_path: Path, content: str) -> tuple[LspClient, Path]:
        f = tmp_path / "sample.py"
        f.write_text(content, encoding="utf-8")
        return LspClient(workspace_root=str(tmp_path)), f

    def test_nonzero_character_passthrough(self, tmp_path):
        """When character > 0, _resolve_column returns it unchanged."""
        client, f = self._make_client(tmp_path, "    def foo():\n        pass\n")
        assert client._resolve_column(str(f), 1, 7) == 7

    def test_zero_character_resolves_to_symbol_on_def(self, tmp_path):
        """character=0 on an indented def line → column of the symbol name."""
        content = "class Foo:\n    def bar(self):\n        pass\n"
        client, f = self._make_client(tmp_path, content)
        # Line 2: "    def bar(self):" → 'bar' starts at column 8
        assert client._resolve_column(str(f), 2, 0) == 8

    def test_zero_character_resolves_to_symbol_on_class(self, tmp_path):
        """character=0 on a class line → column of the class name."""
        client, f = self._make_client(tmp_path, "class Foo:\n    pass\n")
        # Line 1: "class Foo:" → 'Foo' starts at column 6
        assert client._resolve_column(str(f), 1, 0) == 6

    def test_zero_character_resolves_to_symbol_on_async_def(self, tmp_path):
        """character=0 on an async def line → column of the function name."""
        client, f = self._make_client(tmp_path, "    async def fetch(self):\n        pass\n")
        # Line 1: "    async def fetch(self):" → 'fetch' starts at column 14
        assert client._resolve_column(str(f), 1, 0) == 14

    def test_zero_character_no_indentation(self, tmp_path):
        """character=0 on a non-indented line → column 0."""
        client, f = self._make_client(tmp_path, "import os\n")
        assert client._resolve_column(str(f), 1, 0) == 0

    def test_top_level_def_resolves_to_name(self, tmp_path):
        """character=0 on top-level def → column of function name."""
        client, f = self._make_client(tmp_path, "def get_config():\n    pass\n")
        # "def get_config():" → 'get_config' starts at column 4
        assert client._resolve_column(str(f), 1, 0) == 4

    def test_blank_line_returns_zero(self, tmp_path):
        """character=0 on a blank line → 0."""
        client, f = self._make_client(tmp_path, "x = 1\n\ny = 2\n")
        assert client._resolve_column(str(f), 2, 0) == 0

    def test_out_of_range_line_returns_zero(self, tmp_path):
        """Line number beyond file length → 0."""
        client, f = self._make_client(tmp_path, "x = 1\n")
        assert client._resolve_column(str(f), 99, 0) == 0

    def test_nonexistent_file_returns_zero(self):
        """Missing file → 0 (no crash)."""
        client = LspClient(workspace_root="/tmp")
        assert client._resolve_column("/tmp/no_such_file.py", 1, 0) == 0

    def test_tabs_resolved(self, tmp_path):
        """Tab-indented def resolves to the symbol name column."""
        client, f = self._make_client(tmp_path, "\t\tdef foo():\n")
        # "\t\tdef foo():" → 'foo' starts at column 6 (2 tabs + "def " = 6 chars)
        assert client._resolve_column(str(f), 1, 0) == 6

    def test_tabs_non_def_line(self, tmp_path):
        """Tab indentation on a non-def line resolves to first non-whitespace."""
        client, f = self._make_client(tmp_path, "\t\treturn x\n")
        assert client._resolve_column(str(f), 1, 0) == 2

    def test_deeply_nested(self, tmp_path):
        """8-space indent resolves to column 8."""
        client, f = self._make_client(tmp_path, "        return x\n")
        assert client._resolve_column(str(f), 1, 0) == 8


# ---------------------------------------------------------------------------
# Local hover / references integration with _resolve_column
# ---------------------------------------------------------------------------


_has_jedi = False
try:
    import jedi  # noqa: F401
    _has_jedi = True
except ImportError:
    pass

_skip_no_jedi = pytest.mark.skipif(not _has_jedi, reason="jedi not installed")


@_skip_no_jedi
class TestReferencesWithResolveColumn:
    """Verify find_references returns results when character=0."""

    def test_references_indented_method_local(self, tmp_path):
        """find_references on indented method with character=0 should find refs."""
        content = textwrap.dedent("""\
            class Engine:
                def start(self):
                    pass

                def run(self):
                    self.start()
        """)
        f = tmp_path / "sample.py"
        f.write_text(content, encoding="utf-8")
        client = LspClient(workspace_root=str(tmp_path))

        # Line 2: "    def start(self):" — character=0 should resolve to 4
        refs = client.find_references(str(f), 2, 0)
        assert len(refs) >= 1, (
            f"Expected references for 'start', got {len(refs)}. "
            "resolve_column should have fixed column=0."
        )
