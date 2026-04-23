"""Configuration for the overlay shell sandbox.

See ``docs/architecture/overlay-sandbox-plan.md`` §0. Two knobs:

* ``EOS_OVERLAY_MAX_CONCURRENT`` — per-sandbox ``asyncio.Semaphore`` size
  that caps parallel overlay operations. Default 20.
* ``EOS_OVERLAY_UPPER_SIZE_MB`` — tmpfs size cap for each op's upperdir.
  Default 512 MiB. Memory ceiling per sandbox is
  ``max_concurrent * upper_size_mb``.
"""

from __future__ import annotations

import os

DEFAULT_OVERLAY_MAX_CONCURRENT = 20
DEFAULT_OVERLAY_UPPER_SIZE_MB = 512

_ENV_MAX_CONCURRENT = "EOS_OVERLAY_MAX_CONCURRENT"
_ENV_UPPER_SIZE_MB = "EOS_OVERLAY_UPPER_SIZE_MB"


def _positive_int(raw: str, default: int) -> int:
    stripped = (raw or "").strip()
    if not stripped:
        return default
    try:
        value = int(stripped)
    except ValueError:
        return default
    if value <= 0:
        return default
    return value


def overlay_max_concurrent() -> int:
    """Per-sandbox cap on parallel overlay operations."""
    return _positive_int(
        os.environ.get(_ENV_MAX_CONCURRENT, ""),
        DEFAULT_OVERLAY_MAX_CONCURRENT,
    )


def overlay_upper_size_mb() -> int:
    """Tmpfs size cap (MiB) for the per-op upperdir."""
    return _positive_int(
        os.environ.get(_ENV_UPPER_SIZE_MB, ""),
        DEFAULT_OVERLAY_UPPER_SIZE_MB,
    )


__all__ = [
    "DEFAULT_OVERLAY_MAX_CONCURRENT",
    "DEFAULT_OVERLAY_UPPER_SIZE_MB",
    "overlay_max_concurrent",
    "overlay_upper_size_mb",
]
