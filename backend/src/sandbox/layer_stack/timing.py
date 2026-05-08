"""Timing metric helpers for layer-stack operations."""

from __future__ import annotations

import time


def record_elapsed(
    timings: dict[str, float] | None,
    key: str,
    started_at: float,
) -> None:
    if timings is not None:
        timings[key] = time.perf_counter() - started_at


__all__ = ["record_elapsed"]
