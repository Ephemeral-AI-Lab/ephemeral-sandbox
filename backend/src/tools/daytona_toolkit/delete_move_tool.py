"""Daytona-backed file delete and move tools.

These tools validate requested paths under the repo root and submit the
operation through the code-intelligence OCC commit path. Delete and move use
strict base hashes: base drift aborts with ``aborted_version`` and there is no
merge fallback.

CodeAct's shell policy blocks ``rm`` / ``mv`` precisely so that deletions and
moves flow through these OCC-gated tools instead of the unaudited shell path.
"""

from __future__ import annotations

import asyncio
import inspect
import json
import logging
from typing import Any

from pydantic import BaseModel, Field

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import (
    ci_write_required_result,
    get_ci_service,
)
from tools.core.decorator import tool
from tools.daytona_toolkit._daytona_utils import (
    _extend_write_scope,
    _get_repo_root,
    _resolve_path,
    _team_repo_write_error,
    _team_repo_write_warning,
    _write_scope_covers,
)

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------


def _scope_checks(
    context: ToolExecutionContext,
    file_path: str,
    *,
    tool_name: str,
) -> tuple[str | None, str | None]:
    """Apply write-scope policy; return ``(hard_error, soft_warning)``."""
    err = _team_repo_write_error(context, file_path, tool_name=tool_name)
    if err is not None:
        return err, None
    warn = _team_repo_write_warning(context, file_path, tool_name=tool_name)
    return None, warn


def _operation_payload(
    *,
    status: str,
    paths: list[str],
    warnings: list[str],
    conflict_reason: str | None = None,
    message: str | None = None,
) -> str:
    payload: dict[str, Any] = {
        "status": status,
        "paths": paths,
        "warnings": warnings,
    }
    if conflict_reason:
        payload["conflict_reason"] = conflict_reason
    if message:
        payload["message"] = message
    return json.dumps(payload)


def _normalized_path(path: str) -> str:
    if path == "/":
        return path
    return path.rstrip("/") or path


def _repo_guard_error(
    context: ToolExecutionContext,
    file_path: str,
    *,
    tool_name: str,
) -> str | None:
    """Reject mutations outside a concrete repo root."""
    repo_root = _normalized_path(str(_get_repo_root(context) or ""))
    if not repo_root or repo_root == "/":
        return (
            f"{tool_name}: operation requires a non-root "
            "repo_root/daytona_cwd in context."
        )

    path = _normalized_path(file_path)
    if path == repo_root:
        return f"{tool_name}: refusing to operate on repo root: {repo_root}"
    if not path.startswith(repo_root + "/"):
        return (
            f"{tool_name}: refusing operation outside repo root "
            f"{repo_root}: {file_path}"
        )
    return None


def _operation_paths(result: Any, fallback: list[str]) -> list[str]:
    files = getattr(result, "files", None)
    if isinstance(files, (list, tuple)):
        paths = [
            str(getattr(item, "file_path", "") or "")
            for item in files
            if str(getattr(item, "file_path", "") or "").strip()
        ]
        if paths:
            return paths
    return fallback


def _agent_id(context: ToolExecutionContext) -> str:
    for key in ("agent_run_id", "agent_name"):
        value = str(context.metadata.get(key) or "").strip()
        if value:
            return value
    return ""


def _sandbox_uses_async_exec(sandbox: Any) -> bool:
    process = getattr(sandbox, "process", None)
    exec_fn = getattr(process, "exec", None) if process is not None else None
    return bool(exec_fn) and inspect.iscoroutinefunction(exec_fn)


