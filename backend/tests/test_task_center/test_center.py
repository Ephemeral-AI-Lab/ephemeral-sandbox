"""End-to-end tests for ``task_center.center.TaskCenter``."""

from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable

import pytest

from task_center import Status
from task_center.center import TaskCenter


# Type alias for scripted action coroutines.
Action = Callable[[TaskCenter, str], Awaitable[None]]


def _scripted_spawn(scripts: dict[str, Action]):
    async def spawn(task_id: str, tc: TaskCenter, sandbox_id: str | None) -> None:
        del sandbox_id
        action = scripts.get(task_id)
        if action is not None:
            await action(tc, task_id)
        # If no script is registered, the agent "exits without terminal tool"
        # and the dispatcher will mark the task FAILED.
    return spawn


# --------------------------------------------------------------------------- #
# E1 — Trivial task                                                           #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_trivial_task_closes_with_summary() -> None:
    """Root executor calls submit_task_completion with no children."""

    async def root_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_task_completion(task_id, "did the thing")

    tc = TaskCenter(spawn_func=_scripted_spawn({"t1": root_action}))
    root = await tc.run_query("just do it")

    assert root.status is Status.DONE
    assert root.summary == "did the thing"
    assert root.id == "t1"


# --------------------------------------------------------------------------- #
# E2 — Full handoff happy path                                                #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_full_handoff_happy_path() -> None:
    """Root submits a DAG plan; evaluator runs once and closes the root."""

    tasks = [
        {"id": "p1a"},
        {"id": "p1b"},
        {"id": "p2", "deps": ["p1a", "p1b"]},
    ]
    specs = {
        "p1a": {"title": "P1A", "spec": "..."},
        "p1b": {"title": "P1B", "spec": "..."},
        "p2": {"title": "P2", "spec": "..."},
    }

    async def root_action(tc, tid):
        tc.submit_full_handoff(tid, tasks, specs, "Both children produce evidence.")

    async def child_done(tc, tid):
        tc.submit_task_completion(tid, f"done {tid}")

    async def eval_action(tc, tid):
        tc.submit_task_completion(tid, "all criteria satisfied")

    scripts = {
        "t1": root_action,
        "p1a": child_done,
        "p1b": child_done,
        "p2": child_done,
        "t1-eval": eval_action,
    }
    tc = TaskCenter(spawn_func=_scripted_spawn(scripts))
    root = await tc.run_query("Do a DAG task.")

    assert root.status is Status.DONE
    assert root.summary == "all criteria satisfied"
    # Every child + evaluator should be DONE.
    for tid in ("p1a", "p1b", "p2", "t1-eval"):
        assert tc.graph.get(tid).status is Status.DONE


# --------------------------------------------------------------------------- #
# E3 — Continuation                                                           #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_continue_to_work_propagates_through_evaluator() -> None:
    """Evaluator calls submit_continue_to_work; continuation closes the chain."""

    tasks = [{"id": "p1"}]
    specs = {"p1": {"title": "P1", "spec": "..."}}

    async def root_action(tc, tid):
        tc.submit_full_handoff(tid, tasks, specs, "ac")

    async def p1_action(tc, tid):
        tc.submit_task_completion(tid, "done p1")

    async def eval_action(tc, tid):
        tc.submit_continue_to_work(tid, "gap remains: missing X")

    async def cont_action(tc, tid):
        tc.submit_task_completion(tid, "filled the gap")

    scripts = {
        "t1": root_action,
        "p1": p1_action,
        "t1-eval": eval_action,
        "t2": cont_action,  # continuation gets the next sequential id
    }
    tc = TaskCenter(spawn_func=_scripted_spawn(scripts))
    root = await tc.run_query("Driven scenario.")

    # Final summary propagates from the continuation up through the evaluator
    # and the original root.
    assert root.status is Status.DONE
    assert root.summary == "filled the gap"
    assert tc.graph.get("t1-eval").status is Status.DONE
    assert tc.graph.get("t1-eval").summary == "filled the gap"
    cont = tc.graph.get("t2")
    assert cont.status is Status.DONE
    assert cont.subtree_kind == "continuation"
    assert cont.closes_for == "t1-eval"


# --------------------------------------------------------------------------- #
# E4 — Recursive opacity                                                      #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_recursive_opacity_parent_only_sees_direct_children() -> None:
    """A child's nested handoff stays opaque to the parent's evaluator."""

    tasks_root = [{"id": "a"}, {"id": "b"}]
    specs_root = {
        "a": {"title": "A", "spec": "..."},
        "b": {"title": "B", "spec": "..."},
    }
    tasks_a = [{"id": "a1"}]
    specs_a = {"a1": {"title": "A1", "spec": "..."}}

    async def root_action(tc, tid):
        tc.submit_full_handoff(tid, tasks_root, specs_root, "ac")

    async def a_action(tc, tid):
        tc.submit_full_handoff(tid, tasks_a, specs_a, "a's ac")

    async def b_action(tc, tid):
        tc.submit_task_completion(tid, "b done")

    async def a1_action(tc, tid):
        tc.submit_task_completion(tid, "a1 done")

    async def eval_action(tc, tid):
        tc.submit_task_completion(tid, "all good")

    scripts = {
        "t1": root_action,
        "a": a_action,
        "b": b_action,
        "a1": a1_action,
        "a-eval": eval_action,
        "t1-eval": eval_action,
    }
    tc = TaskCenter(spawn_func=_scripted_spawn(scripts))
    root = await tc.run_query("recursive scenario")

    # Root closes successfully.
    assert root.status is Status.DONE

    # The parent's direct children are exactly {a, b, t1-eval}, never a1 or a-eval.
    direct_ids = set(tc.graph.get("t1").children)
    assert direct_ids == {"a", "b", "t1-eval"}
    # a's nested children are not in t1's children list.
    assert "a1" not in direct_ids
    assert "a-eval" not in direct_ids


