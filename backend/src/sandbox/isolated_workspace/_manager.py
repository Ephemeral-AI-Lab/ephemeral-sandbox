"""Process-local IsolatedPipeline singleton, bootstrap, and audit-sink wiring.

Mirrors :mod:`sandbox.ephemeral_workspace._manager` — both modules carry the
private state RPC handlers reach via ``require_pipeline`` / ``set_pipeline``,
plus the one-shot construction the daemon's first ``enter()`` triggers.

iws bootstrap is lazier than eph's because the daemon doesn't know which
``layer_stack_root`` an agent will target at startup — the first
``api.isolated_workspace.enter`` carries it via ``args["layer_stack_root"]``.
"""

from __future__ import annotations

import asyncio
import os
from typing import TYPE_CHECKING, Any

from audit.jsonl import append_jsonl_event
from sandbox.daemon import workspace_server
from sandbox.isolated_workspace._types import (
    AuditSink,
    IsolatedWorkspaceError,
)
from sandbox.overlay.writable_dirs import overlay_writable_root

if TYPE_CHECKING:
    from sandbox.isolated_workspace.pipeline import IsolatedPipeline


_bootstrap_lock = asyncio.Lock()
_pipeline_singleton: "IsolatedPipeline | None" = None

DEFAULT_AUDIT_JSONL_PATH = "/tmp/sandbox_isolated_workspace_events.jsonl"


def set_pipeline(pipeline: "IsolatedPipeline | None") -> None:
    global _pipeline_singleton
    _pipeline_singleton = pipeline


def get_active_pipeline() -> "IsolatedPipeline | None":
    return _pipeline_singleton


def require_pipeline() -> "IsolatedPipeline":
    if _pipeline_singleton is None:
        raise IsolatedWorkspaceError(
            "feature_disabled", "isolated workspace pipeline is not initialized",
        )
    return _pipeline_singleton


def require_arg(args: dict[str, Any], key: str) -> str:
    value = str(args.get(key) or "").strip()
    if not value:
        raise IsolatedWorkspaceError("invalid_argument", f"{key} is required", key=key)
    return value


class _LayerStackAdapter:
    """Adapter from ``workspace_server`` to the pipeline's ``LayerStackPort``.

    Carries per-call ``layer_stack_root`` because the legacy
    ``workspace_server`` functions take it positionally; the iws pipeline
    binds ``layer_stack_root`` at construction and forwards it on each call.
    """

    @staticmethod
    def prepare_workspace_snapshot(
        layer_stack_root: str, *, owner_request_id: str,
    ) -> Any:
        return workspace_server.prepare_workspace_snapshot(
            layer_stack_root,
            owner_request_id=owner_request_id,
        )

    @staticmethod
    def release_workspace_snapshot(layer_stack_root: str, *, lease_id: str) -> bool:
        return workspace_server.release_workspace_snapshot(
            layer_stack_root, lease_id=lease_id,
        )


class _JsonlAuditSink:
    """Append-only JSON-line audit sink for iws events.

    Each ``emit`` writes one object shaped
    ``{"ts": <float>, "type": <event_type>, "payload": <payload>}`` to the
    configured path. Live tests read this file via ``raw_exec`` to verify
    audit sequences; in production the file is the daemon-side mirror of
    the lifecycle events the host recorder would otherwise miss (the iws
    handlers run RPC-direct without going through ``run_scenario``).
    """

    def __init__(self, path: str) -> None:
        self._path = path

    def emit(self, event_type: str, payload: dict[str, Any]) -> None:
        append_jsonl_event(
            self._path, {"type": event_type, "payload": dict(payload)}
        )


def _resolve_audit_path() -> str:
    raw = os.environ.get("EOS_ISOLATED_WORKSPACE_AUDIT_PATH", "").strip()
    return raw or DEFAULT_AUDIT_JSONL_PATH


async def _ensure_manager(args: dict[str, Any]) -> "IsolatedPipeline":
    """Lazily construct the IsolatedPipeline on the first ``enter`` RPC.

    Subsequent calls reuse the singleton via :func:`require_pipeline`. The
    ``_bootstrap_lock`` covers two concurrent first-time callers; only one
    constructs the pipeline and runs ``initialize`` / ``startup_gc``.
    """
    try:
        return require_pipeline()
    except IsolatedWorkspaceError:
        pass
    async with _bootstrap_lock:
        try:
            return require_pipeline()  # racing caller may have constructed it
        except IsolatedWorkspaceError:
            pass
        # Local import avoids a circular at module-load time: pipeline.py
        # would otherwise import _manager (for the singleton accessors) and
        # _manager would import pipeline (for the class). Pipeline import is
        # deferred to first call so module load stays linear.
        from sandbox.isolated_workspace.pipeline import IsolatedPipeline

        layer_stack_root = require_arg(args, "layer_stack_root")
        manager = IsolatedPipeline(
            scratch_root=overlay_writable_root(),
            layer_stack_root=layer_stack_root,
            layer_stack=_LayerStackAdapter(),  # type: ignore[arg-type]
            audit=_JsonlAuditSink(_resolve_audit_path()),
        )
        set_pipeline(manager)
        await manager.initialize()
        return manager


__all__ = [
    "AuditSink",
    "DEFAULT_AUDIT_JSONL_PATH",
    "_JsonlAuditSink",
    "_LayerStackAdapter",
    "_ensure_manager",
    "_resolve_audit_path",
    "get_active_pipeline",
    "require_arg",
    "require_pipeline",
    "set_pipeline",
]
