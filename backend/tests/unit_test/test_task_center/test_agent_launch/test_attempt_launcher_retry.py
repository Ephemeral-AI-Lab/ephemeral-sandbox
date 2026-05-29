"""Main-agent runner-seam coverage at the attempt launcher.

After Phase 2 of the agent-loop termination refactor, the engine no longer
exposes a ``max_terminal_retries`` knob. The runner makes a single attempt;
soft-warning notifications + the overshoot tolerance budget handle gentle
recovery inside the query loop. Tests below substitute a recording fake
runner so we can assert:

- The launcher invokes the runner exactly once per ``launch``.
- A no-terminal result (``status='completed'`` +
  ``terminal_result=None``) closes the task ``FAILED`` via the
  exhaustion-reporter path, leaving the harness free to schedule a new
  attempt row.
- Kwargs passed to the runner include ``persist_agent_run=True`` and
  ``task_id`` of the launch. The deleted ``max_terminal_retries`` kwarg
  must not reappear — the static-source check below guards that.
- Token-usage accounting comes through unchanged — the launcher
  doesn't manipulate the runner's reported counters.
"""

from __future__ import annotations

import asyncio
import inspect
from types import SimpleNamespace
from typing import Any

import pytest

from engine.agent.lifecycle import EphemeralRunResult
from task_center._core.primitives import planner_task_id
from task_center.attempt import AttemptFailReason, AttemptStatus
from task_center.attempt.launch import EphemeralAttemptAgentLauncher
from task_center.attempt.orchestrator_registry import AttemptOrchestratorRegistry
from task_center.attempt.deps import AgentLaunch, AttemptDeps
from task_center.iteration.state import IterationCreationReason
from task_center._core.task_state import TaskCenterTaskRole, TaskCenterTaskStatus
from tools._framework.core.base import ToolResult


def _seed_planner_attempt(
    *,
    workflow_store: Any,
    iteration_store: Any,
    attempt_store: Any,
    task_store: Any,
    task_center_run_id: str,
    attempt_sequence_no: int = 1,
) -> tuple[Any, Any, str]:
    """Insert a goal/iteration/attempt/planner-task row set; return key handles."""
    goal = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="outer-task",
        goal="solve",
    )
    iteration = iteration_store.insert(
        workflow_id=goal.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="solve",
        attempt_budget=4,
    )
    workflow_store.append_iteration_id(goal.id, iteration.id)
    attempt = attempt_store.insert(
        iteration_id=iteration.id,
        attempt_sequence_no=attempt_sequence_no,
    )
    iteration_store.append_attempt_id(iteration.id, attempt.id)
    task_id = planner_task_id(attempt.id)
    task_store.upsert_task(
        task_id=task_id,
        task_center_run_id=task_center_run_id,
        role=TaskCenterTaskRole.PLANNER.value,
        agent_name="planner",
        context_message="plan",
        status=TaskCenterTaskStatus.RUNNING.value,
        summaries=[],
        needs=[],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_planner",
    )
    return goal, attempt, task_id


def _build_launch(*, attempt: Any, goal: Any, task_id: str, task_center_run_id: str) -> AgentLaunch:
    return AgentLaunch(
        task_id=task_id,
        task_center_run_id=task_center_run_id,
        attempt_id=attempt.id,
        role=TaskCenterTaskRole.PLANNER,
        agent_name="planner",
        context="plan context",
        task_guidance="plan the work",
        needs=(),
        workflow_id=goal.id,
    )


def _build_deps(
    *, workflow_store: Any, iteration_store: Any, attempt_store: Any, task_store: Any
) -> AttemptDeps:
    return AttemptDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=SimpleNamespace(),  # noqa: ARG002 - launcher unused for these tests
        orchestrator_registry=AttemptOrchestratorRegistry(),
    )


