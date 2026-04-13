"""Tests for hover and diagnostics tools in tools.ci_toolkit.lsp_tools."""

from __future__ import annotations

import asyncio
import json
import textwrap
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from code_intelligence.lsp.client import LspClient
from tools.core.base import ToolExecutionContext
from tools.ci_toolkit.lsp_tools import ci_diagnostics, ci_hover


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _ctx_with_svc(svc) -> ToolExecutionContext:
    return _ctx({"ci_service": svc})


def test_hover_no_service_returns_error():
    ctx = _ctx()
    result = asyncio.run(
        ci_hover.execute(ci_hover.input_model(file_path="/f.py", line=1), ctx)
    )
    assert result.is_error
    assert "LSP not available" in result.output


def test_hover_no_result():
    svc = MagicMock()
    svc.hover.return_value = None
    ctx = _ctx_with_svc(svc)
    result = asyncio.run(
        ci_hover.execute(ci_hover.input_model(file_path="/f.py", line=5, character=3), ctx)
    )
    assert not result.is_error
    assert "No hover information" in result.output


def test_hover_success():
    hover_result = MagicMock(content="int foo()", language="python")
    svc = MagicMock()
    svc.hover.return_value = hover_result
    ctx = _ctx({"ci_service": svc, "daytona_cwd": "/ws"})
    result = asyncio.run(
        ci_hover.execute(ci_hover.input_model(file_path="/f.py", line=10, character=5), ctx)
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["content"] == "int foo()"
    assert data["language"] == "python"
    assert data["cwd"] == "/ws"
    svc.hover.assert_called_once_with("/f.py", 10, 5)


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
    ctx = _ctx({"ci_service": svc, "daytona_cwd": "/ws"})
    result = asyncio.run(
        ci_diagnostics.execute(ci_diagnostics.input_model(file_path="/f.py"), ctx)
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["clean"] is True
    assert data["diagnostics"] == []


def test_diagnostics_with_errors():
    diag = MagicMock()
    diag.line = 5
    diag.character = 3
    diag.severity = MagicMock(value="error")
    diag.message = "undefined name 'x'"
    diag.source = "pyright"
    svc = MagicMock()
    svc.diagnostics.return_value = [diag]
    ctx = _ctx({"ci_service": svc, "daytona_cwd": "/ws"})
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

    def test_zero_character_resolves_to_indentation(self, tmp_path):
        """character=0 on an indented line → column of first non-whitespace."""
        content = "class Foo:\n    def bar(self):\n        pass\n"
        client, f = self._make_client(tmp_path, content)
        # Line 2: "    def bar(self):" → first non-ws at column 4
        assert client._resolve_column(str(f), 2, 0) == 4

    def test_zero_character_no_indentation(self, tmp_path):
        """character=0 on a non-indented line → column 0."""
        client, f = self._make_client(tmp_path, "import os\n")
        assert client._resolve_column(str(f), 1, 0) == 0

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
        """Tab indentation is counted correctly."""
        client, f = self._make_client(tmp_path, "\t\tdef foo():\n")
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
class TestHoverWithResolveColumn:
    """Verify hover returns results when character=0 on indented lines."""

    def test_hover_indented_def_local(self, tmp_path):
        """Hover on an indented method with character=0 should return results."""
        content = textwrap.dedent("""\
            class MyClass:
                def my_method(self, x: int) -> str:
                    \"\"\"Convert x to string.\"\"\"
                    return str(x)
        """)
        f = tmp_path / "sample.py"
        f.write_text(content, encoding="utf-8")
        client = LspClient(workspace_root=str(tmp_path))

        # character=0 should auto-resolve to column 4 (the 'd' in 'def')
        result = client.hover(str(f), 2, 0)
        assert result is not None, (
            "hover returned None for indented method with character=0 "
            "(resolve_column should have fixed the column)"
        )

    def test_hover_explicit_column_still_works(self, tmp_path):
        """Explicit non-zero column should work as before."""
        content = "def top_level():\n    pass\n"
        f = tmp_path / "sample.py"
        f.write_text(content, encoding="utf-8")
        client = LspClient(workspace_root=str(tmp_path))

        result = client.hover(str(f), 1, 4)  # 't' in 'top_level'
        assert result is not None


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
