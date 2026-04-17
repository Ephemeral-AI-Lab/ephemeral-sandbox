"""Tests for the ci_rename_symbol tool."""

from __future__ import annotations

import asyncio
import hashlib
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

from code_intelligence.types import (
    SemanticFileChange,
    SemanticRenamePlan,
    SymbolInfo,
    SymbolKind,
)
from tools.ci_toolkit.rename_tool import ci_rename_symbol
from tools.core.base import ToolExecutionContext


def _ctx(metadata=None) -> ToolExecutionContext:
    metadata = dict(metadata or {})
    if "ci_service" in metadata and "daytona_sandbox" not in metadata:
        metadata["daytona_sandbox"] = SimpleNamespace()
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _hash(content: str) -> str:
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]


def _change(path: str, *, base: str, final: str) -> SemanticFileChange:
    return SemanticFileChange(
        file_path=path,
        base_content=base,
        base_hash=_hash(base),
        final_content=final,
    )


def _plan(changes) -> SemanticRenamePlan:
    return SemanticRenamePlan(
        new_name="bar",
        origin=("/ws/a.py", 1, 0),
        changes=tuple(changes),
    )


def _rename_response(plan: SemanticRenamePlan | None, *, ok: bool = True, error: str = ""):
    if not ok:
        return SimpleNamespace(
            result=json.dumps({"ok": False, "status": "failed", "error": error}),
            exit_code=1,
        )
    return SimpleNamespace(
        result=json.dumps(
            {
                "ok": True,
                "status": "renamed",
                "files": [
                    {"file_path": c.file_path, "status": "renamed"}
                    for c in (plan.changes if plan else ())
                ],
            }
        ),
        exit_code=0,
    )


def _make_svc(*, plan: SemanticRenamePlan | None):
    svc = MagicMock()
    svc.symbol_index.ensure_built.return_value = True
    svc.symbol_index.find.return_value = [
        SymbolInfo(
            name="foo",
            kind=SymbolKind.FUNCTION,
            file_path="/ws/a.py",
            line=3,
            character=4,
        )
    ]
    if plan is None:
        svc.rename_symbol_plan.return_value = SemanticRenamePlan(
            new_name="bar", origin=("", 0, 0), changes=(),
        )
    else:
        svc.rename_symbol_plan.return_value = plan
    svc.preview_rename_symbol_plan.return_value = svc.rename_symbol_plan.return_value
    svc.exec_process_operation = AsyncMock(return_value=_rename_response(plan))
    return svc


def _run(tool_input, ctx):
    return asyncio.run(
        ci_rename_symbol.execute(ci_rename_symbol.input_model(**tool_input), ctx),
    )


# -- Validation & short-circuits --------------------------------------------


def test_no_service_returns_error():
    result = _run(
        {"symbol": "foo", "new_name": "bar"},
        _ctx(),
    )
    assert result.is_error
    assert "LSP rename not available" in result.output


def test_invalid_new_name_rejected():
    svc = _make_svc(plan=None)
    result = _run(
        {"symbol": "foo", "new_name": "1bad"},
        _ctx({"ci_service": svc}),
    )
    assert result.is_error
    assert "Invalid identifier" in result.output
    svc.rename_symbol_plan.assert_not_called()


