"""Subprocess execution and cwd resolution for workspace-replaced commands."""

from __future__ import annotations

import os
import signal
import subprocess
import threading
import time
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path

from sandbox._shared.command_exec_policy import (
    DEFAULT_COMMAND_EXEC_POLICY,
    CommandExecPolicy,
)

# Polling step for cancel-aware subprocess wait. Small enough to keep
# cancel-to-SIGTERM latency low (AC-3: ≤100 ms next mount), large enough to
# keep CPU overhead negligible for long-running shells.
_CANCEL_POLL_INTERVAL_S = 0.1
# Grace window between SIGTERM (cancel observed) and SIGKILL escalation.
_CANCEL_SIGKILL_GRACE_S = 2.0


def kill_process_group(pid: int, signal_number: int) -> None:
    try:
        os.killpg(pid, signal_number)
    except (ProcessLookupError, PermissionError):
        pass


def resolve_workspace_cwd(
    *,
    declared_workspace_root: str | Path,
    mounted_workspace_root: str | Path,
    cwd: str,
) -> Path:
    """Resolve *cwd* after replacing the declared workspace path.

    Absolute paths must stay under the declared workspace root. Relative paths
    resolve inside the mounted workspace. The returned path is inside
    ``mounted_workspace_root`` so namespace callers and direct unit-test mounts
    share the same policy.
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
        raise ValueError(f"cwd escapes workspace replacement root: {raw}") from exc

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
    cancel_event: threading.Event | None = None,
    pid_recorder: Callable[[int], None] | None = None,
) -> int:
    """Run a subprocess with stdout/stderr captured to ref files.

    If a timeout fires and ``timeout_exit_code`` is ``None`` the
    ``subprocess.TimeoutExpired`` propagates; otherwise that exit code is
    returned so callers can distinguish a user-command timeout from a real
    exit (e.g. GNU `timeout(1)` uses 124).

    Optional background-shell plumbing: when ``cancel_event`` is provided the
    wait loop polls it on a 100 ms tick and escalates SIGTERM → SIGKILL on
    set. ``pid_recorder`` is invoked once with the child's PGID (== PID because
    ``start_new_session``) so the daemon can ``killpg`` from outside this
    blocking call.
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
        if pid_recorder is not None:
            try:
                pid_recorder(proc.pid)
            except Exception:
                # Recorder failures must not break the wait loop; the daemon
                # will fall back to its own TTL reaper to clean up.
                pass
        try:
            try:
                return wait_for_process_with_cancel(
                    proc,
                    timeout_seconds=timeout_seconds,
                    cancel_event=cancel_event,
                )
            except subprocess.TimeoutExpired:
                kill_process_group(proc.pid, signal.SIGKILL)
                try:
                    proc.wait(timeout=2.0)
                except subprocess.TimeoutExpired:
                    pass
                if timeout_exit_code is None:
                    raise
                return timeout_exit_code
        finally:
            if proc.poll() is None:
                kill_process_group(proc.pid, signal.SIGKILL)


def wait_for_process_with_cancel(
    proc: subprocess.Popen,
    *,
    timeout_seconds: float | None,
    cancel_event: threading.Event | None,
) -> int:
    """Wait for ``proc``, honoring ``cancel_event`` if provided.

    Without ``cancel_event``, falls through to plain ``proc.wait`` so the
    foreground path keeps its single ``proc.wait(timeout=...)`` syscall — no
    100 ms polling tax on synchronous shells.

    Public because :class:`PrivateNamespaceStrategy` reuses this for its outer
    unshare process (the kernel-mount holder); we want the same SIGTERM+grace
    semantics there as the inner bash.
    """
    if cancel_event is None:
        return int(proc.wait(timeout=timeout_seconds))

    deadline = None if timeout_seconds is None else time.monotonic() + float(timeout_seconds)
    while True:
        if cancel_event.is_set():
            kill_process_group(proc.pid, signal.SIGTERM)
            try:
                return int(proc.wait(timeout=_CANCEL_SIGKILL_GRACE_S))
            except subprocess.TimeoutExpired:
                kill_process_group(proc.pid, signal.SIGKILL)
                try:
                    return int(proc.wait(timeout=2.0))
                except subprocess.TimeoutExpired:
                    return -int(signal.SIGKILL)
        rc = proc.poll()
        if rc is not None:
            return int(rc)
        if deadline is not None and time.monotonic() > deadline:
            raise subprocess.TimeoutExpired(proc.args, timeout_seconds)
        cancel_event.wait(timeout=_CANCEL_POLL_INTERVAL_S)


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
    cancel_event: threading.Event | None = None,
    pid_recorder: Callable[[int], None] | None = None,
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
        cancel_event=cancel_event,
        pid_recorder=pid_recorder,
    )


def _relative_to_declared_workspace(candidate: Path, declared_root: Path) -> Path:
    candidate_text = os.path.normpath(candidate.as_posix())
    root_text = os.path.normpath(declared_root.as_posix())
    if os.path.commonpath([root_text, candidate_text]) != root_text:
        raise ValueError(f"cwd escapes workspace replacement root: {candidate}")
    return Path(candidate_text).relative_to(root_text)


__all__ = [
    "kill_process_group",
    "resolve_workspace_cwd",
    "run_command_to_refs",
    "subprocess_to_refs",
    "wait_for_process_with_cancel",
]
