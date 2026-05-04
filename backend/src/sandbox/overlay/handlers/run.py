"""Runtime handler for raw overlay capture requests."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.overlay.engine import OverlayCaptureEngine
from sandbox.overlay.runner.snapshot_overlay_runner import SnapshotOverlayRunner
from sandbox.overlay.types import overlay_shell_request_from_dict
from sandbox.overlay.wire import overlay_outcome_to_dict
from sandbox.runtime.overlay_shell.result_envelope import RuntimeResultEnvelope


async def handle(args: dict[str, Any]) -> dict[str, Any]:
    if "layer_stack_root" in args:
        envelope = await _handle_snapshot_overlay(args)
        return envelope.to_dict()

    engine = OverlayCaptureEngine(
        sandbox_id=str(args.get("sandbox_id") or "local"),
        workspace_root=str(args.get("workspace_root") or "/workspace"),
        direct_runtime=True,
    )
    timeout_raw = args.get("timeout")
    timeout = int(timeout_raw) if timeout_raw is not None else None
    outcome = await engine.execute(
        str(args["command"]),
        timeout=timeout,
        stdin=args.get("stdin"),
        description=str(args.get("description") or ""),
        agent_id=str(args.get("agent_id") or ""),
    )
    return overlay_outcome_to_dict(outcome)


async def _handle_snapshot_overlay(args: dict[str, Any]) -> RuntimeResultEnvelope:
    manager = LayerStackManager(str(args["layer_stack_root"]))
    runner = SnapshotOverlayRunner(manager)
    request = overlay_shell_request_from_dict(_snapshot_request_payload(args))
    return await runner.shell(request)


def _snapshot_request_payload(args: dict[str, Any]) -> dict[str, Any]:
    command = args.get("command")
    if not isinstance(command, list):
        raise ValueError("layer-stack overlay.run requires command as argv list")
    env = args.get("env") or {}
    if not isinstance(env, Mapping):
        raise ValueError("layer-stack overlay.run env must be an object")
    return {
        "request_id": str(args.get("request_id") or "overlay-run"),
        "command": command,
        "cwd": str(args.get("cwd") or "."),
        "env": dict(env),
        "timeout_seconds": args.get("timeout_seconds", args.get("timeout")),
    }


__all__ = ["handle"]