def test_no_changes_returns_status_no_changes():
    svc = _make_svc(plan=_plan([]))
    result = _run(
        {"symbol": "foo", "new_name": "bar"},
        _ctx({"ci_service": svc}),
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["status"] == "no_changes"
    assert data["files"] == []
    svc.exec_process_operation.assert_not_called()


# -- Happy path -------------------------------------------------------------


def test_rename_runs_via_single_process_operation():
    changes = [
        _change("/ws/a.py", base="old_a", final="new_a"),
        _change("/ws/b.py", base="old_b", final="new_b"),
    ]
    plan = _plan(changes)
    svc = _make_svc(plan=plan)
    result = _run(
        {"symbol": "foo", "new_name": "bar"},
        _ctx({"ci_service": svc}),
    )
    assert not result.is_error, result.output
    data = json.loads(result.output)
    assert data["status"] == "renamed"
    assert {f["file_path"] for f in data["files"]} == {"/ws/a.py", "/ws/b.py"}
    svc.exec_process_operation.assert_awaited_once()
    kwargs = svc.exec_process_operation.await_args.kwargs
    assert kwargs["edit_type"] == "rename"


def test_sandbox_rename_uses_one_wrapped_command():
    changes = [
        _change("/ws/a.py", base="old_a", final="new_a"),
        _change("/ws/b.py", base="old_b", final="new_b"),
    ]
    plan = _plan(changes)
    svc = _make_svc(plan=plan)
    response = SimpleNamespace(
        result=json.dumps(
            {
                "ok": True,
                "status": "renamed",
                "files": [
                    {"file_path": "/ws/a.py", "status": "renamed"},
                    {"file_path": "/ws/b.py", "status": "renamed"},
                ],
            }
        ),
        exit_code=0,
    )
    ctx = _ctx({"ci_service": svc, "daytona_sandbox": object()})

    with patch(
        "tools.ci_toolkit.rename_tool.exec_ci_process_operation",
        new=AsyncMock(return_value=response),
    ) as exec_op:
        result = _run(
            {"symbol": "foo", "new_name": "bar"},
            ctx,
        )

    assert not result.is_error, result.output
    data = json.loads(result.output)
    assert data["status"] == "renamed"
    exec_op.assert_awaited_once()
    command = exec_op.await_args.args[2]
    assert "bash" in command
    assert "python3 -c" in command


# -- Dry run uses plan without writing -------------------------------------


def test_dry_run_reports_files_without_diff_or_process_exec():
    changes = [_change("/ws/a.py", base="def foo():\n    pass\n", final="def bar():\n    pass\n")]
    svc = _make_svc(plan=_plan(changes))
    result = _run(
        {"symbol": "foo", "new_name": "bar", "dry_run": True},
        _ctx({"ci_service": svc}),
    )
    assert not result.is_error
    data = json.loads(result.output)
    assert data["status"] == "dry_run"
    assert len(data["files"]) == 1
    assert "diff" not in data["files"][0]
    svc.preview_rename_symbol_plan.assert_called_once()
    svc.rename_symbol_plan.assert_not_called()
    svc.exec_process_operation.assert_not_called()


# -- Process failure -------------------------------------------------------


def test_process_failure_surfaces_failed_status():
    changes = [
        _change("/ws/a.py", base="old_a", final="new_a"),
        _change("/ws/b.py", base="old_b", final="new_b"),
        _change("/ws/c.py", base="old_c", final="new_c"),
    ]
    plan = _plan(changes)
    svc = _make_svc(plan=plan)
    svc.exec_process_operation = AsyncMock(
        return_value=_rename_response(plan, ok=False, error="rename process failed")
    )
    result = _run(
        {"symbol": "foo", "new_name": "bar"},
        _ctx({"ci_service": svc}),
    )
    assert result.is_error
    data = json.loads(result.output)
    assert data["status"] == "failed"
    assert all(f["status"] == "failed" for f in data["files"])
    assert "rename process failed" in data["message"]
    assert result.metadata["success_count"] == 0


# -- Name-based resolution --------------------------------------------------


def _sym(name, *, kind=SymbolKind.FUNCTION, file_path="/ws/a.py", line=3,
         container="", character=4, signature=""):
    return SymbolInfo(
        name=name, kind=kind, file_path=file_path, line=line,
        character=character, container=container, signature=signature,
    )


def _make_facade_svc(
    *,
    matches,
    plan: SemanticRenamePlan | None = None,
):
    svc = MagicMock()
    svc.symbol_index.ensure_built.return_value = True
    svc.symbol_index.find.return_value = list(matches)
    if plan is not None:
        svc.rename_symbol_plan.return_value = plan
    else:
        svc.rename_symbol_plan.return_value = SemanticRenamePlan(
            new_name="bar", origin=("", 0, 0), changes=(),
        )
    svc.preview_rename_symbol_plan.return_value = svc.rename_symbol_plan.return_value
    svc.exec_process_operation = AsyncMock(return_value=_rename_response(plan))
    return svc


def _run_facade(tool_input, ctx):
    return asyncio.run(
        ci_rename_symbol.execute(ci_rename_symbol.input_model(**tool_input), ctx),
    )


def test_facade_resolves_unique_symbol_and_delegates():
    match = _sym("foo", line=10, character=4, file_path="/ws/a.py")
    changes = [_change("/ws/a.py", base="def foo():\n    pass\n", final="def bar():\n    pass\n")]
    plan = _plan(changes)
    svc = _make_facade_svc(matches=[match], plan=plan)
    result = _run_facade(
        {"symbol": "foo", "new_name": "bar"},
        _ctx({"ci_service": svc}),
    )
    assert not result.is_error, result.output
    data = json.loads(result.output)
    assert data["status"] == "renamed"
    svc.rename_symbol_plan.assert_called_once()
    # The facade must pivot on the resolved symbol's location.
    call = svc.rename_symbol_plan.call_args
    assert call.args[1] == 10  # line
    assert call.args[2] == 4   # character


def test_facade_uses_name_column_for_indexed_python_declarations():
    match = _sym("foo", line=10, character=0, file_path="/ws/a.py", signature="def foo()")
    changes = [_change("/ws/a.py", base="def foo():\n    pass\n", final="def bar():\n    pass\n")]
    plan = _plan(changes)
    svc = _make_facade_svc(matches=[match], plan=plan)

    result = _run_facade(
        {"symbol": "foo", "new_name": "bar"},
        _ctx({"ci_service": svc}),
    )

    assert not result.is_error, result.output
    call = svc.rename_symbol_plan.call_args
    assert call.args[1] == 10
    assert call.args[2] == 4


def test_facade_returns_ambiguous_for_multiple_matches():
    matches = [
        _sym("Client", kind=SymbolKind.CLASS, file_path="/ws/a.py"),
        _sym("Client", kind=SymbolKind.CLASS, file_path="/ws/b.py"),
    ]
    svc = _make_facade_svc(matches=matches)
    result = _run_facade(
        {"symbol": "Client", "new_name": "Session"},
        _ctx({"ci_service": svc}),
    )
    assert result.is_error
    data = json.loads(result.output)
    assert data["status"] == "ambiguous"
    assert len(data["candidates"]) == 2
    assert {c["file_path"] for c in data["candidates"]} == {"/ws/a.py", "/ws/b.py"}
    svc.rename_symbol_plan.assert_not_called()


def test_facade_disambiguates_by_dotted_parent():
    matches = [
        _sym("bar", container="", file_path="/ws/a.py"),  # module-level
        _sym("bar", container="Foo", file_path="/ws/b.py"),  # method
    ]
    changes = [_change("/ws/b.py", base="old", final="new")]
    plan = _plan(changes)
    svc = _make_facade_svc(matches=matches, plan=plan)
    result = _run_facade(
        {"symbol": "Foo.bar", "new_name": "baz"},
        _ctx({"ci_service": svc}),
    )
    assert not result.is_error, result.output
    data = json.loads(result.output)
    assert data["status"] == "renamed"


def test_facade_disambiguates_by_file_hint():
    matches = [
        _sym("handle", file_path="/ws/frontend/x.py"),
        _sym("handle", file_path="/ws/backend/x.py"),
    ]
    changes = [_change("/ws/backend/x.py", base="old", final="new")]
    plan = _plan(changes)
    svc = _make_facade_svc(matches=matches, plan=plan)
    result = _run_facade(
        {"symbol": "handle", "new_name": "process", "file_hint": "backend/"},
        _ctx({"ci_service": svc}),
    )
    assert not result.is_error, result.output
    data = json.loads(result.output)
    assert data["status"] == "renamed"


def test_facade_disambiguates_by_kind():
    matches = [
        _sym("thing", kind=SymbolKind.FUNCTION, file_path="/ws/a.py"),
        _sym("thing", kind=SymbolKind.CLASS, file_path="/ws/b.py"),
    ]
    # ensure only class match survives — symbol_index.find with kind filter does that
    svc = _make_facade_svc(matches=[matches[1]])
    changes = [_change("/ws/b.py", base="old", final="new")]
    plan = _plan(changes)
    svc.rename_symbol_plan.return_value = plan
    svc.exec_process_operation = AsyncMock(return_value=_rename_response(plan))
    result = _run_facade(
        {"symbol": "thing", "new_name": "thang", "kind": "class"},
        _ctx({"ci_service": svc}),
    )
    assert not result.is_error, result.output
    # Ensure the kind filter was forwarded.
    call = svc.symbol_index.find.call_args
    assert call.kwargs.get("kind") == SymbolKind.CLASS


def test_facade_no_match_returns_helpful_error():
    svc = _make_facade_svc(matches=[])
    result = _run_facade(
        {"symbol": "typo_name", "new_name": "fixed"},
        _ctx({"ci_service": svc}),
    )
    assert result.is_error
    data = json.loads(result.output)
    assert data["status"] == "no_match"
    assert "ci_query_symbol" in data["message"]
    svc.rename_symbol_plan.assert_not_called()


def test_facade_invalid_new_name_rejected_before_resolution():
    svc = _make_facade_svc(matches=[])
    result = _run_facade(
        {"symbol": "foo", "new_name": "1bad"},
        _ctx({"ci_service": svc}),
    )
    assert result.is_error
    assert "Invalid identifier" in result.output
    svc.symbol_index.find.assert_not_called()
