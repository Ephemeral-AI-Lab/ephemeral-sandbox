"""Workspace replacement mount implementation for guarded commands."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from collections.abc import Mapping
from dataclasses import replace
from functools import lru_cache
from pathlib import Path

from sandbox.command_exec.contract.request import CommandExecRequest
from sandbox.command_exec.contract.result import MountMode, ShellProcessResult
from sandbox.command_exec.contract.spec import WorkspaceReplacementMountSpec
from sandbox.command_exec.workspace.environment import run_command_to_refs
from sandbox.timing import monotonic_now


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
        process = _run_private_mount_namespace(
            spec=spec,
            request=request,
            run_dir=run_root,
            timings=timings,
        )
        if not _is_namespace_mount_failure(process):
            return process
        timings["command_exec.private_mount_fallback"] = 1.0
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

    mount_start = monotonic_now()
    for directory in (upperdir, workdir, merged):
        _assert_under_scratch_root(directory, spec)
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)
    if lowerdir.exists():
        shutil.copytree(lowerdir, merged, symlinks=True, dirs_exist_ok=True)
    timings["command_exec.mount_workspace_s"] = monotonic_now() - mount_start

    run_request = replace(
        request,
        command=_rewrite_declared_workspace_refs(
            request.command,
            workspace_root=spec.workspace_root,
            mounted_workspace_root=str(merged),
        ),
        env=_rewrite_declared_workspace_env(
            request.env,
            workspace_root=spec.workspace_root,
            mounted_workspace_root=str(merged),
        ),
    )
    run_start = monotonic_now()
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
    timings["command_exec.run_command_s"] = monotonic_now() - run_start
    return ShellProcessResult(
        exit_code=exit_code,
        stdout_ref=str(stdout_ref),
        stderr_ref=str(stderr_ref),
        mounted_workspace_root=str(merged),
        mount_mode=MountMode.COPY_BACKED,
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
    stdout_ref.parent.mkdir(parents=True, exist_ok=True)
    stderr_ref.parent.mkdir(parents=True, exist_ok=True)
    with stdout_ref.open("wb") as stdout_file, stderr_ref.open("wb") as stderr_file:
        completed = subprocess.run(
            [
                _unshare_path(),
                "-Urm",
                sys.executable,
                "-m",
                "sandbox.command_exec.workspace.namespace_entrypoint",
                str(payload_ref),
            ],
            stdout=stdout_file,
            stderr=stderr_file,
            timeout=timeout,
            check=False,
        )
    _merge_namespace_timings(timings_ref, timings)
    return ShellProcessResult(
        exit_code=int(completed.returncode),
        stdout_ref=str(stdout_ref),
        stderr_ref=str(stderr_ref),
        mounted_workspace_root=spec.workspace_root,
        mount_mode=MountMode.PRIVATE_NAMESPACE,
    )


def _rewrite_declared_workspace_refs(
    command: tuple[str, ...],
    workspace_root: str,
    mounted_workspace_root: str,
) -> tuple[str, ...]:
    """Map path-like workspace references to the copy-backed mounted tree.

    The copy-backed fallback cannot replace `/testbed` in the process mount
    namespace, so absolute workspace paths must point at the temporary merged
    tree. Shell users commonly quote those paths, so quotes are treated as
    path boundaries instead of as "do not rewrite" regions.
    """
    root = str(workspace_root).rstrip("/") or "/"
    if root == "/":
        return command
    return tuple(
        _rewrite_workspace_paths(
            part,
            workspace_root=root,
            mounted_workspace_root=str(mounted_workspace_root),
        )
        for part in command
    )


_WORKSPACE_ENV_KEYS = frozenset({"WORKSPACE_DIR", "PWD", "OLDPWD"})


def _rewrite_declared_workspace_env(
    env: Mapping[str, str],
    *,
    workspace_root: str,
    mounted_workspace_root: str,
) -> dict[str, str]:
    """Rewrite env values that explicitly name the assigned workspace."""
    root = str(workspace_root).rstrip("/") or "/"
    rewritten: dict[str, str] = {}
    for key, value in env.items():
        env_key = str(key)
        env_value = str(value)
        if env_key in _WORKSPACE_ENV_KEYS:
            env_value = _rewrite_path_token(
                env_value,
                workspace_root=root,
                mounted_workspace_root=mounted_workspace_root,
            )
        rewritten[env_key] = env_value
    return rewritten


def _rewrite_workspace_paths(
    value: str,
    *,
    workspace_root: str,
    mounted_workspace_root: str,
) -> str:
    result: list[str] = []
    index = 0
    while index < len(value):
        if _path_starts_at(value, index, workspace_root):
            result.append(mounted_workspace_root)
            index += len(workspace_root)
            continue
        result.append(value[index])
        index += 1
    return "".join(result)


def _rewrite_path_token(
    value: str,
    *,
    workspace_root: str,
    mounted_workspace_root: str,
) -> str:
    if value == workspace_root:
        return mounted_workspace_root
    if value.startswith(workspace_root + "/"):
        return mounted_workspace_root + value[len(workspace_root):]
    return value


def _path_starts_at(value: str, index: int, workspace_root: str) -> bool:
    if not value.startswith(workspace_root, index):
        return False
    before = value[index - 1] if index > 0 else ""
    after_index = index + len(workspace_root)
    after = value[after_index] if after_index < len(value) else ""
    if before and before not in " \t\n\r=:;,&|>(\"'":
        return False
    return not after or after in "/ \t\n\r:;,&|)<\"'"


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


def _is_namespace_mount_failure(process: ShellProcessResult) -> bool:
    if (
        process.mount_mode != MountMode.PRIVATE_NAMESPACE
        or process.exit_code != 126
    ):
        return False
    try:
        lines = Path(process.stderr_ref).read_text(encoding="utf-8").splitlines()
    except OSError:
        return False
    for line in lines:
        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(payload, dict) and payload.get("error_kind") == "mount_failed":
            return True
    return False


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


def _assert_under_scratch_root(path: Path, spec: WorkspaceReplacementMountSpec) -> None:
    scratch_root = Path(spec.scratch_root).resolve(strict=False)
    resolved = path.resolve(strict=False)
    if not resolved.is_relative_to(scratch_root):
        raise RuntimeError(f"path escapes scratch root: {resolved}")


__all__ = [
    "WorkspaceReplacementMountSpec",
    "run_workspace_replaced_command",
]
