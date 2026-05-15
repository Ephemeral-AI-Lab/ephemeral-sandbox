"""Sandbox-wide timing primitives: clock, elapsed recording, payload shape.

Used by daemon, execution, layer_stack, occ, plugin, and host-side code that
needs the same monotonic clock and the same timing-payload normalization.
Audit-signal derivation lives in ``sandbox.audit.timing`` — this module is
intentionally free of audit knowledge.
"""

from __future__ import annotations

import time
from collections.abc import Mapping, MutableMapping
from enum import Enum


def monotonic_now() -> float:
    """Return the sandbox timing clock."""
    return time.perf_counter()


def record_elapsed(
    timings: MutableMapping[str, float] | None,
    key: str,
    started_at: float,
) -> float:
    """Record and return elapsed seconds for ``key`` when a timing map exists."""
    elapsed = monotonic_now() - started_at
    if timings is not None:
        timings[key] = elapsed
    return elapsed


def normalize_timing_map(raw: Mapping[object, object] | None) -> dict[str, float]:
    """Project arbitrary timing payloads into ``dict[str, float]``."""
    if not raw:
        return {}
    return {_timing_key_text(key): float(value) for key, value in raw.items()}


def _timing_key_text(key: object) -> str:
    if isinstance(key, Enum):
        return str(key.value)
    return str(key)


__all__ = [
    "monotonic_now",
    "normalize_timing_map",
    "record_elapsed",
]
