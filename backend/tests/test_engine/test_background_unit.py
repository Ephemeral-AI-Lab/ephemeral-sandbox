"""Unit tests for background-task tool plumbing.

Covers two layers, all offline (no sandbox, no LLM):

    1. `_common.apply_last_n_lines` — line trim, char cap, total budget.
    2. `CheckBackgroundProgress` / `WaitForBackgroundTask` schemas and
       `execute` branches that don't require a running loop to assert.
"""

from __future__ import annotations

import asyncio

import pytest
from pathlib import Path
from pydantic import ValidationError

from tools.builtins.background._common import (
    MAX_TOTAL_OUTPUT_CHARS,
    MIN_PER_ENTRY_CHARS,
    apply_last_n_lines,
    build_background_snapshot_metadata,
    render_background_snapshot,
)
from tools.builtins.background.check_background_progress import (
    CheckBackgroundProgressInput,
    CheckBackgroundProgressTool,
)
from tools.builtins.background.wait_for_background_task import (
    WaitForBackgroundTaskInput,
    WaitForBackgroundTaskTool,
)
from tools.builtins.background.cancel_background_task import (
    CancelBackgroundTaskInput,
    CancelBackgroundTaskTool,
)
from tools.core.base import ToolExecutionContext, ToolResult
from engine.runtime.background_tasks import BackgroundTaskManager


# ---------------------------------------------------------------------------
# apply_last_n_lines
# ---------------------------------------------------------------------------


class TestApplyLastNLines:
    def test_line_trim_keeps_last_n(self) -> None:
        status = [{"output": "\n".join(str(i) for i in range(10))}]
        apply_last_n_lines(status, last_n_lines=3)
        assert status[0]["output"] == "7\n8\n9"

    def test_no_trim_when_under_limit(self) -> None:
        status = [{"output": "a\nb"}]
        apply_last_n_lines(status, last_n_lines=5)
        assert status[0]["output"] == "a\nb"

    def test_non_string_output_untouched(self) -> None:
        status = [{"output": None}, {"output": 123}, {"other": "x"}]
        apply_last_n_lines(status, last_n_lines=3)
        assert status == [{"output": None}, {"output": 123}, {"other": "x"}]

    def test_char_cap_prepends_marker_and_drops_partial_line(self) -> None:
        # One very long entry — line trim keeps it (one line), char cap slices.
        blob = "HEAD" + "x" * 5000 + "\nLINE_A\nLINE_B\nTAIL_END"
        status = [{"output": blob}]
        apply_last_n_lines(status, last_n_lines=100)
        out = status[0]["output"]
        assert out.startswith("... (head truncated)\n")
        # Partial-line drop means the first kept line must be whole
        # (i.e. not a fragment of the giant leading "HEAD..." line).
        kept = out.split("\n", 1)[1]  # after the marker
        first_line = kept.split("\n", 1)[0]
        assert first_line in ("LINE_A", "LINE_B", "TAIL_END")
        assert "TAIL_END" in out

    def test_total_budget_split_across_entries(self) -> None:
        # 10 entries each with 2000 chars → per-entry budget = 4000/10 = 400,
        # but floor MIN_PER_ENTRY_CHARS=200 kicks in via max(), so 400.
        big = "x" * 2000
        status = [{"output": big} for _ in range(10)]
        apply_last_n_lines(status, last_n_lines=1000)
        per_entry = MAX_TOTAL_OUTPUT_CHARS // 10
        assert per_entry >= MIN_PER_ENTRY_CHARS
        for entry in status:
            # marker prefix + (tail up to per_entry) minus partial line drop
            assert entry["output"].startswith("... (head truncated)\n")
            assert len(entry["output"]) <= per_entry + len("... (head truncated)\n") + 1

    def test_min_per_entry_floor(self) -> None:
        # 100 entries → 4000/100 = 40, below floor → floor used = 200
        status = [{"output": "x" * 1000} for _ in range(100)]
        apply_last_n_lines(status, last_n_lines=1000)
        for entry in status:
            assert entry["output"].startswith("... (head truncated)\n")

    def test_empty_list_noop(self) -> None:
        status: list[dict] = []
        apply_last_n_lines(status, last_n_lines=5)
        assert status == []


# ---------------------------------------------------------------------------
# Pydantic schemas — task_id validation now lives on the Field itself
# (TASK_ID_FIELD with min_length=1), exercised end-to-end below.
# ---------------------------------------------------------------------------


