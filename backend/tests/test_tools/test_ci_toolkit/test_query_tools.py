"""Tests for tools.ci_toolkit.query_tools."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from tools.ci_toolkit.query_tools import (
    _svc_or_error,
    ci_query_symbol,
    ci_status,
    ci_workspace_structure,
)
from tools.core.base import ToolExecutionContext

pytestmark = pytest.mark.asyncio  # applies to all async def tests


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _ctx_with_svc(svc) -> ToolExecutionContext:
    return _ctx({"ci_service": svc})


def _paths(result) -> list[str]:
    return json.loads(result.output)["paths"]


# ---------------------------------------------------------------------------
# _svc_or_error helper
# ---------------------------------------------------------------------------


async def test_svc_or_error_no_service_returns_unavailable():
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=None):
        ctx = _ctx()
        svc, err = _svc_or_error(ctx)
    assert svc is None
    assert err is not None
    data = json.loads(err.output)
    assert data["status"] == "unavailable"


async def test_svc_or_error_with_service_returns_svc():
    mock_svc = MagicMock()
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=mock_svc):
        ctx = _ctx_with_svc(mock_svc)
        svc, err = _svc_or_error(ctx)
    assert svc is mock_svc
    assert err is None


# ---------------------------------------------------------------------------
# ci_status
# ---------------------------------------------------------------------------


async def test_ci_status_no_service_returns_unavailable():
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=None):
        result = await ci_status.execute(ci_status.input_model(), _ctx())
    data = json.loads(result.output)
    assert data["status"] == "unavailable"


async def test_ci_status_returns_service_status():
    svc = MagicMock()
    svc.status.return_value = {"ready": True, "files": 42}
    svc.arbiter = None
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_status.execute(ci_status.input_model(), _ctx_with_svc(svc))
    assert not result.is_error
    data = json.loads(result.output)
    assert data["ready"] is True
    assert data["files"] == 42
    assert data["edit_hotspots"]["note"] == "Arbiter history not available"
    svc.status.assert_called_once()


async def test_ci_status_returns_same_run_hotspots():
    svc = MagicMock()
    svc.status.return_value = {"ready": True}
    svc.arbiter.initialized = True
    svc.arbiter.hotspots.return_value = [
        ("src/hot.py", 15),
        ("src/warm.py", 7),
    ]

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_status.execute(
            ci_status.input_model(include_edit_hotspots=True, hotspot_limit=5),
            _ctx_with_svc(svc),
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["edit_hotspots"]["hotspots"] == [
        {"file": "src/hot.py", "edit_count": 15},
        {"file": "src/warm.py", "edit_count": 7},
    ]
    svc.arbiter.hotspots.assert_called_once_with(limit=5, team_run_id=None)


async def test_workspace_structure_no_service():
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=None):
        result = await ci_workspace_structure.execute(ci_workspace_structure.input_model(), _ctx())
    data = json.loads(result.output)
    assert data["status"] == "unavailable"


async def test_workspace_structure_no_symbol_index():
    svc = MagicMock()
    svc.symbol_index = None
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(), _ctx_with_svc(svc)
        )
    data = json.loads(result.output)
    assert data["status"] == "unavailable"
    assert "not available" in data["message"]


async def test_workspace_structure_with_symbol_index():
    """Uses SymbolIndex instance to list sorted file paths."""
    import threading

    # Build a fake SymbolIndex with _lock and _symbols
    class FakeSymbolIndex:
        def __init__(self):
            self._lock = threading.Lock()
            self.is_built = True
            self._symbols = {
                "src/a.py": [],
                "src/b.py": [],
                "src/z.py": [],
            }

    fake_si = FakeSymbolIndex()
    svc = MagicMock()
    svc.symbol_index = fake_si

    # SymbolIndex is a lazy import inside the function; patch at its source module
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.analysis.symbol_index.SymbolIndex", FakeSymbolIndex):
            result = await ci_workspace_structure.execute(
                ci_workspace_structure.input_model(), _ctx_with_svc(svc)
            )

    assert not result.is_error
    assert "src/a.py" in _paths(result)
    assert "src/b.py" in _paths(result)
    # Sorted order
    lines = _paths(result)
    assert lines == sorted(lines)


async def test_workspace_structure_filters_by_path():
    """path parameter filters results to matching prefix."""
    import threading

    class FakeSymbolIndex:
        def __init__(self):
            self._lock = threading.Lock()
            self.is_built = True
            self._symbols = {
                "src/foo/a.py": [],
                "src/bar/b.py": [],
                "tests/c.py": [],
            }

    fake_si = FakeSymbolIndex()
    svc = MagicMock()
    svc.symbol_index = fake_si

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.analysis.symbol_index.SymbolIndex", FakeSymbolIndex):
            result = await ci_workspace_structure.execute(
                ci_workspace_structure.input_model(path="src/foo"), _ctx_with_svc(svc)
            )

    paths = _paths(result)
    assert "src/foo/a.py" in paths
    assert "src/bar/b.py" not in paths
    assert "tests/c.py" not in paths


async def test_workspace_structure_normalizes_absolute_index_paths():
    import threading

    class FakeSymbolIndex:
        def __init__(self):
            self._lock = threading.Lock()
            self.is_built = True
            self._symbols = {
                "/repo/src/foo/a.py": [],
                "/repo/src/bar/b.py": [],
            }

    svc = MagicMock()
    svc.workspace_root = "/repo"
    svc.symbol_index = FakeSymbolIndex()

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.analysis.symbol_index.SymbolIndex", FakeSymbolIndex):
            result = await ci_workspace_structure.execute(
                ci_workspace_structure.input_model(path="src/foo"),
                _ctx_with_svc(svc),
            )

    assert _paths(result) == ["src/foo/a.py"]


async def test_workspace_structure_honors_max_depth_for_warm_index():
    import threading

    class FakeSymbolIndex:
        def __init__(self):
            self._lock = threading.Lock()
            self.is_built = True
            self._symbols = {
                "/repo/src/top.py": [],
                "/repo/src/pkg/nested.py": [],
            }

    svc = MagicMock()
    svc.workspace_root = "/repo"
    svc.symbol_index = FakeSymbolIndex()

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.analysis.symbol_index.SymbolIndex", FakeSymbolIndex):
            result = await ci_workspace_structure.execute(
                ci_workspace_structure.input_model(path="src", max_depth=1),
                _ctx_with_svc(svc),
            )

    paths = _paths(result)
    assert "src/top.py" in paths
    assert "src/pkg/nested.py" not in paths


async def test_workspace_structure_waits_for_building_index():
    """ci_workspace_structure waits for the symbol index build when in progress."""
    import threading

    class FakeSymbolIndex:
        def __init__(self):
            self._lock = threading.Lock()
            self._symbols = {
                "src/a.py": [],
                "src/b.py": [],
            }
            self.is_built = True

        def ensure_built(self, wait=True, timeout=30.0):
            return True

    fake_si = FakeSymbolIndex()
    fake_si.is_built = False  # Start as not built

    def ensure_and_flip(wait=True, timeout=30.0):
        fake_si.is_built = True
        return True

    fake_si.ensure_built = ensure_and_flip

    svc = MagicMock()
    svc.symbol_index = fake_si

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.analysis.symbol_index.SymbolIndex", FakeSymbolIndex):
            result = await ci_workspace_structure.execute(
                ci_workspace_structure.input_model(), _ctx_with_svc(svc)
            )

    assert not result.is_error
    assert "src/a.py" in _paths(result)
    assert "src/b.py" in _paths(result)


async def test_workspace_structure_non_symbol_index_returns_empty():
    """When symbol_index is not a SymbolIndex instance, returns 'No files indexed'."""
    import threading

    class FakeSymbolIndex:
        def __init__(self):
            self._lock = threading.Lock()
            self._symbols = {}

    svc = MagicMock()
    svc.symbol_index = MagicMock()  # not a FakeSymbolIndex instance

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.analysis.symbol_index.SymbolIndex", FakeSymbolIndex):
            result = await ci_workspace_structure.execute(
                ci_workspace_structure.input_model(), _ctx_with_svc(svc)
            )

    assert "No files indexed" in json.loads(result.output)["message"]


async def test_workspace_structure_local_fallback_for_cold_index(tmp_path):
    source = tmp_path / "src"
    nested = source / "pkg"
    nested.mkdir(parents=True)
    (source / "top.py").write_text("def top():\n    pass\n", encoding="utf-8")
    (nested / "nested.py").write_text("def nested():\n    pass\n", encoding="utf-8")
    (source / "notes.bin").write_text("ignore", encoding="utf-8")

    svc = MagicMock()
    svc.workspace_root = str(tmp_path)
    svc.symbol_index = MagicMock()

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path="src", max_depth=1),
            _ctx_with_svc(svc),
        )

    assert not result.is_error
    assert _paths(result) == ["src/top.py"]


async def test_workspace_structure_remote_fallback_for_cold_index():
    svc = MagicMock()
    svc.workspace_root = "/testbed"
    svc.symbol_index = MagicMock()

    sandbox = MagicMock()
    sandbox.process.exec = AsyncMock(
        return_value=MagicMock(
            exit_code=0,
            result="dask/cli.py\ndask/config.py\n",
        )
    )

    ctx = _ctx_with_svc(svc)
    ctx.metadata["daytona_sandbox"] = sandbox
    ctx.metadata["daytona_cwd"] = "/testbed"

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path="dask", max_depth=1),
            ctx,
        )

    assert not result.is_error
    assert _paths(result) == ["dask/cli.py", "dask/config.py"]


# ---------------------------------------------------------------------------
# ci_query_symbol
# ---------------------------------------------------------------------------


async def test_query_symbols_no_service():
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=None):
        result = await ci_query_symbol.execute(ci_query_symbol.input_model(query="foo"), _ctx())
    data = json.loads(result.output)
    assert data["status"] == "unavailable"


async def test_query_symbols_no_results():
    svc = MagicMock()
    svc.is_initialized = True
    svc.query_symbols.return_value = []

    # SymbolKind is a lazy import inside the function; patch at its source module
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="nonexistent"), _ctx_with_svc(svc)
            )

    assert "No symbols matching" in result.output


async def test_query_symbols_normalize_definition_snippets():
    svc = MagicMock()
    svc.is_initialized = True
    svc.query_symbols.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="def reproduce()"),
                _ctx_with_svc(svc),
            )

    svc.query_symbols.assert_called_once_with("reproduce")


async def test_query_symbols_file_path_bootstrap_returns_file_definitions():
    svc = _svc_with_index(symbols=[])
    sym = _make_symbol_info("CmdRun", "dvc/command/run.py", 10, "class")
    svc.symbol_index.file_symbols.return_value = [sym]

    ctx = _ctx_with_svc(svc)
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="dvc/command/run.py", references=True),
            ctx,
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["file"] == "dvc/command/run.py"
    assert data["definitions"][0]["name"] == "CmdRun"
    assert data["confidence"] == "file_symbols"
    assert ctx.metadata["_ci_symbol_navigation_calls"] == 1


async def test_query_symbols_extensionless_file_path_bootstrap_returns_python_file():
    svc = _svc_with_index(symbols=[])
    sym = _make_symbol_info("parse_config", "pkg/config.py", 4, "function")
    svc.symbol_index.file_symbols.side_effect = lambda path: [sym] if path == "pkg/config.py" else []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="pkg/config"),
            _ctx_with_svc(svc),
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["file"] == "pkg/config.py"
    assert data["definitions"][0]["name"] == "parse_config"


async def test_query_symbols_package_path_bootstrap_returns_indexed_child_definitions():
    from code_intelligence.analysis.symbol_index import SymbolIndex

    symbol_index = SymbolIndex(workspace_root="/repo")
    symbol_index.refresh(
        "/repo/dask/dataframe/io/parquet/core.py",
        "def read_parquet(path):\n    return path\n",
    )
    svc = MagicMock()
    svc.is_initialized = True
    svc.workspace_root = "/repo"
    svc.symbol_index = symbol_index
    svc.query_symbols.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="dask/dataframe/io/parquet"),
            _ctx_with_svc(svc),
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["file"] == "dask/dataframe/io/parquet"
    assert data["definitions"][0]["name"] == "read_parquet"


async def test_query_symbols_file_path_bootstrap_errors_when_file_has_no_indexed_symbols():
    svc = _svc_with_index(symbols=[])
    svc.symbol_index.file_symbols.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="dvc/command/missing.py"),
            _ctx_with_svc(svc),
        )

    assert result.is_error
    assert "No indexed symbols found for file" in result.output


async def test_query_symbols_remote_fallback_on_cold_remote_workspace():
    svc = MagicMock()
    svc.is_initialized = False
    svc.workspace_root = "/testbed"
    svc.query_symbols.return_value = []
    svc.symbol_index.is_built = False

    sandbox = MagicMock()
    sandbox.process.exec = AsyncMock(
        return_value=MagicMock(
            exit_code=0,
            result="/testbed/pydantic/json_schema.py:123:def generate_definitions(self):\n",
        )
    )

    ctx = _ctx_with_svc(svc)
    ctx.metadata["daytona_sandbox"] = sandbox
    ctx.metadata["daytona_cwd"] = "/testbed"

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="generate_definitions"),
                ctx,
            )

    assert not result.is_error
    data = json.loads(result.output)
    symbols = data.get("definitions", data) if isinstance(data, dict) else data
    assert symbols[0]["kind"] == "function"
    assert symbols[0]["name"] == "generate_definitions"
    assert symbols[0]["file"] == "/testbed/pydantic/json_schema.py"
    # Full ensure_initialized is NOT called (remote-only sandbox),
    # but the symbol index warmup IS attempted.
    svc.ensure_initialized.assert_not_called()
    svc.symbol_index.ensure_built.assert_called_once_with(wait=True, timeout=60.0)


async def test_maybe_warm_service_waits_for_symbol_index_on_remote_sandbox():
    """_maybe_warm_service should wait for the symbol index on remote sandboxes
    instead of skipping warmup entirely."""
    from tools.ci_toolkit.query_tools import _maybe_warm_service

    svc = MagicMock()
    svc.is_initialized = False
    svc.workspace_root = "/testbed"
    svc.symbol_index.is_built = False

    sandbox = MagicMock()
    ctx = _ctx_with_svc(svc)
    ctx.metadata["daytona_sandbox"] = sandbox

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        _maybe_warm_service(ctx, svc, label="test")

    # Should NOT call full ensure_initialized (remote-only workspace)
    svc.ensure_initialized.assert_not_called()
    # Should wait for symbol index specifically
    svc.symbol_index.ensure_built.assert_called_once_with(wait=True, timeout=60.0)


async def test_maybe_warm_service_skips_when_already_initialized():
    """_maybe_warm_service is a no-op when service is already initialized."""
    from tools.ci_toolkit.query_tools import _maybe_warm_service

    svc = MagicMock()
    svc.is_initialized = True

    _maybe_warm_service(_ctx_with_svc(svc), svc, label="test")

    svc.ensure_initialized.assert_not_called()
    svc.symbol_index.ensure_built.assert_not_called()


async def test_maybe_warm_service_remote_symbol_index_failure_is_silent():
    """Symbol index warmup failure on remote sandbox is logged but not raised."""
    from tools.ci_toolkit.query_tools import _maybe_warm_service

    svc = MagicMock()
    svc.is_initialized = False
    svc.workspace_root = "/testbed"
    svc.symbol_index.is_built = False
    svc.symbol_index.ensure_built.side_effect = RuntimeError("timeout")

    sandbox = MagicMock()
    ctx = _ctx_with_svc(svc)
    ctx.metadata["daytona_sandbox"] = sandbox

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        _maybe_warm_service(ctx, svc, label="test")  # should not raise


async def test_query_symbols_local_workspace_fallback_finds_class(tmp_path):
    source = tmp_path / "pydantic" / "type_adapter.py"
    source.parent.mkdir()
    source.write_text(
        "class TypeAdapter:\n    pass\n",
        encoding="utf-8",
    )

    svc = MagicMock()
    svc.is_initialized = False
    svc.workspace_root = str(tmp_path)
    svc.query_symbols.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="TypeAdapter"),
                _ctx_with_svc(svc),
            )

    assert not result.is_error
    data = json.loads(result.output)
    symbols = data.get("definitions", data) if isinstance(data, dict) else data
    assert symbols[0]["name"] == "TypeAdapter"
    assert symbols[0]["kind"] == "class"
    assert symbols[0]["file"].endswith("type_adapter.py")


async def test_query_symbols_local_workspace_fallback_finds_partial_function(tmp_path):
    source = tmp_path / "pydantic" / "json_schema.py"
    source.parent.mkdir()
    source.write_text(
        "def _extract_discriminator(schema):\n    return schema\n",
        encoding="utf-8",
    )

    svc = MagicMock()
    svc.is_initialized = False
    svc.workspace_root = str(tmp_path)
    svc.query_symbols.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="discriminator", kind="function"),
                _ctx_with_svc(svc),
            )

    assert not result.is_error
    data = json.loads(result.output)
    symbols = data.get("definitions", data) if isinstance(data, dict) else data
    assert symbols[0]["name"] == "_extract_discriminator"
    assert symbols[0]["kind"] == "function"
    assert symbols[0]["file"].endswith("json_schema.py")


async def test_query_symbols_local_fallback_prefers_exact_leaf_match(tmp_path):
    source = tmp_path / "dvc" / "scm" / "git.py"
    source.parent.mkdir(parents=True)
    source.write_text(
        "class CheckoutErrorSuggestGit:\n    pass\n\nclass Git:\n    pass\n",
        encoding="utf-8",
    )

    svc = MagicMock()
    svc.is_initialized = False
    svc.workspace_root = str(tmp_path)
    svc.query_symbols.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="Git"),
                _ctx_with_svc(svc),
            )

    data = json.loads(result.output)
    symbols = data["definitions"]
    assert [symbol["name"] for symbol in symbols] == ["Git"]


async def test_query_symbols_returns_results():
    sym = MagicMock()
    sym.name = "my_func"
    sym.kind.value = "function"
    sym.file_path = "src/mod.py"
    sym.line = 10
    sym.signature = "def my_func(x: int) -> str"

    svc = MagicMock()
    svc.is_initialized = True
    svc.query_symbols.return_value = [sym]

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="my_func"), _ctx_with_svc(svc)
            )

    assert not result.is_error
    data = json.loads(result.output)
    symbols = data["definitions"]
    assert len(symbols) == 1
    assert symbols[0]["name"] == "my_func"
    assert symbols[0]["file"] == "src/mod.py"
    assert symbols[0]["line"] == 10


async def test_query_symbols_prefers_exact_leaf_match_over_substring_noise():
    exact = MagicMock()
    exact.name = "Git"
    exact.kind.value = "class"
    exact.file_path = "dvc/scm/git/__init__.py"
    exact.line = 61
    exact.signature = "class Git"

    noisy = MagicMock()
    noisy.name = "CheckoutErrorSuggestGit"
    noisy.kind.value = "class"
    noisy.file_path = "dvc/exceptions.py"
    noisy.line = 205
    noisy.signature = "class CheckoutErrorSuggestGit"

    svc = MagicMock()
    svc.is_initialized = True
    svc.query_symbols.return_value = [noisy, exact]

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="Git"),
                _ctx_with_svc(svc),
            )

    data = json.loads(result.output)
    symbols = data["definitions"]
    assert [symbol["name"] for symbol in symbols] == ["Git"]


async def test_query_symbols_waits_for_cold_index():
    sym = MagicMock()
    sym.name = "fresh_symbol"
    sym.kind.value = "function"
    sym.file_path = "tests/test_discriminated_union.py"
    sym.line = 42
    sym.signature = "def fresh_symbol() -> None"

    svc = MagicMock()
    svc.is_initialized = False
    svc.query_symbols.return_value = [sym]

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="fresh_symbol"),
                _ctx_with_svc(svc),
            )

    assert not result.is_error
    svc.ensure_initialized.assert_called_once_with(wait=True)


async def test_query_symbols_with_valid_kind_filter():
    """kind parameter filters symbols by SymbolKind."""
    sym_fn = MagicMock()
    sym_fn.name = "my_func"
    sym_fn.file_path = "a.py"
    sym_fn.line = 1
    sym_fn.signature = ""

    sym_cls = MagicMock()
    sym_cls.name = "MyClass"
    sym_cls.file_path = "b.py"
    sym_cls.line = 5
    sym_cls.signature = ""

    # Make kind comparable: same sentinel object for function_kind
    function_kind = object()
    class_kind = object()
    sym_fn.kind = function_kind
    sym_cls.kind = class_kind

    # Patch kind.value access via a wrapper
    fn_kind_mock = MagicMock()
    fn_kind_mock.value = "function"
    cls_kind_mock = MagicMock()
    cls_kind_mock.value = "class"
    sym_fn.kind = fn_kind_mock
    sym_cls.kind = cls_kind_mock

    svc = MagicMock()
    svc.query_symbols.return_value = [sym_fn, sym_cls]

    # SymbolKind("function") returns fn_kind_mock so the filter matches sym_fn
    mock_kind_cls = MagicMock(return_value=fn_kind_mock)

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind", mock_kind_cls):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="", kind="function"),
                _ctx_with_svc(svc),
            )

    data = json.loads(result.output)
    symbols = data["definitions"]
    names = [s["name"] for s in symbols]
    assert "my_func" in names
    assert "MyClass" not in names


async def test_query_symbols_invalid_kind_ignored():
    """Invalid kind string is silently ignored (no filter applied)."""
    sym = MagicMock()
    sym.name = "anything"
    sym.kind.value = "function"
    sym.file_path = "x.py"
    sym.line = 1
    sym.signature = ""

    svc = MagicMock()
    svc.query_symbols.return_value = [sym]

    # SymbolKind raises ValueError for unknown kind
    mock_kind_cls = MagicMock(side_effect=ValueError("bad kind"))

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind", mock_kind_cls):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="anything", kind="bogus"),
                _ctx_with_svc(svc),
            )

    # No filter applied → symbol still in results
    data = json.loads(result.output)
    symbols = data["definitions"]
    assert len(symbols) == 1


async def test_query_symbols_kind_without_value_attr():
    """Symbols whose kind lacks .value use str() fallback."""

    # Use a plain object whose str() is predictable
    class NoValueKind:
        def __str__(self):
            return "custom_kind"

    sym = MagicMock()
    sym.name = "bare_sym"
    sym.file_path = "f.py"
    sym.line = 3
    sym.signature = "sig"
    sym.kind = NoValueKind()  # has no .value attribute

    svc = MagicMock()
    svc.query_symbols.return_value = [sym]

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbol.execute(
                ci_query_symbol.input_model(query="bare_sym"), _ctx_with_svc(svc)
            )

    assert not result.is_error
    data = json.loads(result.output)
    symbols = data["definitions"]
    assert symbols[0]["name"] == "bare_sym"


# ---------------------------------------------------------------------------
# ci_query_symbol (symbol-index-first approach)
# ---------------------------------------------------------------------------


def _make_symbol_info(name="foo", file_path="src/mod.py", line=10, kind_value="function"):
    sym = MagicMock()
    sym.name = name
    sym.file_path = file_path
    sym.line = line
    sym.kind.value = kind_value
    sym.signature = f"{kind_value} {name}"
    return sym


def _svc_with_index(symbols=None, refs=None, *, initialized=True, is_built=True):
    """Build a mock CI service with symbol index and optional LSP refs."""
    svc = MagicMock()
    svc.is_initialized = initialized
    svc.workspace_root = "/testbed"
    svc.symbol_index.is_built = is_built
    svc.symbol_index.find.return_value = symbols or []
    svc.find_references.return_value = refs or []
    svc.tree_cache = None
    svc.lsp_client = MagicMock()
    svc.lsp_client._read_line = MagicMock(return_value=None)
    svc.lsp_client.ensure_ready.return_value = {"python": True, "typescript": False}
    return svc


async def test_query_references_no_service():
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=None):
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="foo", references=True), _ctx()
        )
    data = json.loads(result.output)
    assert data["status"] == "unavailable"


async def test_query_references_no_results():
    svc = _svc_with_index(symbols=[])
    svc.query_symbols.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="nonexistent", references=True),
            _ctx_with_svc(svc),
        )

    assert "No symbols matching" in result.output


async def test_query_references_lsp_returns_results():
    defn = _make_symbol_info("Engine", "src/engine.py", 10, "class")
    ref1 = MagicMock(file_path="src/runner.py", line=5, text="from engine import Engine")
    ref2 = MagicMock(file_path="src/main.py", line=20, text="engine = Engine(config)")

    svc = _svc_with_index(symbols=[defn], refs=[ref1, ref2])
    svc.query_symbols.return_value = [defn]

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="Engine", references=True),
            _ctx_with_svc(svc),
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["confidence"] == "full"
    assert data["reference_status"] == "lsp"
    assert data["total_references"] == 2
    assert "definitions" in data
    files = [r["file"] for r in data["references"]]
    assert "src/runner.py" in files
    assert "src/main.py" in files


async def test_query_references_bootstraps_python_lsp_for_remote_refs():
    defn = _make_symbol_info("Engine", "/testbed/src/engine.py", 10, "class")
    ref = MagicMock(file_path="/testbed/src/main.py", line=20, text="engine = Engine(config)")

    svc = _svc_with_index(symbols=[defn], refs=[ref], initialized=False, is_built=True)
    svc.query_symbols.return_value = [defn]

    sandbox = MagicMock()
    ctx = _ctx_with_svc(svc)
    ctx.metadata["daytona_sandbox"] = sandbox

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="Engine", references=True),
            ctx,
        )

    data = json.loads(result.output)
    assert data["confidence"] == "full"
    assert data["reference_status"] == "lsp"
    svc.lsp_client.ensure_ready.assert_called_once_with(
        install_missing=True,
        languages=("python",),
    )
    svc.find_references.assert_called_once()


async def test_query_references_rebinds_async_sandbox_to_sync_lsp_handle():
    defn = _make_symbol_info("Engine", "/testbed/src/engine.py", 10, "class")
    ref = MagicMock(file_path="/testbed/src/main.py", line=20, text="engine = Engine(config)")

    async def _exec(_command: str, timeout: int = 0):
        raise AssertionError("async sandbox exec should not be used for LSP readiness")

    svc = _svc_with_index(symbols=[defn], refs=[ref], initialized=False, is_built=True)
    svc.query_symbols.return_value = [defn]
    async_sandbox = SimpleNamespace(process=SimpleNamespace(exec=_exec))
    sync_sandbox = SimpleNamespace(process=SimpleNamespace(exec=MagicMock()))
    svc.lsp_client._sandbox = async_sandbox

    def _rebind(sandbox):
        svc.lsp_client._sandbox = sandbox

    svc.rebind_sandbox.side_effect = _rebind

    ctx = _ctx_with_svc(svc)
    ctx.metadata["sandbox_id"] = "sb-123"
    ctx.metadata["daytona_sandbox"] = async_sandbox

    with (
        patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc),
        patch("sandbox.service.SandboxService") as service_cls,
    ):
        service_cls.return_value.get_sandbox_object.return_value = sync_sandbox
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="Engine", references=True),
            ctx,
        )

    data = json.loads(result.output)
    assert data["confidence"] == "full"
    assert data["reference_status"] == "lsp"
    assert "lsp_reason" not in data
    svc.lsp_client.ensure_ready.assert_called_once_with(
        install_missing=True,
        languages=("python",),
    )
    svc.rebind_sandbox.assert_called_once_with(sync_sandbox)
    svc.find_references.assert_called_once()


async def test_query_references_reports_async_sandbox_when_sync_lsp_handle_missing():
    defn = _make_symbol_info("Engine", "/testbed/src/engine.py", 10, "class")

    async def _exec(_command: str, timeout: int = 0):
        return SimpleNamespace(exit_code=0, result="")

    svc = _svc_with_index(symbols=[defn], refs=[])
    svc.query_symbols.return_value = [defn]
    svc.lsp_client._sandbox = SimpleNamespace(process=SimpleNamespace(exec=_exec))

    ctx = _ctx_with_svc(svc)
    ctx.metadata["sandbox_id"] = "sb-123"

    with (
        patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc),
        patch("sandbox.service.SandboxService") as service_cls,
    ):
        service_cls.return_value.get_sandbox_object.side_effect = RuntimeError("no sync")
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="Engine", references=True),
            ctx,
        )

    data = json.loads(result.output)
    assert data["confidence"] == "unavailable"
    assert data["reference_status"] == "definition_fallback"
    assert data["lsp_reason"] == "async_sandbox_lsp_unavailable"
    svc.lsp_client.ensure_ready.assert_not_called()
    svc.find_references.assert_not_called()


async def test_query_references_reports_python_lsp_unavailable():
    defn = _make_symbol_info("Engine", "/testbed/src/engine.py", 10, "class")

    svc = _svc_with_index(symbols=[defn], refs=[])
    svc.query_symbols.return_value = [defn]
    svc.lsp_client.ensure_ready.return_value = {"python": False, "typescript": False}

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="Engine", references=True),
            _ctx_with_svc(svc),
        )

    data = json.loads(result.output)
    assert data["confidence"] == "unavailable"
    assert data["reference_status"] == "definition_fallback"
    assert data["lsp_reason"] == "python_backend_unavailable"
    svc.find_references.assert_not_called()


async def test_query_references_lsp_fails_falls_back_to_definitions():
    defn = _make_symbol_info("Engine", "src/engine.py", 10, "class")
    svc = _svc_with_index(symbols=[defn], refs=[])
    svc.query_symbols.return_value = [defn]
    svc.find_references.side_effect = RuntimeError("LSP timeout")

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="Engine", references=True),
            _ctx_with_svc(svc),
        )

    data = json.loads(result.output)
    assert data["confidence"] == "unavailable"
    assert data["reference_status"] == "definition_fallback"
    assert data["lsp_reason"] == "find_references_error: LSP timeout"
    assert data["total_references"] == 1
    assert "definition:" in data["references"][0]["text"]


async def test_query_references_lsp_empty_falls_back_to_definitions():
    defn = _make_symbol_info("Engine", "src/engine.py", 10, "class")
    svc = _svc_with_index(symbols=[defn], refs=[])
    svc.query_symbols.return_value = [defn]

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="Engine", references=True),
            _ctx_with_svc(svc),
        )

    data = json.loads(result.output)
    assert data["confidence"] == "unavailable"
    assert data["reference_status"] == "definition_fallback"
    assert data["lsp_reason"] == "no_lsp_references"
    assert data["total_references"] == 1
    assert data["references"][0]["file"] == "src/engine.py"


async def test_query_references_prefers_production_over_test_definitions():
    test_defn = _make_symbol_info("Engine", "tests/test_engine.py", 5, "class")
    prod_defn = _make_symbol_info("Engine", "src/engine.py", 10, "class")
    ref = MagicMock(file_path="src/main.py", line=1, text="Engine()")

    svc = _svc_with_index(symbols=[test_defn, prod_defn], refs=[ref])
    svc.query_symbols.return_value = [test_defn, prod_defn]

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbol.execute(
            ci_query_symbol.input_model(query="Engine", references=True),
            _ctx_with_svc(svc),
        )

    data = json.loads(result.output)
    assert data["confidence"] == "full"
    # LSP was called with prod definition first (sorted by priority)
    call_args = svc.find_references.call_args_list[0]
    assert call_args[0][0] == "src/engine.py"


async def test_query_references_resolves_column_from_tree_cache():
    from tools.ci_toolkit.query_tools import _resolve_symbol_column

    svc = MagicMock()
    entry = MagicMock()
    entry.content = b"class Engine:\n    pass\n"
    svc.tree_cache.get_entry.return_value = entry

    col = _resolve_symbol_column(svc, "src/engine.py", 1, "Engine")
    assert col == 6  # "class Engine:" -> Engine starts at col 6


async def test_ci_status_cross_run_hotspots_use_service_arbiter_without_scope_paths():
    svc = MagicMock()
    svc.status.return_value = {"ready": True}
    svc.arbiter.initialized = True
    svc.arbiter.contention_hotspots.return_value = [
        SimpleNamespace(file_path="src/hot.py", contributor_count=2, edit_count=4),
    ]

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_status.execute(
            ci_status.input_model(
                include_edit_hotspots=True,
                hotspot_limit=5,
                hotspot_cross_run=True,
            ),
            _ctx_with_svc(svc),
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["edit_hotspots"]["hotspots"] == [
        {"file": "src/hot.py", "runs_touched": 2, "total_edits": 4}
    ]
    svc.arbiter.contention_hotspots.assert_called_once_with(
        scope_prefixes=None,
        limit=5,
        team_run_id=None,
    )


async def test_ci_status_cross_run_hotspots_filter_by_team_run_id():
    svc = MagicMock()
    svc.status.return_value = {"ready": True}
    svc.arbiter.initialized = True
    svc.arbiter.contention_hotspots.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        await ci_status.execute(
            ci_status.input_model(
                include_edit_hotspots=True,
                hotspot_limit=3,
                hotspot_cross_run=True,
                hotspot_scope_paths=["src/"],
            ),
            _ctx({"ci_service": svc, "team_run_id": "team-1"}),
        )

    svc.arbiter.contention_hotspots.assert_called_once_with(
        scope_prefixes=["src/"],
        limit=3,
        team_run_id="team-1",
    )
