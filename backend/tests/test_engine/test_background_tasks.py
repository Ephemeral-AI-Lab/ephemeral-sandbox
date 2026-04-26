"""Tests for BackgroundTaskManager."""

from __future__ import annotations

import asyncio
from contextlib import suppress
import time
from typing import Any

from engine.runtime.tool_trace import record_tool_trace as _record_tool_trace
from engine.runtime.background_tasks import BackgroundTaskManager
from message.stream_events import BackgroundTaskStarted
from tools.builtins.background.cancel_background_task import (
    CancelBackgroundTaskInput,
    CancelBackgroundTaskTool,
)
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.runtime import ExecutionMetadata


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------


async def _make_tool_coro(
    output: str = "done", is_error: bool = False, delay: float = 0
) -> ToolResult:
    if delay:
        await asyncio.sleep(delay)
    return ToolResult(output=output, is_error=is_error)


def _launch(
    mgr: BackgroundTaskManager,
    *,
    task_id: str = "t1",
    tool_name: str = "test_tool",
    tool_input: dict[str, Any] | None = None,
    delay: float = 0.0,
    output: str = "ok",
    exit_code: int = 0,
    kill_callback=None,
    task_type: str = "tool",
):
    """Thin wrapper: creates the coro and calls mgr.launch with sensible defaults."""
    kwargs: dict[str, Any] = dict(
        task_id=task_id,
        tool_name=tool_name,
        tool_input=tool_input if tool_input is not None else {},
        coro=_make_tool_coro(output=output, delay=delay),
        task_type=task_type,
    )
    if kill_callback is not None:
        kwargs["kill_callback"] = kill_callback
    return mgr.launch(**kwargs)


def _launch_subagent(
    mgr: BackgroundTaskManager,
    *,
    task_id: str = "bg_1",
    delay: float = 10.0,
    coro=None,
):
    """Launch a subagent-typed task."""
    return mgr.launch(
        task_id=task_id,
        tool_name="run_subagent",
        tool_input={"agent_name": "test_subagent"},
        coro=coro if coro is not None else _make_tool_coro(delay=delay),
        task_type="subagent",
    )


def _make_ctx(mgr: BackgroundTaskManager) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd="/tmp", services={"background_task_manager": mgr})


# ---------------------------------------------------------------------------
# 1. launch
# ---------------------------------------------------------------------------


async def test_launch_creates_task() -> None:
    mgr = BackgroundTaskManager()
    event = mgr.launch(
        task_id="t1",
        tool_name="my_tool",
        tool_input={"key": "val"},
        coro=_make_tool_coro(),
    )

    assert isinstance(event, BackgroundTaskStarted)
    assert event.task_id == "t1"
    assert event.tool_name == "my_tool"
    assert event.tool_input == {"key": "val"}
    assert mgr.has_pending()
    assert "t1" in mgr._tasks


# ---------------------------------------------------------------------------
# 2. collect_completed after task finishes
# ---------------------------------------------------------------------------


async def test_collect_completed_after_task_finishes() -> None:
    mgr = BackgroundTaskManager()
    _launch(mgr, task_id="t1", tool_name="fast_tool", output="hello")
    await asyncio.sleep(0.01)

    completed = mgr.collect_completed()
    assert len(completed) == 1
    assert completed[0].status == "delivered"
    assert completed[0].result is not None
    assert completed[0].result.output == "hello"
    assert completed[0].result.is_error is False


# ---------------------------------------------------------------------------
# 3. collect_completed only once
# ---------------------------------------------------------------------------


async def test_collect_completed_only_once() -> None:
    mgr = BackgroundTaskManager()
    _launch(mgr)
    await asyncio.sleep(0.01)

    first = mgr.collect_completed()
    assert len(first) == 1

    second = mgr.collect_completed()
    assert len(second) == 0


# ---------------------------------------------------------------------------
# 4. has_pending
# ---------------------------------------------------------------------------


async def test_has_pending() -> None:
    mgr = BackgroundTaskManager()
    assert mgr.has_pending() is False

    _launch(mgr, delay=1.0)
    assert mgr.has_pending() is True

    # Cancel so we don't leak the slow task.
    await mgr.cancel_all()
    assert mgr.has_pending() is False


# ---------------------------------------------------------------------------
# 5. wait_any returns on completion
# ---------------------------------------------------------------------------


async def test_wait_any_returns_on_completion() -> None:
    mgr = BackgroundTaskManager()
    _launch(mgr, output="waited", delay=0.1)

    start = time.monotonic()
    result = await mgr.wait_any(timeout=5)
    elapsed = time.monotonic() - start

    assert result is not None
    assert result.task_id == "t1"
    assert result.result is not None
    assert result.result.output == "waited"
    # Should complete in roughly 0.1s, not 5s.
    assert elapsed < 1.0


