"""RPC handlers for ``api.isolated_workspace.{enter, exit, status}``.

This top-level handler manages lifecycle: it does NOT participate in R3 import
discipline (the bounded module is :mod:`.ops_handlers`). Singleton and
arg validation live in :mod:`.manager` so that the bounded ops module can
reuse them without pulling in ``request_context``'s OCC imports.

Manager bootstrap: the daemon doesn't ship with a singleton layer_stack_root
at startup (each handler call brings its own), so the manager is constructed
lazily on the first ``enter()`` call using ``args["layer_stack_root"]``. The
construction also runs ``initialize() + startup_gc()`` exactly once. After
that, subsequent calls reuse the singleton via ``require_manager()``.

Audit sink: the manager emits five ``sandbox_isolated_workspace_*`` event
types via its ``AuditSink`` port. Here we wire a JSONL writer that appends
into the in-container path (env-overrideable; default
``/tmp/sandbox_isolated_workspace_events.jsonl``). Live tests read this file
via ``raw_exec`` to verify audit sequences (PLAN §2).
"""

from __future__ import annotations

import asyncio
import os
from pathlib import Path
from typing import Any

from audit.jsonl import append_jsonl_event
from sandbox.daemon import workspace_server
from sandbox.execution.scratch import command_exec_scratch_root
from sandbox.isolated_workspace.manager import (
    IsolatedWorkspaceError,
    IsolatedWorkspaceManager,
    require_arg,
    require_manager,
    set_manager,
)


_bootstrap_lock = asyncio.Lock()

DEFAULT_AUDIT_JSONL_PATH = "/tmp/sandbox_isolated_workspace_events.jsonl"


class _LayerStackAdapter:
    """Adapter from ``workspace_server`` to the manager's ``LayerStackPort``."""

    @staticmethod
    def prepare_workspace_snapshot(
        layer_stack_root: str, *, owner_request_id: str, materialize: bool
    ) -> Any:
        return workspace_server.prepare_workspace_snapshot(
            layer_stack_root,
            owner_request_id=owner_request_id,
            materialize=materialize,
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


async def _ensure_manager(args: dict[str, Any]) -> IsolatedWorkspaceManager:
    try:
        return require_manager()
    except IsolatedWorkspaceError:
        pass
    async with _bootstrap_lock:
        try:
            return require_manager()  # racing caller may have constructed it
        except IsolatedWorkspaceError:
            pass
        layer_stack_root = require_arg(args, "layer_stack_root")
        scratch = command_exec_scratch_root(Path(layer_stack_root))
        manager = IsolatedWorkspaceManager(
            scratch_root=scratch,
            layer_stack_root=layer_stack_root,
            layer_stack=_LayerStackAdapter(),  # type: ignore[arg-type]
            audit=_JsonlAuditSink(_resolve_audit_path()),
        )
        set_manager(manager)
        await manager.initialize()
        return manager


def _error(exc: IsolatedWorkspaceError) -> dict[str, Any]:
    return {
        "success": False,
        "error": {"kind": exc.kind, "message": str(exc), "details": exc.details},
    }


async def enter(args: dict[str, Any]) -> dict[str, Any]:
    try:
        manager = await _ensure_manager(args)
        handle = await manager.enter(require_arg(args, "agent_id"))
    except IsolatedWorkspaceError as exc:
        return _error(exc)
    return {
        "success": True,
        "manifest_version": handle.manifest_version,
        "manifest_root_hash": handle.manifest_root_hash,
    }


async def exit_(args: dict[str, Any]) -> dict[str, Any]:
    try:
        return await require_manager().exit(require_arg(args, "agent_id"))
    except IsolatedWorkspaceError as exc:
        return _error(exc)


async def status(args: dict[str, Any]) -> dict[str, Any]:
    try:
        manager = require_manager()
    except IsolatedWorkspaceError as exc:
        return _error(exc)
    handle = manager.get_handle(require_arg(args, "agent_id"))
    if handle is None:
        return {"success": True, "open": False}
    return {
        "success": True,
        "open": True,
        "manifest_version": handle.manifest_version,
        "created_at": handle.created_at,
        "last_activity": handle.last_activity,
        "freezer_degraded": handle.freezer_degraded,
    }


async def list_open(args: dict[str, Any]) -> dict[str, Any]:
    """Return every agent ID with an open isolated workspace.

    Always-on surface: cheap, read-only, no harness gate. Used by the test
    fixture to know which agents need exiting without hardcoding the list.
    """
    try:
        manager = require_manager()
    except IsolatedWorkspaceError:
        return {"success": True, "open_agent_ids": []}
    return {"success": True, "open_agent_ids": manager.list_open_agents()}


async def test_reset(args: dict[str, Any]) -> dict[str, Any]:
    """Test-only janitor: exit every open handle + sweep orphan resources.

    Gated on ``EOS_ISOLATED_WORKSPACE_TEST_HARNESS=true`` so production
    deployments can't accidentally invoke it. Returns the list of agent IDs
    that were active when called.
    """
    if os.environ.get(
        "EOS_ISOLATED_WORKSPACE_TEST_HARNESS", ""
    ).strip().lower() != "true":
        return {
            "success": False,
            "error": {
                "kind": "forbidden",
                "message": (
                    "api.isolated_workspace.test_reset requires "
                    "EOS_ISOLATED_WORKSPACE_TEST_HARNESS=true"
                ),
                "details": {},
            },
        }
    try:
        manager = require_manager()
    except IsolatedWorkspaceError:
        return {"success": True, "exited_agents": []}
    result = await manager.test_reset()
    return {"success": True, **result}


__all__ = ["enter", "exit_", "list_open", "status", "test_reset"]
