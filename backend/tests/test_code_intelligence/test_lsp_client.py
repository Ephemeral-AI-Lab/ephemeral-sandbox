"""Unit tests for the code intelligence LSP client."""

from __future__ import annotations

from code_intelligence.lsp.client import LspClient
from code_intelligence.types import SymbolKind


def test_python_definitions_maps_known_symbol_kind(monkeypatch) -> None:
    lsp = LspClient(workspace_root="/workspace")
    monkeypatch.setattr(
        lsp,
        "_run_python_script",
        lambda script: (
            '[{"name":"demo","path":"/workspace/demo.py","line":7,"col":2,"type":"function"}]'
        ),
    )

    results = lsp._python_definitions("/workspace/demo.py", 7, 2)

    assert len(results) == 1
    assert results[0].kind is SymbolKind.FUNCTION


def test_python_definitions_preserves_unknown_types(monkeypatch) -> None:
    lsp = LspClient(workspace_root="/workspace")
    monkeypatch.setattr(
        lsp,
        "_run_python_script",
        lambda script: (
            '[{"name":"demo","path":"/workspace/demo.py","line":7,"col":2,"type":"statement"}]'
        ),
    )

    results = lsp._python_definitions("/workspace/demo.py", 7, 2)

    assert len(results) == 1
    assert results[0].kind is SymbolKind.UNKNOWN


def test_reset_backend_availability_clears_cached_readiness() -> None:
    lsp = LspClient(workspace_root="/workspace")
    lsp._py_available = False
    lsp._ts_available = True

    lsp.reset_backend_availability()

    assert lsp._py_available is None
    assert lsp._ts_available is None
