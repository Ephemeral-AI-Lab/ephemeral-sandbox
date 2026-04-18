"""File editing tool for Daytona sandboxes."""

from __future__ import annotations

import json
import logging
import base64
import shlex
import time
from typing import Any

from pydantic import BaseModel, Field

from code_intelligence.editing.change_labels import change_actor_label
from code_intelligence.editing.patcher import SearchReplaceEdit
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import (
    ci_write_required_result,
    exec_ci_process_operation,
    get_ci_service,
)
from tools.daytona_toolkit.tools import (
    _get_cwd,
    _path_error,
    _recover_sandbox,
    _require_sandbox,
    _resolve_path,
)
from tools.daytona_toolkit._daytona_utils import (
    _extract_exit_code,
    _wrap_bash_command,
)
from tools.core.decorator import tool

logger = logging.getLogger(__name__)

_EDIT_EXEC_TIMEOUT = 120

_EDIT_APPLY_SCRIPT = r"""
import base64
import json
import os
import pathlib
import sys
import tempfile

MAX_EDITS_PER_BATCH = 100

def emit(payload, code=0):
    print(json.dumps(payload))
    raise SystemExit(code)

file_path = os.environ["DAYTONA_EDIT_FILE"]
edits = json.loads(base64.b64decode(os.environ["DAYTONA_EDIT_PAYLOAD"]).decode("utf-8"))
dry_run = os.environ.get("DAYTONA_EDIT_DRY_RUN") == "1"
path = pathlib.Path(file_path)

try:
    current = path.read_text(encoding="utf-8")
except FileNotFoundError:
    emit({"ok": False, "error": f"Path does not exist: {file_path}"}, 1)
except Exception as exc:
    emit({"ok": False, "error": f"Cannot read file: {exc}"}, 1)

if len(edits) > MAX_EDITS_PER_BATCH:
    emit(
        {
            "ok": False,
            "errors": [f"Too many edits ({len(edits)} > {MAX_EDITS_PER_BATCH})"],
        },
        2,
    )

result = current
applied = 0
errors = []
warnings = []
for index, edit in enumerate(edits, start=1):
    old_text = str(edit.get("old_text", ""))
    new_text = str(edit.get("new_text", ""))
    if old_text not in result:
        errors.append(f"Edit {index}: search text not found")
        continue
    result = result.replace(old_text, new_text, 1)
    applied += 1

if errors or applied == 0:
    emit({"ok": False, "errors": errors or ["Edit failed"]}, 2)

payload = {
    "ok": True,
    "file_path": file_path,
    "applied_edits": applied,
    "warnings": warnings,
}
if dry_run:
    payload["status"] = "dry_run"
    payload["would_edit"] = True
    emit(payload)

parent = path.parent
parent.mkdir(parents=True, exist_ok=True)
fd, tmp = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=str(parent))
try:
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(result)
    os.replace(tmp, path)
finally:
    try:
        os.unlink(tmp)
    except FileNotFoundError:
        pass
payload["status"] = "edited"
emit(payload)
"""


class DaytonaEditFileInput(BaseModel):
    file_path: str = Field(..., description="Path to the file to edit.")
    old_text: str = Field(
        default="",
        description="Exact text to find in single-edit mode. Pair only with new_text.",
    )
    new_text: str = Field(
        default="",
        description="Replacement text for single-edit mode. Do not send with edits.",
    )
    edits: list[dict[str, Any]] | None = Field(
        default=None,
        description=(
            "Optional batch of edit objects. Supported shape: "
            "{\"strategy\":\"search_replace\",\"search\":\"...\",\"replace\":\"...\"}."
        ),
    )
    description: str = Field(
        default="",
        description="Optional human-readable description of the edit.",
    )
    dry_run: bool = Field(default=False, description="Validate replacements without writing.")


class DaytonaEditFileOutput(BaseModel):
    cwd: str = Field(..., description="Current sandbox working directory.")
    file_path: str = Field(..., description="Resolved file path that was edited.")
    status: str = Field(..., description="Edit result such as edited or dry_run.")
    warnings: list[str] = Field(default_factory=list, description="Non-fatal edit warnings.")
    timings: dict[str, Any] | None = Field(
        default=None,
        description="Optional edit timing metadata.",
    )
    applied_edits: int = Field(
        default=0,
        description="Number of replacements applied or validated.",
    )


def _build_edit_exec_command(
    *,
    file_path: str,
    edits: list[SearchReplaceEdit],
    dry_run: bool,
) -> str:
    payload = base64.b64encode(
        json.dumps(
            [
                {"old_text": edit.old_text, "new_text": edit.new_text}
                for edit in edits
            ],
            ensure_ascii=False,
        ).encode("utf-8")
    ).decode("ascii")
    command = (
        f"DAYTONA_EDIT_FILE={shlex.quote(file_path)} "
        f"DAYTONA_EDIT_PAYLOAD={shlex.quote(payload)} "
        f"DAYTONA_EDIT_DRY_RUN={'1' if dry_run else '0'} "
        f"python3 -c {shlex.quote(_EDIT_APPLY_SCRIPT)}"
    )
    return _wrap_bash_command(command)


