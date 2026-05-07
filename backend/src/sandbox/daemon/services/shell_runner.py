"""Runtime-local command-exec server for guarded shell calls."""

from __future__ import annotations

import os
import shutil
import time
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import cast
from uuid import uuid4

from sandbox.api.tool.result_projection import (
    conflict_and_status,
    published_paths,
)
from sandbox.command_exec.capture.changeset import workspace_changes_to_occ_changes
from sandbox.command_exec.capture.upperdir import capture_workspace_upperdir
from sandbox.command_exec.clients import OCCMutationClient, WorkspaceLeaseClient
from sandbox.command_exec.request import CommandExecRequest
from sandbox.command_exec.result import CommandExecResult, WorkspaceCapture
from sandbox.command_exec.workspace_mount import (
    WorkspaceReplacementMountSpec,
    run_workspace_replaced_command,
)
from sandbox.layer_stack.workspace import require_workspace_binding
from sandbox.occ.changeset.prepared import CommitOptions, PreparedChangeset
from sandbox.occ.changeset.types import ChangesetResult
from sandbox.occ.content.gitignore_oracle import SnapshotGitignoreOracle
from sandbox.overlay.capture.types import read_output_ref
from sandbox.daemon.services import occ_backend
from sandbox.utils.async_bridge import run_sync_in_executor


async def execute_shell_api(args: dict[str, object]) -> dict[str, object]:
    """Public ``api.shell`` execution entrypoint used by the handler layer."""
    layer_stack, occ_client, gitignore, storage_root = _services(args)
    result = await _execute_shell(
        args,
        layer_stack=layer_stack,
        occ_client=occ_client,
        gitignore=gitignore,
        storage_root=storage_root,
    )
    return _payload_from_result(result)


async def _execute_shell(
    args: Mapping[str, object],
    *,
    layer_stack: WorkspaceLeaseClient,
    occ_client: OCCMutationClient,
    gitignore: "SnapshotGitignoreOracle",
    storage_root: Path,
) -> CommandExecResult:
    total_start = time.perf_counter()
    request = _command_request(args)
    run_dir = _run_dir(storage_root, request.request_id)
    timings: dict[str, float] = {}
    timings["command_exec.handler_sync_prelude_s"] = (
        time.perf_counter() - total_start
    )

    lease_start = time.perf_counter()
    lease = layer_stack.prepare_workspace_snapshot(
        workspace_ref=request.workspace_ref,
        request_id=request.request_id,
    )
    timings.update(
        {
            **lease.timings,
            "command_exec.prepare_snapshot_s": time.perf_counter() - lease_start,
        }
    )

    released = False
    try:
        spec = WorkspaceReplacementMountSpec(
            workspace_root=request.workspace_root,
            lowerdir=lease.lowerdir,
            upperdir=str(run_dir / "upper"),
            workdir=str(run_dir / "work"),
            manifest_version=lease.manifest_version,
            lease_id=lease.lease_id,
        )
        process = await run_sync_in_executor(
            run_workspace_replaced_command,
            spec=spec,
            request=request,
            run_dir=run_dir,
            timings=timings,
        )

        capture_start = time.perf_counter()
        path_changes = tuple(
            capture_workspace_upperdir(
                spec=spec,
                snapshot_manifest=lease.manifest,
                mounted_workspace_root=process.mounted_workspace_root,
                copy_backed=process.mount_mode == "copy_backed",
                timings=timings,
            )
        )
        timings["command_exec.capture_upperdir_s"] = (
            time.perf_counter() - capture_start
        )

        occ_start = time.perf_counter()
        changeset = await _apply_workspace_capture(
            path_changes,
            occ_client=occ_client,
            snapshot=lease.manifest,
            request=request,
        )
        timings["command_exec.occ_apply_s"] = time.perf_counter() - occ_start
        release_start = time.perf_counter()
        layer_stack.release_lease(
            workspace_ref=request.workspace_ref,
            lease_id=lease.lease_id,
        )
        released = True
        _drop_transient_lowerdir(lease)
        timings["command_exec.release_snapshot_s"] = (
            time.perf_counter() - release_start
        )
        timings = {
            **timings,
            **changeset.timings,
            **_gitignore_timings(gitignore),
        }
        timings["api.shell.overlay_s"] = (
            timings.get("command_exec.mount_workspace_s", 0.0)
            + timings.get("command_exec.run_command_s", 0.0)
            + timings.get("command_exec.capture_upperdir_s", 0.0)
        )
        timings["api.shell.occ_apply_s"] = timings["command_exec.occ_apply_s"]
        timings["command_exec.total_s"] = time.perf_counter() - total_start
        timings["api.shell.total_s"] = timings["command_exec.total_s"]
        return CommandExecResult(
            exit_code=process.exit_code,
            stdout=read_output_ref(process.stdout_ref),
            stderr=read_output_ref(process.stderr_ref),
            workspace_capture=WorkspaceCapture(
                changes=path_changes,
                snapshot_version=lease.manifest_version,
                mount_mode=process.mount_mode,
            ),
            occ_result=changeset,
            timings=timings,
        )
    finally:
        if not released:
            release_start = time.perf_counter()
            layer_stack.release_lease(
                workspace_ref=request.workspace_ref,
                lease_id=lease.lease_id,
            )
            _drop_transient_lowerdir(lease)
            timings["command_exec.release_snapshot_s"] = (
                time.perf_counter() - release_start
            )
        # Phase 3 improvement #1: drop the run_dir tree so /dev/shm stays
        # bounded across long-running daemons. Capture and OCC commit are
        # done by the time we get here; the tree is no longer load-bearing.
        # ignore_errors=True keeps cleanup non-fatal — a stale dir cannot
        # mask a real exception from the try-block.
        shutil.rmtree(run_dir, ignore_errors=True)


