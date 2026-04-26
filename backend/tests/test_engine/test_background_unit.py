"""Unit tests for background-task tool plumbing.

Covers, all offline (no sandbox, no LLM):

    1. `WaitBackgroundTasks` / `CheckBackgroundTaskResult` /
       `CancelBackgroundTask` schemas and ``execute`` branches that don't
       require a running loop to assert.
    2. `BackgroundTaskManager` extras and live-progress tail behaviour.
"""

from __future__ import annotations

import asyncio
import json

import pytest
from pathlib import Path
from pydantic import ValidationError

from tools.builtins.background._common import (
    build_background_snapshot_metadata,
    normalize_status,
    render_background_snapshot,
    render_tool_command,
)
from tools.builtins.background.wait_background_tasks import (
    WaitBackgroundTasksInput,
    WaitBackgroundTasksTool,
)
from tools.builtins.background.check_background_task_result import (
    CheckBackgroundTaskResultInput,
    CheckBackgroundTaskResultTool,
)
from tools.builtins.background.cancel_background_task import (
    CancelBackgroundTaskInput,
    CancelBackgroundTaskTool,
)
from tools.core.base import ToolExecutionContextService, ToolResult
from engine.runtime.background_tasks import BackgroundTaskManager


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _ctx(manager: BackgroundTaskManager | None) -> ToolExecutionContextService:
    metadata = {"background_task_manager": manager} if manager else {}
    return ToolExecutionContextService(cwd=Path("/tmp"), services=metadata)


# ---------------------------------------------------------------------------
# Schemas
# ---------------------------------------------------------------------------


class TestSchemas:
    @pytest.mark.parametrize("bad_timeout", [0, 0.5, 301, 1000])
    def test_wait_rejects_out_of_range_timeout(self, bad_timeout: float) -> None:
        with pytest.raises(ValidationError):
            WaitBackgroundTasksInput(timeout=bad_timeout)

    def test_wait_accepts_default_timeout(self) -> None:
        args = WaitBackgroundTasksInput()
        assert args.timeout == 30

    def test_check_requires_task_id(self) -> None:
        with pytest.raises(ValidationError):
            CheckBackgroundTaskResultInput()  # type: ignore[call-arg]

    def test_cancel_requires_task_id(self) -> None:
        with pytest.raises(ValidationError):
            CancelBackgroundTaskInput()  # type: ignore[call-arg]


# ---------------------------------------------------------------------------
# render_tool_command / normalize_status
# ---------------------------------------------------------------------------


class TestCommonHelpers:
    def test_render_tool_command_joins_values(self) -> None:
        out = render_tool_command("run_subagent", {"agent_name": "explorer", "prompt": "hi"})
        assert out == "run_subagent(explorer, hi)"

    def test_render_tool_command_no_args(self) -> None:
        assert render_tool_command("noop", {}) == "noop()"

    @pytest.mark.parametrize("raw,expected", [
        ("running", "running"),
        ("completed", "finished"),
        ("delivered", "finished"),
        ("failed", "failed"),
        ("cancelled", "failed"),
    ])
    def test_normalize_status(self, raw: str, expected: str) -> None:
        assert normalize_status(raw) == expected


# ---------------------------------------------------------------------------
# WaitBackgroundTasksTool branches
# ---------------------------------------------------------------------------


class TestWaitBackgroundTasksExecute:
    async def test_no_manager_returns_error(self) -> None:
        tool = WaitBackgroundTasksTool()
        result = await tool.execute(WaitBackgroundTasksInput(timeout=1), _ctx(None))
        assert result.is_error

    async def test_no_tasks_ever(self) -> None:
        tool = WaitBackgroundTasksTool()
        mgr = BackgroundTaskManager()
        result = await tool.execute(WaitBackgroundTasksInput(timeout=1), _ctx(mgr))
        assert not result.is_error
        assert "[NO TASKS]" in result.output
        assert result.metadata["background_snapshot"]["kind"] == "wait_no_tasks"

    async def test_completed_tasks_appear_in_snapshot(self) -> None:
        tool = WaitBackgroundTasksTool()
        mgr = BackgroundTaskManager()

        async def fast(output: str) -> ToolResult:
            return ToolResult(output=output)

        mgr.launch("bg_1", "noop", {"q": "ping"}, fast("hi"))
        await asyncio.sleep(0.01)

        result = await tool.execute(WaitBackgroundTasksInput(timeout=1), _ctx(mgr))
        assert "[COMPLETED]" in result.output
        snap = result.metadata["background_snapshot"]
        assert snap["kind"] == "wait_completed"
        assert snap["statuses"] == [
            {"task_id": "bg_1", "status": "finished", "tool_command": "noop(ping)"},
        ]

    async def test_timeout_returns_timed_out(self) -> None:
        tool = WaitBackgroundTasksTool()
        mgr = BackgroundTaskManager()

        async def slow() -> ToolResult:
            await asyncio.sleep(5)
            return ToolResult(output="done")

        mgr.launch("bg_run", "noop", {}, slow())
        try:
            result = await tool.execute(WaitBackgroundTasksInput(timeout=1), _ctx(mgr))
            assert "[TIMED_OUT" in result.output
            assert result.metadata["background_snapshot"]["kind"] == "wait_timed_out"
        finally:
            await mgr.cancel("bg_run")


