"""Kernel-boundary overlay mount mechanics.

Parameter names use overlayfs vocabulary (lowerdir/upperdir/workdir) because
this is the file that calls the kernel mount API or mount(8) directly.

Ordering invariant (from Step 2a): first fsconfig(SET_STRING, "lowerdir+", path)
call = top priority. manifest.layers is newest-first, so iterate in natural order.
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
from sandbox.execution.overlay.new_mount_api import (
    AT_FDCWD,
    FSCONFIG_CMD_CREATE,
    FSCONFIG_SET_STRING,
    MOVE_MOUNT_F_EMPTY_PATH,
    OVL_MAX_STACK_GUARD,
    SYS_fsconfig,
    SYS_fsmount,
    SYS_fsopen,
    SYS_move_mount,
    LayerStackTooDeep,
    _get_libc,
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
            with suppress(OSError):
                os.close(fd)


def mount_overlay(
    *,
    workspace_root: Path,
    layer_paths: tuple[Path, ...],
    upperdir: Path,
    workdir: Path,
    pass_fds: tuple[int, ...] = (),
) -> None:
    """Mount an overlay filesystem using the new mount API (fsopen/fsconfig/fsmount).

    layer_paths must be ordered newest-first (first element = highest priority lower).
    Raises LayerStackTooDeep if len(layer_paths) > OVL_MAX_STACK_GUARD.
    """
    if len(layer_paths) > OVL_MAX_STACK_GUARD:
        raise LayerStackTooDeep(
            f"layer count {len(layer_paths)} exceeds OVL_MAX_STACK_GUARD={OVL_MAX_STACK_GUARD}"
        )

    libc = _get_libc()
    if libc is None:
        raise OSError("libc not found; cannot call fsopen")

    import ctypes

    fsfd: int = -1
    mfd: int = -1
    try:
        fsfd = libc.syscall(SYS_fsopen, b"overlay", 0)
        if fsfd < 0:
            err = ctypes.get_errno()
            raise OSError(err, os.strerror(err), "fsopen(overlay)")

        for layer in layer_paths:
            layer_bytes = os.fsencode(str(layer))
            ret = libc.syscall(
                SYS_fsconfig, fsfd, FSCONFIG_SET_STRING, b"lowerdir+", layer_bytes, 0
            )
            if ret < 0:
                err = ctypes.get_errno()
                raise OSError(err, os.strerror(err), f"fsconfig lowerdir+={layer}")

        upper_bytes = os.fsencode(str(upperdir))
        ret = libc.syscall(
            SYS_fsconfig, fsfd, FSCONFIG_SET_STRING, b"upperdir", upper_bytes, 0
        )
        if ret < 0:
            err = ctypes.get_errno()
            raise OSError(err, os.strerror(err), "fsconfig upperdir")

        work_bytes = os.fsencode(str(workdir))
        ret = libc.syscall(
            SYS_fsconfig, fsfd, FSCONFIG_SET_STRING, b"workdir", work_bytes, 0
        )
        if ret < 0:
            err = ctypes.get_errno()
            raise OSError(err, os.strerror(err), "fsconfig workdir")

        ret = libc.syscall(SYS_fsconfig, fsfd, FSCONFIG_CMD_CREATE, None, None, 0)
        if ret < 0:
            err = ctypes.get_errno()
            raise OSError(err, os.strerror(err), "fsconfig CMD_CREATE")

        mfd = libc.syscall(SYS_fsmount, fsfd, 0, 0)
        if mfd < 0:
            err = ctypes.get_errno()
            raise OSError(err, os.strerror(err), "fsmount")

        target_bytes = os.fsencode(str(workspace_root))
        ret = libc.syscall(
            SYS_move_mount,
            mfd,
            b"",
            AT_FDCWD,
            target_bytes,
            MOVE_MOUNT_F_EMPTY_PATH,
        )
        if ret < 0:
            err = ctypes.get_errno()
            raise OSError(err, os.strerror(err), f"move_mount -> {workspace_root}")
    finally:
        if mfd >= 0:
            with suppress(OSError):
                os.close(mfd)
        if fsfd >= 0:
            with suppress(OSError):
                os.close(fsfd)


def _mount_overlay_legacy_mount8(
    *,
    workspace_root: Path,
    lowerdir: Path,
    upperdir: Path,
    workdir: Path,
    pass_fds: tuple[int, ...] = (),
) -> None:
    """Legacy path used only by materialize fallback when probe_supported() == False.

    Slated for removal per ADR Follow-up #5 (sunset: Linux >= 5.11 floor by 2027-Q1).
    """
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
    layer_paths: tuple[Path, ...],
    upperdir: Path,
    workdir: Path,
    policy: CommandExecPolicy = DEFAULT_COMMAND_EXEC_POLICY,
) -> MountInputs:
    """Open O_DIRECTORY|O_NOFOLLOW fds for all paths; return /proc/self/fd-backed paths."""
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
                raise ValueError(f"mount scratch dir must not be a symlink: {path}")
            if path.exists() and not path.is_dir():
                raise ValueError(f"mount scratch path is not a directory: {path}")
            path.mkdir(parents=True, exist_ok=True)
            fds.append(_open_dir_no_follow(path))

        # fds layout: [workspace_root, *layer_paths, upperdir, workdir]
        layer_fd_paths = tuple(
            Path(f"/proc/self/fd/{fds[i + 1]}") for i in range(len(layer_paths))
        )
        return MountInputs(
            workspace_root=Path(f"/proc/self/fd/{fds[0]}"),
            layer_paths=layer_fd_paths,
            upperdir=Path(f"/proc/self/fd/{fds[len(layer_paths) + 1]}"),
            workdir=Path(f"/proc/self/fd/{fds[len(layer_paths) + 2]}"),
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
    "_mount_overlay_legacy_mount8",
    "mount_overlay",
    "umount",
    "validate_mount_inputs",
]