def _ci_sandbox_for_sync_occ(
    context: ToolExecutionContext,
    sandbox: Any,
) -> Any | None:
    """Return a sync sandbox handle for CI OCC calls when possible.

    Team tools usually execute against the async Daytona sandbox so normal
    CodeAct/file operations can be awaited. The CI write service is sync,
    though, and its ContentManager drives ``process.exec`` through a sync
    bridge. Rebinding it to the loop-owned async sandbox makes Daytona SDK
    futures cross event loops during delete/move reads.
    """
    if sandbox is None or not _sandbox_uses_async_exec(sandbox):
        return sandbox

    sandbox_id = str(context.metadata.get("sandbox_id") or "").strip()
    if not sandbox_id:
        return None

    try:
        from sandbox.service import SandboxService

        sync_sandbox = SandboxService().get_sandbox_object(sandbox_id)
    except Exception:
        logger.debug(
            "Could not resolve sync Daytona sandbox handle for OCC file op on %s",
            sandbox_id,
            exc_info=True,
        )
        return None

    if _sandbox_uses_async_exec(sync_sandbox):
        logger.debug(
            "Resolved Daytona sandbox handle for %s is still async; "
            "skipping OCC service rebind",
            sandbox_id,
        )
        return None
    return sync_sandbox


def _rebind_service_for_occ(
    context: ToolExecutionContext,
    svc: Any,
) -> None:
    sandbox = context.metadata.get("daytona_sandbox")
    rebind = getattr(svc, "rebind_sandbox", None)
    if sandbox is None or not callable(rebind):
        return
    ci_sandbox = _ci_sandbox_for_sync_occ(context, sandbox)
    if ci_sandbox is not None:
        rebind(ci_sandbox)


def _failure_status(result: Any, *, move: bool) -> tuple[str, str]:
    status = str(getattr(result, "status", "") or "failed")
    conflict_reason = str(getattr(result, "conflict_reason", "") or "")
    if conflict_reason == "not_found":
        return "not_found", "not_found"
    if move and conflict_reason == "dst_exists":
        return "dst_exists", "dst_exists"
    return status, conflict_reason or status


# ---------------------------------------------------------------------------
# daytona_delete_file
# ---------------------------------------------------------------------------


class DaytonaDeleteFileInput(BaseModel):
    file_path: str = Field(
        ...,
        min_length=1,
        description="Path to the file to delete. Must exist at call time.",
    )
    recursive: bool = Field(
        default=False,
        description=(
            "Compatibility flag from the former shell-backed tool. Directory "
            "tree deletes are not performed by CodeAct rm/mv; this OCC file "
            "tool rejects recursive=True until directory-tree OCC support exists."
        ),
    )
    description: str = Field(
        default="",
        description="Optional human-readable description of the delete.",
    )


class DaytonaDeleteFileOutput(BaseModel):
    status: str = Field(
        ...,
        description=(
            "`deleted`, `not_found`, `aborted_version`, `aborted_lock`, or `failed`."
        ),
    )
    paths: list[str] = Field(
        default_factory=list,
        description="Paths affected by the OCC commit.",
    )
    warnings: list[str] = Field(
        default_factory=list,
        description="Advisory warnings (e.g., files outside write_scope).",
    )
    conflict_reason: str | None = Field(
        default=None,
        description="Short reason when status is an abort class.",
    )
    message: str | None = Field(
        default=None,
        description="Human-readable detail.",
    )