# ---------------------------------------------------------------------------
# CheckBackgroundTaskResultTool branches
# ---------------------------------------------------------------------------


class TestCheckBackgroundTaskResultExecute:
    async def test_no_manager_returns_error(self) -> None:
        tool = CheckBackgroundTaskResultTool()
        result = await tool.execute(CheckBackgroundTaskResultInput(task_id="bg_1"), _ctx(None))
        assert result.is_error

    async def test_unknown_task_id(self) -> None:
        tool = CheckBackgroundTaskResultTool()
        mgr = BackgroundTaskManager()
        result = await tool.execute(CheckBackgroundTaskResultInput(task_id="bg_x"), _ctx(mgr))
        assert result.is_error
        assert "bg_x" in result.output

    async def test_running_generic_tool_returns_progress_lines(self) -> None:
        tool = CheckBackgroundTaskResultTool()
        mgr = BackgroundTaskManager()

        async def slow() -> ToolResult:
            await asyncio.sleep(5)
            return ToolResult(output="done")

        mgr.launch("bg_1", "shell", {"cmd": "ls"}, slow())
        try:
            mgr.append_progress("bg_1", "line-a")
            result = await tool.execute(
                CheckBackgroundTaskResultInput(task_id="bg_1"), _ctx(mgr)
            )
            payload = json.loads(result.output)
            assert payload["id"] == "bg_1"
            assert payload["status"] == "running"
            assert payload["tool_command"] == "shell(ls)"
            assert "line-a" in payload["result"]
        finally:
            await mgr.cancel("bg_1")

    async def test_finished_generic_tool_returns_full_output(self) -> None:
        tool = CheckBackgroundTaskResultTool()
        mgr = BackgroundTaskManager()

        async def fast() -> ToolResult:
            return ToolResult(output="x" * 5000)

        mgr.launch("bg_1", "shell", {"cmd": "ls"}, fast())
        await asyncio.sleep(0.01)

        result = await tool.execute(
            CheckBackgroundTaskResultInput(task_id="bg_1"), _ctx(mgr)
        )
        payload = json.loads(result.output)
        assert payload["status"] == "finished"
        # No truncation for shell.
        assert payload["result"] == "x" * 5000

    async def test_subagent_finished_with_terminal_returns_findings(self) -> None:
        tool = CheckBackgroundTaskResultTool()
        mgr = BackgroundTaskManager()

        async def sub() -> ToolResult:
            return ToolResult(
                output="my findings",
                metadata={"subagent_terminal_called": True},
            )

        mgr.launch("bg_1", "run_subagent", {"agent_name": "x", "prompt": "p"}, sub(),
                   task_type="subagent")
        await asyncio.sleep(0.01)

        result = await tool.execute(
            CheckBackgroundTaskResultInput(task_id="bg_1"), _ctx(mgr)
        )
        payload = json.loads(result.output)
        assert payload["status"] == "finished"
        assert payload["result"] == "my findings"
        assert payload["tool_command"] == "run_subagent(x, p)"

    async def test_subagent_finished_without_terminal_marked_failed(self) -> None:
        tool = CheckBackgroundTaskResultTool()
        mgr = BackgroundTaskManager()

        async def sub() -> ToolResult:
            return ToolResult(
                output="ran out of nudges",
                is_error=True,
                metadata={"subagent_terminal_called": False},
            )

        mgr.launch("bg_1", "run_subagent", {"agent_name": "x", "prompt": "p"}, sub(),
                   task_type="subagent")
        await asyncio.sleep(0.01)

        # Register a peek provider so the failed branch has something to show.
        mgr.set_progress_provider("bg_1", lambda n: "peek-snapshot")

        result = await tool.execute(
            CheckBackgroundTaskResultInput(task_id="bg_1"), _ctx(mgr)
        )
        payload = json.loads(result.output)
        assert payload["status"] == "failed"
        assert payload["result"] == "peek-snapshot"


