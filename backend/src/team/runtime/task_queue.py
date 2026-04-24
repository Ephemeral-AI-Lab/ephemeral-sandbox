"""TaskQueue — push-driven ready queue for N worker coroutines.

Replaces the poll-based ``DispatchQueue`` (``pop_ready`` + ``sleep(0.05)``)
with a single ``asyncio.Queue[str]`` that the handler writes into as tasks
become ready. Workers block on ``Queue.get()`` until pushed.

Workflow per worker tick:

    task_id = await self._ready.get()
    update  = await self._executor.run(task_id)
    await self._handler.handle(update)
    await self._executor.post_dispatch(update)

The handler's ``async with self._lock`` serializes the match-block body so
concurrent workers don't interleave graph mutations.
"""

from __future__ import annotations

import asyncio
import logging
from typing import TYPE_CHECKING

from team.models import TaskStatus, TaskStatusUpdate

if TYPE_CHECKING:
    from team.runtime.executor import Executor
    from team.runtime.status_handler import TaskStatusHandler

logger = logging.getLogger(__name__)


class TaskQueue:
    """Bounded-worker push queue driving the handler/executor loop."""

    def __init__(
        self,
        *,
        num_workers: int,
        executor: "Executor",
        handler: "TaskStatusHandler",
    ) -> None:
        self._num_workers = max(1, num_workers)
        self._executor = executor
        self._handler = handler
        self._ready: asyncio.Queue[str] = asyncio.Queue()
        self._workers: list[asyncio.Task[None]] = []
        self._stopped = False

    # ---- public API -----------------------------------------------------

    def enqueue(self, task_id: str) -> None:
        """Non-blocking push. No-op once the queue has been stopped."""
        if self._stopped:
            return
        self._ready.put_nowait(task_id)

    async def start(self) -> None:
        if self._workers:
            return
        for i in range(self._num_workers):
            self._workers.append(
                asyncio.create_task(self._worker_loop(), name=f"task-queue-worker-{i}")
            )

    async def drain_and_stop(self) -> None:
        """Stop accepting new enqueues and cancel worker coroutines."""
        self._stopped = True
        for worker in self._workers:
            worker.cancel()
        if self._workers:
            await asyncio.gather(*self._workers, return_exceptions=True)
        self._workers = []

    @property
    def workers(self) -> list[asyncio.Task[None]]:
        return list(self._workers)

    def pending_count(self) -> int:
        return self._ready.qsize()

    # ---- worker loop ----------------------------------------------------

    async def _worker_loop(self) -> None:
        try:
            while True:
                task_id = await self._ready.get()
                await self._process_one(task_id)
        except asyncio.CancelledError:
            return

    async def _process_one(self, task_id: str) -> None:
        try:
            update = await self._executor.run(task_id)
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            logger.exception("executor.run failed for %s", task_id)
            update = TaskStatusUpdate(
                task_id=task_id,
                status=TaskStatus.FAILED,
                summary=f"executor_exception: {exc}",
            )
        try:
            await self._handler.handle(update)
        except asyncio.CancelledError:
            raise
        except Exception:
            logger.exception("handler.handle failed for %s", task_id)
            return
        try:
            await self._executor.post_dispatch(update)
        except Exception:
            logger.debug("after_dispatch hook raised for %s", task_id, exc_info=True)
