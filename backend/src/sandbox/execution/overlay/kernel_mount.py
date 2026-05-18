"""Kernel-boundary overlay mount mechanics.

Parameter names use overlayfs vocabulary (lowerdir/upperdir/workdir) because
this is the file that calls mount(8) / umount(8) directly.
"""

from __future__ import annotations

import os
import subprocess
from contextlib import suppress
from dataclasses import dataclass
from pathlib import Path

from sandbox.execution.env_policy import (
    DEFAULT_COMMAND_EXEC_POLICY,
    CommandExecPolicy,
)


@dataclass(frozen=True)
class MountInputs:
    workspace_root: Path
    lowerdir: Path
    upperdir: Path
    workdir: Path
    fds: tuple[int, ...]

    def close(self) -> None:
        for fd in self.fds:
            with suppress(OSError):
                os.close(fd)


def mount_overlay(
    *,
    workspace_root: Path,
    lowerdir: Path,
    upperdir: Path,
    workdir: Path,
    pass_fds: tuple[int, ...] = (),
) -> None:
    options = f"lowerdir={lowerdir},upperdir={upperdir},workdir={workdir}"
    subprocess.run(
        ["mount", "-t", "overlay", "overlay", "-o", options, str(workspace_root)],
        check=True,
        capture_output=True,
        text=True,
        pass_fds=pass_fds,
    )


def umount(workspace_root: Path) -> None:
    subprocess.run(
        ["umount", str(workspace_root)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )


def validate_mount_inputs(
    *,
    workspace_root: Path,
    lowerdir: Path,
    upperdir: Path,
    workdir: Path,
    policy: CommandExecPolicy = DEFAULT_COMMAND_EXEC_POLICY,
) -> MountInputs:
    fds: list[int] = []
    try:
        for path in (workspace_root, lowerdir, upperdir, workdir):
            policy.validate_overlay_path_text(path.as_posix())
        for path, label in (
            (workspace_root, "workspace root"),
            (lowerdir, "leased lowerdir"),
        ):
            if path.is_symlink():
                raise ValueError(f"{label} must not be a symlink: {path}")
            if not path.is_dir():
                raise ValueError(f"{label} is missing: {path}")
            fds.append(_open_dir_no_follow(path))
        for path in (upperdir, workdir):
            if path.is_symlink():
                raise ValueError(f"mount scratch dir must not be a symlink: {path}")
            if path.exists() and not path.is_dir():
                raise ValueError(f"mount scratch path is not a directory: {path}")
            path.mkdir(parents=True, exist_ok=True)
            fds.append(_open_dir_no_follow(path))
        return MountInputs(
            workspace_root=Path(f"/proc/self/fd/{fds[0]}"),
            lowerdir=Path(f"/proc/self/fd/{fds[1]}"),
            upperdir=Path(f"/proc/self/fd/{fds[2]}"),
            workdir=Path(f"/proc/self/fd/{fds[3]}"),
            fds=tuple(fds),
        )
    except Exception:
        for fd in fds:
            with suppress(OSError):
                os.close(fd)
        raise


def _open_dir_no_follow(path: Path) -> int:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    return os.open(path, flags)


__all__ = [
    "MountInputs",
    "mount_overlay",
    "umount",
    "validate_mount_inputs",
]
