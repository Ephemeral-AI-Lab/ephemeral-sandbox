"""Workspace replacement mount implementation for guarded commands."""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, replace
from functools import lru_cache
from pathlib import Path

from sandbox.command_exec.workspace.environment import run_command_to_refs
from sandbox.command_exec.contract.request import CommandExecRequest
from sandbox.command_exec.contract.result import ShellProcessResult


@dataclass(frozen=True)
class WorkspaceReplacementMountSpec:
    """Filesystem inputs for replacing the assigned workspace root."""

    workspace_root: str
    lowerdir: str
    upperdir: str
    workdir: str
    manifest_version: int
    lease_id: str

    def __post_init__(self) -> None:
        if not str(self.workspace_root).startswith("/"):
            raise ValueError("workspace_root must be absolute")
        for field_name in ("lowerdir", "upperdir", "workdir", "lease_id"):
            if not str(getattr(self, field_name)).strip():
                raise ValueError(f"{field_name} must not be empty")


def run_workspace_replaced_command(
    *,
    spec: WorkspaceReplacementMountSpec,
    request: CommandExecRequest,
    run_dir: str | Path,
    timings: dict[str, float],
) -> ShellProcessResult:
    """Run a command with the assigned workspace replaced by the leased view."""
    run_root = Path(run_dir)
    run_root.mkdir(parents=True, exist_ok=True)
    if _private_mount_namespace_available():
        return _run_private_mount_namespace(
            spec=spec,
            request=request,
            run_dir=run_root,
            timings=timings,
        )
    return _run_copy_backed_mount(
        spec=spec,
        request=request,
        run_dir=run_root,
        timings=timings,
    )


def _run_copy_backed_mount(
    *,
    spec: WorkspaceReplacementMountSpec,
    request: CommandExecRequest,
    run_dir: Path,
    timings: dict[str, float],
) -> ShellProcessResult:
    lowerdir = Path(spec.lowerdir)
    upperdir = Path(spec.upperdir)
    workdir = Path(spec.workdir)
    merged = run_dir / "workspace"
    stdout_ref = run_dir / "stdout.bin"
    stderr_ref = run_dir / "stderr.bin"

    mount_start = time.perf_counter()
    for directory in (upperdir, workdir, merged):
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)
    if lowerdir.exists():
        shutil.copytree(lowerdir, merged, symlinks=True, dirs_exist_ok=True)
    timings["command_exec.mount_workspace_s"] = time.perf_counter() - mount_start

    run_request = replace(
        request,
        command=_rewrite_declared_workspace_refs(
            request.command,
            workspace_root=spec.workspace_root,
            mounted_workspace_root=str(merged),
        ),
    )
    run_start = time.perf_counter()
    exit_code = run_command_to_refs(
        command=run_request.command,
        declared_workspace_root=spec.workspace_root,
        mounted_workspace_root=merged,
        cwd=run_request.cwd,
        env=run_request.env,
        timeout_seconds=run_request.timeout_seconds,
        stdout_ref=stdout_ref,
        stderr_ref=stderr_ref,
    )
    timings["command_exec.run_command_s"] = time.perf_counter() - run_start
    return ShellProcessResult(
        exit_code=exit_code,
        stdout_ref=str(stdout_ref),
        stderr_ref=str(stderr_ref),
        mounted_workspace_root=str(merged),
        mount_mode="copy_backed",
    )


def _run_private_mount_namespace(
    *,
    spec: WorkspaceReplacementMountSpec,
    request: CommandExecRequest,
    run_dir: Path,
    timings: dict[str, float],
) -> ShellProcessResult:
    stdout_ref = run_dir / "stdout.bin"
    stderr_ref = run_dir / "stderr.bin"
    timings_ref = run_dir / "namespace-timings.json"
    payload_ref = run_dir / "namespace-request.json"
    payload_ref.write_text(
        json.dumps(
            {
                "workspace_root": spec.workspace_root,
                "lowerdir": spec.lowerdir,
                "upperdir": spec.upperdir,
                "workdir": spec.workdir,
                "command": list(request.command),
                "cwd": request.cwd,
                "env": dict(request.env),
                "timeout_seconds": request.timeout_seconds,
                "stdout_ref": str(stdout_ref),
                "stderr_ref": str(stderr_ref),
                "timings_ref": str(timings_ref),
            },
            separators=(",", ":"),
            sort_keys=True,
        ),
        encoding="utf-8",
    )
    timeout = None if request.timeout_seconds is None else request.timeout_seconds + 10
    completed = subprocess.run(
        [
            _unshare_path(),
            "-Urm",
            sys.executable,
            "-m",
            "sandbox.command_exec.workspace.namespace_entrypoint",
            str(payload_ref),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )
    _ensure_refs(stdout_ref, stderr_ref, completed)
    _merge_namespace_timings(timings_ref, timings)
    return ShellProcessResult(
        exit_code=int(completed.returncode),
        stdout_ref=str(stdout_ref),
        stderr_ref=str(stderr_ref),
        mounted_workspace_root=spec.workspace_root,
        mount_mode="private_namespace",
    )


def _rewrite_declared_workspace_refs(
    command: tuple[str, ...],
    workspace_root: str,
    mounted_workspace_root: str,
) -> tuple[str, ...]:
    """Map absolute workspace references to the copy-backed mounted tree.

    The copy-backed fallback cannot replace `/testbed` in the process mount
    namespace. Rewriting only the declared workspace-root token preserves file
    operation semantics for commands that use absolute `/testbed/...` paths
    while keeping `/tmp`, `/root`, and the rest of the sandbox filesystem as
    provider passthrough.
    """
    root = str(workspace_root).rstrip("/") or "/"
    if root == "/":
        return command
    pattern = re.compile(rf"{re.escape(root)}(?=/|$|[\s'\":;,&|)])")
    return tuple(pattern.sub(str(mounted_workspace_root), part) for part in command)


def _merge_namespace_timings(path: Path, timings: dict[str, float]) -> None:
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return
    if not isinstance(raw, dict):
        return
    for key, value in raw.items():
        if isinstance(value, (int, float)):
            timings[str(key)] = float(value)


def _ensure_refs(
    stdout_ref: Path,
    stderr_ref: Path,
    completed: subprocess.CompletedProcess[bytes],
) -> None:
    if not stdout_ref.exists():
        stdout_ref.parent.mkdir(parents=True, exist_ok=True)
        stdout_ref.write_bytes(completed.stdout or b"")
    if not stderr_ref.exists():
        stderr_ref.parent.mkdir(parents=True, exist_ok=True)
        stderr_ref.write_bytes(completed.stderr or b"")


@lru_cache(maxsize=1)
def _private_mount_namespace_available() -> bool:
    if os.name != "posix" or not sys.platform.startswith("linux"):
        return False
    if _unshare_path() == "" or shutil.which("mount") is None:
        return False
    try:
        result = subprocess.run(
            [_unshare_path(), "-Urm", "true"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=2,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return False
    return result.returncode == 0


def _unshare_path() -> str:
    return shutil.which("unshare") or ""


__all__ = [
    "WorkspaceReplacementMountSpec",
    "run_workspace_replaced_command",
]
