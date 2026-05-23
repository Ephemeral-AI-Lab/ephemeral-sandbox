"""Bounded RPC handlers for ``api.isolated_workspace.{shell, read_file, ...}``.

R3 import discipline: this module's transitive imports MUST NOT include
``sandbox.occ.*`` or ``sandbox.daemon.service.sandbox_overlay``. Verified by
``test_isolated_workspace_ops_import_fence``.

Allowed transitive imports:
    - :mod:`sandbox.isolated_workspace.manager` (state machine + bounded
      ``require_manager`` / ``require_arg`` accessors).
    - :mod:`sandbox.isolated_workspace.network` (pure-Python pool + Linux
      ip/nft shell-outs, no OCC).

Any future "let me reuse the existing overlay/publish helper" must add an
import here, which fails CI by tripping the import-fence test.
"""

from __future__ import annotations

import base64
import os
import sys
from typing import Any

from sandbox.isolated_workspace.manager import (
    IsolatedWorkspaceError,
    require_arg,
    require_manager,
)

# Absolute path to the in-ns write helper. We invoke by file path (not
# ``python -m sandbox.isolated_workspace.scripts.in_ns_write``) because after
# setns into the iws's mount namespace, the bundle's import path is no
# longer reliably on sys.path: ``python -m`` then fails with
# ``ModuleNotFoundError: No module named 'sandbox'``. Resolving the script
# via ``__file__`` keeps the helper bundle-local and namespace-portable.
_IN_NS_WRITE_PATH = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "scripts",
    "in_ns_write.py",
)


def _error(exc: IsolatedWorkspaceError) -> dict[str, Any]:
    return {
        "success": False,
        "error": {"kind": exc.kind, "message": str(exc), "details": exc.details},
    }


async def _run(agent_id: str, argv: list[str], *, stdin: bytes | None = None) -> dict[str, Any]:
    try:
        return await require_manager().run_in_handle(agent_id, argv=argv, stdin=stdin)
    except IsolatedWorkspaceError as exc:
        return _error(exc)


async def shell(args: dict[str, Any]) -> dict[str, Any]:
    return await _run(
        require_arg(args, "agent_id"),
        ["/bin/sh", "-c", require_arg(args, "command")],
    )


async def read_file(args: dict[str, Any]) -> dict[str, Any]:
    return await _run(
        require_arg(args, "agent_id"),
        ["/bin/cat", require_arg(args, "path")],
    )


async def write_file(args: dict[str, Any]) -> dict[str, Any]:
    agent_id = require_arg(args, "agent_id")
    path = require_arg(args, "path")
    raw = args.get("content", "")
    content = raw.encode("utf-8") if isinstance(raw, str) else bytes(raw)
    return await _run(
        agent_id,
        [sys.executable, _IN_NS_WRITE_PATH, path],
        stdin=base64.b64encode(content),
    )


async def edit_file(args: dict[str, Any]) -> dict[str, Any]:
    # edit_file shares the in-ns write helper — the agent supplies a full
    # post-edit body; the structural separation is identical to write_file.
    return await write_file(args)


async def grep(args: dict[str, Any]) -> dict[str, Any]:
    return await _run(
        require_arg(args, "agent_id"),
        ["/usr/bin/grep", "-r", "-n",
         require_arg(args, "pattern"),
         str(args.get("path") or "/testbed")],
    )


__all__ = ["edit_file", "grep", "read_file", "shell", "write_file"]