# ---------------------------------------------------------------------------
# Snapshot rendering helpers
# ---------------------------------------------------------------------------


class TestBackgroundSnapshotHelpers:
    def test_progress_passthrough_for_provider_history(self) -> None:
        statuses = [{"task_id": "bg_1", "status": "running", "output": "hello"}]
        output = render_background_snapshot("progress", statuses)
        metadata = build_background_snapshot_metadata("progress", "all", statuses)
        assert json.loads(output) == statuses
        assert metadata["background_snapshot"]["kind"] == "progress"

    def test_wait_completed_render(self) -> None:
        statuses = [{"task_id": "bg_1", "status": "finished", "tool_command": "noop()"}]
        output = render_background_snapshot("wait_completed", statuses)
        assert output.startswith("[COMPLETED]\n[")
        assert "Do not call wait_background_tasks again" in output

    def test_wait_timed_out_render(self) -> None:
        statuses = [{"task_id": "bg_1", "status": "running", "tool_command": "noop()"}]
        output = render_background_snapshot("wait_timed_out", statuses, elapsed_seconds=2.5)
        assert "[TIMED_OUT after 2.5s]" in output
        assert "wait_background_tasks" in output
        assert "cancel_background_task" in output

    def test_wait_no_tasks_render(self) -> None:
        output = render_background_snapshot("wait_no_tasks", [])
        assert output.startswith("[NO TASKS]")


# ---------------------------------------------------------------------------
# CancelBackgroundTaskTool branches
# ---------------------------------------------------------------------------


class TestCancelBackgroundTaskExecute:
    async def test_no_manager_returns_error(self) -> None:
        tool = CancelBackgroundTaskTool()
        result = await tool.execute(CancelBackgroundTaskInput(task_id="bg_1"), _ctx(None))
        assert result.is_error

    async def test_rejects_all_sentinel(self) -> None:
        tool = CancelBackgroundTaskTool()
        mgr = BackgroundTaskManager()
        result = await tool.execute(CancelBackgroundTaskInput(task_id="all"), _ctx(mgr))
        assert result.is_error
        assert "does not support" in result.output

    async def test_unknown_task_id_returns_error(self) -> None:
        tool = CancelBackgroundTaskTool()
        mgr = BackgroundTaskManager()
        result = await tool.execute(CancelBackgroundTaskInput(task_id="bg_missing"), _ctx(mgr))
        assert result.is_error
        assert "bg_missing" in result.output

    async def test_subagent_cancel_reports_early_stop(self) -> None:
        tool = CancelBackgroundTaskTool()
        mgr = BackgroundTaskManager()

        async def _subagent() -> ToolResult:
            await asyncio.sleep(10)
            return ToolResult(output="done")

        mgr.launch(
            task_id="bg_sub",
            tool_name="run_subagent",
            tool_input={"agent_name": "test_subagent"},
            coro=_subagent(),
            task_type="subagent",
        )
        result = await tool.execute(CancelBackgroundTaskInput(task_id="bg_sub"), _ctx(mgr))
        assert result.is_error is False
        assert "early-stop requested" in result.output


# ---------------------------------------------------------------------------
# BackgroundTaskManager — internal API not covered by test_background_tasks.py
# ---------------------------------------------------------------------------