async def _run_edit_exec_command(
    *,
    context: ToolExecutionContext,
    sandbox: Any,
    file_path: str,
    command: str,
) -> tuple[dict[str, Any] | None, Any, ToolResult | None]:
    async def _run(active_sandbox: Any) -> Any:
        return await exec_ci_process_operation(
            context,
            active_sandbox,
            command,
            timeout=_EDIT_EXEC_TIMEOUT,
            description="daytona_edit_file",
        )

    try:
        response = await _run(sandbox)
    except Exception as exc:
        try:
            sandbox = await _recover_sandbox(context, exc)
            response = await _run(sandbox)
        except Exception as recovery_exc:
            return None, sandbox, ToolResult(
                output=_path_error(recovery_exc, "") or f"Cannot execute edit: {recovery_exc}",
                is_error=True,
            )

    stdout = getattr(response, "result", "") or ""
    cleaned, exit_code = _extract_exit_code(
        stdout,
        fallback_exit_code=getattr(response, "exit_code", None),
    )
    try:
        payload = json.loads(cleaned or "{}")
    except json.JSONDecodeError:
        return None, sandbox, ToolResult(
            output=cleaned or "Edit command returned invalid JSON.",
            is_error=True,
        )
    if exit_code not in (0, None) or not bool(payload.get("ok", False)):
        errors = payload.get("errors")
        message = (
            "; ".join(str(item) for item in errors)
            if isinstance(errors, list)
            else str(payload.get("error") or cleaned or "Edit failed")
        )
        return payload, sandbox, ToolResult(output=message, is_error=True)
    return payload, sandbox, None


def _edit_success_result(
    *,
    context: ToolExecutionContext,
    file_path: str,
    warnings: list[str],
    patch_warnings: list[str],
    timings: dict[str, Any] | None = None,
    applied_edits: int = 0,
) -> ToolResult:
    """Build a successful-edit ToolResult with consistent JSON output."""
    payload: dict[str, Any] = {
        "cwd": _get_cwd(context) or "",
        "file_path": file_path,
        "status": "edited",
        "warnings": warnings + patch_warnings,
        "applied_edits": applied_edits,
    }
    if timings:
        payload["timings"] = timings
    return ToolResult(
        output=json.dumps(payload),
        metadata={"file_path": file_path, "timings": dict(timings or {})},
    )


def _scope_overlap_warning(
    context: ToolExecutionContext,
    file_path: str,
) -> str:
    """Check if other agents edited files in the same scope during this edit.

    Returns a warning string if another agent edited a file in the agent's scope,
    otherwise empty string. Call after a successful edit to alert the agent
    about potential concurrent changes in their scope.
    """
    arbiter = getattr(context, "metadata", {}).get("arbiter")
    if arbiter is None or not getattr(arbiter, "initialized", False):
        return ""

    agent_run_id = getattr(context, "metadata", {}).get("agent_run_id", "")
    write_scope: list[str] = getattr(context, "metadata", {}).get("write_scope", [])
    if not write_scope:
        return ""

    task_started_at = getattr(context, "metadata", {}).get("work_item_started_at", 0.0)
    if not task_started_at:
        return ""

    changes = arbiter.changes_since(
        task_started_at,
        team_run_id=str(getattr(context, "metadata", {}).get("team_run_id") or "") or None,
    )
    now = time.time()
    overlap_lines: list[str] = []
    for e in changes:
        if e.agent_run_id == agent_run_id:
            continue
        if not any(e.file_path.startswith(p.rstrip("/")) for p in write_scope):
            continue
        overlap_lines.append(
            f"  - {e.file_path} ({e.edit_type} by {change_actor_label(e)}, {int(now - e.created_at.timestamp())}s ago)"
        )

    if not overlap_lines:
        return ""

    return (
        f"\n[SCOPE OVERLAP WARNING] Other agents edited files in your scope "
        f"while you were editing {file_path}:\n" + "\n".join(overlap_lines)
    )