@tool(
    name="daytona_delete_file",
    description=(
        "Delete a file by validating the target under the repo root and routing "
        "the operation through the OCC-gated code-intelligence commit path. "
        "Base-hash drift aborts with `aborted_version` and no merge fallback. "
        "Use this instead of attempting `rm` in CodeAct; CodeAct `rm` is "
        "blocked intentionally so deletes stay coordinated."
    ),
    short_description="Delete a file through the OCC commit path.",
    input_model=DaytonaDeleteFileInput,
    output_model=DaytonaDeleteFileOutput,
)
async def daytona_delete_file(
    file_path: str,
    recursive: bool = False,
    description: str = "",
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Delete a file through the code-intelligence OCC commit path."""
    file_path = _normalized_path(_resolve_path(file_path, context))
    # Write-scope policy runs as a pre-phase tool guard.
    warnings: list[str] = list(context.metadata.get("guard_pre_warnings") or [])

    svc = get_ci_service(context)
    if svc is None:
        return ci_write_required_result("daytona_delete_file", file_path)

    guard_error = _repo_guard_error(
        context, file_path, tool_name="daytona_delete_file",
    )
    if guard_error is not None:
        return ToolResult(
            output=_operation_payload(
                status="failed",
                paths=[file_path],
                warnings=warnings,
                message=guard_error,
            ),
            is_error=True,
        )

    if recursive:
        return ToolResult(
            output=_operation_payload(
                status="failed",
                paths=[file_path],
                warnings=warnings,
                conflict_reason="recursive_unsupported",
                message=(
                    "Recursive directory deletes are not supported by the "
                    "OCC file delete path."
                ),
            ),
            is_error=True,
        )

    _rebind_service_for_occ(context, svc)

    result = await asyncio.to_thread(
        svc.delete_file,
        file_path,
        agent_id=_agent_id(context),
        description=description or f"delete {file_path}",
    )
    if getattr(result, "success", False):
        paths = _operation_paths(result, [file_path])
        return ToolResult(
            output=_operation_payload(
                status="deleted",
                paths=paths,
                warnings=warnings,
            ),
            metadata={"file_count": len(paths), "success_count": len(paths)},
        )

    payload_status, conflict_reason = _failure_status(result, move=False)
    paths = _operation_paths(result, [file_path])
    return ToolResult(
        output=_operation_payload(
            status=payload_status,
            paths=paths,
            warnings=warnings,
            conflict_reason=conflict_reason,
            message=str(getattr(result, "conflict_reason", "") or conflict_reason),
        ),
        is_error=True,
        metadata={"file_count": len(paths), "success_count": 0},
    )


# ---------------------------------------------------------------------------
# daytona_move_file
# ---------------------------------------------------------------------------


class DaytonaMoveFileInput(BaseModel):
    src_path: str = Field(
        ...,
        min_length=1,
        description="Source file path. Must exist at call time.",
    )
    dst_path: str = Field(
        ...,
        min_length=1,
        description="Destination file path. Must not exist unless overwrite=True.",
    )
    overwrite: bool = Field(
        default=False,
        description=(
            "When True, replace an existing destination through a strict-base "
            "OCC overwrite. Destination drift aborts with aborted_version."
        ),
    )
    recursive: bool = Field(
        default=False,
        description=(
            "Compatibility flag from the former shell-backed tool. Directory "
            "tree moves are not performed by CodeAct rm/mv; this OCC file "
            "tool rejects recursive=True until directory-tree OCC support exists."
        ),
    )
    description: str = Field(
        default="",
        description="Optional human-readable description of the move.",
    )


class DaytonaMoveFileOutput(BaseModel):
    status: str = Field(
        ...,
        description=(
            "`moved`, `dst_exists`, `not_found`, `aborted_version`, "
            "`aborted_overlap`, `aborted_lock`, or `failed`."
        ),
    )
    src_path: str = Field(..., description="Resolved source path.")
    dst_path: str = Field(..., description="Resolved destination path.")
    paths: list[str] = Field(
        default_factory=list,
        description="Paths affected by the OCC commit.",
    )
    warnings: list[str] = Field(
        default_factory=list,
        description="Advisory warnings (e.g., files outside write_scope).",
    )
    conflict_reason: str | None = Field(
        default=None,
        description="Short reason when status is an abort class.",
    )
    message: str | None = Field(
        default=None,
        description="Human-readable detail.",
    )


@tool(
    name="daytona_move_file",
    description=(
        "Move a file by validating source/destination under the repo root and "
        "routing the operation through the OCC-gated code-intelligence commit "
        "path. Base-hash drift on src or dst aborts with `aborted_version` and "
        "no merge fallback; overwrite=True uses strict-base on the destination. "
        "Use this instead of attempting `mv` in CodeAct; CodeAct `mv` is "
        "blocked intentionally so moves stay coordinated."
    ),
    short_description="Move a file through the OCC commit path.",
    input_model=DaytonaMoveFileInput,
    output_model=DaytonaMoveFileOutput,
)
async def daytona_move_file(
    src_path: str,
    dst_path: str,
    overwrite: bool = False,
    recursive: bool = False,
    description: str = "",
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Move a file through the code-intelligence OCC commit path."""
    src_resolved = _normalized_path(_resolve_path(src_path, context))
    dst_resolved = _normalized_path(_resolve_path(dst_path, context))

    # A rename whose src is already in scope is a naming op, not a widening op:
    # the agent owns src, and owning "src at path X" logically owns "src at path Y"
    # after the move. We record that src was in scope so the dst pre-check is
    # suppressed and, on success, write_scope is extended to cover dst (so
    # subsequent edits on dst don't fire outside-scope warnings).
    src_in_scope = _write_scope_covers(context, src_resolved)

    warnings: list[str] = []
    paths_to_check = (src_resolved,) if src_in_scope else (src_resolved, dst_resolved)
    for path in paths_to_check:
        hard_error, soft_warning = _scope_checks(
            context, path, tool_name="daytona_move_file",
        )
        if hard_error is not None:
            return ToolResult(output=hard_error, is_error=True)
        if soft_warning is not None:
            warnings.append(soft_warning)

    svc = get_ci_service(context)
    if svc is None:
        return ci_write_required_result("daytona_move_file", src_resolved)

    guard_errors = [
        _repo_guard_error(context, src_resolved, tool_name="daytona_move_file"),
        _repo_guard_error(context, dst_resolved, tool_name="daytona_move_file"),
    ]
    guard_error = next((err for err in guard_errors if err is not None), None)
    if guard_error is None and dst_resolved == src_resolved:
        guard_error = "daytona_move_file: src_path and dst_path are identical"
    if guard_error is None and dst_resolved.startswith(src_resolved + "/"):
        guard_error = (
            "daytona_move_file: refusing to move a path to a destination "
            f"inside source: {dst_resolved}"
        )
    if guard_error is None and src_resolved.startswith(dst_resolved + "/"):
        guard_error = (
            "daytona_move_file: refusing to replace a destination that "
            f"contains source: {dst_resolved}"
        )
    if guard_error is not None:
        return ToolResult(
            output=_move_payload(
                status="failed",
                src=src_resolved,
                dst=dst_resolved,
                paths=[],
                warnings=warnings,
                message=guard_error,
            ),
            is_error=True,
        )

    if recursive:
        return ToolResult(
            output=_move_payload(
                status="failed",
                src=src_resolved,
                dst=dst_resolved,
                warnings=warnings,
                conflict_reason="recursive_unsupported",
                message=(
                    "Recursive directory moves are not supported by the OCC "
                    "file move path."
                ),
            ),
            is_error=True,
        )

    _rebind_service_for_occ(context, svc)

    result = await asyncio.to_thread(
        svc.move_file,
        src_resolved,
        dst_resolved,
        overwrite=overwrite,
        agent_id=_agent_id(context),
        description=description or f"move {src_resolved} -> {dst_resolved}",
    )
    if getattr(result, "success", False):
        paths = [src_resolved, dst_resolved]
        if src_in_scope:
            _extend_write_scope(context, dst_resolved)
        return ToolResult(
            output=_move_payload(
                status="moved",
                src=src_resolved,
                dst=dst_resolved,
                paths=paths,
                warnings=warnings,
            ),
            metadata={"file_count": len(paths), "success_count": len(paths)},
        )

    payload_status, conflict_reason = _failure_status(result, move=True)
    paths = _operation_paths(result, [src_resolved, dst_resolved])
    return ToolResult(
        output=_move_payload(
            status=payload_status,
            src=src_resolved,
            dst=dst_resolved,
            paths=paths,
            warnings=warnings,
            conflict_reason=conflict_reason,
            message=str(getattr(result, "conflict_reason", "") or conflict_reason),
        ),
        is_error=True,
        metadata={"file_count": len(paths), "success_count": 0},
    )

def _move_payload(
    *,
    status: str,
    src: str,
    dst: str,
    warnings: list[str],
    paths: list[str] | None = None,
    conflict_reason: str | None = None,
    message: str | None = None,
) -> str:
    payload: dict[str, Any] = {
        "status": status,
        "src_path": src,
        "dst_path": dst,
        "paths": (
            paths
            if paths is not None
            else ([src, dst] if status == "moved" else [])
        ),
        "warnings": warnings,
    }
    if conflict_reason:
        payload["conflict_reason"] = conflict_reason
    if message:
        payload["message"] = message
    return json.dumps(payload)
