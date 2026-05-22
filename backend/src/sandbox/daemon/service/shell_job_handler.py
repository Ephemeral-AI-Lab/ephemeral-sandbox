"""Daemon RPC handlers for ``shell.launch`` / ``poll`` / ``cancel`` / ``reap``.

These are the daemon-side ops that drive a background shell job. Engine-side
wiring lives in :mod:`sandbox.api.tool.shell` (background branch) and
:mod:`engine.background.manager`. Plan §RPC verbs.
"""

from __future__ import annotations

from typing import Any, Mapping
from uuid import uuid4

from sandbox.daemon.occ_backend import build_occ_backend
from sandbox.daemon.request_context import require_layer_stack_root
from sandbox.daemon.service.sandbox_overlay import SandboxOverlay
from sandbox.daemon.service.shell_job import (
    ShellJobNotFound,
    get_shell_job_registry,
)
from sandbox.layer_stack.workspace_binding import require_workspace_binding
from sandbox.execution.contract import CommandExecRequest


def shell_launch(args: dict[str, Any]) -> dict[str, Any]:
    """``api.shell.launch``: spawn a background shell, return a job id."""
    layer_stack_root = require_layer_stack_root(args)
    backend = build_occ_backend(layer_stack_root)
    request = _command_request(args)
    overlay = SandboxOverlay(
        occ_client=backend.occ_client,
        workspace_ref=request.workspace_ref,
        layer_stack=backend.layer_stack,
        workspace_root=request.workspace_root,
    )
    registry = get_shell_job_registry()
    result = registry.launch(
        request=request,
        overlay=overlay,
        storage_root=backend.layer_stack.storage_root,
    )
    return {
        "success": True,
        "job_id": str(result["job_id"]),
        "lease_id": str(result["lease_id"]),
        "started_at": float(result["started_at"]),
        "timings": {},
    }


def shell_poll(args: dict[str, Any]) -> dict[str, Any]:
    """``api.shell.poll``: progress snapshot (tail-of-stdout + status)."""
    job_id = _required_job_id(args)
    registry = get_shell_job_registry()
    try:
        snapshot = registry.poll(job_id)
    except ShellJobNotFound:
        return _job_not_found(job_id)
    return {"success": True, **snapshot, "timings": {}}


def shell_cancel(args: dict[str, Any]) -> dict[str, Any]:
    """``api.shell.cancel``: signal-cancel an in-flight job (idempotent)."""
    job_id = _required_job_id(args)
    reason = str(args.get("reason") or "")
    registry = get_shell_job_registry()
    try:
        result = registry.cancel(job_id, reason=reason)
    except ShellJobNotFound:
        return _job_not_found(job_id)
    return {"success": True, **result, "timings": {}}


async def shell_reap(args: dict[str, Any]) -> dict[str, Any]:
    """``api.shell.reap``: wait for the job to finish, return the full result."""
    job_id = _required_job_id(args)
    timeout = _optional_float(args.get("timeout_seconds"))
    registry = get_shell_job_registry()
    try:
        payload = await registry.reap(
            job_id,
            timeout_seconds=timeout if timeout is not None else 300.0,
        )
    except ShellJobNotFound:
        return _job_not_found(job_id)
    return {"success": True, **payload}


def shell_metrics(args: dict[str, Any]) -> dict[str, Any]:
    """``api.shell.metrics``: registry-wide counters for AC-13 visibility.

    Returns ``{active_jobs, ttl_reaped_total}``. The TTL reap counter is the
    only post-mortem signal the integration test (T4) has once the engine
    is SIGKILLed and its ``sandbox_events.jsonl`` writer dies with it.
    """
    del args  # no parameters
    registry = get_shell_job_registry()
    snapshot = registry.metrics()
    return {
        "success": True,
        "active_jobs": int(snapshot["active_jobs"]),
        "ttl_reaped_total": int(snapshot["ttl_reaped_total"]),
        "timings": {},
    }


def _required_job_id(args: Mapping[str, Any]) -> str:
    job_id = str(args.get("job_id") or "").strip()
    if not job_id:
        raise ValueError("job_id is required")
    return job_id


def _job_not_found(job_id: str) -> dict[str, Any]:
    return {
        "success": False,
        "error": {
            "kind": "shell_job_not_found",
            "message": f"shell job not found or already reaped: {job_id}",
            "details": {"job_id": job_id},
        },
        "warnings": [],
        "timings": {},
    }


def _command_request(args: Mapping[str, Any]) -> CommandExecRequest:
    """Mirror the parsing in :mod:`sandbox.daemon.service.shell_runner`.

    Kept inline because the canonical builder there is private to its module
    and slated for refactor in a separate phase (see plan follow-ups).
    """
    command = args.get("command")
    if isinstance(command, str):
        argv: tuple[str, ...] = ("bash", "-lc", command)
    elif isinstance(command, list):
        argv = tuple(str(part) for part in command)
    else:
        raise ValueError("command must be a string or argv list")
    timeout = args.get("timeout_seconds", args.get("timeout"))
    workspace_ref = require_layer_stack_root(args)
    binding = require_workspace_binding(workspace_ref)
    env_raw = args.get("env") or {}
    env = {str(k): str(v) for k, v in env_raw.items()} if isinstance(env_raw, Mapping) else {}
    return CommandExecRequest(
        request_id=str(args.get("request_id") or uuid4().hex),
        workspace_ref=workspace_ref,
        workspace_root=binding.workspace_root,
        command=argv,
        cwd=str(args.get("cwd") or "."),
        env=env,
        timeout_seconds=_optional_float(timeout),
        actor_id=str(args.get("actor_id") or ""),
        description=str(args.get("description") or "shell"),
    )


def _optional_float(value: Any) -> float | None:
    if value is None:
        return None
    if isinstance(value, (int, float, str)):
        try:
            return float(value)
        except (TypeError, ValueError):
            return None
    return None


__all__ = [
    "shell_cancel",
    "shell_launch",
    "shell_metrics",
    "shell_poll",
    "shell_reap",
]