def test_record_tool_trace_ignores_subagent_launches() -> None:
    meta = ExecutionMetadata()

    _record_tool_trace(
        meta,
        "run_subagent",
        {"agent_name": "test_subagent", "prompt": "explore pkg/core.py"},
        tool_use_id="toolu_1",
    )
    _record_tool_trace(
        meta,
        "run_subagent",
        {"agent_name": "test_subagent", "prompt": "explore pkg/core.py"},
        tool_use_id="toolu_1",
    )

    assert meta.extras == {}


def test_record_tool_trace_counts_note_ci_and_sandbox_reads() -> None:
    meta = ExecutionMetadata()

    _record_tool_trace(meta, "read_file_note", {"file_paths": ["pkg/core.py"]})
    _record_tool_trace(meta, "ci_query_symbol", {"query": "Git"})
    _record_tool_trace(meta, "ci_workspace_structure", {"path": "pkg"})
    _record_tool_trace(meta, "ci_diagnostics", {"file_path": "pkg/core.py"})
    _record_tool_trace(meta, "shell", {"code": "shell('pytest -q')"})
    _record_tool_trace(meta, "read_file", {"file_path": "pkg/core.py"})

    assert meta["_read_file_note_calls"] == 1
    assert meta["_note_read_paths_this_turn"] == ["pkg/core.py"]
    assert meta["_ci_context_calls"] == 3
    assert meta["_ci_query_symbol_calls"] == 1
    assert meta["_ci_workspace_structure_calls"] == 1
    assert meta["_ci_diagnostics_calls"] == 1
    assert meta["_shell_calls"] == 1
    assert meta["_read_file_calls"] == 1
    assert meta["_read_paths_this_turn"] == ["pkg/core.py"]


# ---------------------------------------------------------------------------
# 6. wait_any timeout
# ---------------------------------------------------------------------------


async def test_wait_any_timeout() -> None:
    mgr = BackgroundTaskManager()
    _launch(mgr, delay=10)

    result = await mgr.wait_any(timeout=0.1)
    assert result is None

    await mgr.cancel_all()


# ---------------------------------------------------------------------------
# 7. wait_any no pending
# ---------------------------------------------------------------------------


async def test_wait_any_no_pending() -> None:
    mgr = BackgroundTaskManager()
    result = await mgr.wait_any()
    assert result is None


# ---------------------------------------------------------------------------
# 8. cancel running task
# ---------------------------------------------------------------------------


async def test_cancel_running_task() -> None:
    mgr = BackgroundTaskManager()
    _launch(mgr, tool_name="slow", delay=10)

    ok = await mgr.cancel("t1", "test reason")
    assert ok is True

    tracked = mgr._tasks["t1"]
    assert tracked.status == "cancelled"
    assert tracked.result is not None
    assert tracked.result.output == "Cancelled: test reason"
    assert tracked.result.is_error is True


# ---------------------------------------------------------------------------
# 9. cancel nonexistent task
# ---------------------------------------------------------------------------


async def test_cancel_nonexistent_task() -> None:
    mgr = BackgroundTaskManager()
    assert await mgr.cancel("nonexistent_id") is False


# ---------------------------------------------------------------------------
# 10. cancel all
# ---------------------------------------------------------------------------


async def test_cancel_all() -> None:
    mgr = BackgroundTaskManager()
    for i in range(3):
        _launch(mgr, task_id=f"t{i}", tool_name=f"tool{i}", delay=10)

    await mgr.cancel_all()

    for i in range(3):
        assert mgr._tasks[f"t{i}"].status == "cancelled"
    assert mgr.has_pending() is False


async def test_cancel_all_marks_subagent_cancelled_without_asyncio_cancel() -> None:
    mgr = BackgroundTaskManager()
    cancelled = asyncio.Event()

    async def _subagent_coro() -> ToolResult:
        try:
            await asyncio.sleep(10)
        except asyncio.CancelledError:
            cancelled.set()
            raise
        return ToolResult(output="done")

    _launch_subagent(mgr, task_id="bg_1", coro=_subagent_coro())

    await mgr.cancel_all()
    await asyncio.sleep(0)

    tracked = mgr._tasks["bg_1"]
    assert tracked.status == "cancelled"
    assert tracked.result is not None
    assert tracked.result.output == "Cancelled"
    assert cancelled.is_set() is False
    assert tracked.asyncio_task.cancelled() is False

    tracked.asyncio_task.cancel()
    with suppress(asyncio.CancelledError):
        await tracked.asyncio_task


