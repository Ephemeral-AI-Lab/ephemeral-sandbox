"""Helper executed inside a private mount namespace."""

from __future__ import annotations

import json
import os
import sys
from collections.abc import Mapping
from dataclasses import asdict, dataclass, is_dataclass
from enum import StrEnum
from pathlib import Path
from typing import Any

from sandbox._shared.clock import monotonic_now
from sandbox._shared.command_exec_policy import CommandExecPolicy
from sandbox._shared.models import Intent, ToolCallRequest
from sandbox._shared.tool_primitives import VERB_TABLE, shell
from sandbox.overlay.kernel_mount import (
    MountInputs,
    mount_overlay,
    umount,
    validate_mount_inputs,
)


class WorkspaceMountMode(StrEnum):
    MOUNT_OVERLAY = "mount_overlay"
    EXISTING_MOUNT = "existing_mount"


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if len(args) != 1:
        sys.stderr.write("namespace helper requires one JSON payload path\n")
        return 2
    payload = json.loads(Path(args[0]).read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        sys.stderr.write("namespace helper payload must be an object\n")
        return 2
    return execute(payload)


def execute(payload: dict[str, Any]) -> int:
    result_ref_raw = payload.get("result_ref")
    if not result_ref_raw:
        sys.stderr.write("namespace helper payload is missing result_ref\n")
        return 2
    result_ref = Path(str(result_ref_raw))
    result = execute_tool_payload_safely(payload)
    result_ref.parent.mkdir(parents=True, exist_ok=True)
    result_ref.write_text(
        json.dumps(result, separators=(",", ":"), sort_keys=True),
        encoding="utf-8",
    )
    return 0


@dataclass(frozen=True)
class _OverlayMountRequest:
    workspace_root: Path
    layer_paths: tuple[Path, ...]
    upperdir: Path
    workdir: Path
    stdout_ref: Path
    stderr_ref: Path
    timings_ref: Path
    policy: CommandExecPolicy


def _overlay_mount_request(payload: dict[str, Any]) -> _OverlayMountRequest:
    raw_layers = payload["layer_paths"]
    if not isinstance(raw_layers, list) or not raw_layers:
        raise ValueError(f"layer_paths must be a non-empty list; got {raw_layers!r}")
    layer_paths = tuple(Path(str(p)) for p in raw_layers)
    return _OverlayMountRequest(
        workspace_root=Path(str(payload["workspace_root"])),
        layer_paths=layer_paths,
        upperdir=Path(str(payload["upperdir"])),
        workdir=Path(str(payload["workdir"])),
        stdout_ref=Path(str(payload["stdout_ref"])),
        stderr_ref=Path(str(payload["stderr_ref"])),
        timings_ref=Path(str(payload["timings_ref"])),
        policy=CommandExecPolicy.from_payload(
            payload["policy"] if isinstance(payload.get("policy"), dict) else {}
        ),
    )


def execute_tool_payload_safely(
    payload: dict[str, Any],
    *,
    timings: dict[str, float] | None = None,
) -> dict[str, Any]:
    local_timings = timings if timings is not None else {}
    mount_inputs: MountInputs | None = None
    mount_request: _OverlayMountRequest | None = None
    try:
        workspace_mount_mode = _workspace_mount_mode(payload)
        if workspace_mount_mode is WorkspaceMountMode.MOUNT_OVERLAY:
            mount_request = _overlay_mount_request(payload)
            mount_inputs = validate_mount_inputs(
                workspace_root=mount_request.workspace_root,
                layer_paths=mount_request.layer_paths,
                upperdir=mount_request.upperdir,
                workdir=mount_request.workdir,
                policy=mount_request.policy,
            )
            mount_start = monotonic_now()
            mount_overlay(
                workspace_root=mount_inputs.workspace_root,
                layer_paths=mount_inputs.layer_paths,
                upperdir=mount_inputs.upperdir,
                workdir=mount_inputs.workdir,
            )
            local_timings["workspace.mount_s"] = monotonic_now() - mount_start
        return execute_tool_payload(payload, timings=local_timings)
    except Exception as exc:
        return {
            "success": False,
            "status": "error",
            "error": {
                "kind": type(exc).__name__,
                "message": str(exc),
            },
            "timings": local_timings,
        }
    finally:
        if mount_inputs is not None:
            mount_inputs.close()
        if mount_request is not None:
            umount(mount_request.workspace_root)
            _write_timings(mount_request.timings_ref, local_timings)


def _workspace_mount_mode(payload: dict[str, Any]) -> WorkspaceMountMode:
    raw = payload.get("workspace_mount_mode")
    try:
        return WorkspaceMountMode(str(raw))
    except ValueError as exc:
        allowed = ", ".join(mode.value for mode in WorkspaceMountMode)
        raise ValueError(f"workspace_mount_mode must be one of: {allowed}; got {raw!r}") from exc


def execute_tool_payload(
    payload: dict[str, Any],
    *,
    timings: dict[str, float] | None = None,
) -> dict[str, Any]:
    req = ToolCallRequest.from_payload(payload["tool_call"])
    workspace_root = str(payload.get("workspace_root") or "/testbed")
    old_cwd = os.getcwd()
    run_start = monotonic_now()
    try:
        os.chdir(workspace_root)
        denied = _check_host_denylist(req)
        if denied is not None:
            result_payload = denied
        elif req.verb == "shell":
            result = shell.run(
                _shell_argv(req.args),
                workspace_root=workspace_root,
                cwd=str(req.args.get("cwd") or "."),
                env=_string_mapping(req.args.get("env")),
                timeout_seconds=_optional_float(
                    req.args.get("timeout_seconds", req.args.get("timeout"))
                ),
                stdout_ref=Path(str(payload["stdout_ref"])),
                stderr_ref=Path(str(payload["stderr_ref"])),
                policy=CommandExecPolicy.from_payload(
                    payload["policy"] if isinstance(payload.get("policy"), dict) else {}
                ),
            )
            result_payload = _jsonable_result(result)
        else:
            run_primitive = VERB_TABLE[req.verb]
            result_payload = _jsonable_result(run_primitive(req.args))
    finally:
        os.chdir(old_cwd)
    elapsed = monotonic_now() - run_start
    result_payload.setdefault("success", True)
    result_payload.setdefault("status", "ok" if result_payload.get("success") else "error")
    result_payload.setdefault("workspace", "ephemeral")
    result_payload.setdefault("timings", {})
    if isinstance(result_payload["timings"], dict):
        result_payload["timings"]["workspace.tool_s"] = elapsed
        if timings:
            result_payload["timings"].update(timings)
    return result_payload


_HOST_DENYLIST_PREFIXES = ("/etc/", "/var/", "/proc/", "/sys/", "/boot/")


def _check_host_denylist(req: ToolCallRequest) -> dict[str, Any] | None:
    if req.intent != Intent.WRITE_ALLOWED and req.verb not in {
        "write_file",
        "edit_file",
        "shell",
    }:
        return None
    target = str(req.args.get("path") or req.args.get("cwd") or "")
    if not target:
        return None
    if any(
        target == prefix.rstrip("/") or target.startswith(prefix)
        for prefix in _HOST_DENYLIST_PREFIXES
    ):
        return {
            "success": False,
            "status": "error",
            "error": {
                "kind": "forbidden_host_path",
                "path": target,
                "message": "writes to system paths are denied inside the namespace child",
            },
            "changed_paths": [],
            "timings": {},
        }
    return None


def _shell_argv(args: Any) -> list[str]:
    if not isinstance(args, dict):
        raise ValueError("shell args must be an object")
    command = args.get("command")
    if isinstance(command, list):
        return [str(part) for part in command]
    if isinstance(command, str):
        return ["bash", "-lc", command]
    raise ValueError("command must be a string or argv list")


def _string_mapping(raw: object) -> dict[str, str]:
    if not isinstance(raw, dict):
        return {}
    return {str(key): str(value) for key, value in raw.items()}


def _optional_float(raw: object) -> float | None:
    if raw is None:
        return None
    return float(raw)


def _jsonable_result(value: Any) -> dict[str, Any]:
    if is_dataclass(value) and not isinstance(value, type):
        return {str(k): _jsonable(v) for k, v in asdict(value).items()}
    if isinstance(value, Mapping):
        return {str(k): _jsonable(v) for k, v in value.items()}
    raise TypeError(f"tool primitive returned non-object result: {type(value).__name__}")


def _jsonable(value: Any) -> Any:
    if is_dataclass(value) and not isinstance(value, type):
        return {str(k): _jsonable(v) for k, v in asdict(value).items()}
    if isinstance(value, Mapping):
        return {str(k): _jsonable(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_jsonable(v) for v in value]
    return value


def _write_timings(path: Path, timings: dict[str, float]) -> None:
    path.write_text(
        json.dumps(timings, separators=(",", ":"), sort_keys=True),
        encoding="utf-8",
    )


__all__ = [
    "execute",
    "execute_tool_payload",
    "execute_tool_payload_safely",
    "main",
    "WorkspaceMountMode",
]


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