class TestBackgroundTaskManagerExtras:
    async def test_next_alias_is_monotonic(self) -> None:
        mgr = BackgroundTaskManager()
        ids = [mgr.next_alias() for _ in range(3)]
        assert ids == ["bg_1", "bg_2", "bg_3"]

    async def test_get_status_unknown_id_returns_empty(self) -> None:
        mgr = BackgroundTaskManager()
        assert mgr.get_status("nope") == []

    async def test_wait_for_unknown_id_returns_none(self) -> None:
        mgr = BackgroundTaskManager()
        assert await mgr.wait_for("nope", timeout=0.1) is None

    async def test_wait_for_already_completed_returns_immediately(self) -> None:
        mgr = BackgroundTaskManager()

        async def quick() -> ToolResult:
            return ToolResult(output="hi")

        alias = mgr.next_alias()
        mgr.launch(alias, "noop", {}, quick())
        await asyncio.sleep(0)
        await asyncio.sleep(0)
        tracked = await mgr.wait_for(alias, timeout=1)
        assert tracked is not None
        assert tracked.status in ("completed", "delivered")

    async def test_wait_for_running_then_timeout_returns_none(self) -> None:
        mgr = BackgroundTaskManager()

        async def slow() -> ToolResult:
            await asyncio.sleep(5)
            return ToolResult(output="done")

        alias = mgr.next_alias()
        mgr.launch(alias, "noop", {}, slow())
        try:
            result = await mgr.wait_for(alias, timeout=0.05)
            assert result is None  # still running
        finally:
            await mgr.cancel(alias, "")

    async def test_has_pending_reflects_running_state(self) -> None:
        mgr = BackgroundTaskManager()

        async def slow() -> ToolResult:
            await asyncio.sleep(5)
            return ToolResult(output="done")

        assert not mgr.has_pending()
        alias = mgr.next_alias()
        mgr.launch(alias, "noop", {}, slow())
        assert mgr.has_pending()
        await mgr.cancel(alias, "")
        assert not mgr.has_pending()

    async def test_get_status_returns_full_output_no_truncation(self) -> None:
        """get_status no longer truncates output — that's the tool layer's job."""
        mgr = BackgroundTaskManager()

        async def big() -> ToolResult:
            return ToolResult(output="x" * 5000)

        alias = mgr.next_alias()
        mgr.launch(alias, "noop", {}, big())
        await asyncio.sleep(0)
        await asyncio.sleep(0)
        snap = mgr.get_status(alias)
        assert snap and len(snap[0]["output"]) == 5000


# ---------------------------------------------------------------------------
# Live progress tail — append_progress / make_progress_callback / get_status
# ---------------------------------------------------------------------------


class TestLiveProgressTail:
    async def test_append_progress_buffers_running_lines(self) -> None:
        mgr = BackgroundTaskManager()

        async def slow() -> ToolResult:
            await asyncio.sleep(5)
            return ToolResult(output="done")

        alias = mgr.next_alias()
        mgr.launch(alias, "noop", {}, slow())
        try:
            mgr.append_progress(alias, "first")
            mgr.append_progress(alias, "second\nthird")
            tail = mgr._tasks[alias].progress_lines[-3:]
            assert tail == ["first", "second", "third"]
        finally:
            await mgr.cancel(alias, "")

    async def test_append_progress_unknown_task_is_noop(self) -> None:
        mgr = BackgroundTaskManager()
        mgr.append_progress("bg_nope", "ignored")  # must not raise

    async def test_append_progress_after_finish_is_noop(self) -> None:
        mgr = BackgroundTaskManager()

        async def quick() -> ToolResult:
            return ToolResult(output="hi")

        alias = mgr.next_alias()
        mgr.launch(alias, "noop", {}, quick())
        await asyncio.sleep(0)
        await asyncio.sleep(0)
        before = list(mgr._tasks[alias].progress_lines)
        mgr.append_progress(alias, "late")
        assert mgr._tasks[alias].progress_lines == before

    async def test_make_progress_callback_round_trip(self) -> None:
        mgr = BackgroundTaskManager()

        async def slow() -> ToolResult:
            await asyncio.sleep(5)
            return ToolResult(output="done")

        alias = mgr.next_alias()
        mgr.launch(alias, "noop", {}, slow())
        try:
            cb = mgr.make_progress_callback(alias)
            cb("alpha")
            cb("beta")
            assert mgr._tasks[alias].progress_lines[-2:] == ["alpha", "beta"]
        finally:
            await mgr.cancel(alias, "")

    async def test_get_status_surfaces_live_tail_for_running(self) -> None:
        mgr = BackgroundTaskManager()

        async def slow() -> ToolResult:
            await asyncio.sleep(5)
            return ToolResult(output="done")

        alias = mgr.next_alias()
        mgr.launch(alias, "noop", {}, slow())
        try:
            mgr.append_progress(alias, "live-1")
            mgr.append_progress(alias, "live-2")
            snap = mgr.get_status(alias)
            assert snap and snap[0]["status"] == "running"
            assert snap[0]["output"].endswith("live-1\nlive-2")
        finally:
            await mgr.cancel(alias, "")

    async def test_get_status_running_task_carries_start_stamp(self) -> None:
        mgr = BackgroundTaskManager()

        async def slow() -> ToolResult:
            await asyncio.sleep(5)
            return ToolResult(output="done")

        alias = mgr.next_alias()
        mgr.launch(alias, "noop", {}, slow())
        try:
            snap = mgr.get_status(alias)
            assert snap and snap[0]["status"] == "running"
            assert "output" in snap[0]
            assert snap[0]["output"].startswith("[started:")
        finally:
            await mgr.cancel(alias, "")