async def test_cancel_subagent_requests_early_stop_and_preserves_result() -> None:
    mgr = BackgroundTaskManager()
    cancelled = asyncio.Event()

    async def _subagent_coro() -> ToolResult:
        try:
            await asyncio.sleep(10)
        except asyncio.CancelledError:
            cancelled.set()
            return ToolResult(output="partial summary")
        return ToolResult(output="done")

    _launch_subagent(mgr, task_id="bg_early", coro=_subagent_coro())

    ok = await mgr.cancel("bg_early", "enough evidence")
    assert ok is True
    await asyncio.sleep(0)

    tracked = mgr._tasks["bg_early"]
    assert cancelled.is_set() is True
    assert tracked.status == "completed"
    assert tracked.stop_mode == "early_stop"
    assert tracked.completion_mode == "early_stopped"
    assert tracked.result is not None
    assert tracked.result.output == "partial summary"


# ---------------------------------------------------------------------------
# 11. task that raises exception
# ---------------------------------------------------------------------------


async def _raise_coro() -> ToolResult:
    raise ValueError("something broke")


async def test_task_that_raises_exception() -> None:
    mgr = BackgroundTaskManager()
    mgr.launch(
        task_id="t1",
        tool_name="bad_tool",
        tool_input={},
        coro=_raise_coro(),
    )
    await asyncio.sleep(0.01)

    completed = mgr.collect_completed()
    assert len(completed) == 1

    task = completed[0]
    # After collect, status is "delivered" (was "failed").
    assert task.status == "delivered"
    assert task.result is not None
    assert task.result.is_error is True
    assert "something broke" in task.result.output


# ---------------------------------------------------------------------------
# 12. get_status all
# ---------------------------------------------------------------------------


async def test_get_status_all() -> None:
    mgr = BackgroundTaskManager()
    _launch(mgr, task_id="t1", tool_name="tool_a", delay=10)
    _launch(mgr, task_id="t2", tool_name="tool_b", delay=10)

    statuses = mgr.get_status()
    assert len(statuses) == 2

    ids = {s["task_id"] for s in statuses}
    assert ids == {"t1", "t2"}

    for s in statuses:
        assert "tool_name" in s
        assert "status" in s
        assert "elapsed_seconds" in s

    await mgr.cancel_all()


# ---------------------------------------------------------------------------
# 13. get_status by id
# ---------------------------------------------------------------------------


async def test_get_status_by_id() -> None:
    mgr = BackgroundTaskManager()
    _launch(mgr, task_id="t1", tool_name="tool_a", delay=10)
    _launch(mgr, task_id="t2", tool_name="tool_b", delay=10)

    statuses = mgr.get_status(task_id="t1")
    assert len(statuses) == 1
    assert statuses[0]["task_id"] == "t1"

    await mgr.cancel_all()


# ---------------------------------------------------------------------------
# 15. get_status truncates output
# ---------------------------------------------------------------------------


async def test_get_status_returns_full_output_for_tool_layer_to_trim() -> None:
    """Manager returns the raw output verbatim — trimming lives at the tool layer."""
    mgr = BackgroundTaskManager()
    long_output = "x" * 5000
    _launch(mgr, output=long_output)
    await asyncio.sleep(0.01)

    statuses = mgr.get_status()
    assert len(statuses) == 1
    assert statuses[0]["output"] == long_output


# ---------------------------------------------------------------------------
# 16. progress_lines populated
# ---------------------------------------------------------------------------


async def test_progress_lines_populated() -> None:
    mgr = BackgroundTaskManager()
    _launch(mgr, output="line1\nline2\nline3")
    await asyncio.sleep(0.01)

    tracked = mgr._tasks["t1"]
    assert tracked.progress_lines == ["line1", "line2", "line3"]


# ---------------------------------------------------------------------------
# 17. multiple concurrent tasks
# ---------------------------------------------------------------------------


async def test_multiple_concurrent_tasks() -> None:
    mgr = BackgroundTaskManager()
    _launch(mgr, task_id="fast", tool_name="fast", output="fast_done", delay=0.01)
    _launch(mgr, task_id="medium", tool_name="medium", output="medium_done", delay=0.05)
    _launch(mgr, task_id="slow", tool_name="slow", output="slow_done", delay=0.1)

    # wait_any should return the fastest first.
    first = await mgr.wait_any(timeout=5)
    assert first is not None
    assert first.task_id == "fast"

    # Wait for the rest.
    await asyncio.sleep(0.15)
    remaining = mgr.collect_completed()
    remaining_ids = {t.task_id for t in remaining}
    assert "medium" in remaining_ids
    assert "slow" in remaining_ids


