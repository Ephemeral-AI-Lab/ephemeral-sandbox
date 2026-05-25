"""Linux mount syscall ctypes wrappers for overlayfs.

Syscall numbers (x86_64 and aarch64 share the same generic ABI values):
  Source: arch/x86/entry/syscalls/syscall_64.tbl (kernel v5.2+)
          arch/arm64/include/uapi/asm/unistd.h (generic ABI, same numbers)

  move_mount  429
  fsopen      430
  fsconfig    431
  fsmount     432

These numbers have been stable on both architectures since Linux 5.2.
A unit test asserts equality across arch entries so a future arch addition
that diverges will fail loudly instead of silently regressing.

The kernel hard limit OVL_MAX_STACK = 500 is the only overlay layer ceiling;
runtime code does not maintain a separate depth ceiling.
"""

from __future__ import annotations

import ctypes
import ctypes.util
import errno
import logging
import os
import sys
from functools import cache
from typing import NoReturn

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Syscall numbers — x86_64 and aarch64 generic ABI (identical values)
# ---------------------------------------------------------------------------

SYS_move_mount: int = 429
SYS_fsopen: int = 430
SYS_fsconfig: int = 431
SYS_fsmount: int = 432

# ---------------------------------------------------------------------------
# fsconfig cmd constants (linux/mount.h)
# ---------------------------------------------------------------------------

FSCONFIG_SET_STRING: int = 1
FSCONFIG_CMD_CREATE: int = 6

# ---------------------------------------------------------------------------
# move_mount flags (linux/mount.h)
# ---------------------------------------------------------------------------

MOVE_MOUNT_F_EMPTY_PATH: int = 0x00000004

# ---------------------------------------------------------------------------
# AT_FDCWD
# ---------------------------------------------------------------------------

AT_FDCWD: int = -100

OVL_MAX_STACK: int = 500


# ---------------------------------------------------------------------------
# Exceptions
# ---------------------------------------------------------------------------


class MountSyscallsUnavailable(OSError):
    """Raised when required mount syscalls are not accessible."""


# ---------------------------------------------------------------------------
# libc handle
# ---------------------------------------------------------------------------


@cache
def _get_libc() -> ctypes.CDLL | None:
    name = ctypes.util.find_library("c")
    if name is None:
        return None
    try:
        return ctypes.CDLL(name, use_errno=True)
    except OSError:
        return None


# ---------------------------------------------------------------------------
# Probe (cached)
# ---------------------------------------------------------------------------


@cache
def probe_supported() -> bool:
    """Return True if fsopen/fsconfig/fsmount/move_mount are usable on this host.

    Handles three negative cases distinctly:
      ENOSYS — kernel too old (< 5.2) or syscall not compiled in
      EPERM  — seccomp/cgroup/capability denial
      EBADF  — caller-context misconfiguration
    All three return False with a distinct structured log line.
    """
    if sys.platform != "linux":
        logger.debug("overlay.mount_syscalls.unavailable platform=%s", sys.platform)
        return False

    libc = _get_libc()
    if libc is None:
        logger.warning("overlay.mount_syscalls.unavailable reason=libc_not_found")
        return False

    # Try fsopen("overlay", 0). On success we get a fd >= 0 that we close.
    # On the probe host we may or may not have CAP_SYS_ADMIN; we only need
    # the syscall to be reachable (not ENOSYS/EPERM).
    result = libc.syscall(SYS_fsopen, b"overlay", 0)
    if result >= 0:
        try:
            os.close(result)
        except OSError:
            pass
        return True

    err = ctypes.get_errno()
    if err == errno.ENOSYS:
        logger.warning("overlay.mount_syscalls.unavailable errno=ENOSYS reason=kernel_too_old")
    elif err == errno.EPERM:
        logger.warning(
            "overlay.mount_syscalls.unavailable errno=EPERM reason=capability_or_seccomp_denial"
        )
    elif err == errno.EBADF:
        logger.warning(
            "overlay.mount_syscalls.unavailable errno=EBADF reason=caller_context_misconfig"
        )
    else:
        logger.warning("overlay.mount_syscalls.unavailable errno=%d reason=unknown", err)
    return False


# ---------------------------------------------------------------------------
# Raw syscall wrappers
# ---------------------------------------------------------------------------


def _libc_or_raise() -> ctypes.CDLL:
    libc = _get_libc()
    if libc is None:
        raise MountSyscallsUnavailable("libc not found") from None
    return libc


def _raise_last_os_error(filename: object | None = None) -> NoReturn:
    err = ctypes.get_errno()
    if filename is None:
        raise OSError(err, os.strerror(err))
    raise OSError(err, os.strerror(err), filename)


def fsopen(fsname: bytes) -> int:
    """Call fsopen(2) for the given filesystem name; return the fs context fd."""
    libc = _libc_or_raise()
    fd = libc.syscall(SYS_fsopen, fsname, 0)
    if fd < 0:
        _raise_last_os_error(fsname)
    return fd


def fsconfig_string(fd: int, key: bytes, value: bytes) -> None:
    """Call fsconfig(2) with FSCONFIG_SET_STRING to set a string parameter."""
    libc = _libc_or_raise()
    ret = libc.syscall(SYS_fsconfig, fd, FSCONFIG_SET_STRING, key, value, 0)
    if ret < 0:
        _raise_last_os_error(f"{key!r}={value!r}")


def fsconfig_create(fd: int) -> None:
    """Call fsconfig(2) with FSCONFIG_CMD_CREATE to instantiate the fs."""
    libc = _libc_or_raise()
    ret = libc.syscall(SYS_fsconfig, fd, FSCONFIG_CMD_CREATE, None, None, 0)
    if ret < 0:
        _raise_last_os_error()


def fsmount(fsfd: int) -> int:
    """Call fsmount(2); return a mount fd."""
    libc = _libc_or_raise()
    mfd = libc.syscall(SYS_fsmount, fsfd, 0, 0)
    if mfd < 0:
        _raise_last_os_error()
    return mfd


def move_mount(from_fd: int, target_path: bytes) -> None:
    """Call move_mount(2) with MOVE_MOUNT_F_EMPTY_PATH to attach mount fd to target."""
    libc = _libc_or_raise()
    ret = libc.syscall(
        SYS_move_mount,
        from_fd,
        b"",
        AT_FDCWD,
        target_path,
        MOVE_MOUNT_F_EMPTY_PATH,
    )
    if ret < 0:
        _raise_last_os_error(target_path)


__all__ = [
    "SYS_fsopen",
    "SYS_fsconfig",
    "SYS_fsmount",
    "SYS_move_mount",
    "FSCONFIG_SET_STRING",
    "FSCONFIG_CMD_CREATE",
    "MOVE_MOUNT_F_EMPTY_PATH",
    "AT_FDCWD",
    "OVL_MAX_STACK",
    "MountSyscallsUnavailable",
    "probe_supported",
    "fsopen",
    "fsconfig_string",
    "fsconfig_create",
    "fsmount",
    "move_mount",
]
