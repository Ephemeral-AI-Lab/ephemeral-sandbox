"""Automatic per-operation workspace overlay for plugin dispatch."""

from __future__ import annotations

import asyncio
import json
import shutil
import sys
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any
from uuid import uuid4

from sandbox._shared.shell_contract import CommandExecRequest
from sandbox.overlay.capability import mount_syscalls_supported
from sandbox.overlay.namespace_runner import detect_private_mount_namespace
from sandbox.layer_stack.workspace_binding import (
    WorkspaceBindingError,
    require_workspace_binding,
)
from sandbox.ephemeral_workspace.plugin.op_context import PluginOpContext


PluginOpHandler = Callable[..., Awaitable[Any]]


async def run_plugin_op_with_workspace_overlay(
    plugin_handler: PluginOpHandler,
    args: dict[str, Any],
    ctx: PluginOpContext,
    plugin_name: str,
    op_name: str,
) -> Any:
    """Run one plugin op inside a private workspace overlay namespace.

    The parent daemon owns lease acquisition, upperdir publication, and lease
    release. The child process only sees a normal filesystem where the bound
    workspace root is replaced by the leased snapshot plus a private upperdir.
    """
    del plugin_handler
    if not _overlay_namespace_available():
        raise RuntimeError(
            "automatic plugin workspace overlay requires private mount namespace "
            "and the overlay required mount syscalls"
        )

    overlay = ctx.overlay
    acquire_overlay = getattr(overlay, "acquire_operation_overlay", None)
    publish_cycle = getattr(overlay, "publish_cycle", None)
    if not callable(acquire_overlay) or not callable(publish_cycle):
        raise RuntimeError("plugin overlay dispatch requires daemon EphemeralPipeline")

    workspace_root = _bound_workspace_root(ctx)
    invocation_id = f"plugin:{plugin_name}:{op_name}:{uuid4().hex[:8]}"
    handle = acquire_overlay(
        invocation_id=invocation_id,
        workspace_root=workspace_root,
    )
    try:
        if not getattr(handle, "layer_paths", None):
            raise RuntimeError("plugin operation overlay did not provide layer paths")
        response = await _run_child_plugin_op(
            plugin_name=plugin_name,
            op_name=op_name,
            args=args,
            ctx=ctx,
            handle=handle,
            workspace_root=workspace_root,
        )
        publish = await publish_cycle(
            request=CommandExecRequest(
                invocation_id=f"{invocation_id}:publish",
                workspace_ref=ctx.layer_stack_root,
                workspace_root=workspace_root,
                command=(f"plugin.{plugin_name}.{op_name}",),
                cwd=".",
                env={},
                timeout_seconds=None,
                agent_id=getattr(ctx.caller, "agent_id", ""),
                description=f"plugin.{plugin_name}.{op_name}",
            ),
            upperdir=str(handle.upperdir),
            snapshot=handle.manifest,
            run_maintenance=True,
        )
        return _attach_publish_result(response, publish)
    finally:
        release = getattr(handle, "release", None)
        if callable(release):
            release()


def _bound_workspace_root(ctx: PluginOpContext) -> str:
    binding = require_workspace_binding(ctx.layer_stack_root)
    overlay_root = str(getattr(ctx.overlay, "workspace_root", "") or "").strip()
    effective = Path(overlay_root or binding.workspace_root)
    bound = Path(binding.workspace_root)
    if effective != bound:
        raise WorkspaceBindingError(
            "plugin overlay workspace_root does not match workspace binding: "
            f"{effective} != {binding.workspace_root}"
        )
    return bound.as_posix()


def _overlay_namespace_available() -> bool:
    return mount_syscalls_supported() and detect_private_mount_namespace()


async def _run_child_plugin_op(
    *,
    plugin_name: str,
    op_name: str,
    args: dict[str, Any],
    ctx: PluginOpContext,
    handle: Any,
    workspace_root: str,
) -> Any:
    unshare = shutil.which("unshare")
    if not unshare:
        raise RuntimeError("automatic plugin workspace overlay requires unshare")
    run_dir = Path(str(handle.run_dir))
    payload_ref = run_dir / "plugin-overlay-request.json"
    output_ref = run_dir / "plugin-overlay-output.json"
    caller = getattr(ctx, "caller", None)
    caller_payload = (
        caller.audit_fields()
        if caller is not None and hasattr(caller, "audit_fields")
        else {}
    )
    payload_ref.write_text(
        json.dumps(
            {
                "plugin_name": plugin_name,
                "op_name": op_name,
                "args": args,
                "layer_stack_root": ctx.layer_stack_root,
                "workspace_root": workspace_root,
                "manifest_key": str(getattr(handle, "manifest_key", "")),
                "manifest_version": int(getattr(handle, "manifest_version", 0)),
                "root_hash": str(getattr(handle, "root_hash", "")),
                "layer_paths": list(getattr(handle, "layer_paths", ()) or ()),
                "upperdir": str(handle.upperdir),
                "workdir": str(handle.workdir),
                "output_ref": output_ref.as_posix(),
                "caller": caller_payload,
                "metadata": dict(getattr(ctx, "metadata", {}) or {}),
            },
            separators=(",", ":"),
            sort_keys=True,
        ),
        encoding="utf-8",
    )
    proc = await asyncio.create_subprocess_exec(
        unshare,
        "-Urm",
        sys.executable,
        "-m",
        "sandbox.ephemeral_workspace.plugin.overlay_child",
        payload_ref.as_posix(),
        stdout=asyncio.subprocess.DEVNULL,
        stderr=asyncio.subprocess.PIPE,
    )
    _stdout, stderr = await proc.communicate()
    if proc.returncode != 0:
        detail = (stderr or b"").decode("utf-8", "replace").strip()
        raise RuntimeError(detail or "plugin overlay child failed")
    return json.loads(output_ref.read_text(encoding="utf-8"))


def _attach_publish_result(response: Any, publish: Any) -> Any:
    if not isinstance(response, dict):
        return response
    timings = response.get("timings")
    if not isinstance(timings, dict):
        timings = {}
        response["timings"] = timings
    for key, value in dict(getattr(publish, "timings", {}) or {}).items():
        if isinstance(value, (int, float)):
            timings[str(key)] = float(value)

    changeset = getattr(publish, "changeset", None)
    path_changes = getattr(publish, "path_changes", ()) or ()
    response["plugin_overlay"] = {
        "changed_paths": [
            str(getattr(change, "path", change)) for change in path_changes
        ],
        "published_manifest_version": getattr(
            changeset, "published_manifest_version", None
        ),
    }
    if changeset is not None and not bool(getattr(changeset, "success", True)):
        response["success"] = False
        response["error"] = {
            "kind": "plugin_overlay_publish_failed",
            "message": "plugin workspace changes failed OCC publication",
        }
    return response


__all__ = ["run_plugin_op_with_workspace_overlay"]
