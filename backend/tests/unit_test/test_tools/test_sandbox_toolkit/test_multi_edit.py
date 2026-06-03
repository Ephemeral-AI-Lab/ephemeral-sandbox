"""Tests for tools.sandbox.multi_edit."""

from __future__ import annotations

import asyncio
import importlib
import json
from pathlib import Path
from typing import Any

from sandbox.api import EditFileResult
from sandbox._shared.edit_apply import SearchReplaceError, apply_search_replace
from tools._framework.core.base import ToolExecutionContextService
from tools.sandbox.multi_edit import multi_edit

from ._helpers import run_tool_safely

multi_edit_module = importlib.import_module("tools.sandbox.multi_edit.multi_edit")


class _CannedApi:
    def __init__(self, result: EditFileResult | None = None) -> None:
        self.result = result or EditFileResult(
            success=True, changed_paths=("/ws/file.py",), applied_edits=1
        )
        self.calls: list[tuple[str, Any]] = []

    async def edit_file(self, sandbox_id: str, request: Any) -> EditFileResult:
        self.calls.append((sandbox_id, request))
        return self.result


class _ApplyingApi:
    """Mimic the daemon: apply edits sequentially against evolving content,
    all-or-nothing."""

    def __init__(self, content: str) -> None:
        self.content = content
        self.calls: list[tuple[str, Any]] = []

    async def edit_file(self, sandbox_id: str, request: Any) -> EditFileResult:
        self.calls.append((sandbox_id, request))
        current = self.content
        try:
            for edit in request.edits:
                current = apply_search_replace(
                    current,
                    edit.old_text,
                    edit.new_text,
                    replace_all=edit.replace_all,
                )
        except SearchReplaceError as exc:
            # All-or-nothing: nothing is committed when any edit fails.
            return EditFileResult(
                success=False,
                changed_paths=(request.path,),
                status="aborted_overlap",
                conflict_reason=exc.message,
                applied_edits=0,
            )
        self.content = current
        return EditFileResult(
            success=True,
            changed_paths=(request.path,),
            applied_edits=len(request.edits),
        )


def _ctx_with_api(api: Any, **services: Any) -> ToolExecutionContextService:
    return ToolExecutionContextService(
        cwd=Path("/tmp"),
        services={
            "sandbox_id": "sb-1",
            "sandbox_api": api,
            "repo_root": "/ws",
            **services,
        },
    )


def _run(args: dict, ctx: ToolExecutionContextService):
    return asyncio.run(run_tool_safely(multi_edit, args, context=ctx))


def test_empty_edits_returns_typed_error() -> None:
    api = _CannedApi()
    ctx = _ctx_with_api(api)

    result = _run({"file_path": "/ws/f.py", "edits": []}, ctx)

    assert result.is_error
    assert "at least one edit" in result.output
    assert api.calls == []


def test_missing_sandbox_id_returns_error() -> None:
    ctx = ToolExecutionContextService(cwd=Path("/tmp"), services={"repo_root": "/ws"})

    result = _run(
        {"file_path": "/ws/f.py", "edits": [{"old_text": "a", "new_text": "b"}]}, ctx
    )

    assert result.is_error
    assert result.metadata.get("sandbox_required") is True


def test_edits_flow_through_in_order_with_per_edit_replace_all(
    monkeypatch,
) -> None:
    api = _CannedApi()
    ctx = _ctx_with_api(api)
    monkeypatch.setattr(multi_edit_module, "sandbox_api", api)

    _run(
        {
            "file_path": "/ws/file.py",
            "edits": [
                {"old_text": "a", "new_text": "b"},
                {"old_text": "c", "new_text": "d", "replace_all": True},
            ],
        },
        ctx,
    )

    request = api.calls[0][1]
    assert [(e.old_text, e.new_text, e.replace_all) for e in request.edits] == [
        ("a", "b", False),
        ("c", "d", True),
    ]


def test_applied_edits_is_edit_count(monkeypatch) -> None:
    # Canned API reports applied_edits=1, but the tool reports len(edits) (D4).
    api = _CannedApi(
        EditFileResult(success=True, changed_paths=("/ws/file.py",), applied_edits=1)
    )
    ctx = _ctx_with_api(api)
    monkeypatch.setattr(multi_edit_module, "sandbox_api", api)

    result = _run(
        {
            "file_path": "/ws/file.py",
            "edits": [
                {"old_text": "a", "new_text": "b"},
                {"old_text": "c", "new_text": "d"},
                {"old_text": "e", "new_text": "f"},
            ],
        },
        ctx,
    )

    payload = json.loads(result.output)
    assert payload["applied_edits"] == 3


def test_sequential_apply_against_evolving_content(monkeypatch) -> None:
    api = _ApplyingApi("alpha alpha\nbeta\n")
    ctx = _ctx_with_api(api)
    monkeypatch.setattr(multi_edit_module, "sandbox_api", api)

    result = _run(
        {
            "file_path": "/ws/file.py",
            "edits": [
                {"old_text": "alpha", "new_text": "ALPHA", "replace_all": True},
                {"old_text": "ALPHA ALPHA", "new_text": "merged"},
            ],
        },
        ctx,
    )

    assert not result.is_error
    assert api.content == "merged\nbeta\n"


def test_all_or_nothing_aborts_on_failed_edit(monkeypatch) -> None:
    api = _ApplyingApi("hello world\n")
    ctx = _ctx_with_api(api)
    monkeypatch.setattr(multi_edit_module, "sandbox_api", api)

    result = _run(
        {
            "file_path": "/ws/file.py",
            "edits": [
                {"old_text": "hello", "new_text": "hi"},
                {"old_text": "missing-anchor", "new_text": "x"},
            ],
        },
        ctx,
    )

    assert result.is_error
    # Nothing committed: the first (valid) edit did not land.
    assert api.content == "hello world\n"
    payload = json.loads(result.output)
    assert payload["conflict_reason"] == "anchor not found"
