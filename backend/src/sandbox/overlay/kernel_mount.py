"""Kernel-boundary overlay mount mechanics.

Parameter names use overlayfs vocabulary (lowerdir/upperdir/workdir) because
this is the file that calls the Linux mount syscalls directly.

Ordering invariant (from Step 2a): first fsconfig(SET_STRING, "lowerdir+", path)
call = top priority. manifest.layers is newest-first, so iterate in natural order.
"""

from __future__ import annotations

import os
import subprocess
from contextlib import suppress
from dataclasses import dataclass
from pathlib import Path

from sandbox._shared.command_exec_policy import (
    DEFAULT_COMMAND_EXEC_POLICY,
    CommandExecPolicy,
)
from sandbox.overlay.mount_syscalls import (
    fsconfig_create,
    fsconfig_string,
    fsmount,
    fsopen,
    move_mount,
)


@dataclass(frozen=True)
class MountInputs:
    workspace_root: Path
    layer_paths: tuple[Path, ...]
    upperdir: Path
    workdir: Path
    fds: tuple[int, ...]

    def close(self) -> None:
        for fd in self.fds:
            _close_fd(fd)


def _close_fd(fd: int) -> None:
    with suppress(OSError):
        os.close(fd)


def mount_overlay(
    *,
    workspace_root: Path,
    layer_paths: tuple[Path, ...],
    upperdir: Path,
    workdir: Path,
) -> None:
    """Mount an overlay filesystem using fsopen/fsconfig/fsmount.

    layer_paths must be ordered newest-first (first element = highest priority lower).
    """
    fsfd: int = -1
    mfd: int = -1
    try:
        fsfd = fsopen(b"overlay")
        for layer in layer_paths:
            fsconfig_string(fsfd, b"lowerdir+", os.fsencode(str(layer)))
        fsconfig_string(fsfd, b"upperdir", os.fsencode(str(upperdir)))
        fsconfig_string(fsfd, b"workdir", os.fsencode(str(workdir)))
        fsconfig_create(fsfd)
        mfd = fsmount(fsfd)
        move_mount(mfd, os.fsencode(str(workspace_root)))
    finally:
        if mfd >= 0:
            _close_fd(mfd)
        if fsfd >= 0:
            _close_fd(fsfd)


def umount(
    workspace_root: Path,
    *,
    lazy: bool = False,
    raise_on_failure: bool = False,
) -> None:
    """Unmount all mounts stacked at ``workspace_root``.

    Persistent daemon overlays may be remounted across runtime-bundle upgrades
    or interrupted tests. A single ``umount`` only peels the top mount; loop
    until the path is no longer a mountpoint so the backing checkout is visible
    to raw provider setup commands again.

    ``lazy`` falls back to ``umount -l`` when a normal umount returns non-zero,
    matching the LSP namespace-remount detach contract. ``raise_on_failure``
    raises ``RuntimeError`` instead of silently returning when the path remains
    a mountpoint after exhausting available strategies; default
    ``(False, False)`` keeps the non-raising behavior used by teardown callers.
    """
    for _ in range(64):
        if not _is_mountpoint(workspace_root):
            return
        result = subprocess.run(
            ["umount", str(workspace_root)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if result.returncode == 0:
            continue
        if lazy:
            lazy_result = subprocess.run(
                ["umount", "-l", str(workspace_root)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
            if lazy_result.returncode == 0:
                return
        if raise_on_failure:
            raise RuntimeError(f"failed to detach existing mount: {workspace_root}")
        return
    if raise_on_failure and _is_mountpoint(workspace_root):
        raise RuntimeError(f"failed to detach existing mount: {workspace_root}")


def _is_mountpoint(path: Path) -> bool:
    try:
        result = subprocess.run(
            ["mountpoint", "-q", str(path)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
    except OSError:
        return True
    return result.returncode == 0


def validate_mount_inputs(
    *,
    workspace_root: Path,
    layer_paths: tuple[Path, ...],
    upperdir: Path,
    workdir: Path,
    policy: CommandExecPolicy = DEFAULT_COMMAND_EXEC_POLICY,
) -> MountInputs:
    """Open O_DIRECTORY|O_NOFOLLOW fds and return mount-safe input paths.

    The overlay source directories are passed through fd-backed paths to pin
    the validated objects. The target mountpoint remains the real path because
    move_mount(2) does not accept a /proc/self/fd symlink as the destination
    path in the namespace helper.
    """
    fds: list[int] = []
    try:
        all_paths = (workspace_root,) + layer_paths + (upperdir, workdir)
        for path in all_paths:
            policy.validate_overlay_path_text(path.as_posix())

        if workspace_root.is_symlink():
            raise ValueError(f"workspace root must not be a symlink: {workspace_root}")
        if not workspace_root.is_dir():
            raise ValueError(f"workspace root is missing: {workspace_root}")
        fds.append(_open_dir_no_follow(workspace_root))

        for layer in layer_paths:
            if layer.is_symlink():
                raise ValueError(f"leased lowerdir must not be a symlink: {layer}")
            if not layer.is_dir():
                raise ValueError(f"leased lowerdir is missing: {layer}")
            fds.append(_open_dir_no_follow(layer))

        for path in (upperdir, workdir):
            if path.is_symlink():
                raise ValueError(f"overlay upper/work dir must not be a symlink: {path}")
            if path.exists() and not path.is_dir():
                raise ValueError(f"overlay upper/work path is not a directory: {path}")
            path.mkdir(parents=True, exist_ok=True)
            fds.append(_open_dir_no_follow(path))

        # fds layout: [workspace_root, *layer_paths, upperdir, workdir]
        layer_fd_paths = tuple(Path(f"/proc/self/fd/{fds[i + 1]}") for i in range(len(layer_paths)))
        return MountInputs(
            workspace_root=workspace_root,
            layer_paths=layer_fd_paths,
            upperdir=Path(f"/proc/self/fd/{fds[len(layer_paths) + 1]}"),
            workdir=Path(f"/proc/self/fd/{fds[len(layer_paths) + 2]}"),
            fds=tuple(fds),
        )
    except Exception:
        for fd in fds:
            _close_fd(fd)
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
