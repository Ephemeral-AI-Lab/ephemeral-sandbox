"""Phase 2.5 slice 6 — ``background_tool.*`` daemon-ring emitter coverage."""

from __future__ import annotations

import asyncio
import threading
from typing import Any

import pytest

from engine.background.task_supervisor import (
    BackgroundTaskStatus,
    BackgroundTaskSupervisor,
)
from notification import SystemNotificationService
from sandbox.daemon.audit_buffer import get_audit_buffer
from tools import ToolResult


_AUDIT_CURSOR = {"seq": -1}


def _drain_background_events() -> list[dict[str, Any]]:
    buf = get_audit_buffer()
    snap = buf.pull(after_seq=_AUDIT_CURSOR["seq"], limit=10_000)
    events = snap.get("events", [])
    if events:
        _AUDIT_CURSOR["seq"] = int(events[-1]["seq"])
    return [evt for evt in events if str(evt.get("type", "")).startswith("background_tool.")]


@pytest.fixture(autouse=True)
def _reset_audit_cursor() -> None:
    buf = get_audit_buffer()
    cursor = -1
    while True:
        snap = buf.pull(after_seq=cursor, limit=10_000)
        events = snap.get("events", [])
        if not events:
            break
        cursor = int(events[-1]["seq"])
    _AUDIT_CURSOR["seq"] = cursor
    yield


@pytest.mark.asyncio
async def test_background_tool_lifecycle_emits_started_completed_delivered() -> None:
    sup = BackgroundTaskSupervisor()

    async def _ok() -> ToolResult:
        return ToolResult(output="hello")

    sup.launch(
        task_id="bg_1",
        tool_name="exec_command",
        tool_input={"cmd": "ls"},
        coro=_ok(),
        agent_id="agent-x",
    )
    # Wait for the asyncio task done-callback to flip status.
    await asyncio.sleep(0.05)
    completed = sup.collect_completed()
    assert [t.task_id for t in completed] == ["bg_1"]

    events = _drain_background_events()
    types = [e["type"] for e in events]
    assert types == [
        "background_tool.started",
        "background_tool.completed",
        "background_tool.delivered",
    ]
    completed_event = next(e for e in events if e["type"] == "background_tool.completed")
    section = completed_event["payload"]["background_tool"]
    assert section["background_task_id"] == "bg_1"
    assert section["status"] == BackgroundTaskStatus.COMPLETED.value
    assert section["tool_name"] == "exec_command"


@pytest.mark.asyncio
async def test_background_tool_failed_lifecycle() -> None:
    sup = BackgroundTaskSupervisor()

    async def _boom() -> ToolResult:
        raise ValueError("nope")

    sup.launch(
        task_id="bg_fail",
        tool_name="exec_command",
        tool_input={},
        coro=_boom(),
    )
    await asyncio.sleep(0.05)
    sup.collect_completed()

    events = _drain_background_events()
    failed = next(e for e in events if e["type"] == "background_tool.failed")
    assert failed["payload"]["background_tool"]["background_task_id"] == "bg_fail"
    assert failed["payload"]["background_tool"]["error_kind"] == "error"


@pytest.mark.asyncio
async def test_background_tool_cancelled_lifecycle() -> None:
    sup = BackgroundTaskSupervisor()

    async def _long_running() -> ToolResult:
        await asyncio.sleep(5)
        return ToolResult(output="done")

    sup.launch(
        task_id="bg_cancel",
        tool_name="exec_command",
        tool_input={},
        coro=_long_running(),
    )
    await sup.cancel("bg_cancel", reason="user_request")
    # Give the asyncio task a tick to finish cancellation cleanup.
    await asyncio.sleep(0.05)
    sup.collect_completed()
    events = _drain_background_events()
    cancelled = next(e for e in events if e["type"] == "background_tool.cancelled")
    section = cancelled["payload"]["background_tool"]
    assert section["cancel_reason"] == "user_request"
    assert section["background_task_id"] == "bg_cancel"