class TestSchemas:
    def test_check_requires_task_id(self) -> None:
        with pytest.raises(ValidationError):
            CheckBackgroundProgressInput()  # type: ignore[call-arg]

    def test_check_rejects_empty_task_id(self) -> None:
        with pytest.raises(ValidationError):
            CheckBackgroundProgressInput(task_id="")

    def test_check_rejects_last_n_lines_zero(self) -> None:
        with pytest.raises(ValidationError):
            CheckBackgroundProgressInput(task_id="bg_1", last_n_lines=0)

    def test_check_accepts_all(self) -> None:
        assert CheckBackgroundProgressInput(task_id="all").task_id == "all"

    def test_wait_requires_task_id(self) -> None:
        with pytest.raises(ValidationError):
            WaitForBackgroundTaskInput()  # type: ignore[call-arg]

    @pytest.mark.parametrize("bad_timeout", [0, 0.5, 301, 1000])
    def test_wait_rejects_out_of_range_timeout(self, bad_timeout: float) -> None:
        with pytest.raises(ValidationError):
            WaitForBackgroundTaskInput(task_id="bg_1", timeout=bad_timeout)

    def test_wait_rejects_last_n_lines_zero(self) -> None:
        with pytest.raises(ValidationError):
            WaitForBackgroundTaskInput(task_id="bg_1", last_n_lines=0)

    def test_cancel_requires_task_id(self) -> None:
        with pytest.raises(ValidationError):
            CancelBackgroundTaskInput()  # type: ignore[call-arg]


# ---------------------------------------------------------------------------
# Tool.execute branches
# ---------------------------------------------------------------------------


def _ctx(manager: BackgroundTaskManager | None) -> ToolExecutionContext:
    metadata = {"background_task_manager": manager} if manager else {}
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata)


class TestCheckBackgroundProgressExecute:
    async def test_no_manager_returns_error(self) -> None:
        tool = CheckBackgroundProgressTool()
        args = CheckBackgroundProgressInput(task_id="all")
        result = await tool.execute(args, _ctx(None))
        assert result.is_error
        assert "not available" in result.output

    async def test_empty_manager_returns_benign(self) -> None:
        tool = CheckBackgroundProgressTool()
        mgr = BackgroundTaskManager()
        args = CheckBackgroundProgressInput(task_id="all")
        result = await tool.execute(args, _ctx(mgr))
        assert not result.is_error
        assert "No background tasks" in result.output

    async def test_unknown_task_id_is_error(self) -> None:
        tool = CheckBackgroundProgressTool()
        mgr = BackgroundTaskManager()
        args = CheckBackgroundProgressInput(task_id="bg_nonexistent")
        result = await tool.execute(args, _ctx(mgr))
        assert result.is_error
        assert "bg_nonexistent" in result.output

    async def test_all_prefers_running_tasks(self) -> None:
        tool = CheckBackgroundProgressTool()
        mgr = BackgroundTaskManager()

        async def fast() -> ToolResult:
            return ToolResult(output="done")

        async def slow() -> ToolResult:
            await asyncio.sleep(5)
            return ToolResult(output="later")

        mgr.launch("bg_done", "t", {}, fast())
        mgr.launch("bg_run", "t", {}, slow())
        await asyncio.sleep(0.01)

        try:
            result = await tool.execute(CheckBackgroundProgressInput(task_id="all"), _ctx(mgr))
            assert '"task_id": "bg_run"' in result.output
            assert '"task_id": "bg_done"' not in result.output
            assert result.metadata["background_snapshot"]["kind"] == "progress"
            assert result.metadata["background_snapshot"]["scope"] == "all"
        finally:
            await mgr.cancel("bg_run")


