"""Daemon-local workspace change events for overlay-backed consumers."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Literal

from sandbox.overlay.path_change import OverlayPathChange

WorkspaceChangeReason = Literal[
    "publish",
    "foreign_publish",
    "remount",
    "full_resync",
]


@dataclass(frozen=True)
class WorkspacePathChange:
    path: str
    kind: Literal["write", "delete", "symlink", "opaque_dir"]
    existed_before: bool

    @classmethod
    def from_overlay_change(cls, change: OverlayPathChange) -> WorkspacePathChange:
        return cls(
            path=change.path,
            kind=change.kind,
            existed_before=bool(getattr(change, "existed_before", False)),
        )


@dataclass(frozen=True)
class WorkspaceChangeEvent:
    reason: WorkspaceChangeReason
    from_version: int
    to_version: int
    changes: tuple[WorkspacePathChange, ...] = ()


class WorkspaceChangeEventBus:
    """Small bounded fanout bus for daemon-local workspace change events."""

    def __init__(self) -> None:
        self._subscribers: dict[str, _WorkspaceChangeSubscriber] = {}

    def subscribe(
        self,
        subscriber_id: str,
        *,
        max_queue: int = 256,
    ) -> asyncio.Queue[WorkspaceChangeEvent]:
        if max_queue <= 0:
            raise ValueError("max_queue must be positive")
        subscriber = _WorkspaceChangeSubscriber(max_queue=max_queue)
        self._subscribers[subscriber_id] = subscriber
        return subscriber.queue

    def unsubscribe(self, subscriber_id: str) -> None:
        self._subscribers.pop(subscriber_id, None)

    def emit(self, event: WorkspaceChangeEvent) -> None:
        for subscriber in tuple(self._subscribers.values()):
            subscriber.put(event)


class _WorkspaceChangeSubscriber:
    def __init__(self, *, max_queue: int) -> None:
        self.queue: asyncio.Queue[WorkspaceChangeEvent] = asyncio.Queue(
            maxsize=max_queue
        )

    def put(self, event: WorkspaceChangeEvent) -> None:
        if self.queue.full():
            self._drop_all()
            self.queue.put_nowait(
                WorkspaceChangeEvent(
                    reason="full_resync",
                    from_version=event.from_version,
                    to_version=event.to_version,
                    changes=(),
                )
            )
            return
        self.queue.put_nowait(event)

    def _drop_all(self) -> None:
        while True:
            try:
                self.queue.get_nowait()
            except asyncio.QueueEmpty:
                return


__all__ = [
    "WorkspacePathChange",
    "WorkspaceChangeEventBus",
    "WorkspaceChangeEvent",
    "WorkspaceChangeReason",
]
