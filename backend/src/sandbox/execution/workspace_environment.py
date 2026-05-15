"""cwd and environment policy for workspace-replaced commands."""

from __future__ import annotations

import os
import signal
import subprocess
from collections.abc import Mapping, Sequence
from pathlib import Path

from sandbox.execution.env_policy import (
    DEFAULT_COMMAND_EXEC_POLICY,
    CommandExecPolicy,
)


def resolve_workspace_cwd(
    *,
    declared_workspace_root: str | Path,
    mounted_workspace_root: str | Path,
    cwd: str,
) -> Path:
    """Resolve *cwd* after replacing the declared workspace path.

    Absolute paths must stay under the declared workspace root. Relative paths
    resolve inside the mounted workspace. The returned path is inside
    ``mounted_workspace_root`` so copy-backed test mounts and real namespace
    mounts share the same policy.
    """
    declared_root = Path(declared_workspace_root)
    mounted_root = Path(mounted_workspace_root)
    raw = str(cwd or ".").strip() or "."
    candidate = Path(raw)
    if candidate.is_absolute():
        rel = _relative_to_declared_workspace(candidate, declared_root)
        resolved = mounted_root / rel
    else:
        resolved = mounted_root / candidate

    # Belt-and-suspenders containment check: the request boundary already
    # rejects `..` in relative cwd, but verify the resolved path still falls
    # inside the mounted workspace root before any side effect (mkdir).
    mounted_root_resolved = mounted_root.resolve(strict=False)
    resolved_check = resolved.resolve(strict=False)
    try:
        resolved_check.relative_to(mounted_root_resolved)
    except ValueError as exc:
        raise ValueError(
            f"cwd escapes workspace replacement root: {raw}"
        ) from exc

    resolved.mkdir(parents=True, exist_ok=True)
    return resolved


def subprocess_to_refs(
    *,
    command: Sequence[str],
    cwd: Path,
    env: Mapping[str, str],
    timeout_seconds: float | None,
    stdout_ref: str | Path,
    stderr_ref: str | Path,
    timeout_exit_code: int | None = None,
) -> int:
    """Run a subprocess with stdout/stderr captured to ref files.

    If a timeout fires and ``timeout_exit_code`` is ``None`` the
    ``subprocess.TimeoutExpired`` propagates; otherwise that exit code is
    returned so callers can distinguish a user-command timeout from a real
    exit (e.g. GNU `timeout(1)` uses 124).
    """
    stdout_path = Path(stdout_ref)
    stderr_path = Path(stderr_ref)
    stdout_path.parent.mkdir(parents=True, exist_ok=True)
    stderr_path.parent.mkdir(parents=True, exist_ok=True)
    with stdout_path.open("wb") as stdout_file, stderr_path.open("wb") as stderr_file:
        # start_new_session=True puts the child into its own process group so
        # we can SIGKILL the whole tree on timeout — `subprocess.run` would
        # leave grandchildren (e.g. `bash -c "sleep 1000 &"`) alive otherwise.
        proc = subprocess.Popen(
            list(command),
            cwd=cwd,
            env=dict(env),
            stdout=stdout_file,
            stderr=stderr_file,
            start_new_session=True,
        )
        try:
            try:
                return int(proc.wait(timeout=timeout_seconds))
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
                try:
                    proc.wait(timeout=2.0)
                except subprocess.TimeoutExpired:
                    pass
                if timeout_exit_code is None:
                    raise
                return timeout_exit_code
        finally:
            if proc.poll() is None:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass


def run_command_to_refs(
    *,
    command: Sequence[str],
    declared_workspace_root: str | Path,
    mounted_workspace_root: str | Path,
    cwd: str,
    env: Mapping[str, str],
    timeout_seconds: float | None,
    stdout_ref: str | Path,
    stderr_ref: str | Path,
    policy: CommandExecPolicy = DEFAULT_COMMAND_EXEC_POLICY,
) -> int:
    """Run a guarded command and write stdout/stderr to reference files."""
    resolved_cwd = resolve_workspace_cwd(
        declared_workspace_root=declared_workspace_root,
        mounted_workspace_root=mounted_workspace_root,
        cwd=cwd,
    )
    return subprocess_to_refs(
        command=command,
        cwd=resolved_cwd,
        env=policy.command_environment(env),
        timeout_seconds=timeout_seconds,
        stdout_ref=stdout_ref,
        stderr_ref=stderr_ref,
    )


def _relative_to_declared_workspace(candidate: Path, declared_root: Path) -> Path:
    candidate_text = os.path.normpath(candidate.as_posix())
    root_text = os.path.normpath(declared_root.as_posix())
    if os.path.commonpath([root_text, candidate_text]) != root_text:
        raise ValueError(f"cwd escapes workspace replacement root: {candidate}")
    return Path(candidate_text).relative_to(root_text)


__all__ = [
    "resolve_workspace_cwd",
    "run_command_to_refs",
    "subprocess_to_refs",
]
