"""Stage 5 — partial-plan continuation chain.

Tests the close_partial_success → spawn(continuation, prior_graph_id=…)
loop and the build_continuation_note traversal. Pinned by the roadmap as:
"New test backend/tests/test_task_center/test_partial_chain.py builds a
2-segment chain: partial → partial-closes-as-full → root_task DONE."
"""

from __future__ import annotations

from task_center.model import Status
from task_center.runtime import Orchestrator, TaskCenter


def _spawn_orch_running_planner(tc: TaskCenter, root_id: str) -> Orchestrator:
    """Spawn a planner-led graph and drive its planner to RUNNING.

    Mirrors the dispatcher's transition before the planner calls a
    terminal. Returns the orchestrator for the new graph.
    """
    orch = Orchestrator.spawn(
        tc, root_task_id=root_id, request_plan_note="please plan"
    )
    tc.graph.transition(orch.planner.id, Status.RUNNING)
    return orch


def _drive_dag_to_done(tc: TaskCenter, node_ids: list[str]) -> None:
    """Walk a DAG of READY/PENDING nodes to DONE, respecting dep order."""
    remaining = list(node_ids)
    while remaining:
        progressed = False
        for nid in list(remaining):
            task = tc.graph.get(nid)
            if all(
                tc.graph.get(d).status is Status.DONE for d in task.needs
            ) and task.status is Status.READY:
                tc.graph.transition(nid, Status.RUNNING)
                tc.graph.transition(nid, Status.DONE)
                remaining.remove(nid)
                progressed = True
            elif (
                task.status is Status.PENDING
                and all(
                    tc.graph.get(d).status is Status.DONE for d in task.needs
                )
            ):
                tc.graph.transition(nid, Status.READY)
                tc.graph.transition(nid, Status.RUNNING)
                tc.graph.transition(nid, Status.DONE)
                remaining.remove(nid)
                progressed = True
        if not progressed:
            raise AssertionError(
                f"DAG drive stuck — remaining: {remaining}"
            )


# ---- close_partial_success: state transitions -----------------------------


def test_close_partial_success_marks_planner_done_and_spawns_continuation() -> None:
    tc = TaskCenter()
    root = tc._create_executor(
        input="root goal",
        harness_graph_id=None,
        needs=frozenset(),
        status=Status.READY,
    )
    tc.graph.transition(root.id, Status.RUNNING)
    tc.graph.transition(root.id, Status.HANDOFF)
    orch = _spawn_orch_running_planner(tc, root.id)
    err = orch.materialize_partial_plan(
        task_dep_graphs=[{"id": "shim", "deps": [], "role": "executor"}],
        task_details={"shim": "build the shim"},
        what_to_do_next="bulk fan-out after shim lands",
        evaluation_specification="checkpoint reached",
    )
    assert err is None
    # Drive dag + evaluator to a state where evaluator success can fire.
    _drive_dag_to_done(tc, ["shim"])
    evaluator = orch.evaluator
    assert evaluator is not None
    tc.graph.transition(evaluator.id, Status.READY)
    tc.graph.transition(evaluator.id, Status.RUNNING)

    # Evaluator success fires — partial branch.
    tc.submit_task_success(evaluator.id, "shim verified")

    # Planner DONE, evaluator DONE, root_task still HANDOFF (chain alive).
    assert tc.graph.get(orch.planner.id).status is Status.DONE
    assert tc.graph.get(evaluator.id).status is Status.DONE
    assert tc.graph.get(root.id).status is Status.HANDOFF
    # Root_task carries a segment_success summary.
    assert any(s.kind == "segment_success" for s in tc.graph.get(root.id).summaries)
    # A new graph was spawned with prior_graph_id pointing at the prior.
    new_graphs = [
        g
        for g in tc.graph.harness_graphs.values()
        if g.prior_graph_id == orch.graph_id
    ]
    assert len(new_graphs) == 1
    assert new_graphs[0].root_task_id == root.id


