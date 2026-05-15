"""Runtime handler for layer-stack snapshot overlay requests."""

from __future__ import annotations

from collections.abc import Mapping
from pathlib import Path
from typing import Any
from typing import cast

from sandbox.daemon.service.layer_stack_client import LayerStackClient
from sandbox.execution.contract import (
    CommandExecRequest,
    MountMode,
    OverlayCapture,
    OverlayShellRequest,
    ShellProcessResult,
    WorkspaceReplacementMountSpec,
)
from sandbox.execution.orchestrator import (
    execute_command,
    run_workspace_replaced_command,
)
from sandbox.execution.policy import CommandExecPolicy
from sandbox.layer_stack.manifest import Manifest

_OVERLAY_COMMAND_POLICY = CommandExecPolicy(
    host_env_keys=frozenset(
        {
            "PATH",
            "HOME",
            "USER",
            "LANG",
            "LC_ALL",
            "TERM",
            "TZ",
        }
    ),
)


async def handle(args: dict[str, Any]) -> dict[str, Any]:
    if "layer_stack_root" not in args:
        raise ValueError("overlay.run requires layer_stack_root")
    capture = await _handle_snapshot_overlay(args)
    return capture.to_dict()


async def _handle_snapshot_overlay(args: dict[str, Any]) -> OverlayCapture:
    layer_stack = LayerStackClient(str(args["layer_stack_root"]))
    overlay_request = OverlayShellRequest.from_dict(_snapshot_request_payload(args))
    result = await execute_command(
        _command_request(
            overlay_request,
            layer_stack_root=layer_stack.storage_root,
            workspace_root=str(args.get("workspace_root") or "/workspace"),
        ),
        layer_stack=layer_stack,
        occ_client=None,
        storage_root=layer_stack.storage_root,
        occ_apply=False,
        mount_mode=MountMode.COPY_BACKED,
        command_runner=_run_overlay_command,
    )
    return OverlayCapture(
        exit_code=result.exit_code,
        stdout_ref=result.stdout_ref,
        stderr_ref=result.stderr_ref,
        snapshot_version=result.workspace_capture.snapshot_version,
        changes=tuple(result.workspace_capture.changes),
        snapshot_manifest=cast(
            Manifest | None,
            result.workspace_capture.snapshot_manifest,
        ),
        timings=result.timings,
    )


def _command_request(
    request: OverlayShellRequest,
    *,
    layer_stack_root: Path,
    workspace_root: str,
) -> CommandExecRequest:
    return CommandExecRequest(
        request_id=request.request_id,
        workspace_ref=layer_stack_root.as_posix(),
        workspace_root=workspace_root,
        command=request.command,
        cwd=request.cwd,
        env=request.env,
        timeout_seconds=request.timeout_seconds,
    )


def _run_overlay_command(
    *,
    spec: WorkspaceReplacementMountSpec,
    request: CommandExecRequest,
    run_dir: str | Path,
    timings: dict[str, float],
    mount_mode: MountMode | None = None,
) -> ShellProcessResult:
    return run_workspace_replaced_command(
        spec=spec,
        request=request,
        run_dir=run_dir,
        timings=timings,
        mount_mode=mount_mode,
        policy=_OVERLAY_COMMAND_POLICY,
    )


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
