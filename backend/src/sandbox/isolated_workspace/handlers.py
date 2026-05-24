"""RPC handlers for ``api.isolated_workspace.{enter, exit, status}``.

This top-level handler manages lifecycle only. Foreground tool operations use
``api.v1.<verb>`` and route through ``sandbox.daemon.dispatch``.

Pipeline bootstrap, audit-sink wiring, and the layer-stack adapter live in
:mod:`._manager` post-Phase-2.6 C3; this module is the pure RPC surface.
"""

from __future__ import annotations

import os
from typing import Any

from sandbox.isolated_workspace._manager import (
    _ensure_manager,
    require_arg,
    require_pipeline,
)
from sandbox.isolated_workspace._types import IsolatedWorkspaceError


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
        return await require_pipeline().exit(require_arg(args, "agent_id"))
    except IsolatedWorkspaceError as exc:
        return _error(exc)


async def status(args: dict[str, Any]) -> dict[str, Any]:
    try:
        manager = require_pipeline()
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
    }


async def list_open(args: dict[str, Any]) -> dict[str, Any]:
    """Return every agent ID with an open isolated workspace.

    Always-on surface: cheap, read-only, no harness gate. Used by the test
    fixture to know which agents need exiting without hardcoding the list.
    """
    try:
        manager = require_pipeline()
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
        manager = require_pipeline()
    except IsolatedWorkspaceError:
        return {"success": True, "exited_agents": []}
    result = await manager.test_reset()
    return {"success": True, **result}


__all__ = ["enter", "exit_", "list_open", "status", "test_reset"]