async def _apply_workspace_capture(
    path_changes: Sequence[object],
    *,
    occ_client: OCCMutationClient,
    snapshot: object,
    request: CommandExecRequest,
) -> ChangesetResult:
    typed_changes = workspace_changes_to_occ_changes(path_changes)  # type: ignore[arg-type]
    if not typed_changes:
        return ChangesetResult(
            files=(),
            timings={},
            published_manifest_version=None,
        )
    # Single-path captures opt out of cross-path atomicity so
    # ``OccSerialMerger._disjoint_batches`` can coalesce them with other
    # concurrent disjoint commits. Multi-path captures keep ``atomic=True``
    # so a single failed validation rejects the whole capture.
    distinct_paths = {change.path for change in typed_changes}
    is_atomic = len(distinct_paths) > 1
    result = await occ_client.apply_changeset(
        typed_changes,
        snapshot=snapshot,
        options=CommitOptions(
            atomic=is_atomic,
            caller_id=request.actor_id,
            description=request.description,
        ),
        workspace_ref=request.workspace_ref,
    )
    if isinstance(result, PreparedChangeset):
        raise TypeError("command-exec OCC client returned an uncommitted changeset")
    return result


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
        "conflict": _conflict_to_dict(conflict),
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


def _services(
    args: Mapping[str, object],
) -> tuple[
    WorkspaceLeaseClient,
    OCCMutationClient,
    "SnapshotGitignoreOracle",
    Path,
]:
    backend = occ_backend.build_occ_backend(_layer_stack_root(args))
    return cast(
        tuple[
            WorkspaceLeaseClient,
            OCCMutationClient,
            "SnapshotGitignoreOracle",
            Path,
        ],
        (
            backend.layer_stack,
            backend.occ_client,
            backend.gitignore,
            backend.layer_stack.storage_root,
        ),
    )


def _command_request(args: Mapping[str, object]) -> CommandExecRequest:
    command = args.get("command")
    if isinstance(command, str):
        argv: tuple[str, ...] = ("bash", "-lc", command)
    elif isinstance(command, list):
        argv = tuple(str(part) for part in command)
    else:
        raise ValueError("command must be a string or argv list")
    timeout = args.get("timeout_seconds", args.get("timeout"))
    workspace_ref = _layer_stack_root(args)
    binding = require_workspace_binding(workspace_ref)
    return CommandExecRequest(
        request_id=str(args.get("request_id") or uuid4().hex),
        workspace_ref=workspace_ref,
        workspace_root=binding.workspace_root,
        command=argv,
        cwd=str(args.get("cwd") or "."),
        env={str(k): str(v) for k, v in _mapping(args.get("env")).items()},
        timeout_seconds=_optional_float(timeout),
        actor_id=str(args.get("actor_id") or ""),
        description=str(args.get("description") or "shell"),
    )


def _layer_stack_root(args: Mapping[str, object]) -> str:
    layer_stack_root = str(args.get("layer_stack_root") or "").strip()
    if not layer_stack_root:
        raise ValueError("layer_stack_root is required")
    return layer_stack_root


def _run_dir(storage_root: Path, request_id: str) -> Path:
    safe_id = "".join(
        char if char.isalnum() or char in ("-", "_") else "-"
        for char in request_id
    ).strip("-")
    run_parent = _command_exec_runtime_root(storage_root)
    return run_parent / f"{safe_id or 'request'}-{uuid4().hex[:8]}"


def _command_exec_runtime_root(storage_root: Path) -> Path:
    shm = Path("/dev/shm")
    if shm.is_dir() and os.access(shm, os.W_OK):
        root_key = "".join(
            ch if ch.isalnum() else "-"
            for ch in str(storage_root.resolve(strict=False))
        ).strip("-")
        return shm / "eos-command-exec" / (root_key[-48:] or "layer-stack")
    return storage_root / "runtime" / "command_exec"


def _drop_transient_lowerdir(lease: object) -> None:
    raw = str(getattr(lease, "lowerdir", "")).strip()
    if not raw:
        return
    lowerdir = Path(raw)
    shutil.rmtree(lowerdir.parent, ignore_errors=True)


def _gitignore_timings(
    gitignore: "SnapshotGitignoreOracle",
) -> dict[str, float]:
    return {
        "gitignore.cache_hits_total": float(gitignore.cache_hits),
        "gitignore.cache_misses_total": float(gitignore.cache_misses),
    }


def _conflict_to_dict(conflict: object | None) -> dict[str, object] | None:
    if conflict is None:
        return None
    return {
        "reason": getattr(conflict, "reason", ""),
        "conflict_file": getattr(conflict, "conflict_file", None),
        "message": getattr(conflict, "message", ""),
    }


def _mapping(value: object) -> Mapping[str, object]:
    return value if isinstance(value, Mapping) else {}


def _optional_float(value: object) -> float | None:
    if value is None:
        return None
    if isinstance(value, (str, int, float)):
        return float(value)
    raise TypeError(f"expected numeric value, got {type(value).__name__}")
