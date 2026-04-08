"""Unit tests for team.dispatcher.Dispatcher."""

from __future__ import annotations

import pytest

from team.artifact_store import InMemoryArtifactStore
from team.dispatcher import Dispatcher
from team.types import (
    AgentResult,
    BudgetConfig,
    BudgetExceeded,
    BudgetState,
    Plan,
    WorkItem,
    WorkItemSpec,
    WorkItemStatus,
)


def _make_dispatcher(budgets: BudgetConfig | None = None) -> Dispatcher:
    budgets = budgets or BudgetConfig()
    state = BudgetState()
    store = InMemoryArtifactStore(budgets, state)
    return Dispatcher(
        team_run_id="T1",
        budgets=budgets,
        budget_state=state,
        artifact_store=store,
    )


def _wi(id_: str, deps: list[str] | None = None, depth: int = 0) -> WorkItem:
    return WorkItem(
        id=id_,
        team_run_id="T1",
        agent_name="a",
        status=WorkItemStatus.PENDING,
        deps=deps or [],
        root_id=id_,
        depth=depth,
    )


@pytest.fixture(autouse=True)
def _patch_agent_exists(monkeypatch):
    from team import validation

    monkeypatch.setattr(validation, "_agent_exists", lambda name: True)


@pytest.mark.asyncio
async def test_add_work_item_enqueues_when_ready():
    disp = _make_dispatcher()
    await disp.add_work_item(_wi("A"))
    assert disp.graph["A"].status == WorkItemStatus.READY
    wi_id = await disp.pop_ready()
    assert wi_id == "A"


@pytest.mark.asyncio
async def test_readiness_propagates_after_complete():
    disp = _make_dispatcher()
    await disp.add_work_item(_wi("A"))
    await disp.add_work_item(_wi("B", deps=["A"]))
    assert disp.graph["B"].status == WorkItemStatus.PENDING
    await disp.pop_ready()
    await disp.mark_running("A", "AR1")
    await disp.complete("A", AgentResult(artifact={"out": 1}, summary="ok"))
    assert disp.graph["A"].status == WorkItemStatus.DONE
    assert disp.graph["B"].status == WorkItemStatus.READY
    assert await disp.pop_ready() == "B"


@pytest.mark.asyncio
async def test_compute_readiness_ignores_parent_id():
    disp = _make_dispatcher()
    a = _wi("A")
    b = _wi("B")
    b.parent_id = "A"  # provenance, NOT a dependency
    await disp.add_work_item(a)
    await disp.add_work_item(b)
    # Both should be immediately READY — parent_id must not block readiness.
    assert disp.graph["A"].status == WorkItemStatus.READY
    assert disp.graph["B"].status == WorkItemStatus.READY


@pytest.mark.asyncio
async def test_fail_cascades_cancelled_to_successors():
    disp = _make_dispatcher()
    await disp.add_work_item(_wi("A"))
    await disp.add_work_item(_wi("B", deps=["A"]))
    await disp.add_work_item(_wi("C", deps=["B"]))
    await disp.fail("A", "boom")
    assert disp.graph["A"].status == WorkItemStatus.FAILED
    assert disp.graph["B"].status == WorkItemStatus.CANCELLED
    assert disp.graph["C"].status == WorkItemStatus.CANCELLED


@pytest.mark.asyncio
async def test_max_work_items_budget_enforced():
    disp = _make_dispatcher(BudgetConfig(max_work_items=2))
    await disp.add_work_item(_wi("A"))
    await disp.add_work_item(_wi("B"))
    with pytest.raises(BudgetExceeded):
        await disp.add_work_item(_wi("C"))


@pytest.mark.asyncio
async def test_complete_inserts_plan_atomically():
    disp = _make_dispatcher()
    await disp.add_work_item(_wi("PLANNER"))
    await disp.pop_ready()
    await disp.mark_running("PLANNER", "AR1")
    plan = Plan(
        items=[
            WorkItemSpec(agent_name="a", local_id="x"),
            WorkItemSpec(agent_name="a", local_id="y", deps=["x"]),
        ]
    )
    new_items = await disp.complete(
        "PLANNER",
        AgentResult(artifact={"p": 1}, summary="s", submitted_plan=plan),
    )
    assert len(new_items) == 2
    assert disp.graph["PLANNER"].status == WorkItemStatus.DONE
    # First new item ready, second still PENDING
    statuses = {wi.id: wi.status for wi in new_items}
    assert WorkItemStatus.READY in statuses.values()


@pytest.mark.asyncio
async def test_invalid_plan_fails_parent_without_partial_insert():
    disp = _make_dispatcher()
    await disp.add_work_item(_wi("PLANNER"))
    await disp.pop_ready()
    await disp.mark_running("PLANNER", "AR1")
    # cross-run dep — triggers InvalidPlan inside complete()
    plan = Plan(items=[WorkItemSpec(agent_name="a", deps=["ghost"])])
    before = set(disp.graph)
    await disp.complete(
        "PLANNER", AgentResult(artifact={"p": 1}, summary="s", submitted_plan=plan)
    )
    assert disp.graph["PLANNER"].status == WorkItemStatus.FAILED
    assert set(disp.graph) == before  # nothing added


@pytest.mark.asyncio
async def test_checkpoint_rollback_round_trip():
    disp = _make_dispatcher()
    await disp.add_work_item(_wi("A"))
    cp = await disp.checkpoint(label="t0", project_context={"g": "x"})
    await disp.pop_ready()
    await disp.mark_running("A", "AR1")
    await disp.complete("A", AgentResult(artifact="done", summary="ok"))
    assert disp.graph["A"].status == WorkItemStatus.DONE

    captured = {"pc": None}
    await disp.rollback_to(
        cp.id,
        project_context_setter=lambda pc: captured.__setitem__("pc", pc),
    )
    assert disp.graph["A"].status == WorkItemStatus.READY
    assert captured["pc"] == {"g": "x"}
    # Ready queue was rebuilt
    assert await disp.pop_ready() == "A"


@pytest.mark.asyncio
async def test_checkpoint_ring_buffer_drops_oldest():
    disp = _make_dispatcher()
    disp._checkpoints.clear()  # start empty
    for _ in range(12):  # default maxlen is 10
        await disp.checkpoint(label=None, project_context=None)
    assert len(disp.list_checkpoints()) == 10
