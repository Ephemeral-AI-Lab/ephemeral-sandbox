"""OCC state and patching limits."""

from __future__ import annotations

ARBITER_LOCK_TIMEOUT = 30.0
PATCHER_MAX_DIFF_SIZE = 100_000

__all__ = [
    "ARBITER_LOCK_TIMEOUT",
    "PATCHER_MAX_DIFF_SIZE",
]
