"""Thin async client for isolated workspace daemon RPCs.

Wraps :func:`sandbox.host.daemon_client.call_daemon_api` so individual tests
read as intent (``enter()``, ``shell()``, ``exit()``) instead of envelope
boilerplate.

Each helper returns the raw daemon JSON response and lets the caller assert
on ``response["success"]`` / ``response["error"]["kind"]``. Transport errors
propagate; lifecycle errors are surfaced inside the response envelope so test
assertions stay explicit.

Lifecycle calls use ``api.isolated_workspace.*``. Tool calls use
``api.v1.<verb>`` and rely on daemon pipeline resolution.
"""

from __future__ import annotations

from typing import Any

from sandbox.host.daemon_client import _DaemonDispatchError, call_daemon_api


DEFAULT_TIMEOUT_S = 30

# Per-iws layer-stack metadata path. The iws workspace_root is fixed
# (/testbed via ISOLATED_WORKSPACE_ROOT) but the binding metadata must live
# at a DIFFERENT path per the workspace-binding constraint (layer_stack_root
# cannot equal or be inside workspace_root). Tests use this constant.
IWS_LAYER_STACK_ROOT = "/tmp/eos-sandbox-runtime/layer-stack"


async def _call_lifecycle(
    sandbox_id: str,
    op: str,
    args: dict[str, Any],
    *,
    timeout: int,
) -> dict[str, Any]:
    """Call a lifecycle op and surface domain errors as response dicts.

    The module's docstring promises: "lifecycle errors are surfaced inside
    the response envelope so test assertions stay explicit." The underlying
    ``call_daemon_api`` raises ``_DaemonDispatchError`` for any response
    with an ``error`` key — both system-level dispatch errors AND domain
    errors the iws handlers return as ``{"success": False, "error": ...}``.
    Tests in the failure_modes tier assert on the dict form (e.g. checking
    ``resp.get("error", {}).get("kind")``); catch the exception here and
    rebuild the envelope so they see the dict path.
    """
    try:
        return await call_daemon_api(sandbox_id, op, args, timeout=timeout)
    except _DaemonDispatchError as exc:
        return {
            "success": False,
            "error": {
                "kind": exc.kind,
                "message": exc.message,
                "details": exc.details or {},
            },
        }


async def enter(
    sandbox_id: str,
    agent_id: str,
    *,
    layer_stack_root: str,
    timeout: int = DEFAULT_TIMEOUT_S,
) -> dict[str, Any]:
    return await _call_lifecycle(
        sandbox_id,
        "api.isolated_workspace.enter",
        {"agent_id": agent_id, "layer_stack_root": layer_stack_root},
        timeout=timeout,
    )


async def exit_(
    sandbox_id: str,
    agent_id: str,
    *,
    timeout: int = DEFAULT_TIMEOUT_S,
) -> dict[str, Any]:
    return await call_daemon_api(
        sandbox_id,
        "api.isolated_workspace.exit",
        {"agent_id": agent_id},
        timeout=timeout,
    )


async def status(
    sandbox_id: str,
    agent_id: str,
    *,
    timeout: int = DEFAULT_TIMEOUT_S,
) -> dict[str, Any]:
    return await call_daemon_api(
        sandbox_id,
        "api.isolated_workspace.status",
        {"agent_id": agent_id},
        timeout=timeout,
    )


async def list_open(
    sandbox_id: str,
    *,
    timeout: int = DEFAULT_TIMEOUT_S,
) -> dict[str, Any]:
    return await call_daemon_api(
        sandbox_id,
        "api.isolated_workspace.list_open",
        {},
        timeout=timeout,
    )


async def test_reset(
    sandbox_id: str,
    *,
    timeout: int = DEFAULT_TIMEOUT_S,
) -> dict[str, Any]:
    return await call_daemon_api(
        sandbox_id,
        "api.isolated_workspace.test_reset",
        {},
        timeout=timeout,
    )


async def shell(
    sandbox_id: str,
    agent_id: str,
    command: str,
    *,
    timeout: int = DEFAULT_TIMEOUT_S,
) -> dict[str, Any]:
    return await call_daemon_api(
        sandbox_id,
        "api.v1.shell",
        {"agent_id": agent_id, "command": command},
        timeout=timeout,
    )


async def read_file(
    sandbox_id: str,
    agent_id: str,
    path: str,
    *,
    timeout: int = DEFAULT_TIMEOUT_S,
) -> dict[str, Any]:
    return await call_daemon_api(
        sandbox_id,
        "api.v1.read_file",
        {"agent_id": agent_id, "path": path},
        timeout=timeout,
    )


async def write_file(
    sandbox_id: str,
    agent_id: str,
    path: str,
    content: bytes | str,
    *,
    timeout: int = DEFAULT_TIMEOUT_S,
) -> dict[str, Any]:
    body = content.decode("utf-8", errors="replace") if isinstance(content, bytes) else content
    return await call_daemon_api(
        sandbox_id,
        "api.v1.write_file",
        {"agent_id": agent_id, "path": path, "content": body},
        timeout=timeout,
    )


async def edit_file(
    sandbox_id: str,
    agent_id: str,
    path: str,
    edits: list[dict[str, Any]],
    *,
    timeout: int = DEFAULT_TIMEOUT_S,
) -> dict[str, Any]:
    return await call_daemon_api(
        sandbox_id,
        "api.v1.edit_file",
        {"agent_id": agent_id, "path": path, "edits": edits},
        timeout=timeout,
    )


async def grep(
    sandbox_id: str,
    agent_id: str,
    pattern: str,
    *,
    path: str = "/testbed",
    glob_filter: str | None = None,
    output_mode: str | None = None,
    case_insensitive: bool | None = None,
    line_numbers: bool | None = None,
    multiline: bool | None = None,
    timeout: int = DEFAULT_TIMEOUT_S,
) -> dict[str, Any]:
    args: dict[str, Any] = {
        "agent_id": agent_id,
        "pattern": pattern,
        "path": path,
    }
    if glob_filter is not None:
        args["glob_filter"] = glob_filter
    if output_mode is not None:
        args["output_mode"] = output_mode
    if case_insensitive is not None:
        args["case_insensitive"] = case_insensitive
    if line_numbers is not None:
        args["line_numbers"] = line_numbers
    if multiline is not None:
        args["multiline"] = multiline
    return await call_daemon_api(
        sandbox_id,
        "api.v1.grep",
        args,
        timeout=timeout,
    )


async def glob(
    sandbox_id: str,
    agent_id: str,
    pattern: str,
    *,
    path: str = "/testbed",
    timeout: int = DEFAULT_TIMEOUT_S,
) -> dict[str, Any]:
    return await call_daemon_api(
        sandbox_id,
        "api.v1.glob",
        {"agent_id": agent_id, "pattern": pattern, "path": path},
        timeout=timeout,
    )


__all__ = [
    "DEFAULT_TIMEOUT_S",
    "IWS_LAYER_STACK_ROOT",
    "edit_file",
    "enter",
    "exit_",
    "glob",
    "grep",
    "list_open",
    "read_file",
    "shell",
    "status",
    "test_reset",
    "write_file",
]
