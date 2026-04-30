"""Shared data structures for language-server clients."""

from __future__ import annotations

import threading
from dataclasses import dataclass
from typing import Any


@dataclass
class _CacheEntry:
    """Cached LSP query result."""

    result: Any
    expires_at: float


@dataclass
class _InflightQuery:
    """One in-progress cached query shared by concurrent callers."""

    event: threading.Event
    result: Any = None
    error: BaseException | None = None


@dataclass
class LspTelemetry:
    """LSP client telemetry."""

    queries: int = 0
    errors: int = 0
    successes: int = 0
    cache_hits: int = 0
    script_runs: int = 0
    script_successes: int = 0
    script_errors: int = 0