# ---------------------------------------------------------------------------
# 18. cancel invokes kill_callback
# ---------------------------------------------------------------------------


async def test_cancel_invokes_kill_callback() -> None:
    """cancel() should call the kill_callback to physically stop the process."""
    killed: list[str] = []

    async def _fake_kill() -> None:
        killed.append("killed")

    mgr = BackgroundTaskManager()
    _launch(mgr, tool_name="slow", delay=10, kill_callback=_fake_kill)

    ok = await mgr.cancel("t1", "test kill")
    assert ok is True
    assert killed == ["killed"], "kill_callback was not invoked"

    tracked = mgr._tasks["t1"]
    assert tracked.status == "cancelled"
    assert tracked.result is not None
    assert "test kill" in tracked.result.output


# ---------------------------------------------------------------------------
# 19. cancel_all invokes kill_callbacks
# ---------------------------------------------------------------------------


async def test_cancel_all_invokes_kill_callbacks() -> None:
    """cancel_all() should call kill_callback for every running task."""
    killed: list[str] = []

    async def _fake_kill_1() -> None:
        killed.append("t1")

    async def _fake_kill_2() -> None:
        killed.append("t2")

    mgr = BackgroundTaskManager()
    _launch(mgr, task_id="t1", tool_name="tool1", delay=10, kill_callback=_fake_kill_1)
    _launch(mgr, task_id="t2", tool_name="tool2", delay=10, kill_callback=_fake_kill_2)

    await mgr.cancel_all()
    assert set(killed) == {"t1", "t2"}, f"Expected both callbacks invoked, got {killed}"
    assert mgr.has_pending() is False


# ---------------------------------------------------------------------------
# 20. cancel without kill_callback still works (logical cancel)
# ---------------------------------------------------------------------------


async def test_cancel_without_kill_callback() -> None:
    """cancel() with no kill_callback should still mark as cancelled."""
    mgr = BackgroundTaskManager()
    _launch(mgr, tool_name="slow", delay=10)

    ok = await mgr.cancel("t1", "no kill cb")
    assert ok is True
    assert mgr._tasks["t1"].status == "cancelled"


# ---------------------------------------------------------------------------
# 21. kill_callback exception does not prevent cancel
# ---------------------------------------------------------------------------


async def test_kill_callback_exception_does_not_prevent_cancel() -> None:
    """If kill_callback raises, the task should still be marked cancelled."""

    async def _bad_kill() -> None:
        raise RuntimeError("sandbox connection lost")

    mgr = BackgroundTaskManager()
    _launch(mgr, tool_name="slow", delay=10, kill_callback=_bad_kill)

    ok = await mgr.cancel("t1", "kill failed but cancel ok")
    assert ok is True
    assert mgr._tasks["t1"].status == "cancelled"
    assert "kill failed but cancel ok" in mgr._tasks["t1"].result.output


# ---------------------------------------------------------------------------
# 22. wait_for: targets specific task and preserves other completions
# ---------------------------------------------------------------------------


async def test_wait_for_does_not_consume_other_task_completions() -> None:
    """wait_for(target) must NOT swallow completion events for other tasks.

    Guards the bug where the previous implementation routed specific-task
    waits through wait_any(), which calls collect_completed as a side
    effect and silently consumed completions for unrelated background
    tasks before the engine could deliver them.
    """
    mgr = BackgroundTaskManager()
    _launch(mgr, task_id="fast", tool_name="fast", output="fast-done", delay=0.01)
    _launch(mgr, task_id="slow", tool_name="slow", output="slow-done", delay=0.30)

    result = await mgr.wait_for("slow", timeout=0.05)
    assert result is None
    assert mgr._tasks["slow"].status == "running"

    completed = mgr.collect_completed()
    assert "fast" in [t.task_id for t in completed], (
        "wait_for() must not swallow other tasks' completions"
    )

    # Cleanup the still-running slow task.
    await mgr.cancel("slow")


# ---------------------------------------------------------------------------
# 23. wait_for: returns immediately for already-finished task
# ---------------------------------------------------------------------------


async def test_wait_for_returns_immediately_for_finished_task() -> None:
    mgr = BackgroundTaskManager()
    _launch(mgr)
    await asyncio.sleep(0.01)

    start = time.monotonic()
    result = await mgr.wait_for("t1", timeout=10.0)
    elapsed = time.monotonic() - start

    assert result is not None
    assert result.task_id == "t1"
    assert elapsed < 0.1, "should return immediately, not wait"


