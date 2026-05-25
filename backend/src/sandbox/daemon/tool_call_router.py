"""Route daemon tool calls to isolated, direct layer-stack, or overlay execution."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any
from uuid import uuid4

from sandbox._shared.clock import monotonic_now
from sandbox._shared.models import Intent, ToolCallRequest, ToolCallResult
from sandbox._shared.workspace_pipeline import WorkspacePipeline
from sandbox.daemon.occ_runtime_services import get_occ_runtime_services
from sandbox.daemon.operation_payloads import (
    project_changeset_result,
    project_conflict_result,
    require_layer_stack_root,
    require_single_file_path,
)
from sandbox.ephemeral_workspace.overlay_registry import get_sandbox_overlay
from sandbox.isolated_workspace._control_plane.pipeline_registry import get_active_pipeline
from sandbox.layer_stack.workspace_binding import (
    WorkspaceBindingError,
    require_workspace_binding,
)
from sandbox.occ.changeset import EditChange, build_api_write_change, is_published_status


async def resolve_workspace_pipeline(req: ToolCallRequest) -> WorkspacePipeline:
    """Return isolated pipeline for open iws handles, otherwise ephemeral."""
    iws = get_active_pipeline()
    if iws is not None and iws.get_handle(req.agent_id) is not None:
        return iws
    return await get_sandbox_overlay(
        require_layer_stack_root(req.args),
        start=False,
    )


async def route_workspace_tool_call(
    args: dict[str, Any],
    *,
    verb: str,
    intent: Intent,
) -> ToolCallResult:
    if verb in {"read_file", "write_file", "edit_file"}:
        require_single_file_path(args)
    agent_id = _request_agent_id(args)
    req = ToolCallRequest(
        invocation_id=str(args.get("invocation_id") or uuid4().hex),
        agent_id=agent_id,
        verb=verb,
        intent=intent,
        args=args,
        background=bool(args.get("background", False)),
    )
    iws = get_active_pipeline()
    if iws is None or iws.get_handle(agent_id) is None:
        if verb == "read_file":
            if _can_route_through_layer_stack(req):
                return _read_file_from_layer_stack(req)
        if verb == "write_file":
            if _can_route_through_layer_stack(req):
                return await _write_file_to_layer_stack(req)
        if verb == "edit_file":
            if _can_route_through_layer_stack(req):
                return await _edit_file_in_layer_stack(req)
    pipeline = await resolve_workspace_pipeline(req)
    return await pipeline.run_tool_call(req)


def _read_file_from_layer_stack(req: ToolCallRequest) -> ToolCallResult:
    total_start = monotonic_now()
    root = require_layer_stack_root(req.args)
    services = get_occ_runtime_services(root)
    path = _bound_layer_path(root, require_single_file_path(req.args))
    read_start = monotonic_now()
    content, exists = services.manager.read_text(path)
    return {
        "success": True,
        "workspace": "ephemeral",
        "content": content if exists else "",
        "exists": exists,
        "encoding": "utf-8",
        "timings": {
            **_layer_stack_file_resource_timings(services, changed_path_count=0),
            "api.read.layer_stack_read_s": monotonic_now() - read_start,
            "api.read.total_s": monotonic_now() - total_start,
        },
    }


async def _write_file_to_layer_stack(req: ToolCallRequest) -> ToolCallResult:
    total_start = monotonic_now()
    root = require_layer_stack_root(req.args)
    services = get_occ_runtime_services(root)
    path = _bound_layer_path(root, require_single_file_path(req.args))
    content = str(req.args.get("content") if req.args.get("content") is not None else "")
    if not bool(req.args.get("overwrite", True)):
        _current, exists = services.manager.read_text(path)
        if exists:
            return {
                **project_conflict_result(
                    verb="write",
                    status="rejected",
                    reason="create_only_existing",
                    path=path,
                    message="file already exists",
                    total_start=total_start,
                    timings_extra=_layer_stack_file_resource_timings(
                        services,
                        changed_path_count=0,
                    ),
                ),
                "workspace": "ephemeral",
            }
    result = await services.occ_service.apply_changeset(
        [build_api_write_change(path=path, final_content=content)]
    )
    payload = project_changeset_result(
        result,
        verb="write",
        total_start=total_start,
        gitignore=services.gitignore,
        timings_extra=_layer_stack_file_resource_timings(
            services,
            changed_path_count=_published_file_count(result),
        ),
    )
    payload["workspace"] = "ephemeral"
    return payload


async def _edit_file_in_layer_stack(req: ToolCallRequest) -> ToolCallResult:
    total_start = monotonic_now()
    root = require_layer_stack_root(req.args)
    services = get_occ_runtime_services(root)
    path = _bound_layer_path(root, require_single_file_path(req.args))
    changes = _edit_changes(req.args, path)
    result = await services.occ_service.apply_changeset(changes)
    payload = project_changeset_result(
        result,
        verb="edit",
        total_start=total_start,
        gitignore=services.gitignore,
        timings_extra=_layer_stack_file_resource_timings(
            services,
            changed_path_count=_published_file_count(result),
        ),
    )
    payload["workspace"] = "ephemeral"
    payload["applied_edits"] = len(changes) if result.success else 0
    return payload


def _edit_changes(args: Mapping[str, object], path: str) -> list[EditChange]:
    raw_edits = args.get("edits")
    if not isinstance(raw_edits, list):
        raise ValueError("edits must be a list")
    changes: list[EditChange] = []
    for raw in raw_edits:
        if not isinstance(raw, dict):
            raise ValueError("each edit must be an object")
        expected_raw = raw.get("expected_occurrences")
        expected = 1 if expected_raw is None else int(expected_raw)
        if expected < 0:
            raise ValueError("expected_occurrences must be >= 0")
        old_text = str(raw.get("old_text") if raw.get("old_text") is not None else "")
        if not old_text:
            raise ValueError(f"edit anchor old_text must be non-empty for {path}")
        changes.append(
            EditChange(
                path=path,
                old_text=old_text,
                new_text=str(raw.get("new_text") if raw.get("new_text") is not None else ""),
                expected_occurrences=expected,
            )
        )
    return changes


def _can_route_through_layer_stack(req: ToolCallRequest) -> bool:
    try:
        root = require_layer_stack_root(req.args)
        _bound_layer_path(root, require_single_file_path(req.args))
    except WorkspaceBindingError:
        return False
    return True


def _bound_layer_path(layer_stack_root: str, raw_path: str) -> str:
    binding = require_workspace_binding(layer_stack_root)
    if raw_path.startswith("/"):
        return binding.layer_path_from_absolute(raw_path)
    return binding.layer_path_from_relative(raw_path)


def _published_file_count(result: object) -> int:
    files = getattr(result, "files", ())
    return sum(1 for file in files if is_published_status(file.status))


def _layer_stack_file_resource_timings(
    services: Any,
    *,
    changed_path_count: int,
) -> dict[str, float]:
    manifest = services.manager.read_active_manifest()
    layers = tuple(getattr(manifest, "layers", ()) or ())
    return {
        "resource.command_exec.changed_path_count": float(changed_path_count),
        "resource.layer_stack.manifest_depth": float(len(layers)),
        "resource.layer_stack.manifest_path_count": float(len(layers)),
        "resource.command_exec.run_dir_tree_exists": 0.0,
        "resource.command_exec.run_dir_tree_bytes": 0.0,
        "resource.command_exec.run_dir_tree_file_count": 0.0,
        "resource.command_exec.run_dir_tree_dir_count": 0.0,
        "resource.command_exec.run_dir_tree_entry_count": 0.0,
        "resource.command_exec.run_dir_tree_truncated": 0.0,
        "resource.command_exec.workspace_tree_exists": 0.0,
        "resource.command_exec.workspace_tree_bytes": 0.0,
        "resource.command_exec.workspace_tree_file_count": 0.0,
        "resource.command_exec.workspace_tree_dir_count": 0.0,
        "resource.command_exec.workspace_tree_entry_count": 0.0,
        "resource.command_exec.workspace_tree_truncated": 0.0,
        "resource.command_exec.upperdir_tree_exists": 0.0,
        "resource.command_exec.upperdir_tree_bytes": 0.0,
        "resource.command_exec.upperdir_tree_file_count": 0.0,
        "resource.command_exec.upperdir_tree_dir_count": 0.0,
        "resource.command_exec.upperdir_tree_entry_count": 0.0,
        "resource.command_exec.upperdir_tree_truncated": 0.0,
    }


def _request_agent_id(args: dict[str, Any]) -> str:
    caller = args.get("caller")
    raw = ""
    if isinstance(caller, dict):
        raw = str(caller.get("agent_id") or caller.get("agent_run_id") or "")
    if not raw:
        raw = str(args.get("agent_id") or "default")
    raw = raw.strip()
    return raw or "default"


__all__ = ["resolve_workspace_pipeline", "route_workspace_tool_call"]