def test_background_tool_emitter_adds_no_new_threads_on_launch() -> None:
    sup = BackgroundTaskSupervisor()
    before = threading.active_count()

    async def _drive() -> None:
        async def _ok() -> ToolResult:
            return ToolResult(output="ok")

        sup.launch(
            task_id="bg_thread",
            tool_name="exec_command",
            tool_input={},
            coro=_ok(),
        )
        await asyncio.sleep(0.02)
        sup.collect_completed()

    asyncio.run(_drive())
    after = threading.active_count()
    assert after == before


@pytest.mark.asyncio
async def test_background_tool_heartbeat_reuses_existing_timer(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Phase 2.6 backfill — heartbeat reuses one asyncio task; no new threads.

    Closes the §Tests gap from phase 2.5 (which wired the heartbeat emit but
    did not pin the no-new-thread + existing-timer-reuse invariants).
    """
    import engine.background.task_supervisor as task_supervisor

    monkeypatch.setattr(task_supervisor, "_HEARTBEAT_INTERVAL_S", 0.05)
    # Heartbeat path tries to call ``sandbox.api.heartbeat`` — stub it so the
    # test does not depend on a live sandbox.
    import sys

    fake = type(sys)("sandbox.api")

    async def _fake_heartbeat(sandbox_id: str, invocation_ids: list[str]) -> None:
        del sandbox_id, invocation_ids
        return None

    fake.heartbeat = _fake_heartbeat  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "sandbox.api", fake)

    sup = task_supervisor.BackgroundTaskSupervisor()
    thread_before = threading.active_count()
    task_count_before = len(asyncio.all_tasks())

    async def _long() -> ToolResult:
        await asyncio.sleep(0.3)
        return ToolResult(output="done")

    sup.launch(
        task_id="bg_hb",
        tool_name="exec_command",
        tool_input={},
        coro=_long(),
        agent_id="agent-hb",
        uses_sandbox=True,
        sandbox_id="sb-x",
        sandbox_invocation_id="inv-y",
    )
    # Two ticks of the heartbeat (0.05 s × 2 + slack).
    await asyncio.sleep(0.18)
    events = _drain_background_events()
    heartbeats = [e for e in events if e["type"] == "background_tool.heartbeat"]
    assert heartbeats, "expected at least one background_tool.heartbeat emit"
    for evt in heartbeats:
        section = evt["payload"]["background_tool"]
        assert section["background_task_id"] == "bg_hb"
        assert evt["lane"] == "sample"

    # Invariant (a): thread count unchanged. The heartbeat is an asyncio
    # task on the existing loop — NOT a new thread.
    assert threading.active_count() == thread_before
    # Invariant (c): supervisor owns exactly ONE heartbeat task.
    assert sup._heartbeat_task is not None  # noqa: SLF001
    delta_tasks = len(asyncio.all_tasks()) - task_count_before
    # Exactly two tasks added: the background task + the heartbeat. The
    # heartbeat is NOT respawned per tick (would surface as > 2).
    assert delta_tasks == 2

    # Cancel so the test finishes promptly.
    await sup.cancel_all()


@pytest.mark.asyncio
async def test_command_session_natural_exit_completion_emits_one_notification(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api as sandbox_api

    calls = 0

    async def _collect_command_completions(
        sandbox_id: str,
        *,
        agent_id: str,
        command_session_ids: list[str],
    ) -> list[dict[str, Any]]:
        nonlocal calls
        calls += 1
        assert sandbox_id == "sb-1"
        assert agent_id == "agent-1"
        assert command_session_ids == ["cmd_1"]
        return [
            {
                "command_session_id": "cmd_1",
                "agent_id": "agent-1",
                "command": "printf done",
                "result": {
                    "status": "ok",
                    "exit_code": 0,
                    "output": {"stdout": "done\n", "stderr": ""},
                },
            }
        ]

    monkeypatch.setattr(sandbox_api, "collect_command_completions", _collect_command_completions)

    sup = BackgroundTaskSupervisor()
    sup.register_command_session(
        command_session_id="cmd_1",
        sandbox_id="sb-1",
        agent_id="agent-1",
        command="printf done",
    )

    notes = await sup.collect_command_session_completion_notifications()
    assert len(notes) == 1
    assert '[BACKGROUND COMPLETED] command_session_id="cmd_1"' in notes[0]
    assert "status=ok exit_code=0" in notes[0]
    assert "command: printf done" in notes[0]
    assert "stdout:\ndone" in notes[0]
    assert not sup.has_pending()
    assert sup.count_by_agent("agent-1") == 0

    assert await sup.collect_command_session_completion_notifications() == []
    assert calls == 1
    await asyncio.sleep(0)


@pytest.mark.asyncio
async def test_command_session_timeout_completion_notification_is_failed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api as sandbox_api

    async def _collect_command_completions(
        sandbox_id: str,
        *,
        agent_id: str,
        command_session_ids: list[str],
    ) -> list[dict[str, Any]]:
        del sandbox_id, agent_id, command_session_ids
        return [
            {
                "command_session_id": "cmd_timeout",
                "result": {
                    "status": "timed_out",
                    "exit_code": 124,
                    "output": {"stdout": "", "stderr": "timeout\n"},
                },
            }
        ]

    monkeypatch.setattr(sandbox_api, "collect_command_completions", _collect_command_completions)

    sup = BackgroundTaskSupervisor()
    sup.register_command_session(
        command_session_id="cmd_timeout",
        sandbox_id="sb-1",
        agent_id="agent-1",
        command="sleep 60",
    )

    notes = await sup.collect_command_session_completion_notifications()
    assert len(notes) == 1
    assert "status=timed_out exit_code=124" in notes[0]
    assert "stderr:\ntimeout" in notes[0]
    assert sup._command_sessions["cmd_timeout"].status == BackgroundTaskStatus.DELIVERED
    await asyncio.sleep(0)


@pytest.mark.asyncio
async def test_tool_reported_command_session_result_suppresses_completion_notification(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api as sandbox_api

    async def _collect_command_completions(
        sandbox_id: str,
        *,
        agent_id: str,
        command_session_ids: list[str],
    ) -> list[dict[str, Any]]:
        raise AssertionError("tool-reported command-session records should not be polled")

    monkeypatch.setattr(sandbox_api, "collect_command_completions", _collect_command_completions)

    sup = BackgroundTaskSupervisor()
    sup.register_command_session(
        command_session_id="cmd_reported",
        sandbox_id="sb-1",
        agent_id="agent-1",
        command="sleep 60",
    )
    assert sup.has_pending()

    sup.mark_command_session_result_reported_by_tool(
        command_session_id="cmd_reported",
        result={
            "status": "cancelled",
            "exit_code": None,
            "output": {"stdout": "", "stderr": ""},
        },
    )

    assert await sup.collect_command_session_completion_notifications() == []
    assert not sup.has_pending()
    assert sup.count_by_agent("agent-1") == 0
    await asyncio.sleep(0)


@pytest.mark.asyncio
async def test_generic_command_session_not_found_does_not_suppress_completion_notification() -> (
    None
):
    from sandbox._shared.models import CommandOutput, ExecCommandResult
    from tools._framework.core.context import ToolExecutionContextService
    from tools.sandbox._lib.command_session_tool import mark_command_session_result_reported_by_tool

    sup = BackgroundTaskSupervisor()
    sup.register_command_session(
        command_session_id="cmd_missing",
        sandbox_id="sb-1",
        agent_id="agent-1",
        command="printf done",
    )

    mark_command_session_result_reported_by_tool(
        ToolExecutionContextService(
            cwd=".",
            services={"background_task_manager": sup},
        ),
        ExecCommandResult(
            success=False,
            status="error",
            exit_code=None,
            output=CommandOutput(stderr="command_session_not_found"),
        ),
        command_session_id="cmd_missing",
    )

    assert sup.has_pending()
    assert sup.count_by_agent("agent-1") == 1

    sup.mark_command_session_result_reported_by_tool(
        command_session_id="cmd_missing",
        result={
            "status": "cancelled",
            "exit_code": None,
            "output": {"stdout": "", "stderr": ""},
        },
    )
    await asyncio.sleep(0)


@pytest.mark.asyncio
async def test_subagent_completion_emits_typed_notification() -> None:
    sup = BackgroundTaskSupervisor()

    async def _done() -> ToolResult:
        return ToolResult(
            output="findings",
            metadata={"subagent_terminal_called": True},
        )

    sup.launch(
        task_id="subagent_1",
        tool_name="run_subagent",
        tool_input={"agent_name": "explorer"},
        coro=_done(),
        task_type="subagent",
    )
    await asyncio.sleep(0.05)

    notes = sup.collect_subagent_completion_notifications()

    assert len(notes) == 1
    assert '[SUBAGENT COMPLETED] subagent_session_id="subagent_1"' in notes[0]
    assert "status=finished" in notes[0]
    assert "agent_name: explorer" in notes[0]
    assert "result:\nfindings" in notes[0]
    assert sup._tasks["subagent_1"].status == BackgroundTaskStatus.DELIVERED
    assert sup.collect_subagent_completion_notifications() == []


@pytest.mark.asyncio
async def test_generic_completion_collection_does_not_deliver_subagents() -> None:
    sup = BackgroundTaskSupervisor()

    async def _done() -> ToolResult:
        return ToolResult(
            output="findings",
            metadata={"subagent_terminal_called": True},
        )

    sup.launch(
        task_id="subagent_1",
        tool_name="run_subagent",
        tool_input={"agent_name": "explorer"},
        coro=_done(),
        task_type="subagent",
    )
    await asyncio.sleep(0.05)

    assert sup.collect_completed() == []
    assert sup._tasks["subagent_1"].status == BackgroundTaskStatus.COMPLETED
    assert sup.collect_subagent_completion_notifications()


@pytest.mark.asyncio
async def test_query_loop_helper_drains_command_session_completion_into_notifications(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from engine.query.loop import _drain_background_completion_notifications
    import sandbox.api as sandbox_api

    async def _collect_command_completions(
        sandbox_id: str,
        *,
        agent_id: str,
        command_session_ids: list[str],
    ) -> list[dict[str, Any]]:
        del sandbox_id, agent_id, command_session_ids
        return [
            {
                "command_session_id": "cmd_2",
                "result": {
                    "status": "ok",
                    "exit_code": 0,
                    "output": {"stdout": "done\n", "stderr": ""},
                },
            }
        ]

    monkeypatch.setattr(sandbox_api, "collect_command_completions", _collect_command_completions)

    sup = BackgroundTaskSupervisor()
    sup.register_command_session(
        command_session_id="cmd_2",
        sandbox_id="sb-1",
        agent_id="agent-1",
    )
    service = SystemNotificationService()
    service.register_agent_run()

    await _drain_background_completion_notifications(sup, service)

    pending = service.pop_pending_notifications()
    assert len(pending) == 1
    assert '[BACKGROUND COMPLETED] command_session_id="cmd_2"' in pending[0].text
    await asyncio.sleep(0)


@pytest.mark.asyncio
async def test_query_loop_helper_drains_subagent_completion_into_notifications() -> None:
    from engine.query.loop import _drain_background_completion_notifications

    sup = BackgroundTaskSupervisor()

    async def _done() -> ToolResult:
        return ToolResult(
            output="summary",
            metadata={"subagent_terminal_called": True},
        )

    sup.launch(
        task_id="subagent_1",
        tool_name="run_subagent",
        tool_input={"agent_name": "explorer"},
        coro=_done(),
        task_type="subagent",
    )
    await asyncio.sleep(0.05)
    service = SystemNotificationService()
    service.register_agent_run()

    await _drain_background_completion_notifications(sup, service)

    pending = service.pop_pending_notifications()
    assert len(pending) == 1
    assert '[SUBAGENT COMPLETED] subagent_session_id="subagent_1"' in pending[0].text