# ---------------------------------------------------------------------------
# 24. wait_for: unknown id returns None without raising
# ---------------------------------------------------------------------------


async def test_wait_for_unknown_id_returns_none() -> None:
    mgr = BackgroundTaskManager()
    result = await mgr.wait_for("nonexistent", timeout=1.0)
    assert result is None


# ---------------------------------------------------------------------------
# 25. get_status unknown id returns empty list (not all tasks)
# ---------------------------------------------------------------------------


async def test_get_status_unknown_id_returns_empty() -> None:
    """Unknown task_id must return [] — never silently fall through to 'all'."""
    mgr = BackgroundTaskManager()
    _launch(mgr, task_id="real", tool_name="t")

    assert mgr.get_status("ghost") == []
    assert len(mgr.get_status("real")) == 1
    assert len(mgr.get_status()) == 1


# ---------------------------------------------------------------------------
# 26. cancel_all does NOT call asyncio.cancel when kill_callback present
# ---------------------------------------------------------------------------


async def test_cancel_all_skips_asyncio_cancel_when_kill_callback_present() -> None:
    """If kill_callback is provided, cancel_all must NOT call asyncio.Task.cancel().

    Calling .cancel() through Daytona SDK process.exec corrupts the shared
    sandbox connection — the kill_callback is the safe path.
    """
    kill_called: list[str] = []

    async def _kill() -> None:
        kill_called.append("yes")

    mgr = BackgroundTaskManager()
    _launch(mgr, tool_name="slow", delay=10, kill_callback=_kill)
    task = mgr._tasks["t1"].asyncio_task

    await mgr.cancel_all()

    assert kill_called == ["yes"]
    assert not task.cancelled(), (
        "cancel_all must not invoke asyncio.Task.cancel() when a "
        "kill_callback is provided"
    )

    # Clean up so the test doesn't leak the still-running asyncio task.
    task.cancel()
    try:
        await task
    except BaseException:
        pass


async def test_cancel_all_falls_back_to_asyncio_cancel_without_kill_callback() -> None:
    mgr = BackgroundTaskManager()
    _launch(mgr, tool_name="slow", delay=10)
    task = mgr._tasks["t1"].asyncio_task

    await mgr.cancel_all()
    await asyncio.sleep(0)

    assert task.cancelled() or task.done()


# ---------------------------------------------------------------------------
# 27. done_callback does not overwrite cancelled status
# ---------------------------------------------------------------------------


async def test_done_callback_skips_cancelled_status() -> None:
    """If cancel() ran first, the asyncio task completing later must not
    overwrite the cancelled state with completed."""
    mgr = BackgroundTaskManager()
    _launch(mgr, output="late-completion", delay=0.05)
    await mgr.cancel("t1", "early")
    await asyncio.sleep(0.10)

    assert mgr._tasks["t1"].status == "cancelled"
    assert "early" in mgr._tasks["t1"].result.output
    assert "late-completion" not in mgr._tasks["t1"].result.output


async def test_done_callback_handles_asyncio_cancel_without_loop_error() -> None:
    """Cancelling a pure-Python background task must not trigger the loop's
    exception handler from the done callback."""
    mgr = BackgroundTaskManager()
    loop = asyncio.get_running_loop()
    observed: list[dict[str, object]] = []
    previous_handler = loop.get_exception_handler()

    def _handler(loop, context):  # type: ignore[no-untyped-def]
        observed.append(context)

    loop.set_exception_handler(_handler)
    try:
        _launch(mgr, task_id="t_cancel", tool_name="t", delay=10)
        await mgr.cancel("t_cancel", "stop")
        await asyncio.sleep(0)
    finally:
        loop.set_exception_handler(previous_handler)

    assert observed == []
    assert mgr._tasks["t_cancel"].status == "cancelled"
    assert "stop" in mgr._tasks["t_cancel"].result.output


# ---------------------------------------------------------------------------
# ---------------------------------------------------------------------------
# 31. cancel_background_task tool rejects task_id="all"
# ---------------------------------------------------------------------------


async def test_cancel_tool_rejects_all_sentinel() -> None:
    mgr = BackgroundTaskManager()
    _launch(mgr, task_id="bg_1", tool_name="t", delay=10)

    tool = CancelBackgroundTaskTool()
    args = CancelBackgroundTaskInput(task_id="all")
    result = await tool.execute(args, _make_ctx(mgr))
    assert result.is_error is True
    assert "does not support" in result.output
    assert mgr._tasks["bg_1"].status == "running"

    await mgr.cancel("bg_1")