@pytest.mark.asyncio
async def test_main_planner_engine_retry_keeps_attempt_sequence_no_at_one(
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id,
    register_test_agents,
) -> None:
    """A successful inner runner result keeps the attempt sequence at 1.

    The runner's single attempt resolved successfully (planner's
    terminal submission mutated the task to DONE) and no
    exhaustion-reporting fires. The runner is called exactly once.
    """
    goal, attempt, task_id = _seed_planner_attempt(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    launch = _build_launch(
        attempt=attempt, goal=goal, task_id=task_id, task_center_run_id=task_center_run_id
    )
    deps = _build_deps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )

    captured_kwargs: list[dict[str, Any]] = []

    async def _success_runner(*args: Any, **kwargs: Any) -> EphemeralRunResult:
        captured_kwargs.append(kwargs)
        # Simulate the planner's terminal submission tool transitioning
        # the task off RUNNING — real submission tools do this via
        # ``set_task_status``.
        task_store.set_task_status(
            task_id, status=TaskCenterTaskStatus.DONE.value, summary={}
        )
        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=ToolResult(
                output="full plan", is_error=False, is_terminal=True
            ),
            agent_name="planner",
            event_count=12,
        )

    launcher = EphemeralAttemptAgentLauncher(
        config=SimpleNamespace(),
        deps_provider=lambda: deps,
        runner=_success_runner,
    )
    launcher.launch(launch)
    await asyncio.wait_for(launcher.wait_for_idle(), timeout=1.0)

    # Exactly ONE runner invocation per launch.
    assert len(captured_kwargs) == 1
    # The attempt was NOT replaced or rerun by the launcher.
    refreshed_attempt = attempt_store.get(attempt.id)
    assert refreshed_attempt is not None
    assert refreshed_attempt.attempt_sequence_no == 1
    # Task moved off RUNNING via the simulated terminal submission.
    refreshed_task = task_store.get_task(task_id)
    assert refreshed_task is not None
    assert refreshed_task["status"] == TaskCenterTaskStatus.DONE.value


@pytest.mark.asyncio
async def test_main_planner_no_terminal_result_marks_attempt_failed(
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id,
    register_test_agents,
) -> None:
    """Runner returns no terminal_result → launcher closes that one Attempt FAILED.

    With no orchestrator registered, the launcher falls back to the
    _fail_unowned_attempt path which closes the task FAILED and the
    attempt with PLANNER_FAILED. The harness is then free to schedule
    a new attempt_sequence_no=2 (not exercised here — the assertion is
    that the failure was recorded on attempt_sequence_no=1).
    """
    goal, attempt, task_id = _seed_planner_attempt(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    launch = _build_launch(
        attempt=attempt, goal=goal, task_id=task_id, task_center_run_id=task_center_run_id
    )
    deps = _build_deps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )

    runner_calls: list[int] = []

    async def _exhausted_runner(*args: Any, **kwargs: Any) -> EphemeralRunResult:
        runner_calls.append(1)
        # Agent exited gracefully without delivering a terminal result
        # (e.g. text-only response, or overshoot-tolerance exhausted).
        # No crash, but no terminal_result either.
        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=None,
            agent_name="planner",
            event_count=8,
        )

    launcher = EphemeralAttemptAgentLauncher(
        config=SimpleNamespace(),
        deps_provider=lambda: deps,
        runner=_exhausted_runner,
    )
    launcher.launch(launch)
    await asyncio.wait_for(launcher.wait_for_idle(), timeout=1.0)

    # One runner call — recovery is the query loop's concern, not the launcher's.
    assert len(runner_calls) == 1
    # The launcher marked this Attempt FAILED — harness can now create
    # attempt_sequence_no=2.
    refreshed_attempt = attempt_store.get(attempt.id)
    assert refreshed_attempt is not None
    assert refreshed_attempt.attempt_sequence_no == 1
    assert refreshed_attempt.status == AttemptStatus.FAILED
    assert refreshed_attempt.fail_reason == AttemptFailReason.PLANNER_FAILED
    refreshed_task = task_store.get_task(task_id)
    assert refreshed_task is not None
    assert refreshed_task["status"] == TaskCenterTaskStatus.FAILED.value


@pytest.mark.asyncio
async def test_attempt_harness_records_runner_token_usage(
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id,
    register_test_agents,
) -> None:
    """Token-count accounting passes through unchanged from the runner.

    The launcher does NOT manipulate ``event_count`` or any token-count
    field on the runner's :class:`EphemeralRunResult`. The result reaches
    the runner's own bookkeeping — the launcher only inspects ``status``.
    """
    goal, attempt, task_id = _seed_planner_attempt(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    launch = _build_launch(
        attempt=attempt, goal=goal, task_id=task_id, task_center_run_id=task_center_run_id
    )
    deps = _build_deps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )

    captured_results: list[EphemeralRunResult] = []

    async def _runner(*args: Any, **kwargs: Any) -> EphemeralRunResult:
        result = EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=ToolResult(
                output="plan", is_error=False, is_terminal=True
            ),
            agent_name="planner",
            event_count=42,  # mimics aggregated cross-attempt count
        )
        captured_results.append(result)
        task_store.set_task_status(
            task_id, status=TaskCenterTaskStatus.DONE.value, summary={}
        )
        return result

    launcher = EphemeralAttemptAgentLauncher(
        config=SimpleNamespace(),
        deps_provider=lambda: deps,
        runner=_runner,
    )
    launcher.launch(launch)
    await asyncio.wait_for(launcher.wait_for_idle(), timeout=1.0)

    # The runner's result reached the launcher's flow unchanged — the
    # launcher consumed it without rewriting event_count.
    assert len(captured_results) == 1
    assert captured_results[0].event_count == 42