class TestWaitForBackgroundTaskExecute:
    async def test_no_manager_returns_error(self) -> None:
        tool = WaitForBackgroundTaskTool()
        args = WaitForBackgroundTaskInput(task_id="all", timeout=1)
        result = await tool.execute(args, _ctx(None))
        assert result.is_error

    async def test_all_with_no_tasks_ever(self) -> None:
        tool = WaitForBackgroundTaskTool()
        mgr = BackgroundTaskManager()
        args = WaitForBackgroundTaskInput(task_id="all", timeout=1)
        result = await tool.execute(args, _ctx(mgr))
        assert not result.is_error
        assert "[NO TASKS RUNNING]" in result.output

    async def test_unknown_specific_id(self) -> None:
        tool = WaitForBackgroundTaskTool()
        mgr = BackgroundTaskManager()
        args = WaitForBackgroundTaskInput(task_id="bg_nope", timeout=1)
        result = await tool.execute(args, _ctx(mgr))
        assert result.is_error
        assert "bg_nope" in result.output

    async def test_all_prefers_fresh_completions_over_delivered_history(self) -> None:
        tool = WaitForBackgroundTaskTool()
        mgr = BackgroundTaskManager()

        async def fast(output: str) -> ToolResult:
            return ToolResult(output=output)

        mgr.launch("bg_old", "t", {}, fast("old"))
        await asyncio.sleep(0.01)
        mgr.collect_completed()

        mgr.launch("bg_new", "t", {}, fast("new"))
        await asyncio.sleep(0.01)

        result = await tool.execute(WaitForBackgroundTaskInput(task_id="all", timeout=1), _ctx(mgr))
        assert result.is_error is False
        assert "[COMPLETED]" in result.output
        assert '"task_id": "bg_new"' in result.output
        assert '"task_id": "bg_old"' not in result.output
        assert result.metadata["background_snapshot"]["kind"] == "wait_completed"

    async def test_wait_timeout_returns_snapshot_metadata(self) -> None:
        tool = WaitForBackgroundTaskTool()
        mgr = BackgroundTaskManager()

        async def slow() -> ToolResult:
            await asyncio.sleep(5)
            return ToolResult(output="done")

        mgr.launch("bg_run", "t", {}, slow())
        try:
            result = await tool.execute(
                WaitForBackgroundTaskInput(task_id="bg_run", timeout=1),
                _ctx(mgr),
            )
            assert "[TIMED_OUT" in result.output
            assert result.metadata["background_snapshot"]["kind"] == "wait_timed_out"
            assert result.metadata["background_snapshot"]["scope"] == "bg_run"
        finally:
            await mgr.cancel("bg_run")

    async def test_wait_no_tasks_returns_snapshot_metadata(self) -> None:
        tool = WaitForBackgroundTaskTool()
        mgr = BackgroundTaskManager()

        result = await tool.execute(
            WaitForBackgroundTaskInput(task_id="all", timeout=1),
            _ctx(mgr),
        )
        assert result.metadata["background_snapshot"]["kind"] == "wait_no_tasks"
        assert result.metadata["background_snapshot"]["statuses"] == []


class TestBackgroundSnapshotHelpers:
    def test_progress_render_matches_metadata_shape(self) -> None:
        statuses = [{"task_id": "bg_1", "status": "running", "output": "hello"}]
        output = render_background_snapshot("progress", statuses)
        metadata = build_background_snapshot_metadata("progress", "all", statuses)
        assert (
            output
            == '[\n  {\n    "task_id": "bg_1",\n    "status": "running",\n    "output": "hello"\n  }\n]'
        )
        assert metadata["background_snapshot"]["scope"] == "all"

    def test_wait_completed_render_matches_tool_branch(self) -> None:
        statuses = [{"task_id": "bg_1", "status": "completed", "output": "done"}]
        output = render_background_snapshot("wait_completed", statuses)
        assert output.startswith("[COMPLETED]\n[")

    def test_wait_timed_out_render_matches_tool_branch(self) -> None:
        statuses = [{"task_id": "bg_1", "status": "running", "output": "still"}]
        output = render_background_snapshot("wait_timed_out", statuses, elapsed_seconds=2.5)
        assert "[TIMED_OUT after 2.5s]" in output
        assert output.endswith(
            "Call wait_for_background_task again to continue waiting, or cancel_background_task to stop."
        )

    def test_wait_no_tasks_render_matches_tool_branch(self) -> None:
        output = render_background_snapshot("wait_no_tasks", [])
        assert output == (
            "[NO TASKS RUNNING] 0 background tasks are pending and "
            "none have ever been launched in this session. Do not poll "
            "or wait unless you launch new background work."
        )


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
            tool_input={"agent_name": "scout"},
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
        # Let the asyncio task settle.
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
            # The manager stamps a "[started: ...]" line at launch time, so
            # tail-only lines appended via append_progress live after that.
            mgr.append_progress(alias, "first")
            # Multi-line chunk should be split.
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
            # The manager prepends a "[started: ...]" stamp at launch; the
            # appended lines must appear at the tail.
            assert snap[0]["output"].endswith("live-1\nlive-2")
        finally:
            await mgr.cancel(alias, "")

    async def test_get_status_running_task_carries_start_stamp(self) -> None:
        """Running tasks always carry an `[started: ...]` stamp in output even
        before any progress lines are appended, so check_background_progress
        always has something to surface."""
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