# --------------------------------------------------------------------------- #
# E5 — Skip-back pipelining                                                   #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_dag_pipelining_launches_unblocked_task_while_sibling_runs() -> None:
    """Task d launches as soon as dependency a is DONE while sibling b runs."""

    tasks = [
        {"id": "a"},
        {"id": "b"},
        {"id": "c", "deps": ["a", "b"]},
        {"id": "d", "deps": ["a"]},
    ]
    specs = {tid: {"title": tid, "spec": "..."} for tid in ("a", "b", "c", "d")}

    b_can_finish = asyncio.Event()
    c_can_finish = asyncio.Event()
    d_observed: dict[str, str] = {}

    async def root_action(tc, tid):
        tc.submit_full_handoff(tid, tasks, specs, "ac")

    async def a_action(tc, tid):
        tc.submit_task_completion(tid, "a done")

    async def b_action(tc, tid):
        # Wait until d has run while b was still RUNNING.
        await b_can_finish.wait()
        tc.submit_task_completion(tid, "b done")

    async def c_action(tc, tid):
        await c_can_finish.wait()
        tc.submit_task_completion(tid, "c done")

    async def d_action(tc, tid):
        # When d gets to run, b should still be RUNNING; d does not wait on
        # unrelated sibling tasks.
        d_observed["b_status"] = tc.graph.get("b").status.value
        d_observed["c_status"] = tc.graph.get("c").status.value
        tc.submit_task_completion(tid, "d done")
        # Now release the rest so the test can finish.
        b_can_finish.set()
        c_can_finish.set()

    async def eval_action(tc, tid):
        tc.submit_task_completion(tid, "all done")

    scripts = {
        "t1": root_action,
        "a": a_action,
        "b": b_action,
        "c": c_action,
        "d": d_action,
        "t1-eval": eval_action,
    }
    tc = TaskCenter(spawn_func=_scripted_spawn(scripts))
    root = await tc.run_query("DAG pipelining scenario")

    assert root.status is Status.DONE
    # When d ran, b had not yet completed.
    # b was either RUNNING or PENDING depending on dispatcher timing — but
    # crucially NOT DONE when d started.
    assert d_observed["b_status"] != "done"
    # c needs both a and b, so it had not yet started or was waiting.
    assert d_observed["c_status"] in ("pending", "running")


# --------------------------------------------------------------------------- #
# Bonus — agent that exits without a terminal tool is marked FAILED           #
# --------------------------------------------------------------------------- #


@pytest.mark.asyncio
async def test_agent_without_terminal_call_is_marked_failed() -> None:
    """If the spawn function returns without calling submit_*, mark FAILED."""

    async def root_does_nothing(tc, tid):
        # No submit_* call.
        return

    tc = TaskCenter(spawn_func=_scripted_spawn({"t1": root_does_nothing}))
    root = await tc.run_query("no-op")

    assert root.status is Status.FAILED
    assert root.summary == "agent exited without a terminal tool call"


@pytest.mark.asyncio
async def test_child_failure_fails_whole_team_run() -> None:
    """Any child failure closes the active root as FAILED instead of hanging."""

    tasks = [{"id": "child"}]
    specs = {"child": {"title": "Child", "spec": "..."}}

    async def root_action(tc, tid):
        tc.submit_full_handoff(tid, tasks, specs, "child must succeed")

    async def child_returns_without_terminal(tc, tid):
        del tc, tid
        return

    tc = TaskCenter(
        spawn_func=_scripted_spawn(
            {
                "t1": root_action,
                "child": child_returns_without_terminal,
            }
        )
    )
    root = await asyncio.wait_for(tc.run_query("handoff"), timeout=1)

    assert tc.graph.get("child").status is Status.FAILED
    assert root.status is Status.FAILED
    assert root.summary == (
        "team run failed because task 'child' failed: "
        "agent exited without a terminal tool call"
    )


@pytest.mark.asyncio
async def test_run_query_passes_sandbox_id_to_spawned_tasks() -> None:
    seen: list[tuple[str, str | None]] = []

    async def spawn(task_id: str, tc: TaskCenter, sandbox_id: str | None) -> None:
        seen.append((task_id, sandbox_id))
        tc.submit_task_completion(task_id, "done")

    tc = TaskCenter(spawn_func=spawn)
    await tc.run_query("use selected sandbox", sandbox_id="sandbox-123")

    assert seen == [("t1", "sandbox-123")]


@pytest.mark.asyncio
async def test_each_query_gets_fresh_graph_for_agent_supplied_child_ids() -> None:
    tasks = [{"id": "a"}]
    specs = {"a": {"title": "A", "spec": "..."}}

    async def root_action(tc, tid):
        tc.submit_full_handoff(tid, tasks, specs, "a completes")

    async def child_action(tc, tid):
        tc.submit_task_completion(tid, "a done")

    async def eval_action(tc, tid):
        tc.submit_task_completion(tid, "ok")

    tc = TaskCenter(
        spawn_func=_scripted_spawn(
            {
                "t1": root_action,
                "t2": root_action,
                "a": child_action,
                "t1-eval": eval_action,
                "t2-eval": eval_action,
            }
        )
    )

    first = await tc.run_query("first")
    second = await tc.run_query("second")

    assert first.status is Status.DONE
    assert second.status is Status.DONE
    assert tc.graph.get("a").summary == "a done"