@tool(
    name="daytona_edit_file",
    description=(
        "Edit a file atomically. Use exactly one mode: "
        "(1) `old_text` + `new_text` for a single replacement or "
        "(2) `edits=[{\"strategy\":\"search_replace\",\"search\":\"...\",\"replace\":\"...\"}]` "
        "for batched replacements. Never send `new_text` together with `edits`. "
        "Before calling, compare `file_path` to your `scope_paths`; if it is outside "
        "scope, do not attempt the edit to see whether the tool allows it, because the "
        "attempt itself is a failed lane. "
        "In coordinated team lanes, if live evidence says the target is an outside-scope "
        "owner, missing module, compatibility shim, re-export, or import bridge, do not "
        "call this tool; submit `submit_task_summary(type='fail')` so replanning can widen "
        "or resequence the task. Test imports, collection errors, and target counts naming "
        "the path are not exceptions, and `scope_paths` alone is not enough to create an "
        "absent test-derived module path. In coordinated team lanes, test files are read/verify-only "
        "and this tool blocks test-file writes unless explicit authorization is present. "
        "This outside-scope guidance is not a runtime hard gate."
    ),
    short_description="Apply atomic file edits.",
    input_model=DaytonaEditFileInput,
    output_model=DaytonaEditFileOutput,
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
    """Edit a file in the Daytona sandbox atomically."""
    try:
        sandbox = await _require_sandbox(context)
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)
    tool_started = time.perf_counter()
    tool_timings: dict[str, float] = {}

    file_path = _resolve_path(file_path, context)
    # Write-scope policy runs as a pre-phase tool guard; the advisory
    # (if any) is already on ``context.metadata["guard_pre_warnings"]``.
    warnings: list[str] = list(context.metadata.get("guard_pre_warnings") or [])

    normalized_edits, edit_error, legacy_not_found = _normalize_edits(
        old_text=old_text,
        new_text=new_text,
        edits=edits,
    )
    if edit_error is not None:
        if warnings:
            return ToolResult(output=f"{edit_error}\n\n" + "\n".join(warnings), is_error=True)
        return ToolResult(output=edit_error, is_error=True)

    if get_ci_service(context) is None:
        return ci_write_required_result("daytona_edit_file", file_path)

    exec_started = time.perf_counter()
    exec_payload, sandbox, exec_error = await _run_edit_exec_command(
        context=context,
        sandbox=sandbox,
        file_path=file_path,
        command=_build_edit_exec_command(
            file_path=file_path,
            edits=normalized_edits,
            dry_run=dry_run,
        ),
    )
    tool_timings["exec_apply"] = round(time.perf_counter() - exec_started, 6)
    if exec_error is not None:
        if (
            legacy_not_found
            and exec_payload is not None
            and exec_payload.get("errors") == ["Edit 1: search text not found"]
        ):
            return ToolResult(
                output=f"Search text not found in {file_path}",
                is_error=True,
            )
        return exec_error
    assert exec_payload is not None

    patch_warnings = [str(item) for item in (exec_payload.get("warnings") or [])]
    applied_edits = int(exec_payload.get("applied_edits") or 0)

    if dry_run:
        output = json.dumps(
            {
                "cwd": _get_cwd(context) or "",
                "file_path": file_path,
                "status": "dry_run",
                "warnings": warnings + patch_warnings,
                "applied_edits": applied_edits,
            }
        )
        return ToolResult(output=output, metadata={"dry_run": True})

    scope_warning = _scope_overlap_warning(context, file_path)
    if scope_warning:
        warnings.append(scope_warning)
    tool_timings["tool_total"] = round(time.perf_counter() - tool_started, 6)
    return _edit_success_result(
        context=context,
        file_path=file_path,
        warnings=warnings,
        patch_warnings=patch_warnings,
        timings={"tool": tool_timings},
        applied_edits=applied_edits,
    )


def _normalize_edits(
    *,
    old_text: str,
    new_text: str,
    edits: list[dict[str, Any]] | None,
) -> tuple[list[SearchReplaceEdit], str | None, bool]:
    """Validate and normalize tool inputs into patcher edit objects."""
    if edits is not None:
        if old_text or new_text:
            return [], "Provide either `old_text`/`new_text` or `edits`, not both.", False
        normalized: list[SearchReplaceEdit] = []
        for index, edit in enumerate(edits, start=1):
            if not isinstance(edit, dict):
                return [], f"Edit {index}: each edit must be an object.", False
            strategy = str(edit.get("strategy") or "").strip()

            # Auto-recover: LLMs sometimes omit strategy but pass recognizable keys
            if not strategy:
                if "old_text" in edit or "new_text" in edit or "old_string" in edit or "new_string" in edit:
                    strategy = "search_replace"
                elif "search" in edit or "replace" in edit:
                    strategy = "search_replace"

            if strategy == "search_replace":
                # Accept common LLM key variants: search/replace, old_text/new_text, old_string/new_string
                search = edit.get("search") or edit.get("old_text") or edit.get("old_string")
                replace = edit.get("replace") or edit.get("new_text") or edit.get("new_string")
                if not isinstance(search, str) or not isinstance(replace, str):
                    return (
                        [],
                        f"Edit {index}: search_replace requires string `search` and `replace`.",
                        False,
                    )
                normalized.append(SearchReplaceEdit(old_text=search, new_text=replace))
            else:
                return [], (
                    f"Edit {index}: unknown strategy '{strategy}'. "
                    "Use `{{\"strategy\": \"search_replace\", \"search\": \"...\", \"replace\": \"...\"}}` "
                    "or use top-level `old_text`/`new_text` for a single edit."
                ), False
        if not normalized:
            return [], "At least one edit is required.", False
        return normalized, None, False

    if not old_text:
        return [], (
            "Provide `old_text` (text to find) and `new_text` (replacement), "
            "or use `edits` with strategy `search_replace`."
        ), False
    return [SearchReplaceEdit(old_text=old_text, new_text=new_text)], None, True
