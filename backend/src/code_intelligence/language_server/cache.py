"""Thread-safe query caching for language-server clients."""

from __future__ import annotations

import threading
import time
from collections import OrderedDict
from collections.abc import Callable
from typing import Any, TypeVar, cast

from code_intelligence.language_server.models import (
    LspTelemetry,
    _CacheEntry,
    _InflightQuery,
)

_T = TypeVar("_T")


class LspCacheMixin:
    _cache_lock: threading.Lock
    _counter_lock: threading.Lock
    _cache: OrderedDict[str, _CacheEntry]
    _inflight: dict[str, _InflightQuery]
    _cache_ttl: float
    _cache_max: int
    _telemetry: LspTelemetry

    def _run_cached_query(
        self,
        key: str,
        loader: Callable[[], _T],
        *,
        cache_when: Callable[[_T], bool] | None = None,
    ) -> _T:
        cached = self._get_cached(key)
        if cached is not None:
            return cast(_T, cached)

        with self._cache_lock:
            inflight = self._inflight.get(key)
            if inflight is None:
                inflight = _InflightQuery(event=threading.Event())
                self._inflight[key] = inflight
                owner = True
            else:
                owner = False

        if not owner:
            inflight.event.wait()
            if inflight.error is not None:
                raise inflight.error
            return cast(_T, inflight.result)

        try:
            result = loader()
            should_cache = True if cache_when is None else cache_when(result)
            with self._cache_lock:
                if should_cache:
                    self._cache[key] = _CacheEntry(
                        result=result,
                        expires_at=time.time() + self._cache_ttl,
                    )
                    self._cache.move_to_end(key)
                    while len(self._cache) > self._cache_max:
                        self._cache.popitem(last=False)
                inflight.result = result
            return result
        except BaseException as exc:
            with self._cache_lock:
                inflight.error = exc
            raise
        finally:
            with self._cache_lock:
                self._inflight.pop(key, None)
                inflight.event.set()

    def _get_cached(self, key: str) -> Any:
        with self._cache_lock:
            entry = self._cache.get(key)
            if entry and entry.expires_at > time.time():
                self._cache.move_to_end(key)
                with self._counter_lock:
                    self._telemetry.cache_hits += 1
                return entry.result
            if entry:
                del self._cache[key]
        return None

