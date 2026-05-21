"""Runtime-local command-exec server for guarded shell calls."""

from __future__ import annotations

from collections.abc import Mapping
from pathlib import Path
from uuid import uuid4

from sandbox.execution import (
    CommandExecRequest,
    CommandExecResult,
    OCCMutationClient,
    WorkspaceLeaseClient,
    execute_command,
    run_workspace_replaced_command,
)
from sandbox.layer_stack.workspace_binding import require_workspace_binding
from sandbox.occ.gitignore import SnapshotGitignoreOracle
from sandbox.daemon.occ_backend import build_occ_backend
from sandbox.daemon.request_context import require_layer_stack_root
from sandbox.daemon.result_projection import (
    conflict_and_status,
    conflict_to_dict,
    gitignore_cache_timings,
    published_paths,
)
from sandbox.daemon.service.sandbox_overlay import SandboxOverlay


async def execute_shell_api(args: dict[str, object]) -> dict[str, object]:
    """Public ``api.shell`` execution entrypoint used by the handler layer."""
    backend = build_occ_backend(require_layer_stack_root(args))
    result = await _execute_shell(
        args,
        layer_stack=backend.layer_stack,
        occ_client=backend.occ_client,
        gitignore=backend.gitignore,
        storage_root=backend.layer_stack.storage_root,
    )
    return _payload_from_result(result)


async def _execute_shell(
    args: Mapping[str, object],
    *,
    layer_stack: WorkspaceLeaseClient,
    occ_client: OCCMutationClient,
    gitignore: SnapshotGitignoreOracle,
    storage_root: Path,
) -> CommandExecResult:
    request = _command_request(args)
    overlay = SandboxOverlay(
        occ_client=occ_client,
        workspace_ref=request.workspace_ref,
        layer_stack=layer_stack,
        workspace_root=request.workspace_root,
    )
    return await execute_command(
        request,
        layer_stack=layer_stack,
        capture_publisher=overlay,
        storage_root=storage_root,
        timing_provider=lambda: gitignore_cache_timings(gitignore),
        command_runner=run_workspace_replaced_command,
    )


def _payload_from_result(result: CommandExecResult) -> dict[str, object]:
    changeset = result.occ_result
    files = getattr(changeset, "files", ())
    conflict, conflict_status = conflict_and_status(files)
    command_failed = result.exit_code != 0
    success = not command_failed and bool(getattr(changeset, "success", False))
    status = "ok" if success else conflict_status if conflict is not None else "error"
    return {
        "success": success,
        "exit_code": result.exit_code,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "changed_paths": list(published_paths(files)),
        "status": status,
        "conflict": conflict_to_dict(conflict),
        "conflict_reason": conflict.message if conflict is not None else None,
        "workspace_capture": {
            "snapshot_version": result.workspace_capture.snapshot_version,
            "mount_mode": result.workspace_capture.mount_mode,
            "changes": [
                change.to_dict() if hasattr(change, "to_dict") else str(change)
                for change in result.workspace_capture.changes
            ],
        },
        "warnings": [],
        "timings": result.timings,
    }


# WR-08: conservative argv-size cap below typical Linux ARG_MAX (~128 KiB).
# A caller pushing a large blob into a single argv element used to trip
# the kernel's E2BIG at exec time with an opaque OSError; this surfaces a
# structured ValueError before the syscall.
_MAX_ARGV_BYTES = 128 * 1024


def _command_request(args: Mapping[str, object]) -> CommandExecRequest:
    command = args.get("command")
    if isinstance(command, str):
        argv: tuple[str, ...] = ("bash", "-lc", command)
    elif isinstance(command, list):
        argv = tuple(str(part) for part in command)
    else:
        raise ValueError("command must be a string or argv list")
    argv_bytes = sum(len(part.encode("utf-8")) for part in argv) + len(argv)
    if argv_bytes > _MAX_ARGV_BYTES:
        raise ValueError(
            f"argv exceeds {_MAX_ARGV_BYTES} bytes ({argv_bytes}); "
            "stream large blobs via stdin instead"
        )
    timeout = args.get("timeout_seconds", args.get("timeout"))
    workspace_ref = require_layer_stack_root(args)
    binding = require_workspace_binding(workspace_ref)
    env = _safe_env(_mapping(args.get("env")))
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


def _safe_env(raw: Mapping[object, object]) -> dict[str, str]:
    """Validate caller env mapping; reject NUL / ``=`` / empty keys (WR-04)."""
    result: dict[str, str] = {}
    for k, v in raw.items():
        key = str(k)
        value = str(v)
        if not key:
            raise ValueError("env entry has empty key")
        if "\0" in key or "\0" in value:
            raise ValueError(f"env entry contains NUL byte: {key!r}")
        if "=" in key:
            # execvpe constructs `NAME=VALUE`; a `=` in NAME silently
            # corrupts the child env.
            raise ValueError(f"env key cannot contain '=': {key!r}")
        result[key] = value
    return result


def _mapping(value: object) -> Mapping[str, object]:
    return value if isinstance(value, Mapping) else {}


def _optional_float(value: object) -> float | None:
    if value is None:
        return None
    if isinstance(value, (str, int, float)):
        return float(value)
    raise TypeError(f"expected numeric value, got {type(value).__name__}")
