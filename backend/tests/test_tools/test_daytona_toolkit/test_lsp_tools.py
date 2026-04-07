"""Tests for tools.daytona_toolkit.lsp_tools."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.lsp_tools import (
    daytona_lsp_hover,
    daytona_lsp_definition,
    daytona_lsp_references,
    daytona_lsp_diagnostics,
)


pytestmark = pytest.mark.asyncio


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _ctx_with_svc(svc) -> ToolExecutionContext:
    return _ctx({"ci_service": svc})


# ---------------------------------------------------------------------------
# daytona_lsp_hover
# ---------------------------------------------------------------------------

async def test_hover_no_service_returns_error():
    ctx = _ctx()
    result = await daytona_lsp_hover.execute(
        daytona_lsp_hover.input_model(file_path="/f.py", line=1), ctx
    )
    assert result.is_error
    assert "LSP not available" in result.output


async def test_hover_no_result():
    svc = MagicMock()
    svc.hover.return_value = None
    ctx = _ctx_with_svc(svc)
    result = await daytona_lsp_hover.execute(
        daytona_lsp_hover.input_model(file_path="/f.py", line=5, character=3), ctx
    )
    assert not result.is_error
    assert "No hover information" in result.output


async def test_hover_success():
    hover_result = MagicMock(content="int foo()", language="python")
    svc = MagicMock()
    svc.hover.return_value = hover_result
    ctx = _ctx({"ci_service": svc, "daytona_cwd": "/ws"})
    result = await daytona_lsp_hover.execute(
        daytona_lsp_hover.input_model(file_path="/f.py", line=10, character=5), ctx
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["content"] == "int foo()"
    assert data["language"] == "python"
    assert data["cwd"] == "/ws"
    svc.hover.assert_called_once_with("/f.py", 10, 5)


# ---------------------------------------------------------------------------
# daytona_lsp_definition
# ---------------------------------------------------------------------------

async def test_definition_no_service_returns_error():
    ctx = _ctx()
    result = await daytona_lsp_definition.execute(
        daytona_lsp_definition.input_model(file_path="/f.py", line=1), ctx
    )
    assert result.is_error
    assert "LSP not available" in result.output


async def test_definition_no_results():
    svc = MagicMock()
    svc.find_definitions.return_value = []
    ctx = _ctx_with_svc(svc)
    result = await daytona_lsp_definition.execute(
        daytona_lsp_definition.input_model(file_path="/f.py", line=5), ctx
    )
    assert not result.is_error
    assert "No definitions found" in result.output


async def test_definition_success():
    sym = MagicMock()
    sym.name = "foo"
    sym.kind = MagicMock(value="function")
    sym.file_path = "/other.py"
    sym.line = 42
    sym.character = 0
    sym.signature = "def foo(): ..."
    svc = MagicMock()
    svc.find_definitions.return_value = [sym]
    ctx = _ctx({"ci_service": svc, "daytona_cwd": "/ws"})
    result = await daytona_lsp_definition.execute(
        daytona_lsp_definition.input_model(
            file_path="/f.py", line=10, character=3, symbol="foo"
        ),
        ctx,
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert len(data["definitions"]) == 1
    d = data["definitions"][0]
    assert d["name"] == "foo"
    assert d["kind"] == "function"
    assert d["file_path"] == "/other.py"
    assert d["line"] == 42


async def test_definition_kind_without_value_attr():
    """If kind has no .value, str() is used."""
    sym = MagicMock(spec=["name", "kind", "file_path", "line", "character", "signature"])
    sym.name = "bar"
    sym.kind = "variable"  # plain string, no .value
    sym.file_path = "/x.py"
    sym.line = 1
    sym.character = 0
    sym.signature = "bar = 1"
    svc = MagicMock()
    svc.find_definitions.return_value = [sym]
    ctx = _ctx_with_svc(svc)
    result = await daytona_lsp_definition.execute(
        daytona_lsp_definition.input_model(file_path="/f.py", line=1), ctx
    )
    data = json.loads(result.output)
    assert data["definitions"][0]["kind"] == "variable"


# ---------------------------------------------------------------------------
# daytona_lsp_references
# ---------------------------------------------------------------------------

async def test_references_no_service_returns_error():
    ctx = _ctx()
    result = await daytona_lsp_references.execute(
        daytona_lsp_references.input_model(file_path="/f.py", line=1), ctx
    )
    assert result.is_error
    assert "LSP not available" in result.output


async def test_references_no_results():
    svc = MagicMock()
    svc.find_references.return_value = []
    ctx = _ctx_with_svc(svc)
    result = await daytona_lsp_references.execute(
        daytona_lsp_references.input_model(file_path="/f.py", line=3), ctx
    )
    assert not result.is_error
    assert "No references found" in result.output


async def test_references_success():
    ref = MagicMock(file_path="/a.py", line=7, character=2, text="foo()")
    svc = MagicMock()
    svc.find_references.return_value = [ref]
    ctx = _ctx({"ci_service": svc, "daytona_cwd": "/ws"})
    result = await daytona_lsp_references.execute(
        daytona_lsp_references.input_model(
            file_path="/f.py", line=1, character=0, symbol="foo"
        ),
        ctx,
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["total_references"] == 1
    assert data["references"][0]["file_path"] == "/a.py"
    assert data["references"][0]["text"] == "foo()"


async def test_references_capped_at_50():
    refs = [MagicMock(file_path=f"/f{i}.py", line=i, character=0, text="x") for i in range(100)]
    svc = MagicMock()
    svc.find_references.return_value = refs
    ctx = _ctx_with_svc(svc)
    result = await daytona_lsp_references.execute(
        daytona_lsp_references.input_model(file_path="/f.py", line=1), ctx
    )
    data = json.loads(result.output)
    assert data["total_references"] == 100
    assert len(data["references"]) == 50  # capped


# ---------------------------------------------------------------------------
# daytona_lsp_diagnostics
# ---------------------------------------------------------------------------

async def test_diagnostics_no_service_returns_error():
    ctx = _ctx()
    result = await daytona_lsp_diagnostics.execute(
        daytona_lsp_diagnostics.input_model(file_path="/f.py"), ctx
    )
    assert result.is_error
    assert "LSP not available" in result.output


async def test_diagnostics_clean():
    svc = MagicMock()
    svc.diagnostics.return_value = []
    ctx = _ctx({"ci_service": svc, "daytona_cwd": "/ws"})
    result = await daytona_lsp_diagnostics.execute(
        daytona_lsp_diagnostics.input_model(file_path="/f.py"), ctx
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["clean"] is True
    assert data["diagnostics"] == []


async def test_diagnostics_with_errors():
    diag = MagicMock()
    diag.line = 5
    diag.character = 3
    diag.severity = MagicMock(value="error")
    diag.message = "undefined name 'x'"
    diag.source = "pyright"
    svc = MagicMock()
    svc.diagnostics.return_value = [diag]
    ctx = _ctx({"ci_service": svc, "daytona_cwd": "/ws"})
    result = await daytona_lsp_diagnostics.execute(
        daytona_lsp_diagnostics.input_model(file_path="/f.py"), ctx
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["clean"] is False
    assert len(data["diagnostics"]) == 1
    d = data["diagnostics"][0]
    assert d["line"] == 5
    assert d["severity"] == "error"
    assert d["message"] == "undefined name 'x'"
    assert d["source"] == "pyright"


async def test_diagnostics_severity_without_value_attr():
    diag = MagicMock(spec=["line", "character", "severity", "message", "source"])
    diag.line = 1
    diag.character = 0
    diag.severity = "warning"  # plain string
    diag.message = "unused import"
    diag.source = "flake8"
    svc = MagicMock()
    svc.diagnostics.return_value = [diag]
    ctx = _ctx_with_svc(svc)
    result = await daytona_lsp_diagnostics.execute(
        daytona_lsp_diagnostics.input_model(file_path="/f.py"), ctx
    )
    data = json.loads(result.output)
    assert data["diagnostics"][0]["severity"] == "warning"