@pytest.mark.asyncio
async def test_continuation_planner_attempt_does_not_pass_retry_kwarg(
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id,
    register_test_agents,
) -> None:
    """For attempt_sequence_no>1, the launcher does not pass ``max_terminal_retries``.

    The kwarg was deleted in Phase 2 of the agent-loop termination
    refactor. This test pins the launcher's runner-call kwargs to the
    post-deletion shape: ``persist_agent_run`` and ``task_id`` present,
    no resurrection of ``max_terminal_retries``.
    """
    goal, attempt, task_id = _seed_planner_attempt(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        attempt_sequence_no=2,
    )
    launch = _build_launch(
        attempt=attempt, goal=goal, task_id=task_id, task_center_run_id=task_center_run_id
    )
    deps = _build_deps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )

    captured_kwargs: list[dict[str, Any]] = []

    async def _runner(*args: Any, **kwargs: Any) -> EphemeralRunResult:
        captured_kwargs.append(kwargs)
        task_store.set_task_status(
            task_id, status=TaskCenterTaskStatus.DONE.value, summary={}
        )
        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=ToolResult(
                output="continuation plan",
                is_error=False,
                is_terminal=True,
            ),
            agent_name="planner",
            event_count=1,
        )

    launcher = EphemeralAttemptAgentLauncher(
        config=SimpleNamespace(),
        deps_provider=lambda: deps,
        runner=_runner,
    )
    launcher.launch(launch)
    await asyncio.wait_for(launcher.wait_for_idle(), timeout=1.0)

    assert len(captured_kwargs) == 1
    # The launcher must not pass the deleted ``max_terminal_retries`` kwarg.
    assert "max_terminal_retries" not in captured_kwargs[0]
    # Sanity check: the launch still routed through with the correct
    # task_id binding.
    assert captured_kwargs[0]["task_id"] == task_id
    assert captured_kwargs[0]["persist_agent_run"] is True


def test_launcher_runner_kwargs_do_not_reference_deleted_retry_kwarg() -> None:
    """Static check: launcher source does NOT pass ``max_terminal_retries``.

    The kwarg was deleted from ``run_ephemeral_agent`` in Phase 2 of the
    agent-loop termination refactor. Re-introducing it at the launcher
    seam would raise ``TypeError`` at runtime; this static check catches
    such refactors at test time.
    """
    source = inspect.getsource(EphemeralAttemptAgentLauncher._run_launch)
    assert "max_terminal_retries" not in source, (
        "EphemeralAttemptAgentLauncher._run_launch must NOT pass a "
        "``max_terminal_retries`` kwarg — the engine deleted that kwarg "
        "in Phase 2 of the agent-loop termination refactor. The soft-"
        "limit + overshoot tolerance pathway in the query loop now "
        "handles graceful recovery instead."
    )


@pytest.mark.asyncio
async def test_main_agent_launches_with_two_user_messages(
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id,
    register_test_agents,
) -> None:
    """Non-entry agents launch with initial_messages=[<context>] + prompt=<task_guidance>."""
    from message.message import Message

    goal, attempt, task_id = _seed_planner_attempt(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    launch = _build_launch(
        attempt=attempt, goal=goal, task_id=task_id, task_center_run_id=task_center_run_id
    )
    deps = _build_deps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )

    captured: list[dict[str, Any]] = []

    async def _spy_runner(*args: Any, **kwargs: Any) -> EphemeralRunResult:
        captured.append({"args": args, "kwargs": kwargs})
        task_store.set_task_status(
            task_id, status=TaskCenterTaskStatus.DONE.value, summary={}
        )
        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=ToolResult(
                output="plan", is_error=False, is_terminal=True
            ),
            agent_name="planner",
            event_count=1,
        )

    launcher = EphemeralAttemptAgentLauncher(
        config=SimpleNamespace(),
        deps_provider=lambda: deps,
        runner=_spy_runner,
    )
    launcher.launch(launch)
    await asyncio.wait_for(launcher.wait_for_idle(), timeout=1.0)

    assert len(captured) == 1
    args = captured[0]["args"]
    kwargs = captured[0]["kwargs"]
    # Spawn prompt is the task_guidance text from _build_launch.
    assert args[1] == "plan the work"
    # initial_messages carries the rendered context (one user msg).
    initial_messages = kwargs.get("initial_messages")
    assert isinstance(initial_messages, list)
    assert len(initial_messages) == 1
    msg = initial_messages[0]
    assert isinstance(msg, Message)
    assert msg.role == "user"
    assert msg.assistant_text == "plan context"