def test_partial_chain_terminates_when_segment_full_closes() -> None:
    """2-segment chain: partial → full → root_task DONE.

    The first segment is partial (continuation chain alive). The second
    segment is full (chain terminates → root_task transitions DONE via
    the existing full-plan closure path).
    """
    tc = TaskCenter()
    root = tc._create_executor(
        input="migrate from v1 to v2",
        harness_graph_id=None,
        needs=frozenset(),
        status=Status.READY,
    )
    tc.graph.transition(root.id, Status.RUNNING)
    tc.graph.transition(root.id, Status.HANDOFF)

    # Segment 1 — partial.
    seg1 = _spawn_orch_running_planner(tc, root.id)
    seg1.materialize_partial_plan(
        task_dep_graphs=[{"id": "shim", "deps": [], "role": "executor"}],
        task_details={"shim": "shim"},
        what_to_do_next="bulk migrate after shim",
        evaluation_specification="shim landed",
    )
    _drive_dag_to_done(tc, ["shim"])
    seg1_eval = seg1.evaluator
    assert seg1_eval is not None
    tc.graph.transition(seg1_eval.id, Status.READY)
    tc.graph.transition(seg1_eval.id, Status.RUNNING)
    tc.submit_task_success(seg1_eval.id, "shim approved")

    # Segment 2 — full (continuation graph created by seg1's close).
    seg2_graph = next(
        g
        for g in tc.graph.harness_graphs.values()
        if g.prior_graph_id == seg1.graph_id
    )
    seg2 = Orchestrator(graph_id=seg2_graph.id, tc=tc)
    tc.graph.transition(seg2.planner.id, Status.RUNNING)
    seg2.materialize_full_plan(
        task_dep_graphs=[{"id": "bulk", "deps": [], "role": "executor"}],
        task_details={"bulk": "bulk migration"},
        evaluation_specification="all sites migrated",
    )
    _drive_dag_to_done(tc, ["bulk"])
    seg2_eval = seg2.evaluator
    assert seg2_eval is not None
    tc.graph.transition(seg2_eval.id, Status.READY)
    tc.graph.transition(seg2_eval.id, Status.RUNNING)
    tc.submit_task_success(seg2_eval.id, "migration complete")

    # Root_task is now DONE (chain terminated via seg2's full-plan closure).
    root_task = tc.graph.get(root.id)
    assert root_task.status is Status.DONE
    summary_kinds = [s.kind for s in root_task.summaries]
    assert "segment_success" in summary_kinds
    assert "child_success" in summary_kinds


# ---- build_continuation_note ----------------------------------------------


def test_build_continuation_note_walks_chain() -> None:
    tc = TaskCenter()
    root = tc._create_executor(
        input="ROOT-GOAL-INPUT",
        harness_graph_id=None,
        needs=frozenset(),
        status=Status.READY,
    )
    tc.graph.transition(root.id, Status.RUNNING)
    tc.graph.transition(root.id, Status.HANDOFF)

    # Segment 1 — close partial.
    seg1 = _spawn_orch_running_planner(tc, root.id)
    seg1.materialize_partial_plan(
        task_dep_graphs=[{"id": "a", "deps": [], "role": "executor"}],
        task_details={"a": "a"},
        what_to_do_next="DO SEG1 NEXT",
        evaluation_specification="seg1 ok",
    )
    _drive_dag_to_done(tc, ["a"])
    eval1 = seg1.evaluator
    assert eval1 is not None
    tc.graph.transition(eval1.id, Status.READY)
    tc.graph.transition(eval1.id, Status.RUNNING)
    tc.submit_task_success(eval1.id, "SEG1-EVAL-SUMMARY")

    # Segment 2 (continuation) — read its build_continuation_note.
    seg2_graph = next(
        g
        for g in tc.graph.harness_graphs.values()
        if g.prior_graph_id == seg1.graph_id
    )
    seg2 = Orchestrator(graph_id=seg2_graph.id, tc=tc)
    note = seg2.build_continuation_note()
    assert "ROOT-GOAL-INPUT" in note
    assert "DO SEG1 NEXT" in note
    assert "SEG1-EVAL-SUMMARY" in note
    assert note.startswith("ROOT_GOAL: ")
