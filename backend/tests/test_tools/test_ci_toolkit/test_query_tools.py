"""Tests for tools.ci_toolkit.query_tools."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from engine.core.query import _merge_submission_metadata
from tools.core.base import ToolExecutionContext
from tools.core.runtime import ExecutionMetadata
from tools.ci_toolkit.query_tools import (
    _svc_or_error,
    ci_status,
    ci_scoped_status,
    ci_scope_status,
    ci_workspace_structure,
    ci_query_symbols,
    ci_query_references,
    ci_edit_hotspots,
    ci_recent_changes,
)


pytestmark = pytest.mark.asyncio  # applies to all async def tests


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _ctx_with_svc(svc) -> ToolExecutionContext:
    return _ctx({"ci_service": svc})


def _benchmark_root_metadata(
    svc,
    *,
    team_run_id: str = "TR1",
    loaded_refs: list[str] | None = None,
    extra: dict | None = None,
) -> dict:
    metadata = {
        "agent_name": "team_planner",
        "team_run_id": team_run_id,
        "work_item_id": "ROOT",
        "ci_service": svc,
    }
    if loaded_refs is not None:
        metadata["_loaded_skill_references_by_skill_this_turn"] = {
            "team-planner-playbook": loaded_refs,
        }
    if extra:
        metadata.update(extra)
    return metadata


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
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_status.execute(ci_status.input_model(), _ctx_with_svc(svc))
    assert not result.is_error
    data = json.loads(result.output)
    assert data["ready"] is True
    assert data["files"] == 42
    svc.status.assert_called_once()


async def test_ci_scope_status_returns_live_scope_packet():
    svc = MagicMock()
    svc.ledger.generation = 3
    svc.ledger.recent_entries.return_value = []
    svc.arbiter.generation = 7
    svc.arbiter.active_reservations.return_value = [
        {
            "token_id": "tok-1",
            "file_path": "src/app.py",
            "agent_id": "worker-1",
            "issued_at": 1.0,
            "expires_at": 2.0,
        }
    ]
    svc.arbiter.active_edit_intents.return_value = [
        {
            "intent_id": "intent-1",
            "file_path": "src/app.py",
            "agent_id": "worker-1",
            "scope": "symbol",
            "symbols": ["app.main"],
            "issued_at": 1.0,
            "heartbeat_at": 1.5,
            "expires_at": 2.0,
        }
    ]
    svc.arbiter.hotspots.return_value = [("src/app.py", 4)]
    svc.symbol_index.generation = 11
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        ctx = _ctx_with_svc(svc)
        result = await ci_scope_status.execute(
            ci_scope_status.input_model(scope_paths=["src"]),
            ctx,
        )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["scope_paths"] == ["src"]
    assert data["ledger_generation"] == 3
    assert data["arbiter_generation"] == 7
    assert data["symbol_index_generation"] == 11
    assert data["active_reservations"][0]["file_path"] == "src/app.py"
    assert data["active_edit_intents"][0]["scope"] == "symbol"
    assert data["coherence_token"]
    assert data["admission"]["mode"] == "serialize"
    assert data["admission"]["allow_parallel_fanout"] is False
    assert result.metadata["scope_packet"]["coherence_token"] == data["coherence_token"]
    assert result.metadata["coherence_token"] == data["coherence_token"]
    assert ctx.metadata["scope_packet"]["coherence_token"] == data["coherence_token"]
    assert ctx.metadata["coherence_token"] == data["coherence_token"]


async def test_ci_scoped_status_alias_returns_live_scope_packet():
    svc = MagicMock()
    svc.ledger.generation = 1
    svc.ledger.recent_entries.return_value = []
    svc.arbiter.generation = 2
    svc.arbiter.active_reservations.return_value = []
    svc.arbiter.active_edit_intents.return_value = []
    svc.arbiter.hotspots.return_value = []
    svc.symbol_index.generation = 3
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        ctx = _ctx_with_svc(svc)
        result = await ci_scoped_status.execute(
            ci_scoped_status.input_model(scope_paths=["src"]),
            ctx,
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["scope_paths"] == ["src"]
    assert result.metadata["scope_packet"]["scope_paths"] == ["src"]


async def test_ci_scope_status_defaults_to_default_scope_paths_when_unspecified():
    svc = MagicMock()
    svc.ledger.generation = 1
    svc.ledger.recent_entries.return_value = []
    svc.arbiter.generation = 2
    svc.arbiter.active_reservations.return_value = []
    svc.arbiter.hotspots.return_value = []
    svc.symbol_index.generation = 3
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        ctx = _ctx(
            {
                "ci_service": svc,
                "default_scope_paths": ["pydantic/networks.py"],
            }
        )
        result = await ci_scope_status.execute(ci_scope_status.input_model(), ctx)

    assert not result.is_error
    data = json.loads(result.output)
    assert data["scope_paths"] == ["pydantic/networks.py"]


async def test_ci_scope_status_rejects_scout_caller():
    svc = MagicMock()
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_scope_status.execute(
            ci_scope_status.input_model(scope_paths=["src"]),
            _ctx({"agent_name": "scout", "ci_service": svc}),
        )

    assert result.is_error
    assert "scout is read-only and may use only" in result.output
    assert "ci_scope_status" in result.output


async def test_workspace_structure_allows_single_narrow_preanchor_pass_on_benchmark_root_planner(monkeypatch):
    svc = MagicMock()
    svc.symbol_index = MagicMock()
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR1" else None)
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path="pkg", max_depth=3),
            _ctx(_benchmark_root_metadata(svc, loaded_refs=["exploration-script"])),
        )

    assert not result.is_error
    assert result.metadata["_benchmark_root_preanchor_structure_done"] is True


async def test_workspace_structure_rejects_root_listing_before_scope_status_on_benchmark_root_planner(monkeypatch):
    svc = MagicMock()
    svc.symbol_index = MagicMock()
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR1" else None)
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path=""),
            _ctx(_benchmark_root_metadata(svc, loaded_refs=["exploration-script"])),
        )

    assert result.is_error
    assert "must open with a narrow" in result.output


async def test_workspace_structure_rejects_second_preanchor_pass_before_scope_status_on_benchmark_root_planner(monkeypatch):
    svc = MagicMock()
    svc.symbol_index = MagicMock()
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR1" else None)
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        ctx = _ctx(
            _benchmark_root_metadata(svc, loaded_refs=["exploration-script"])
        )
        first = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path="pkg", max_depth=3),
            ctx,
        )
        second = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path="pkg/io", max_depth=3),
            ctx,
        )

    assert not first.is_error
    assert second.is_error
    assert "one narrow `ci_workspace_structure(...)` opener" in second.output


async def test_ci_scope_status_allows_missing_paths_on_benchmark_root_planner(monkeypatch):
    svc = MagicMock()
    svc.symbol_index = SimpleNamespace(
        generation=3,
        _symbols={
            "/testbed/pkg/core.py": [],
            "/testbed/pkg/io/real.py": [],
            "/testbed/pkg/tests/test_api.py": [],
        },
    )
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR_MISSING" else None)
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_scope_status.execute(
            ci_scope_status.input_model(scope_paths=["pkg/missing.py", "pkg/io"]),
            _ctx(
                _benchmark_root_metadata(
                    svc,
                    team_run_id="TR_MISSING",
                    extra={"_benchmark_root_scope_anchor_done": True},
                )
            ),
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["scope_paths"] == ["pkg/io", "pkg/missing.py"]


async def test_ci_scope_status_allows_missing_paths_on_benchmark_root_planner_via_sandbox(monkeypatch):
    svc = MagicMock()
    svc.symbol_index = SimpleNamespace(generation=3, _symbols={})
    sandbox = SimpleNamespace(
        process=SimpleNamespace(
            exec=AsyncMock()
        )
    )
    async def _exec(command: str, timeout: int = 10):
        if "pkg/missing.py" in command:
            return SimpleNamespace(exit_code=0, result="0")
        return SimpleNamespace(exit_code=0, result="1")

    sandbox.process.exec.side_effect = _exec
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR_REMOTE" else None)
    with (
        patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc),
        patch("tools.ci_toolkit.query_tools.get_daytona_sandbox", return_value=sandbox),
        patch(
            "tools.ci_toolkit.query_tools.resolve_daytona_path",
            side_effect=lambda path, context: f"/testbed/{path}",
        ),
    ):
        result = await ci_scope_status.execute(
            ci_scope_status.input_model(scope_paths=["pkg/missing.py", "pkg/io"]),
            _ctx(
                _benchmark_root_metadata(
                    svc,
                    team_run_id="TR_REMOTE",
                    extra={
                        "_benchmark_root_scope_anchor_done": True,
                        "daytona_sandbox": sandbox,
                    },
                )
            ),
        )

    assert not result.is_error
    data = json.loads(result.output)
    assert data["scope_paths"] == ["pkg/io", "pkg/missing.py"]


async def test_ci_query_symbols_rejects_test_only_hits_for_benchmark_root_planner(monkeypatch):
    svc = MagicMock()
    svc.is_initialized = True
    svc.query_symbols.return_value = [
        SimpleNamespace(
            name="backends",
            kind=SimpleNamespace(value="variable"),
            file_path="pkg/tests/test_api.py",
            line=10,
            signature="backends = ...",
        )
    ]
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR2" else None)
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbols.execute(
            ci_query_symbols.input_model(query="backends"),
            _ctx(
                {
                    "agent_name": "team_planner",
                    "team_run_id": "TR2",
                    "work_item_id": "ROOT",
                    "ci_service": svc,
                    "_benchmark_root_preanchor_structure_done": True,
                    "_benchmark_root_scope_anchor_done": True,
                }
            ),
        )

    assert result.is_error
    assert "matches landed only inside already-named benchmark test files" in result.output


async def test_merge_submission_metadata_propagates_benchmark_root_scope_anchor_flag():
    original = ExecutionMetadata()
    updated = ExecutionMetadata()
    updated["_benchmark_root_scope_anchor_done"] = True

    _merge_submission_metadata(original=original, updated=updated, result_metadata=None)

    assert original["_benchmark_root_scope_anchor_done"] is True


async def test_workspace_structure_requires_exploration_reference_for_benchmark_root_planner(monkeypatch):
    svc = MagicMock()
    svc.symbol_index = MagicMock()
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR_REF" else None)
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path="pkg", max_depth=3),
            _ctx(_benchmark_root_metadata(svc, team_run_id="TR_REF")),
        )

    assert result.is_error
    assert "exploration-script" in result.output


async def test_workspace_structure_rejects_tests_directory_anchor(monkeypatch):
    svc = MagicMock()
    svc.symbol_index = MagicMock()
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR_TESTS" else None)
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path="pkg/tests", max_depth=3),
            _ctx(
                _benchmark_root_metadata(
                    svc,
                    team_run_id="TR_TESTS",
                    loaded_refs=["exploration-script"],
                )
            ),
        )

    assert result.is_error
    assert "not a benchmark test path" in result.output


async def test_ci_scope_status_requires_one_exact_anchor_on_benchmark_root_planner(monkeypatch):
    svc = MagicMock()
    svc.ledger.generation = 3
    svc.ledger.recent_entries.return_value = []
    svc.arbiter.generation = 7
    svc.arbiter.active_reservations.return_value = []
    svc.arbiter.active_edit_intents.return_value = []
    svc.arbiter.hotspots.return_value = []
    svc.symbol_index = SimpleNamespace(generation=11, _symbols={"/testbed/pkg/core.py": []})
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR_SCOPE" else None)
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        ctx = _ctx(
            _benchmark_root_metadata(
                svc,
                team_run_id="TR_SCOPE",
                loaded_refs=["exploration-script"],
            )
        )
        structure = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path="pkg", max_depth=3),
            ctx,
        )
        result = await ci_scope_status.execute(
            ci_scope_status.input_model(scope_paths=["pkg/core.py", "pkg/io"]),
            ctx,
        )

    assert not structure.is_error
    assert result.is_error
    assert "anchor exactly one existing production path" in result.output


# ---------------------------------------------------------------------------
# ci_workspace_structure
# ---------------------------------------------------------------------------

async def test_workspace_structure_no_service():
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=None):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(), _ctx()
        )
    data = json.loads(result.output)
    assert data["status"] == "unavailable"


async def test_workspace_structure_no_symbol_index():
    svc = MagicMock()
    svc.symbol_index = None
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(), _ctx_with_svc(svc)
        )
    assert "not available" in result.output


async def test_workspace_structure_with_symbol_index():
    """Uses SymbolIndex instance to list sorted file paths."""
    import threading

    # Build a fake SymbolIndex with _lock and _symbols
    class FakeSymbolIndex:
        def __init__(self):
            self._lock = threading.Lock()
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
    assert "src/a.py" in result.output
    assert "src/b.py" in result.output
    # Sorted order
    lines = result.output.strip().splitlines()
    assert lines == sorted(lines)


async def test_workspace_structure_filters_by_path():
    """path parameter filters results to matching prefix."""
    import threading

    class FakeSymbolIndex:
        def __init__(self):
            self._lock = threading.Lock()
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

    assert "src/foo/a.py" in result.output
    assert "src/bar/b.py" not in result.output
    assert "tests/c.py" not in result.output


async def test_workspace_structure_rejects_scout_listing_outside_assigned_scope():
    svc = MagicMock()
    svc.symbol_index = MagicMock()

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path="src/replacement"),
            _ctx(
                {
                    "agent_name": "scout",
                    "ci_service": svc,
                    "scope_packet": {"scope_paths": ["src/owned.py"]},
                }
            ),
        )

    assert result.is_error
    assert "scout must stay within the assigned `target_paths`" in result.output
    assert "report zero coverage" in result.output


async def test_workspace_structure_rejects_scout_root_listing_when_scope_is_concrete():
    svc = MagicMock()
    svc.symbol_index = MagicMock()

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(),
            _ctx(
                {
                    "agent_name": "scout",
                    "ci_service": svc,
                    "scope_packet": {"scope_paths": ["src/owned.py"]},
                }
            ),
        )

    assert result.is_error
    assert "may not list the workspace root" in result.output


async def test_workspace_structure_allows_immediate_parent_for_single_file_target():
    svc = MagicMock()
    svc.symbol_index = MagicMock()

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path="src/pkg"),
            _ctx(
                {
                    "agent_name": "scout",
                    "ci_service": svc,
                    "scope_packet": {"scope_paths": ["src/pkg/owned.py"]},
                }
            ),
        )

    assert not result.is_error


async def test_workspace_structure_rejects_grandparent_for_single_file_target():
    svc = MagicMock()
    svc.symbol_index = MagicMock()

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_workspace_structure.execute(
            ci_workspace_structure.input_model(path="src"),
            _ctx(
                {
                    "agent_name": "scout",
                    "ci_service": svc,
                    "scope_packet": {"scope_paths": ["src/pkg/owned.py"]},
                }
            ),
        )

    assert result.is_error
    assert "Enumerate only the exact target path or, for a single-file target, its immediate parent" in result.output


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

    assert "No files indexed" in result.output


# ---------------------------------------------------------------------------
# ci_query_symbols
# ---------------------------------------------------------------------------

async def test_query_symbols_no_service():
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=None):
        result = await ci_query_symbols.execute(
            ci_query_symbols.input_model(query="foo"), _ctx()
        )
    data = json.loads(result.output)
    assert data["status"] == "unavailable"


async def test_query_symbols_rejects_scout_caller():
    svc = MagicMock()
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_symbols.execute(
            ci_query_symbols.input_model(query="foo"),
            _ctx({"agent_name": "scout", "ci_service": svc}),
        )

    assert result.is_error
    assert "scout is read-only and may use only" in result.output
    assert "ci_query_symbols" in result.output


async def test_query_symbols_no_results():
    svc = MagicMock()
    svc.is_initialized = True
    svc.query_symbols.return_value = []

    # SymbolKind is a lazy import inside the function; patch at its source module
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbols.execute(
                ci_query_symbols.input_model(query="nonexistent"), _ctx_with_svc(svc)
            )

    assert "No symbols matching" in result.output


async def test_query_symbols_remote_fallback_on_cold_remote_workspace():
    svc = MagicMock()
    svc.is_initialized = False
    svc.workspace_root = "/testbed"
    svc.query_symbols.return_value = []

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
            result = await ci_query_symbols.execute(
                ci_query_symbols.input_model(query="generate_definitions"),
                ctx,
            )

    assert not result.is_error
    symbols = json.loads(result.output)
    assert symbols[0]["kind"] == "function"
    assert symbols[0]["name"] == "generate_definitions"
    assert symbols[0]["file"] == "/testbed/pydantic/json_schema.py"
    svc.ensure_initialized.assert_not_called()


async def test_query_symbols_remote_python_fallback_when_rg_unavailable():
    svc = MagicMock()
    svc.is_initialized = False
    svc.workspace_root = "/testbed"
    svc.query_symbols.return_value = []

    sandbox = MagicMock()

    async def _exec(command: str, timeout: int = 30):
        if command.startswith("python -c "):
            return MagicMock(
                exit_code=0,
                result=json.dumps(
                    [
                        {
                            "file": "/testbed/tests/test_discriminated_union.py",
                            "line": 1703,
                            "kind": "function",
                            "snippet": (
                                "def "
                                "test_presence_of_discriminator_when_generating_type_adaptor_json_schema_definitions():"
                            ),
                        }
                    ]
                ),
            )
        return MagicMock(exit_code=127, result="")

    sandbox.process.exec = AsyncMock(side_effect=_exec)

    ctx = _ctx_with_svc(svc)
    ctx.metadata["daytona_sandbox"] = sandbox
    ctx.metadata["daytona_cwd"] = "/testbed"

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbols.execute(
                ci_query_symbols.input_model(
                    query="test_presence_of_discriminator_when_generating_type_adaptor_json_schema_definitions"
                ),
                ctx,
            )

    assert not result.is_error
    symbols = json.loads(result.output)
    assert symbols[0]["name"] == (
        "test_presence_of_discriminator_when_generating_type_adaptor_json_schema_definitions"
    )
    assert symbols[0]["kind"] == "function"
    assert symbols[0]["file"] == "/testbed/tests/test_discriminated_union.py"
    assert sandbox.process.exec.await_count >= 2


async def test_query_symbols_prefers_code_symbol_over_doc_text_match():
    svc = MagicMock()
    svc.is_initialized = False
    svc.workspace_root = "/testbed"
    svc.query_symbols.return_value = []

    sandbox = MagicMock()

    async def _exec(command: str, timeout: int = 30):
        if command.startswith("python -c "):
            return MagicMock(
                exit_code=0,
                result=json.dumps(
                    [
                        {
                            "file": "/testbed/pydantic/json_schema.py",
                            "line": 338,
                            "kind": "function",
                            "snippet": "    def generate_definitions(self, inputs):",
                        }
                    ]
                ),
            )
        return MagicMock(
            exit_code=0,
            result="/testbed/HISTORY.md:633:generate_definitions handles aliases better now\n",
        )

    sandbox.process.exec = AsyncMock(side_effect=_exec)

    ctx = _ctx_with_svc(svc)
    ctx.metadata["daytona_sandbox"] = sandbox
    ctx.metadata["daytona_cwd"] = "/testbed"

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbols.execute(
                ci_query_symbols.input_model(query="generate_definitions"),
                ctx,
            )

    assert not result.is_error
    symbols = json.loads(result.output)
    assert symbols[0]["file"] == "/testbed/pydantic/json_schema.py"
    assert symbols[0]["kind"] == "function"


async def test_query_symbols_local_workspace_fallback_finds_class(tmp_path):
    source = tmp_path / "pydantic" / "type_adapter.py"
    source.parent.mkdir()
    source.write_text(
        "class TypeAdapter:\n"
        "    pass\n",
        encoding="utf-8",
    )

    svc = MagicMock()
    svc.is_initialized = False
    svc.workspace_root = str(tmp_path)
    svc.query_symbols.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbols.execute(
                ci_query_symbols.input_model(query="TypeAdapter"),
                _ctx_with_svc(svc),
            )

    assert not result.is_error
    symbols = json.loads(result.output)
    assert symbols[0]["name"] == "TypeAdapter"
    assert symbols[0]["kind"] == "class"
    assert symbols[0]["file"].endswith("type_adapter.py")


async def test_query_symbols_local_workspace_fallback_finds_partial_function(tmp_path):
    source = tmp_path / "pydantic" / "json_schema.py"
    source.parent.mkdir()
    source.write_text(
        "def _extract_discriminator(schema):\n"
        "    return schema\n",
        encoding="utf-8",
    )

    svc = MagicMock()
    svc.is_initialized = False
    svc.workspace_root = str(tmp_path)
    svc.query_symbols.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        with patch("code_intelligence.types.SymbolKind"):
            result = await ci_query_symbols.execute(
                ci_query_symbols.input_model(query="discriminator", kind="function"),
                _ctx_with_svc(svc),
            )

    assert not result.is_error
    symbols = json.loads(result.output)
    assert symbols[0]["name"] == "_extract_discriminator"
    assert symbols[0]["kind"] == "function"
    assert symbols[0]["file"].endswith("json_schema.py")


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
            result = await ci_query_symbols.execute(
                ci_query_symbols.input_model(query="my_func"), _ctx_with_svc(svc)
            )

    assert not result.is_error
    symbols = json.loads(result.output)
    assert len(symbols) == 1
    assert symbols[0]["name"] == "my_func"
    assert symbols[0]["file"] == "src/mod.py"
    assert symbols[0]["line"] == 10


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
            result = await ci_query_symbols.execute(
                ci_query_symbols.input_model(query="fresh_symbol"),
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
            result = await ci_query_symbols.execute(
                ci_query_symbols.input_model(query="", kind="function"),
                _ctx_with_svc(svc),
            )

    symbols = json.loads(result.output)
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
            result = await ci_query_symbols.execute(
                ci_query_symbols.input_model(query="anything", kind="bogus"),
                _ctx_with_svc(svc),
            )

    # No filter applied → symbol still in results
    symbols = json.loads(result.output)
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
            result = await ci_query_symbols.execute(
                ci_query_symbols.input_model(query="bare_sym"), _ctx_with_svc(svc)
            )

    assert not result.is_error
    symbols = json.loads(result.output)
    assert symbols[0]["name"] == "bare_sym"


# ---------------------------------------------------------------------------
# ci_query_references
# ---------------------------------------------------------------------------

async def test_query_references_no_service():
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=None):
        result = await ci_query_references.execute(
            ci_query_references.input_model(file_path="/f.py", symbol="foo"), _ctx()
        )
    data = json.loads(result.output)
    assert data["status"] == "unavailable"


async def test_query_references_no_results():
    svc = MagicMock()
    svc.find_references.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_references.execute(
            ci_query_references.input_model(file_path="/f.py", symbol="missing"),
            _ctx_with_svc(svc),
        )

    assert "No references found" in result.output


async def test_query_references_reports_cold_state_when_ci_not_ready():
    svc = MagicMock()
    svc.is_initialized = False
    svc.workspace_root = "/missing"
    svc.find_references.return_value = []
    svc.lsp_client.connected = False

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_references.execute(
            ci_query_references.input_model(file_path="/f.py", symbol="missing"),
            _ctx_with_svc(svc),
        )

    data = json.loads(result.output)
    assert data["status"] == "cold"
    assert data["lsp_connected"] is False


async def test_query_references_returns_results():
    ref = MagicMock()
    ref.file_path = "src/user.py"
    ref.line = 20
    ref.text = "result = foo(x)"

    svc = MagicMock()
    svc.find_references.return_value = [ref]

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_references.execute(
            ci_query_references.input_model(file_path="/f.py", symbol="foo", line=5, character=3),
            _ctx_with_svc(svc),
        )

    assert not result.is_error
    refs = json.loads(result.output)
    assert len(refs) == 1
    assert refs[0]["file"] == "src/user.py"
    assert refs[0]["line"] == 20
    assert refs[0]["text"] == "result = foo(x)"
    svc.find_references.assert_called_once_with("/f.py", "foo", 5, 3)


async def test_query_references_truncates_at_50_and_shows_total():
    """More than 50 results are truncated; total count shown in output."""
    refs = []
    for i in range(60):
        r = MagicMock()
        r.file_path = f"file{i}.py"
        r.line = i
        r.text = f"ref {i}"
        refs.append(r)

    svc = MagicMock()
    svc.find_references.return_value = refs

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_query_references.execute(
            ci_query_references.input_model(file_path="/f.py", symbol="big"),
            _ctx_with_svc(svc),
        )

    # Output has two parts: JSON array + trailing count note
    parts = result.output.split("\n\n")
    shown = json.loads(parts[0])
    assert len(shown) == 50
    assert "60 total" in parts[1]


# ---------------------------------------------------------------------------
# ci_edit_hotspots
# ---------------------------------------------------------------------------

async def test_edit_hotspots_no_service():
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=None):
        result = await ci_edit_hotspots.execute(
            ci_edit_hotspots.input_model(), _ctx()
        )
    data = json.loads(result.output)
    assert data["status"] == "unavailable"


async def test_edit_hotspots_no_arbiter():
    svc = MagicMock()
    svc.arbiter = None

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_edit_hotspots.execute(
            ci_edit_hotspots.input_model(), _ctx_with_svc(svc)
        )

    assert "Arbiter not available" in result.output


async def test_edit_hotspots_no_results():
    svc = MagicMock()
    svc.arbiter.hotspots.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_edit_hotspots.execute(
            ci_edit_hotspots.input_model(), _ctx_with_svc(svc)
        )

    assert "No edit hotspots" in result.output


async def test_edit_hotspots_returns_results():
    svc = MagicMock()
    svc.arbiter.hotspots.return_value = [
        ("src/hot.py", 15),
        ("src/warm.py", 7),
    ]

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_edit_hotspots.execute(
            ci_edit_hotspots.input_model(limit=5), _ctx_with_svc(svc)
        )

    assert not result.is_error
    items = json.loads(result.output)
    assert len(items) == 2
    assert items[0]["file"] == "src/hot.py"
    assert items[0]["edit_count"] == 15
    svc.arbiter.hotspots.assert_called_once_with(limit=5)


# ---------------------------------------------------------------------------
# ci_recent_changes
# ---------------------------------------------------------------------------

async def test_recent_changes_no_service():
    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=None):
        result = await ci_recent_changes.execute(
            ci_recent_changes.input_model(), _ctx()
        )
    data = json.loads(result.output)
    assert data["status"] == "unavailable"


async def test_recent_changes_no_ledger():
    svc = MagicMock()
    svc.ledger = None

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_recent_changes.execute(
            ci_recent_changes.input_model(), _ctx_with_svc(svc)
        )

    assert "Ledger not available" in result.output


async def test_recent_changes_no_files():
    svc = MagicMock()
    svc.ledger.recent_files.return_value = []

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_recent_changes.execute(
            ci_recent_changes.input_model(seconds=30.0), _ctx_with_svc(svc)
        )

    assert "No files changed" in result.output
    assert "30.0s" in result.output


async def test_recent_changes_returns_files():
    svc = MagicMock()
    svc.ledger.recent_files.return_value = ["src/a.py", "src/b.py"]

    with patch("tools.ci_toolkit.query_tools.get_ci_service", return_value=svc):
        result = await ci_recent_changes.execute(
            ci_recent_changes.input_model(seconds=120.0), _ctx_with_svc(svc)
        )

    assert not result.is_error
    files = json.loads(result.output)
    assert "src/a.py" in files
    assert "src/b.py" in files
    svc.ledger.recent_files.assert_called_once_with(seconds=120.0)
