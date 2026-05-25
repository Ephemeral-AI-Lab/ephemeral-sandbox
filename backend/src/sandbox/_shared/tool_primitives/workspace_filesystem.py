"""Workspace filesystem safety helpers for tool primitives."""

from __future__ import annotations

import ctypes
import errno
import os
from collections.abc import Iterator
from pathlib import Path
import platform
import stat

_AT_FDCWD = -100
_RESOLVE_NO_SYMLINKS = 0x04


class _OpenHow(ctypes.Structure):
    _fields_ = [
        ("flags", ctypes.c_uint64),
        ("mode", ctypes.c_uint64),
        ("resolve", ctypes.c_uint64),
    ]


def open_no_follow(path: str | Path, flags: int, mode: int = 0o666) -> int:
    """Open a path while refusing every symlink component.

    ``os.open(path, O_NOFOLLOW)`` only protects the final component. This helper
    uses ``openat2(RESOLVE_NO_SYMLINKS)`` on Linux kernels that expose it,
    otherwise walks from ``/`` with ``dir_fd`` and ``O_DIRECTORY | O_NOFOLLOW``
    for each intermediate segment, preserving the daemon request-context policy.
    """
    parts = _absolute_parts(path)
    fd = _openat2_no_symlinks(str(Path(path)), flags, mode)
    if fd is not None:
        return fd
    return _open_by_component(parts, flags, mode, path)


def required_workspace_path(raw: object) -> str:
    """Resolve a required tool path inside the current workspace."""
    text = str(raw or "").strip()
    if not text:
        raise ValueError("path is required")
    return _resolve_workspace_path(Path(text), original=text)


def search_root_path(raw: object) -> str:
    """Resolve an optional grep/glob search root inside the current workspace."""
    text = str(raw or ".")
    return _resolve_workspace_path(Path(text), original=text)


def display_workspace_path(path: Path) -> str:
    """Display ``path`` relative to the current workspace when possible."""
    cwd = Path.cwd().resolve(strict=False)
    try:
        return path.resolve(strict=False).relative_to(cwd).as_posix()
    except ValueError:
        return path.as_posix()


def _open_by_component(
    parts: tuple[str, ...],
    flags: int,
    mode: int,
    original_path: str | Path,
) -> int:
    dir_fd = os.open("/", os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for segment in parts[:-1]:
            next_fd = os.open(
                segment,
                os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=dir_fd,
            )
            os.close(dir_fd)
            dir_fd = next_fd
        return os.open(parts[-1], flags | os.O_NOFOLLOW, mode, dir_fd=dir_fd)
    except OSError as exc:
        if exc.errno in {errno.ELOOP, errno.ENOTDIR}:
            raise ValueError(f"refusing to follow symlink: {original_path}") from exc
        raise
    finally:
        os.close(dir_fd)


def _openat2_no_symlinks(path: str, flags: int, mode: int) -> int | None:
    syscall_number = _openat2_syscall_number()
    if syscall_number is None:
        return None
    how = _OpenHow(
        flags=flags,
        mode=mode,
        resolve=_RESOLVE_NO_SYMLINKS,
    )
    libc = ctypes.CDLL(None, use_errno=True)
    raw_fd = libc.syscall(
        ctypes.c_long(syscall_number),
        ctypes.c_int(_AT_FDCWD),
        ctypes.c_char_p(os.fsencode(path)),
        ctypes.byref(how),
        ctypes.c_size_t(ctypes.sizeof(how)),
    )
    if raw_fd >= 0:
        return int(raw_fd)
    err = ctypes.get_errno()
    if err in {errno.ENOSYS, errno.EINVAL, errno.EPERM}:
        return None
    if err == errno.ELOOP:
        raise ValueError(f"refusing to follow symlink: {path}") from None
    raise OSError(err, os.strerror(err), path)


def _openat2_syscall_number() -> int | None:
    if platform.system() != "Linux":
        return None
    machine = platform.machine().lower()
    if machine in {"x86_64", "amd64"}:
        return 437
    if machine in {"aarch64", "arm64"}:
        return 56
    return None


def read_bytes_no_follow(path: str | Path) -> bytes:
    fd = open_no_follow(path, os.O_RDONLY)
    with os.fdopen(fd, "rb") as handle:
        return handle.read()


def is_regular_file_no_follow(path: str | Path) -> bool:
    try:
        fd = open_no_follow(path, os.O_RDONLY)
    except (OSError, ValueError):
        return False
    try:
        return stat.S_ISREG(os.fstat(fd).st_mode)
    finally:
        os.close(fd)


def write_bytes_no_follow(
    path: str | Path,
    data: bytes,
    *,
    overwrite: bool = True,
) -> None:
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    flags = os.O_WRONLY | os.O_CREAT
    flags |= os.O_TRUNC if overwrite else os.O_EXCL
    fd = open_no_follow(target, flags)
    with os.fdopen(fd, "wb") as handle:
        handle.write(data)


def write_text_no_follow(
    path: str | Path,
    content: str,
    *,
    create_only: bool = False,
) -> None:
    write_bytes_no_follow(
        path,
        content.encode("utf-8"),
        overwrite=not create_only,
    )


def walk_dirs_no_follow(root: str | Path) -> Iterator[Path]:
    """Yield files under ``root`` without descending through symlink dirs."""
    for current_root, dirs, files in os.walk(root, followlinks=False):
        dirs[:] = [name for name in dirs if not (Path(current_root) / name).is_symlink()]
        for name in files:
            path = Path(current_root) / name
            if not path.is_symlink():
                yield path


def _absolute_parts(path: str | Path) -> tuple[str, ...]:
    candidate = Path(path)
    if not candidate.is_absolute():
        raise ValueError(f"path must be absolute: {path!r}")
    parts = tuple(part for part in candidate.parts if part not in ("", "/"))
    if not parts:
        raise ValueError("path must not be filesystem root")
    if any(part in (".", "..") for part in parts):
        raise ValueError(f"path contains unsafe segment: {path!r}")
    return parts


def _resolve_workspace_path(candidate: Path, *, original: str) -> str:
    if not candidate.is_absolute():
        if ".." in candidate.parts:
            raise ValueError(f"path escapes workspace via '..': {original}")
        candidate = Path.cwd() / candidate
    return candidate.as_posix()


__all__ = [
    "display_workspace_path",
    "open_no_follow",
    "is_regular_file_no_follow",
    "read_bytes_no_follow",
    "required_workspace_path",
    "search_root_path",
    "walk_dirs_no_follow",
    "write_bytes_no_follow",
    "write_text_no_follow",
]
