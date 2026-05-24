"""LSP plugin in-sandbox runtime entry point."""

from __future__ import annotations

import asyncio
import os
import time
from typing import Any

from sandbox.ephemeral_workspace.plugin.op_registry import register_plugin_op

from plugins.catalog.lsp.runtime.apply import apply_workspace_edit
from plugins.catalog.lsp.runtime.session_manager import get_session


async def warm_plugin_runtime(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    """Start the Pyright sidecar during plugin ensure so first tool calls are warm."""
    del args
    session = await get_session(ctx)
    timeout_s = _warm_start_timeout_s()
    try:
        await asyncio.wait_for(session.start(), timeout=timeout_s)
    except TimeoutError:
        return {
            "success": True,
            "manifest_key": session.manifest_key,
            "runtime_start_timeout_s": timeout_s,
            "runtime_start_deferred": True,
        }
    return {
        "success": True,
        "manifest_key": session.manifest_key,
    }


def _warm_start_timeout_s() -> float:
    raw = os.environ.get("EOS_LSP_WARM_START_TIMEOUT_S", "").strip()
    if not raw:
        return 8.0
    try:
        return max(0.1, float(raw))
    except ValueError:
        return 8.0


@register_plugin_op("lsp", "hover", auto_workspace_overlay=False)
async def hover(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    return await _run_timed_lsp_op("hover", ctx, lambda session: session.hover(args))


@register_plugin_op("lsp", "find_definitions", auto_workspace_overlay=False)
async def find_definitions(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    return await _run_timed_lsp_op(
        "find_definitions",
        ctx,
        lambda session: session.find_definitions(args),
    )


@register_plugin_op("lsp", "find_references", auto_workspace_overlay=False)
async def find_references(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    return await _run_timed_lsp_op(
        "find_references",
        ctx,
        lambda session: session.find_references(args),
    )


@register_plugin_op("lsp", "diagnostics", auto_workspace_overlay=False)
async def diagnostics(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    return await _run_timed_lsp_op(
        "diagnostics",
        ctx,
        lambda session: session.diagnostics(args),
    )


@register_plugin_op("lsp", "query_symbols", auto_workspace_overlay=False)
async def query_symbols(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    return await _run_timed_lsp_op(
        "query_symbols",
        ctx,
        lambda session: session.query_symbols(args),
    )


@register_plugin_op("lsp", "apply_workspace_edit", auto_workspace_overlay=False)
async def apply_workspace_edit_op(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    edit = args.get("edit") if isinstance(args.get("edit"), dict) else args
    return await apply_workspace_edit(edit, ctx)


@register_plugin_op("lsp", "rename", auto_workspace_overlay=False)
async def rename(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    session = await get_session(ctx)
    edit = await session.rename(args)
    result = await apply_workspace_edit(
        edit,
        ctx,
        workspace_root=session.workspace_root,
        expected_manifest_key=session.manifest_key,
    )
    return {"edit": edit, "apply": result}


@register_plugin_op("lsp", "format", auto_workspace_overlay=False)
async def format_document(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    session = await get_session(ctx)
    edit = await session.format_document(args)
    result = await apply_workspace_edit(
        edit,
        ctx,
        workspace_root=session.workspace_root,
        expected_manifest_key=session.manifest_key,
    )
    return {"edit": edit, "apply": result}


@register_plugin_op("lsp", "code_actions", auto_workspace_overlay=False)
async def code_actions(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    session = await get_session(ctx)
    return await session.code_actions(args)


@register_plugin_op("lsp", "apply_code_action", auto_workspace_overlay=False)
async def apply_code_action(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    action = args.get("action") if isinstance(args.get("action"), dict) else args
    edit = action.get("edit") if isinstance(action.get("edit"), dict) else {}
    session = await get_session(ctx)
    result = await apply_workspace_edit(
        edit,
        ctx,
        workspace_root=session.workspace_root,
        expected_manifest_key=session.manifest_key,
    )
    return {"action": action, "apply": result}


async def _run_timed_lsp_op(
    op_name: str,
    ctx: Any,
    call: Any,
) -> dict[str, Any]:
    timings: dict[str, float] = {}
    start_s = time.monotonic()
    session = await get_session(ctx)
    timings["lsp.get_session_s"] = time.monotonic() - start_s
    start_count = int(getattr(session, "audit_start_count", 0))
    refresh_count = int(getattr(session, "audit_refresh_count", 0))
    remount_count = int(getattr(session, "audit_remount_count", 0))
    op_start_s = time.monotonic()
    result = await call(session)
    timings[f"lsp.{op_name}.body_s"] = time.monotonic() - op_start_s
    timings["lsp.total_s"] = time.monotonic() - start_s
    _attach_session_timings(
        timings,
        session,
        start_count=start_count,
        refresh_count=refresh_count,
        remount_count=remount_count,
    )
    if not isinstance(result, dict):
        return {"result": result, "timings": timings}
    existing = result.get("timings")
    if isinstance(existing, dict):
        existing.update(timings)
    else:
        result["timings"] = timings
    return result


def _attach_session_timings(
    timings: dict[str, float],
    session: Any,
    *,
    start_count: int,
    refresh_count: int,
    remount_count: int,
) -> None:
    current_start_count = int(getattr(session, "audit_start_count", 0))
    current_refresh_count = int(getattr(session, "audit_refresh_count", 0))
    current_remount_count = int(getattr(session, "audit_remount_count", 0))
    timings["lsp.session.start_count_total"] = float(current_start_count)
    timings["lsp.session.start_count_delta"] = float(
        current_start_count - start_count
    )
    timings["lsp.session.refresh_count_total"] = float(current_refresh_count)
    timings["lsp.session.refresh_count_delta"] = float(
        current_refresh_count - refresh_count
    )
    timings["lsp.session.remount_count_total"] = float(current_remount_count)
    timings["lsp.session.remount_count_delta"] = float(
        current_remount_count - remount_count
    )
    timings["lsp.session.last_start_s"] = float(
        getattr(session, "audit_last_start_s", 0.0)
    )
    timings["lsp.session.last_remount_s"] = float(
        getattr(session, "audit_last_remount_s", 0.0)
    )
    timings["lsp.session.private_overlay_namespace"] = float(
        bool(getattr(session, "_uses_private_overlay_namespace", False))
    )
    timings["lsp.session.has_overlay_handle"] = float(
        getattr(session, "_overlay_handle", None) is not None
    )
    timings["lsp.session.layer_path_count"] = float(
        len(getattr(session, "_overlay_layer_paths", ()) or ())
    )
