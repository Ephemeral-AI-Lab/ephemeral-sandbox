"""Tests for tools.sandbox_toolkit.edit_file."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

from sandbox.api.models import EditFileResult
from tools.core.base import ToolExecutionContextService
from tools.core.safe_execution import run_tool_safely
from tools.sandbox_toolkit.edit_file import edit_file


class _EditApi:
    def __init__(self, result: EditFileResult | None = None) -> None:
        self.result = result or EditFileResult(
            success=True,
            changed_paths=("/ws/file.py",),
            applied_edits=1,
        )
        self.calls: list[tuple[str, Any]] = []

    async def edit_file(self, sandbox_id: str, request: Any) -> EditFileResult:
        self.calls.append((sandbox_id, request))
        return self.result


def _ctx(services=None) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"), services=services or {})


def _ctx_with_api(api: _EditApi, **services: Any) -> ToolExecutionContextService:
    return _ctx(
        {
            "sandbox_id": "sb-1",
            "sandbox_api": api,
            "repo_root": "/ws",
            **services,
        }
    )


def _run(args: dict, ctx: ToolExecutionContextService):
    return asyncio.run(run_tool_safely(edit_file, args, context=ctx))


def test_missing_sandbox_api_returns_error() -> None:
    ctx = _ctx({"sandbox_id": "sb-1", "repo_root": "/ws"})

    result = _run({"file_path": "/ws/f.py", "old_text": "a", "new_text": "b"}, ctx)

    assert result.is_error
    assert result.metadata.get("sandbox_api_required") is True


def test_edits_schema_is_not_exposed() -> None:
    schema = edit_file.to_api_schema()["input_schema"]

    assert "edits" not in schema.get("properties", {})


def test_extra_edits_input_does_not_create_batch_edit() -> None:
    api = _EditApi()
    ctx = _ctx_with_api(api)

    result = _run(
        {
            "file_path": "/ws/f.py",
            "old_text": "a",
            "new_text": "b",
            "edits": [{"strategy": "search_replace", "search": "x", "replace": "y"}],
        },
        ctx,
    )

    assert result.is_error
    assert "Invalid input for edit_file" in result.output
    assert "edits" in result.output
    assert api.calls == []


def test_missing_old_text_is_rejected() -> None:
    api = _EditApi()
    ctx = _ctx_with_api(api)

    result = _run({"file_path": "/ws/f.py"}, ctx)

    assert result.is_error
    assert "Provide `old_text`" in result.output
    assert api.calls == []


def test_single_old_new_text_edit_succeeds() -> None:
    api = _EditApi(EditFileResult(success=True, changed_paths=("/ws/file.py",), applied_edits=1))
    ctx = _ctx_with_api(api)

    result = _run(
        {"file_path": "/ws/file.py", "old_text": "foo", "new_text": "bar"},
        ctx,
    )

    assert not result.is_error
    assert len(api.calls) == 1
    sandbox_id, request = api.calls[0]
    assert sandbox_id == "sb-1"
    assert request.path == "/ws/file.py"
    assert request.edits[0].old_text == "foo"
    assert request.edits[0].new_text == "bar"
    payload = json.loads(result.output)
    assert payload["status"] == "edited"
    assert payload["changed_paths"] == ["/ws/file.py"]
    assert payload["conflict_reason"] is None
    assert payload["applied_edits"] == 1
    assert "timings" not in payload
    assert "warnings" not in payload


def test_aborted_version_is_surfaced_to_caller() -> None:
    api = _EditApi(
        EditFileResult(
            success=False,
            changed_paths=("/ws/file.py",),
            conflict_reason="drift",
        )
    )
    ctx = _ctx_with_api(api)

    result = _run(
        {"file_path": "/ws/file.py", "old_text": "a", "new_text": "b"},
        ctx,
    )

    assert result.is_error
    payload = json.loads(result.output)
    assert payload["status"] == "aborted_version"
    assert payload["changed_paths"] == ["/ws/file.py"]
    assert payload["conflict_reason"] == "drift"
    assert payload["conflict_file"] == "/ws/file.py"
    assert "warnings" not in payload


def test_patch_failed_in_single_edit_mode_uses_structured_payload() -> None:
    api = _EditApi(
        EditFileResult(
            success=False,
            changed_paths=("/ws/file.py",),
            conflict_reason="patch_failed",
        )
    )
    ctx = _ctx_with_api(api)

    result = _run(
        {"file_path": "/ws/file.py", "old_text": "missing", "new_text": "x"},
        ctx,
    )

    assert result.is_error
    payload = json.loads(result.output)
    assert payload["status"] == "failed"
    assert payload["conflict_reason"] == "patch_failed"


def test_actor_and_description_flow_through_to_api() -> None:
    api = _EditApi()
    ctx = _ctx_with_api(api, agent_run_id="run-42")

    _run(
        {
            "file_path": "/ws/file.py",
            "old_text": "a",
            "new_text": "b",
            "description": "tidy imports",
        },
        ctx,
    )

    request = api.calls[0][1]
    assert request.actor.agent_id == "run-42"
    assert request.description == "tidy imports"
