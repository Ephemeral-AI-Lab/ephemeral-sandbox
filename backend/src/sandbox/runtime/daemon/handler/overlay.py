"""Runtime handler for layer-stack snapshot overlay requests."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from sandbox.layer_stack.manager import LayerStackManager
from sandbox.overlay import (
    OverlayCapture,
    OverlayShellRequest,
    OverlaySnapshotRunner,
)


async def handle(args: dict[str, Any]) -> dict[str, Any]:
    if "layer_stack_root" not in args:
        raise ValueError("overlay.run requires layer_stack_root")
    capture = await _handle_snapshot_overlay(args)
    return capture.to_dict()


async def _handle_snapshot_overlay(args: dict[str, Any]) -> OverlayCapture:
    manager = LayerStackManager(str(args["layer_stack_root"]))
    runner = OverlaySnapshotRunner(manager)
    request = OverlayShellRequest.from_dict(_snapshot_request_payload(args))
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
