"""Daemon RPC operation implementations.

Module layout:

* In-flight registry surface — ``cancel``, ``heartbeat``, ``inflight_count``.
* Tool operation routes — ``read_file``, ``write_file``, ``edit_file``,
  ``glob``, ``grep``, ``shell``. ``WORKSPACE_TOOL_OPS`` threads ``args`` and
  the static verb/intent pair through
  :func:`sandbox.daemon.workspace_tool.dispatch.dispatch_workspace_tool_call`.
* Layer-stack diagnostic surface — ``layer_metrics``, ``runtime_ready``.
* Layer-stack control surface — ``build_workspace_base``, ``ensure_workspace_base``,
  ``workspace_binding``, ``acquire_snapshot``, ``release_lease``,
  ``fence_stale_staging``.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import time
from collections.abc import Callable
from dataclasses import fields
from typing import Any

from sandbox.shared.clock import monotonic_now
from sandbox.shared.models import Intent
from sandbox.daemon import layer_stack_runtime, occ_runtime_services
from sandbox.daemon.occ_runtime_services import OccRuntimeServices
from sandbox.daemon.workspace_tool.payloads import (
    require_layer_stack_root,
    require_nonempty_string_arg,
)
from sandbox.daemon.workspace_tool.dispatch import dispatch_workspace_tool_call
from sandbox.daemon.rpc.in_flight import get_in_flight_registry
from sandbox.layer_stack.manifest import (
    manifest_path,
    read_manifest,
)
from sandbox.layer_stack.workspace_binding import (
    read_workspace_binding,
    require_workspace_binding,
)
from sandbox.overlay.namespace_runner import detect_private_mount_namespace


_CANCEL_CLEANUP_WAIT_S = 5.0
_STARTED_AT_MONO = time.monotonic()
WORKSPACE_TOOL_ROUTES: dict[str, Intent] = {
    "edit_file": Intent.WRITE_ALLOWED,
    "glob": Intent.READ_ONLY,
    "grep": Intent.READ_ONLY,
    "read_file": Intent.READ_ONLY,
    "shell": Intent.WRITE_ALLOWED,
    "write_file": Intent.WRITE_ALLOWED,
}
_WORKSPACE_TOOL_OP_ALIASES: dict[str, tuple[str, ...]] = {
    "edit_file": ("api.edit_file", "api.v1.edit_file"),
    "glob": ("api.glob", "api.v1.glob"),
    "grep": ("api.grep", "api.v1.grep"),
    "read_file": ("api.read_file", "api.v1.read_file"),
    "shell": ("api.v1.shell",),
    "write_file": ("api.write_file", "api.v1.write_file"),
}


def _make_workspace_tool_handler(
    verb: str,
    intent: Intent,
) -> Callable[[dict[str, Any]], object]:
    async def _dispatch(args: dict[str, Any]) -> dict[str, object]:
        return await dispatch_workspace_tool_call(args, verb=verb, intent=intent)

    _dispatch.__name__ = f"{verb}_handler"
    return _dispatch


WORKSPACE_TOOL_HANDLERS = {
    verb: _make_workspace_tool_handler(verb, intent)
    for verb, intent in WORKSPACE_TOOL_ROUTES.items()
}
WORKSPACE_TOOL_OPS = {
    op: WORKSPACE_TOOL_HANDLERS[verb]
    for verb, aliases in _WORKSPACE_TOOL_OP_ALIASES.items()
    for op in aliases
}


# ---------------------------------------------------------------------------
# In-flight registry surface (api.v1.{cancel, heartbeat, inflight_count})
# ---------------------------------------------------------------------------


async def cancel(args: dict[str, Any]) -> dict[str, object]:
    invocation_id = str(args.get("invocation_id") or "").strip()
    task = get_in_flight_registry().cancel_task(invocation_id)
    cancelled = task is not None
    if task is not None:
        with contextlib.suppress(asyncio.CancelledError, Exception):
            await asyncio.wait_for(
                asyncio.shield(task),
                timeout=_CANCEL_CLEANUP_WAIT_S,
            )
    return {
        "success": True,
        "invocation_id": invocation_id,
        "cancelled": cancelled,
        "already_done": not cancelled,
        "cleanup_done": task.done() if task is not None else True,
    }


async def heartbeat(args: dict[str, Any]) -> dict[str, object]:
    raw_ids = args.get("invocation_ids") or []
    invocation_ids = [str(value) for value in raw_ids] if isinstance(raw_ids, list) else []
    touched = get_in_flight_registry().heartbeat(invocation_ids)
    return {"success": True, "touched": touched}


async def inflight_count(args: dict[str, Any]) -> dict[str, object]:
    agent_id = str(args.get("agent_id") or "").strip()
    count = get_in_flight_registry().count_by_agent(agent_id)
    return {"success": True, "agent_id": agent_id, "count": count}


# ---------------------------------------------------------------------------
# Layer-stack diagnostic surface (api.layer_metrics, api.runtime.ready)
# ---------------------------------------------------------------------------


async def layer_metrics(args: dict[str, object]) -> dict[str, object]:
    """Summarize layer-stack storage and lease state for one runtime root."""
    root = require_layer_stack_root(args)
    manager = occ_runtime_services.get_occ_runtime_services(root).layer_stack_manager
    manifest = manager.read_active_manifest()
    binding = read_workspace_binding(root)
    layer_dirs = tuple((manager.storage_root / "layers").iterdir())
    staging_dirs = tuple((manager.storage_root / "staging").iterdir())
    on_disk_layer_ids = {entry.name for entry in layer_dirs if entry.is_dir()}
    active_layer_ids = {layer.layer_id for layer in manifest.layers}
    leased_layer_ids = {layer.layer_id for layer in manager.leased_layers()}
    referenced_layer_ids = active_layer_ids | leased_layer_ids
    orphan_layer_ids = sorted(on_disk_layer_ids - referenced_layer_ids)
    missing_layer_ids = sorted(referenced_layer_ids - on_disk_layer_ids)
    total_bytes = 0
    for entry in manager.storage_root.rglob("*"):
        if entry.is_file() or entry.is_symlink():
            total_bytes += entry.lstat().st_size
    return {
        "success": True,
        "manifest_version": manifest.version,
        "manifest_depth": manifest.depth,
        "active_leases": manager.active_lease_count(),
        "leased_layers": len(leased_layer_ids),
        "layer_dirs": len(layer_dirs),
        "referenced_layers": len(referenced_layer_ids),
        "orphan_layer_count": len(orphan_layer_ids),
        "missing_layer_count": len(missing_layer_ids),
        "orphan_layer_ids": orphan_layer_ids[:20],
        "missing_layer_ids": missing_layer_ids[:20],
        "staging_dirs": len(staging_dirs),
        "storage_bytes": total_bytes,
        "workspace_bound": binding is not None,
        "workspace_root": binding.workspace_root if binding is not None else "",
        "base_root_hash": binding.base_root_hash if binding is not None else "",
    }


def runtime_ready(args: dict[str, object]) -> dict[str, object]:
    """Return binary daemon readiness plus per-plane probe details."""
    total_start = monotonic_now()
    root = require_layer_stack_root(args)
    timings: dict[str, float] = {}
    probes = [
        _run_probe("control_plane", lambda: _probe_control_plane(root), timings=timings),
        _run_probe("data_plane", lambda: _probe_data_plane(root), timings=timings),
        _run_probe("mutation_gate", lambda: _probe_mutation_gate(root), timings=timings),
    ]
    return {
        "success": True,
        "ready": all(probe["status"] == "ok" for probe in probes),
        "probes": probes,
        "daemon_pid": os.getpid(),
        "uptime_s": max(0.0, time.monotonic() - _STARTED_AT_MONO),
        "timings": {
            **timings,
            "runtime.ready.total_s": monotonic_now() - total_start,
        },
    }


def _probe_control_plane(layer_stack_root: str) -> dict[str, object]:
    binding = require_workspace_binding(layer_stack_root)
    manager = layer_stack_runtime.get_layer_stack_manager(layer_stack_root)
    manifest = read_manifest(manifest_path(layer_stack_root))
    # Also exercise the manager API; this catches a broken manager cache even
    # when the manifest file itself can be read directly.
    manager_manifest = manager.read_active_manifest()
    if manager_manifest.version != manifest.version:
        raise RuntimeError("manager manifest version does not match active manifest file")
    return {
        "workspace_root": binding.workspace_root,
        "manifest_version": manifest.version,
        "manifest_depth": manifest.depth,
        "base_root_hash": binding.base_root_hash,
    }


def _probe_data_plane(layer_stack_root: str) -> dict[str, object]:
    services = occ_runtime_services.get_occ_runtime_services(layer_stack_root)
    if not isinstance(services, OccRuntimeServices):
        raise RuntimeError(
            f"operation services returned {type(services).__name__}; expected OccRuntimeServices"
        )
    mount_mode = "private_namespace" if detect_private_mount_namespace() else "unavailable"
    return {
        "handlers_services_ready": True,
        "shell_services_ready": True,
        "workspace_mount_mode": mount_mode,
    }


def _probe_mutation_gate(layer_stack_root: str) -> dict[str, object]:
    services = occ_runtime_services.get_occ_runtime_services(layer_stack_root)
    if not isinstance(services, OccRuntimeServices):
        raise RuntimeError(f"OCC runtime services type mismatch: {type(services).__name__}")
    present_fields = [field.name for field in fields(OccRuntimeServices)]
    return {
        "backend_ready": True,
        "backend_fields": present_fields,
        "occ_client_class": type(getattr(services, "occ_client", None)).__name__,
    }


def _run_probe(
    name: str,
    probe: Callable[[], dict[str, object]],
    *,
    timings: dict[str, float],
) -> dict[str, object]:
    start = monotonic_now()
    try:
        details = probe()
        status = "ok"
    except Exception as exc:
        status = "down"
        details = {
            "error_type": type(exc).__name__,
            "error": str(exc),
        }
    timings[f"runtime.ready.{name}_s"] = monotonic_now() - start
    return {
        "name": name,
        "status": status,
        "details": details,
    }


# ---------------------------------------------------------------------------
# Layer-stack control surface (api.{ensure,build}_workspace_base,
# api.workspace_binding, api.acquire_snapshot, api.release_lease,
# api.layer_stack.fence_stale_staging)
# ---------------------------------------------------------------------------


async def build_workspace_base(args: dict[str, object]) -> dict[str, object]:
    """Build (or rebuild on ``reset``) the layer-stack workspace base.

    ``reset=True`` drops peer runtime caches before rebuilding so the new
    base is rebound cleanly; that side effect is part of the public
    contract, not an internal optimization.
    """
    total_start = monotonic_now()
    layer_stack_root = require_layer_stack_root(args)
    workspace_root = require_nonempty_string_arg(args, "workspace_root")
    reset = bool(args.get("reset", False))
    if reset:
        await _drop_peer_runtime_caches(
            layer_stack_root,
            workspace_root=workspace_root,
        )
    timings: dict[str, float] = {}
    binding = layer_stack_runtime.build_workspace_base(
        layer_stack_root,
        workspace_root=workspace_root,
        reset=reset,
        timings=timings,
    )
    return {
        "success": True,
        "created": True,
        "binding": binding.to_dict(),
        "timings": {
            **timings,
            "api.workspace_base.total_s": monotonic_now() - total_start,
        },
    }


async def ensure_workspace_base(args: dict[str, object]) -> dict[str, object]:
    total_start = monotonic_now()
    binding, created = layer_stack_runtime.ensure_workspace_base(
        require_layer_stack_root(args),
        workspace_root=require_nonempty_string_arg(args, "workspace_root"),
    )
    return {
        "success": True,
        "created": created,
        "binding": binding.to_dict(),
        "timings": {
            "api.workspace_base.total_s": monotonic_now() - total_start,
        },
    }


async def workspace_binding(args: dict[str, object]) -> dict[str, object]:
    binding = require_workspace_binding(require_layer_stack_root(args))
    return {
        "success": True,
        "binding": binding.to_dict(),
    }


async def acquire_snapshot(args: dict[str, object]) -> dict[str, object]:
    total_start = monotonic_now()
    result = layer_stack_runtime.acquire_snapshot(
        require_layer_stack_root(args),
        owner_request_id=require_nonempty_string_arg(args, "request_id"),
    )
    payload = result.to_dict()
    timings = payload.get("timings")
    if not isinstance(timings, dict):
        timings = {}
    payload["timings"] = {
        **timings,
        "api.acquire_snapshot.total_s": monotonic_now() - total_start,
    }
    return {
        "success": True,
        **payload,
    }


async def release_lease(args: dict[str, object]) -> dict[str, object]:
    released = layer_stack_runtime.release_lease(
        require_layer_stack_root(args),
        lease_id=require_nonempty_string_arg(args, "lease_id"),
    )
    return {
        "success": True,
        "released": released,
    }


async def commit_to_workspace(args: dict[str, object]) -> dict[str, object]:
    """Project the active overlay onto the bound workspace root.

    Privileged tear-down sync op (no permission model exists — see
    ``api.acquire_snapshot`` precedent). Refuses to run while any
    snapshot lease is active; the surfacing ``RuntimeError`` indicates
    an agent-loop bug that should be fixed at the source, not papered
    over here.
    """
    total_start = monotonic_now()
    workspace_root = require_nonempty_string_arg(args, "workspace_root")
    timings: dict[str, float] = {}
    new_manifest = layer_stack_runtime.commit_to_workspace(
        require_layer_stack_root(args),
        workspace_root=workspace_root,
        timings=timings,
    )
    return {
        "success": True,
        "manifest_version": new_manifest.version,
        "timings": {
            **timings,
            "api.commit_to_workspace.total_s": monotonic_now() - total_start,
        },
    }


async def fence_stale_staging(args: dict[str, object]) -> dict[str, object]:
    return layer_stack_runtime.fence_stale_staging(require_layer_stack_root(args))


async def _drop_peer_runtime_caches(
    layer_stack_root: str,
    *,
    workspace_root: str,
) -> None:
    from sandbox.ephemeral_workspace.pipeline_registry import stop_ephemeral_pipeline

    await stop_ephemeral_pipeline(layer_stack_root, workspace_root=workspace_root)
    occ_runtime_services.drop_occ_runtime_services(layer_stack_root)


__all__ = [
    "WORKSPACE_TOOL_HANDLERS",
    "WORKSPACE_TOOL_OPS",
    "WORKSPACE_TOOL_ROUTES",
    "build_workspace_base",
    "cancel",
    "commit_to_workspace",
    "ensure_workspace_base",
    "fence_stale_staging",
    "heartbeat",
    "inflight_count",
    "layer_metrics",
    "acquire_snapshot",
    "release_lease",
    "runtime_ready",
    "workspace_binding",
]
