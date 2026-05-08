"""Public sandbox file-edit verb."""

from __future__ import annotations

from sandbox.api.tool._payload import (
    conflict_from_payload,
    int_from_payload,
    paths_from_payload,
    timings_from_payload,
)
from sandbox.contract import EditFileRequest, EditFileResult
from sandbox.host.daemon_client import call_daemon_api


async def edit_file(sandbox_id: str, request: EditFileRequest) -> EditFileResult:
    """Apply search/replace edits through sandbox-local OCC."""
    raw = await call_daemon_api(
        sandbox_id,
        "api.edit_file",
        {
            "path": request.path,
            "edits": [
                {"old_text": edit.old_text, "new_text": edit.new_text}
                for edit in request.edits
            ],
            "actor_id": request.caller.agent_id,
            "description": request.description or f"edit {request.path}",
        },
        timeout=60,
    )
    conflict = conflict_from_payload(raw.get("conflict"))
    return EditFileResult(
        success=bool(raw.get("success", False)),
        changed_paths=paths_from_payload(raw.get("changed_paths")),
        applied_edits=int_from_payload(raw.get("applied_edits"), default=0),
        status=str(raw.get("status", "")),
        conflict=conflict,
        conflict_reason=(
            str(raw.get("conflict_reason"))
            if raw.get("conflict_reason") is not None
            else None
        ),
        timings=timings_from_payload(raw.get("timings")),
    )


__all__ = ["edit_file"]
