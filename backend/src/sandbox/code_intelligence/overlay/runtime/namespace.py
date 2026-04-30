"""Namespace mount setup for the sandbox-side overlay runtime."""

from __future__ import annotations

import os
import subprocess
from typing import Any

_NS_ROOT = "/tmp/eos-shell-ns"
_NS_TMP = "/tmp/eos-shell-ns/tmp"
_NS_UPPER = "/tmp/eos-shell-ns/tmp/upper"
_NS_WORK = "/tmp/eos-shell-ns/tmp/work"
_NS_LOWER = "/tmp/eos-shell-ns/lower"
_NS_MERGED = "/tmp/eos-shell-ns/merged"


class OverlayMountError(RuntimeError):
    """Raised when the namespace mount setup fails."""


def _run(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs
    )


def setup_mounts(*, live_root: str, upper_size_mb: int) -> None:
    """Mount the overlay stack inside the namespace."""
    for directory in (_NS_ROOT, _NS_TMP, _NS_LOWER, _NS_MERGED):
        os.makedirs(directory, exist_ok=True)
    _check(
        _run(
            [
                "mount",
                "-t",
                "tmpfs",
                "-o",
                f"size={upper_size_mb}m",
                "tmpfs",
                _NS_TMP,
            ]
        ),
        step="tmpfs /ns/tmp",
    )
    for directory in (_NS_UPPER, _NS_WORK):
        os.makedirs(directory, exist_ok=True)
    _check(
        _run(["mount", "--bind", live_root, _NS_LOWER]),
        step=f"bind {live_root} -> /ns/lower",
    )
    overlay_opts = (
        f"lowerdir={_NS_LOWER},upperdir={_NS_UPPER},"
        f"workdir={_NS_WORK},userxattr"
    )
    _check(
        _run(["mount", "-t", "overlay", "overlay", "-o", overlay_opts, _NS_MERGED]),
        step="mount overlay",
    )
    _check(
        _run(["mount", "--bind", _NS_MERGED, live_root]),
        step=f"bind /ns/merged -> {live_root}",
    )


def _check(proc: subprocess.CompletedProcess[bytes], *, step: str) -> None:
    if proc.returncode == 0:
        return
    stderr = proc.stderr.decode("utf-8", "replace")
    raise OverlayMountError(f"{step}: rc={proc.returncode} stderr={stderr!r}")


__all__ = [
    "OverlayMountError",
    "_NS_LOWER",
    "_NS_MERGED",
    "_NS_ROOT",
    "_NS_TMP",
    "_NS_UPPER",
    "_NS_WORK",
    "setup_mounts",
]