@pytest.mark.asyncio
async def test_launch_without_task_guidance_falls_back_to_single_user_message(
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id,
    register_test_agents,
) -> None:
    """Agents whose recipe emits no task_guidance launch single-message.

    The launcher must NOT pass initial_messages when task_guidance
    is None or empty — the context becomes the spawn prompt directly.
    """
    goal, attempt, task_id = _seed_planner_attempt(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    # Use the planner agent (registered by the fixture) but with
    # ``task_guidance=None`` to exercise the single-message
    # fallback. The fallback decision in ``_run_launch`` keys on
    # ``task_guidance``, not on role.
    launch = AgentLaunch(
        task_id=task_id,
        task_center_run_id=task_center_run_id,
        attempt_id=attempt.id,
        role=TaskCenterTaskRole.PLANNER,
        agent_name="planner",
        context="execute this task",
        task_guidance=None,  # no task-guidance prose
        needs=(),
        workflow_id=goal.id,
    )
    deps = _build_deps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )

    captured: list[dict[str, Any]] = []

    async def _spy_runner(*args: Any, **kwargs: Any) -> EphemeralRunResult:
        captured.append({"args": args, "kwargs": kwargs})
        task_store.set_task_status(
            task_id, status=TaskCenterTaskStatus.DONE.value, summary={}
        )
        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=ToolResult(
                output="ok", is_error=False, is_terminal=True
            ),
            agent_name="planner",
            event_count=1,
        )

    launcher = EphemeralAttemptAgentLauncher(
        config=SimpleNamespace(),
        deps_provider=lambda: deps,
        runner=_spy_runner,
    )
    launcher.launch(launch)
    await asyncio.wait_for(launcher.wait_for_idle(), timeout=1.0)

    assert len(captured) == 1
    args = captured[0]["args"]
    kwargs = captured[0]["kwargs"]
    # Context becomes the spawn prompt; no initial_messages seeded.
    assert args[1] == "execute this task"
    assert kwargs.get("initial_messages") is None


@pytest.mark.asyncio
async def test_main_agent_launches_with_skill_as_prompt_and_context_guidance_initial(
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id,
    register_test_agents,
) -> None:
    """Skill+guidance (4-row) shape: the skill is the spawn prompt (lands last),
    initial_messages = [context, guidance] in canonical order."""
    from message.message import Message

    goal, attempt, task_id = _seed_planner_attempt(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    launch = AgentLaunch(
        task_id=task_id,
        task_center_run_id=task_center_run_id,
        attempt_id=attempt.id,
        role=TaskCenterTaskRole.PLANNER,
        agent_name="planner",
        context="plan context",
        task_guidance="plan the work",
        skill="Load skill: planner",
        needs=(),
        workflow_id=goal.id,
    )
    deps = _build_deps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
    )

    captured: list[dict[str, Any]] = []

    async def _spy_runner(*args: Any, **kwargs: Any) -> EphemeralRunResult:
        captured.append({"args": args, "kwargs": kwargs})
        task_store.set_task_status(
            task_id, status=TaskCenterTaskStatus.DONE.value, summary={}
        )
        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=ToolResult(
                output="plan", is_error=False, is_terminal=True
            ),
            agent_name="planner",
            event_count=1,
        )

    launcher = EphemeralAttemptAgentLauncher(
        config=SimpleNamespace(),
        deps_provider=lambda: deps,
        runner=_spy_runner,
    )
    launcher.launch(launch)
    await asyncio.wait_for(launcher.wait_for_idle(), timeout=1.0)

    assert len(captured) == 1
    args = captured[0]["args"]
    kwargs = captured[0]["kwargs"]
    # Skill is the spawn prompt (appended last → row 4).
    assert args[1] == "Load skill: planner"
    initial_messages = kwargs.get("initial_messages")
    assert isinstance(initial_messages, list)
    assert all(isinstance(m, Message) and m.role == "user" for m in initial_messages)
    # Canonical order: context, then guidance.
    assert [m.assistant_text for m in initial_messages] == ["plan context", "plan the work"]
