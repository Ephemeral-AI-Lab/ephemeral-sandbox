"""Process-local IsolatedPipeline registry, bootstrap, and audit-sink wiring.

This module owns the daemon-process registry that RPC handlers reach via
``require_pipeline``, plus the one-shot construction the daemon's first
``enter()`` triggers.

iws bootstrap is lazier than eph's because the daemon doesn't know which
``layer_stack_root`` an agent will target at startup — the first
``api.isolated_workspace.enter`` carries it via ``args["layer_stack_root"]``.
"""

from __future__ import annotations

import asyncio
import os
from collections.abc import Mapping
from typing import TYPE_CHECKING, Any

from audit.jsonl import append_jsonl_event
from sandbox.daemon import layer_stack_runtime
from sandbox.isolated_workspace._control_plane.types import IsolatedWorkspaceError
from sandbox.occ.layer_stack_adapter import LayerStackPortAdapter
from sandbox.overlay.writable_dirs import overlay_writable_root

if TYPE_CHECKING:
    from sandbox.isolated_workspace.pipeline import IsolatedPipeline


_bootstrap_lock = asyncio.Lock()
_active_pipeline: "IsolatedPipeline | None" = None

DEFAULT_AUDIT_JSONL_PATH = "/tmp/sandbox_isolated_workspace_events.jsonl"


def get_active_pipeline() -> "IsolatedPipeline | None":
    return _active_pipeline


def require_pipeline() -> "IsolatedPipeline":
    if _active_pipeline is None:
        raise IsolatedWorkspaceError(
            "feature_disabled",
            "isolated workspace pipeline is not initialized",
        )
    return _active_pipeline


def require_isolated_workspace_arg(args: Mapping[str, Any], key: str) -> str:
    value = str(args.get(key) or "").strip()
    if not value:
        raise IsolatedWorkspaceError("invalid_argument", f"{key} is required", key=key)
    return value


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
        append_jsonl_event(self._path, {"type": event_type, "payload": dict(payload)})


async def ensure_pipeline(args: dict[str, Any]) -> "IsolatedPipeline":
    """Lazily construct the IsolatedPipeline on the first ``enter`` RPC.

    Subsequent calls reuse the active singleton directly. The ``_bootstrap_lock``
    covers two concurrent first-time callers; only one constructs the pipeline
    and runs ``initialize`` / startup orphan recovery.
    """
    global _active_pipeline
    if _active_pipeline is not None:
        return _active_pipeline
    async with _bootstrap_lock:
        if _active_pipeline is not None:
            return _active_pipeline
        # Local import avoids a circular at module-load time: pipeline.py
        # would otherwise import pipeline_registry (for the registry accessors)
        # and pipeline_registry would import pipeline (for the class). Pipeline
        # import is deferred to first call so module load stays linear.
        from sandbox.isolated_workspace.pipeline import IsolatedPipeline

        layer_stack_root = require_isolated_workspace_arg(args, "layer_stack_root")
        # Phase 2.6 C3.5b: bind a LayerStackPortAdapter ONCE at construction so
        # the iws pipeline speaks the same kwarg-only LayerStackSnapshotPort
        # contract as eph (no per-call ``layer_stack_root`` arg threaded
        # through the lease/release call sites).
        layer_stack = LayerStackPortAdapter(
            layer_stack_runtime.get_layer_stack_manager(layer_stack_root),
        )
        pipeline = IsolatedPipeline(
            scratch_root=overlay_writable_root(),
            layer_stack=layer_stack,
            audit=_JsonlAuditSink(
                os.environ.get("EOS_ISOLATED_WORKSPACE_AUDIT_PATH", "").strip()
                or DEFAULT_AUDIT_JSONL_PATH
            ),
        )
        _active_pipeline = pipeline
        await pipeline.initialize()
        return pipeline
