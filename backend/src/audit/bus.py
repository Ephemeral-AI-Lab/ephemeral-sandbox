"""Synchronous in-memory audit event bus."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass

from audit.base import AuditEvent, AuditSink


AuditHandler = Callable[[AuditEvent], None]


@dataclass(frozen=True, slots=True)
class AuditDispatchError:
    """Captured subscriber failure."""

    event: AuditEvent
    error: BaseException


class AuditEventBus(AuditSink):
    """Single-process synchronous fanout bus.

    Subscriber errors are captured so audit collection cannot interrupt the
    emitting domain path. Test harnesses can inspect ``errors`` and fail the
    scenario explicitly.
    """

    def __init__(self) -> None:
        self._handlers: list[AuditHandler] = []
        self.errors: list[AuditDispatchError] = []

    def publish(self, event: AuditEvent) -> None:
        for handler in list(self._handlers):
            try:
                handler(event)
            except BaseException as exc:  # noqa: BLE001
                self.errors.append(AuditDispatchError(event=event, error=exc))

    def subscribe(self, handler: AuditHandler) -> Callable[[], None]:
        self._handlers.append(handler)

        def _unsubscribe() -> None:
            try:
                self._handlers.remove(handler)
            except ValueError:
                pass

        return _unsubscribe


__all__ = ["AuditDispatchError", "AuditEventBus", "AuditHandler"]
