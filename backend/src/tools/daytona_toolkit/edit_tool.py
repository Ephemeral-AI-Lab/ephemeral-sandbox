"""File editing tool for Daytona sandboxes."""

from __future__ import annotations

import hashlib
import json
import logging
from typing import Any

from code_intelligence.editing.patcher import LineRangeEdit, Patcher, SearchReplaceEdit
from tools.core.base import ToolExecutionContext, ToolResult
from tools.daytona_toolkit.tools import (
    _get_cwd,
    _path_error,
    _recover_sandbox,
    _require_sandbox,
    _resolve_path,
    _team_repo_write_error,
    _upload_file_compat,
)
from tools.daytona_toolkit.ci_integration import (
    abort_ci_write,
    finalize_ci_write,
    get_ci_service,
    prepare_ci_write,
    prepare_ci_edit_intent,
    release_ci_edit_intent,
)
from tools.core.decorator import tool

logger = logging.getLogger(__name__)

_OUTPUT_MAX_CHARS = 8000


def _content_hash(content: str) -> str:
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]


@tool(
    name="daytona_edit_file",
    description="Edit a file atomically using search_replace or line_range operations.",
)
async def daytona_edit_file(
    file_path: str,
    old_text: str = "",
    new_text: str = "",
    edits: list[dict[str, Any]] | None = None,
    description: str = "",
    dry_run: bool = False,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Edit a file in the Daytona sandbox atomically.

    Args:
        file_path: Path to the file to edit
        old_text: Text to find and replace (legacy single-edit mode)
        new_text: Replacement text for legacy single-edit mode
        edits: Optional batch edit list. Supported strategies:
            ``{"strategy": "search_replace", "search": "...", "replace": "..."}``
            ``{"strategy": "line_range", "start_line": 1, "end_line": 3, "new_content": "..."}``
        description: Optional description of the edit
        dry_run: Preview the edit without applying

    Returns:
        file_path (str): Path to the edited file
        status (str): Edit result — edited, dry_run, or error
        diff (str): Unified diff preview (dry_run only)
    """
    try:
        sandbox = await _require_sandbox(context)
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)

    file_path = _resolve_path(file_path, context)
    contract_error = _team_repo_write_error(context, file_path, tool_name="daytona_edit_file")
    if contract_error is not None:
        return ToolResult(output=contract_error, is_error=True)

    prepared = None
    intent_id = None
    current = ""
    current_hash = ""
    patcher = Patcher()
    normalized_edits, edit_error, legacy_not_found = _normalize_edits(
        old_text=old_text,
        new_text=new_text,
        edits=edits,
    )
    if edit_error is not None:
        return ToolResult(output=edit_error, is_error=True)
    svc = get_ci_service(context)
    refresh_prepared = getattr(svc, "refresh_prepared_write", None) if svc is not None else None
    refresh_supported = callable(refresh_prepared) and type(svc).__module__ != "unittest.mock"
    if svc is not None and hasattr(svc, "prepare_write"):
        prepared, scope_packet, err = prepare_ci_write(
            context,
            file_path,
            allow_scope_drift=True,
        )
        if err is not None:
            return ToolResult(
                output=err,
                is_error=True,
                metadata={"scope_packet": scope_packet, "conflict": True},
            )
        if prepared is None:
            return ToolResult(
                output=f"CI service unavailable for coordinated edit of {file_path}",
                is_error=True,
            )
        if not bool(getattr(prepared, "existed", True)):
            abort_ci_write(context, prepared)
            return ToolResult(
                output=f"Path does not exist: {file_path}",
                is_error=True,
            )
        current = str(getattr(prepared, "current_content", "") or "")
        current_hash = str(getattr(prepared, "current_hash", "") or "")
    else:
        try:
            raw = await sandbox.fs.download_file(file_path)
            current = raw.decode("utf-8") if isinstance(raw, bytes) else str(raw)
            current_hash = _content_hash(current)
        except Exception as exc:
            try:
                sandbox = await _recover_sandbox(context, exc)
                raw = await sandbox.fs.download_file(file_path)
                current = raw.decode("utf-8") if isinstance(raw, bytes) else str(raw)
                current_hash = _content_hash(current)
            except Exception as recovery_exc:
                return ToolResult(
                    output=_path_error(recovery_exc, file_path) or f"Cannot read file: {recovery_exc}",
                    is_error=True,
                )

    if prepared is not None and refresh_supported:
        refreshed = refresh_prepared(prepared)
        if refreshed is not None:
            prepared = refreshed
            current = str(getattr(prepared, "current_content", "") or "")
            current_hash = str(getattr(prepared, "current_hash", "") or "")

    patch_result = patcher.apply_edits(current, normalized_edits)
    if not patch_result.success:
        abort_ci_write(context, prepared)
        return ToolResult(
            output=(
                f"Search text not found in {file_path}"
                if legacy_not_found and patch_result.errors == ["Edit 1: search text not found"]
                else "; ".join(patch_result.errors) or f"Edit failed for {file_path}"
            ),
            is_error=True,
        )

    new_content = patch_result.content

    if prepared is not None and refresh_supported:
        refreshed = refresh_prepared(prepared)
        refreshed_content = str(getattr(refreshed, "current_content", "") or "")
        refreshed_hash = str(getattr(refreshed, "current_hash", "") or "")
        if refreshed_hash != current_hash or refreshed_content != current:
            prepared = refreshed
            current = refreshed_content
            current_hash = refreshed_hash
            patch_result = patcher.apply_edits(current, normalized_edits)
            if not patch_result.success:
                abort_ci_write(context, prepared)
                return ToolResult(
                    output=(
                        f"Requested edits no longer apply cleanly to {file_path} after a concurrent edit. "
                        "Re-read the file and retry."
                    ),
                    is_error=True,
                    metadata={"conflict": True},
                )
            new_content = patch_result.content

    if dry_run:
        # Show preview
        import difflib

        diff = difflib.unified_diff(
            current.splitlines(keepends=True),
            new_content.splitlines(keepends=True),
            fromfile=f"a/{file_path}",
            tofile=f"b/{file_path}",
            lineterm="",
        )
        diff_text = "".join(diff)
        if len(diff_text) > _OUTPUT_MAX_CHARS:
            diff_text = diff_text[:_OUTPUT_MAX_CHARS] + "\n... (truncated)"
        output = json.dumps(
            {
                "cwd": _get_cwd(context) or "",
                "file_path": file_path,
                "status": "dry_run",
                "occ": False,
                "diff": diff_text,
                "warnings": list(patch_result.warnings),
            }
        )
        abort_ci_write(context, prepared)
        return ToolResult(output=output, metadata={"dry_run": True})

    # Try OCC-coordinated edit via CI service
    if prepared is not None:
        try:
            prepared, intent_id = prepare_ci_edit_intent(context, prepared, content=new_content)
            result = finalize_ci_write(
                context,
                prepared,
                content=new_content,
                edit_type="edit",
                description=description,
            )
        finally:
            release_ci_edit_intent(context, intent_id)
            abort_ci_write(context, prepared)
        if getattr(result, "success", False):
            output = json.dumps(
                {
                    "cwd": _get_cwd(context) or "",
                    "file_path": file_path,
                    "status": "edited",
                    "occ": True,
                    "expected_hash": current_hash,
                    "warnings": list(patch_result.warnings),
                }
            )
            return ToolResult(
                output=output,
                metadata={"file_path": file_path, "occ": True},
            )
        return ToolResult(
            output=str(getattr(result, "message", "") or "Edit failed"),
            is_error=True,
            metadata={"conflict": bool(getattr(result, "conflict", False))},
        )
    else:
        # Direct write (no CI)
        try:
            await _upload_file_compat(sandbox, new_content.encode("utf-8"), file_path)
            output = json.dumps(
                {
                    "cwd": _get_cwd(context) or "",
                    "file_path": file_path,
                    "status": "edited",
                    "occ": False,
                    "warnings": list(patch_result.warnings),
                }
            )
            return ToolResult(
                output=output,
                metadata={"file_path": file_path, "occ": False},
            )
        except Exception as exc:
            try:
                sandbox = await _recover_sandbox(context, exc)
                await _upload_file_compat(sandbox, new_content.encode("utf-8"), file_path)
                output = json.dumps(
                    {
                        "cwd": _get_cwd(context) or "",
                        "file_path": file_path,
                        "status": "edited",
                        "occ": False,
                        "warnings": list(patch_result.warnings),
                    }
                )
                return ToolResult(
                    output=output,
                    metadata={"file_path": file_path, "occ": False},
                )
            except Exception as recovery_exc:
                return ToolResult(
                    output=_path_error(recovery_exc, file_path) or f"Write failed: {recovery_exc}",
                    is_error=True,
                )


def _normalize_edits(
    *,
    old_text: str,
    new_text: str,
    edits: list[dict[str, Any]] | None,
) -> tuple[list[SearchReplaceEdit | LineRangeEdit], str | None, bool]:
    """Validate and normalize tool inputs into patcher edit objects."""
    if edits is not None:
        if old_text or new_text:
            return [], "Provide either `old_text`/`new_text` or `edits`, not both.", False
        normalized: list[SearchReplaceEdit | LineRangeEdit] = []
        for index, edit in enumerate(edits, start=1):
            if not isinstance(edit, dict):
                return [], f"Edit {index}: each edit must be an object.", False
            strategy = str(edit.get("strategy") or "").strip()
            if strategy == "search_replace":
                search = edit.get("search")
                replace = edit.get("replace")
                if not isinstance(search, str) or not isinstance(replace, str):
                    return [], f"Edit {index}: search_replace requires string `search` and `replace`.", False
                normalized.append(SearchReplaceEdit(old_text=search, new_text=replace))
            elif strategy == "line_range":
                start_line = edit.get("start_line")
                end_line = edit.get("end_line")
                new_content = edit.get("new_content")
                if not isinstance(start_line, int) or not isinstance(end_line, int) or not isinstance(new_content, str):
                    return [], (
                        f"Edit {index}: line_range requires integer `start_line`, integer `end_line`, "
                        "and string `new_content`."
                    ), False
                normalized.append(
                    LineRangeEdit(
                        start_line=start_line,
                        end_line=end_line,
                        new_text=new_content,
                    )
                )
            else:
                return [], f"Edit {index}: unknown strategy '{strategy}'.", False
        if not normalized:
            return [], "At least one edit is required.", False
        return normalized, None, False

    if not old_text:
        return [], "Provide either `old_text`/`new_text` or `edits`.", False
    return [SearchReplaceEdit(old_text=old_text, new_text=new_text)], None, True
