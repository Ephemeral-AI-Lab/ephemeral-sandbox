"""Shared transient transport recovery for sandbox API mutations."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from typing import TypeVar

from sandbox.api._impl._payload import is_transient_transport_error

TResult = TypeVar("TResult")


async def call_with_transient_recovery(
    *,
    attempts: int,
    call: Callable[[], Awaitable[TResult]],
    recover: Callable[[int], Awaitable[TResult | None]],
) -> TResult:
    """Retry transient transport failures after checking for committed effects."""
    last_exc: Exception | None = None
    for attempt_no in range(1, attempts + 1):
        try:
            return await call()
        except Exception as exc:
            if not is_transient_transport_error(exc):
                raise
            recovered = await recover(attempt_no)
            if recovered is not None:
                return recovered
            last_exc = exc
    assert last_exc is not None
    raise last_exc


__all__ = ["call_with_transient_recovery"]
