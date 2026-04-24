from __future__ import annotations

import pytest

from team.core.models import TaskStatus, TaskStatusUpdate
from team.runtime.task_queue import TaskQueue


class _Executor:
    def __init__(self, update: TaskStatusUpdate) -> None:
        self.update = update
        self.posted: list[TaskStatusUpdate] = []

    async def run(self, task_id: str) -> TaskStatusUpdate:
        assert task_id == self.update.task_id
        return self.update

    async def post_dispatch(self, update: TaskStatusUpdate) -> None:
        self.posted.append(update)


class _FailingOnceHandler:
    def __init__(self) -> None:
        self.updates: list[TaskStatusUpdate] = []

    async def handle(self, update: TaskStatusUpdate) -> None:
        self.updates.append(update)
        if len(self.updates) == 1:
            raise RuntimeError("handler exploded")


@pytest.mark.asyncio
async def test_process_one_converts_handler_exception_to_failed_update() -> None:
    executor = _Executor(
        TaskStatusUpdate(task_id="task-1", status=TaskStatus.DONE, summary="ok")
    )
    handler = _FailingOnceHandler()
    queue = TaskQueue(num_workers=1, executor=executor, handler=handler)

    await queue._process_one("task-1")

    assert [update.status for update in handler.updates] == [
        TaskStatus.DONE,
        TaskStatus.FAILED,
    ]
    assert handler.updates[1].summary == "handler_exception: handler exploded"
    assert executor.posted == [handler.updates[1]]
