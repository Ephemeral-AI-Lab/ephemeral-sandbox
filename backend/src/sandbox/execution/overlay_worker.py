"""Worker entrypoint for one command against a leased snapshot overlay.

Also hosts the in-worker subprocess wrapper (`run_user_command`,
`OverlayCommandResult`) because it has exactly one consumer
(`execute_request` below). Keeping it here avoids the pipeline→worker
import cycle that an earlier consolidation introduced.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

from sandbox.layer_stack.manifest import Manifest
from sandbox.execution.overlay_capture import capture_changes
from sandbox.execution.overlay_mounts import cleanup_runtime_run_dir, mount_snapshot
from sandbox.execution.overlay_request import OverlayShellRequest
from sandbox.execution.overlay_result import OverlayCapture, write_overlay_capture
from sandbox.execution.workspace_environment import subprocess_to_refs
from sandbox.timing import monotonic_now


# Host env vars that the user command needs to function (PATH for argv0
# resolution, HOME/TERM for shells, locale vars for tooling that branches on
# encoding). Host secrets are intentionally absent from this allow-list.
_HOST_ENV_ALLOWLIST: tuple[str, ...] = (
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "TERM",
    "TZ",
)


@dataclass(frozen=True)
class OverlayCommandResult:
    exit_code: int
    stdout_ref: str
    stderr_ref: str


def run_user_command(
    *,
    command: tuple[str, ...],
    workspace_root: str | Path,
    cwd: str,
    env: dict[str, str],
    timeout_seconds: float | None,
    stdout_ref: str | Path,
    stderr_ref: str | Path,
) -> OverlayCommandResult:
    root = Path(workspace_root).resolve()
    candidate = Path(cwd)
    if not candidate.is_absolute():
        candidate = root / candidate
    resolved_cwd = candidate.resolve()
    if not resolved_cwd.is_relative_to(root):
        raise ValueError(f"cwd escapes mounted workspace: {cwd!r}")
    resolved_cwd.mkdir(parents=True, exist_ok=True)

    child_env = {
        **{k: os.environ[k] for k in _HOST_ENV_ALLOWLIST if k in os.environ},
        **env,
        "GIT_OPTIONAL_LOCKS": "0",
    }

    stdout_path = Path(stdout_ref)
    stderr_path = Path(stderr_ref)
    exit_code = subprocess_to_refs(
        command=command,
        cwd=resolved_cwd,
        env=child_env,
        timeout_seconds=timeout_seconds,
        stdout_ref=stdout_path,
        stderr_ref=stderr_path,
        # 124 follows GNU `timeout(1)` so callers can distinguish a
        # user-command timeout from infrastructure failure.
        timeout_exit_code=124,
    )
    return OverlayCommandResult(
        exit_code=exit_code,
        stdout_ref=str(stdout_path),
        stderr_ref=str(stderr_path),
    )


def execute_request(
    *,
    request: OverlayShellRequest,
    manifest: Manifest,
    storage_root: str | Path,
    run_dir: str | Path,
) -> OverlayCapture:
    total_start = monotonic_now()
    timings: dict[str, float] = {}
    run_dir_path = Path(run_dir)
    try:
        mount_start = monotonic_now()
        mounted = mount_snapshot(
            manifest=manifest,
            storage_root=storage_root,
            run_dir=run_dir,
            timings=timings,
        )
        timings["overlay.mount_snapshot_s"] = monotonic_now() - mount_start
        command_start = monotonic_now()
        command = run_user_command(
            command=request.command,
            workspace_root=mounted.workspace_root,
            cwd=request.cwd,
            env=dict(request.env),
            timeout_seconds=request.timeout_seconds,
            stdout_ref=run_dir_path / "stdout.bin",
            stderr_ref=run_dir_path / "stderr.bin",
        )
        timings["overlay.run_command_s"] = monotonic_now() - command_start
        capture_start = monotonic_now()
        changes = capture_changes(
            mounted.upperdir,
            lowerdir=mounted.lowerdir,
            workspace_root=mounted.workspace_root,
            timings=timings,
        )
        timings["overlay.capture_changes_s"] = monotonic_now() - capture_start
        timings["overlay.total_s"] = monotonic_now() - total_start
        capture = OverlayCapture(
            exit_code=command.exit_code,
            stdout_ref=command.stdout_ref,
            stderr_ref=command.stderr_ref,
            snapshot_version=manifest.version,
            changes=changes,
            snapshot_manifest=manifest,
            timings=timings,
        )
        write_overlay_capture(run_dir, capture)
        return capture
    finally:
        cleanup_runtime_run_dir(run_dir_path)


__all__ = [
    "OverlayCommandResult",
    "execute_request",
    "run_user_command",
]
