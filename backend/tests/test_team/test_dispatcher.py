"""Unit tests for team.runtime.dispatcher.Dispatcher."""

from __future__ import annotations

import pytest

from team.artifacts.store import InMemoryArtifactStore
from team.errors import BudgetExceeded
from team.models import (
    AgentResult,
    BudgetConfig,
    BudgetState,
    Plan,
    ReplanPlan,
    ReplanItemSpec,
    ReplanRequest,
    RetryRequest,
    WorkItem,
    WorkItemKind,
    WorkItemSpec,
    WorkItemStatus,
)
from team.runtime.dispatcher import Dispatcher


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


def _wi(
    id_: str,
    deps: list[str] | None = None,
    depth: int = 0,
    kind: WorkItemKind = WorkItemKind.ATOMIC,
    **overrides,
) -> WorkItem:
    base = dict(
        id=id_,
        team_run_id="T1",
        agent_name="a",
        status=WorkItemStatus.PENDING,
        kind=kind,
        deps=deps or [],
        root_id=id_,
        depth=depth,
    )
    base.update(overrides)
    return WorkItem(**base)


@pytest.fixture(autouse=True)
def _patch_agent_exists(monkeypatch):
    from team.planning import validation

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
    await disp.add_work_item(_wi("PLANNER", kind=WorkItemKind.EXPANDABLE))
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
async def test_validator_dep_on_expandable_waits_for_full_descendant_subtree():
    disp = _make_dispatcher()
    await disp.add_work_item(_wi("PLANNER", kind=WorkItemKind.EXPANDABLE, local_id="branch"))
    await disp.add_work_item(_wi("VAL", deps=["PLANNER"], agent_name="validator"))

    assert await disp.pop_ready() == "PLANNER"
    await disp.mark_running("PLANNER", "AR-plan")
    new_items = await disp.complete(
        "PLANNER",
        AgentResult(
            artifact={"planner": True},
            summary="planned",
            submitted_plan=Plan(
                items=[WorkItemSpec(agent_name="developer", local_id="dev1")]
            ),
        ),
    )

    child = new_items[0]
    assert disp.graph["VAL"].status == WorkItemStatus.PENDING
    assert child.status == WorkItemStatus.READY
    assert await disp.pop_ready() == child.id

    await disp.mark_running(child.id, "AR-dev")
    await disp.complete(child.id, AgentResult(artifact={"fixed": True}, summary="done"))

    assert disp.graph["VAL"].status == WorkItemStatus.READY
    assert {dep.source_wi_id for dep in disp.graph["VAL"].dep_artifacts} == {
        "PLANNER",
        child.id,
    }
    assert {dep.display_name for dep in disp.graph["VAL"].dep_artifacts} == {
        "branch",
        "dev1",
    }


@pytest.mark.asyncio
async def test_terminal_descendant_failure_cancels_dependents_waiting_on_ancestor_subtree():
    disp = _make_dispatcher()
    await disp.add_work_item(_wi("PLANNER", kind=WorkItemKind.EXPANDABLE))
    await disp.add_work_item(_wi("VAL", deps=["PLANNER"], agent_name="validator"))

    assert await disp.pop_ready() == "PLANNER"
    await disp.mark_running("PLANNER", "AR-plan")
    new_items = await disp.complete(
        "PLANNER",
        AgentResult(
            artifact={"planner": True},
            summary="planned",
            submitted_plan=Plan(items=[WorkItemSpec(agent_name="developer", local_id="dev1")]),
        ),
    )

    child = new_items[0]
    assert await disp.pop_ready() == child.id
    await disp.mark_running(child.id, "AR-dev")
    await disp.fail(child.id, "boom")

    assert disp.graph[child.id].status == WorkItemStatus.FAILED
    assert disp.graph["VAL"].status == WorkItemStatus.CANCELLED


@pytest.mark.asyncio
async def test_request_replan_keeps_ancestor_subtree_dependent_pending():
    disp = _make_dispatcher()
    await disp.add_work_item(_wi("PLANNER", kind=WorkItemKind.EXPANDABLE))
    await disp.add_work_item(_wi("VAL", deps=["PLANNER"], agent_name="validator"))

    assert await disp.pop_ready() == "PLANNER"
    await disp.mark_running("PLANNER", "AR-plan")
    new_items = await disp.complete(
        "PLANNER",
        AgentResult(
            artifact={"planner": True},
            summary="planned",
            submitted_plan=Plan(items=[WorkItemSpec(agent_name="developer", local_id="dev1")]),
        ),
    )

    child = new_items[0]
    assert await disp.pop_ready() == child.id
    await disp.mark_running(child.id, "AR-dev")
    replanner = await disp.request_replan(
        child.id,
        ReplanRequest(reason="need corrective split", context="traceback"),
    )

    assert disp.graph[child.id].status == WorkItemStatus.FAILED
    assert disp.graph["VAL"].status == WorkItemStatus.PENDING
    assert replanner.status == WorkItemStatus.READY


@pytest.mark.asyncio
async def test_retry_validator_cancels_failed_child_validators_in_dependency_subtree():
    disp = _make_dispatcher()
    branch = _wi(
        "PLANNER",
        kind=WorkItemKind.EXPANDABLE,
        agent_name="team_planner",
        status=WorkItemStatus.DONE,
    )
    child_validator = _wi(
        "VAL-CHILD",
        agent_name="validator",
        status=WorkItemStatus.FAILED,
        parent_id="PLANNER",
        root_id="PLANNER",
    )
    validator = _wi(
        "VAL-PARENT",
        deps=["PLANNER"],
        agent_name="validator",
        status=WorkItemStatus.RUNNING,
    )
    disp.graph = {wi.id: wi for wi in (branch, child_validator, validator)}

    await disp.retry_work_item("VAL-PARENT", RetryRequest(reason="retry exact verifier"))

    assert disp.graph["VAL-CHILD"].status == WorkItemStatus.CANCELLED
    assert disp.graph["VAL-PARENT"].status == WorkItemStatus.READY


@pytest.mark.asyncio
async def test_prepare_for_resume_promotes_validator_after_superseding_failed_child_validator():
    disp = _make_dispatcher()
    branch = _wi(
        "PLANNER",
        kind=WorkItemKind.EXPANDABLE,
        agent_name="team_planner",
        status=WorkItemStatus.DONE,
    )
    child_validator = _wi(
        "VAL-CHILD",
        agent_name="validator",
        status=WorkItemStatus.FAILED,
        parent_id="PLANNER",
        root_id="PLANNER",
    )
    validator = _wi(
        "VAL-PARENT",
        deps=["PLANNER"],
        agent_name="validator",
        status=WorkItemStatus.PENDING,
    )
    disp.graph = {wi.id: wi for wi in (branch, child_validator, validator)}

    await disp.prepare_for_resume()

    assert disp.graph["VAL-CHILD"].status == WorkItemStatus.CANCELLED
    assert disp.graph["VAL-PARENT"].status == WorkItemStatus.READY
    assert await disp.pop_ready() == "VAL-PARENT"


@pytest.mark.asyncio
async def test_invalid_plan_fails_parent_without_partial_insert():
    disp = _make_dispatcher()
    await disp.add_work_item(_wi("PLANNER", kind=WorkItemKind.EXPANDABLE))
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
async def test_atomic_submitting_plan_is_rejected():
    disp = _make_dispatcher()
    await disp.add_work_item(_wi("A"))  # atomic
    await disp.pop_ready()
    await disp.mark_running("A", "AR1")
    plan = Plan(items=[WorkItemSpec(agent_name="a", local_id="x")])
    await disp.complete(
        "A", AgentResult(artifact={}, summary="s", submitted_plan=plan)
    )
    assert disp.graph["A"].status == WorkItemStatus.FAILED
    assert "only expandable" in (disp.graph["A"].failure_reason or "")


@pytest.mark.asyncio
async def test_expandable_without_plan_is_rejected():
    disp = _make_dispatcher()
    await disp.add_work_item(_wi("P", kind=WorkItemKind.EXPANDABLE))
    await disp.pop_ready()
    await disp.mark_running("P", "AR1")
    await disp.complete("P", AgentResult(artifact={}, summary="s"))
    assert disp.graph["P"].status == WorkItemStatus.FAILED
    assert "did not submit a plan" in (disp.graph["P"].failure_reason or "")


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
async def test_checkpoint_rollback_restores_replan_budget():
    disp = _make_dispatcher()
    cp = await disp.checkpoint(label="t0", project_context={"g": "x"})

    disp.budget_state.replans_used = 3

    await disp.rollback_to(cp.id, project_context_setter=lambda pc: None)

    assert disp.budget_state.replans_used == 0


@pytest.mark.asyncio
async def test_apply_replan_reattaches_failed_validator_to_new_fix_tasks():
    disp = _make_dispatcher()

    dev = _wi("DEV")
    dev.agent_name = "developer"
    validator = _wi("VAL", deps=["DEV"])
    validator.agent_name = "validator"
    validator.root_id = "ROOT"

    await disp.add_work_item(dev)
    await disp.add_work_item(validator)

    assert await disp.pop_ready() == "DEV"
    await disp.mark_running("DEV", "AR-dev")
    await disp.complete("DEV", AgentResult(artifact={"out": 1}, summary="done"))

    assert await disp.pop_ready() == "VAL"
    await disp.mark_running("VAL", "AR-val")
    replanner = await disp.request_replan(
        "VAL",
        ReplanRequest(reason="tests failed", context="traceback"),
    )

    assert "replan" not in replanner.payload
    assert disp.graph["VAL"].status == WorkItemStatus.FAILED

    assert await disp.pop_ready() == replanner.id
    await disp.mark_running(replanner.id, "AR-replan")

    result = await disp.apply_replan(
        replan_wi_id=replanner.id,
        add_specs=[{"agent_name": "developer", "local_id": "fix"}],
        cancel_ids=[],
        target_depth=validator.depth,
        target_parent_id=validator.parent_id,
        target_root_id=validator.root_id,
    )

    assert result == {"added": 1, "cancelled": 0}
    fix = next(wi for wi in disp.graph.values() if wi.local_id == "fix")
    reattached = disp.graph["VAL"]
    assert reattached.status == WorkItemStatus.PENDING
    assert reattached.failure_reason is None
    assert reattached.finished_at is None
    assert reattached.deps == ["DEV", fix.id]

    assert await disp.pop_ready() == fix.id
    await disp.mark_running(fix.id, "AR-fix")
    await disp.complete(fix.id, AgentResult(artifact={"fixed": True}, summary="ok"))

    assert disp.graph["VAL"].status == WorkItemStatus.READY


@pytest.mark.asyncio
async def test_apply_replan_leaves_failed_validator_terminal_when_replan_adds_replacement_validator():
    disp = _make_dispatcher()

    dev = _wi("DEV")
    dev.agent_name = "developer"
    validator = _wi("VAL", deps=["DEV"])
    validator.agent_name = "validator"
    validator.root_id = "ROOT"

    await disp.add_work_item(dev)
    await disp.add_work_item(validator)

    assert await disp.pop_ready() == "DEV"
    await disp.mark_running("DEV", "AR-dev")
    await disp.complete("DEV", AgentResult(artifact={"out": 1}, summary="done"))

    assert await disp.pop_ready() == "VAL"
    await disp.mark_running("VAL", "AR-val")
    replanner = await disp.request_replan(
        "VAL",
        ReplanRequest(reason="adjacent deterministic failures", context="traceback"),
    )

    assert await disp.pop_ready() == replanner.id
    await disp.mark_running(replanner.id, "AR-replan")

    result = await disp.apply_replan(
        replan_wi_id=replanner.id,
        add_specs=[
            {"agent_name": "developer", "local_id": "fix"},
            {
                "agent_name": "validator",
                "local_id": "val-replacement",
                "deps": ["fix"],
            },
        ],
        cancel_ids=[],
        target_depth=validator.depth,
        target_parent_id=validator.parent_id,
        target_root_id=validator.root_id,
        replace_failed_validator=True,
    )

    assert result == {"added": 2, "cancelled": 0}
    assert disp.graph["VAL"].status == WorkItemStatus.FAILED

    fix = next(wi for wi in disp.graph.values() if wi.local_id == "fix")
    replacement = next(wi for wi in disp.graph.values() if wi.local_id == "val-replacement")
    assert replacement.deps == [fix.id]
    assert replacement.status == WorkItemStatus.PENDING

    assert await disp.pop_ready() == fix.id
    await disp.mark_running(fix.id, "AR-fix")
    await disp.complete(fix.id, AgentResult(artifact={"fixed": True}, summary="ok"))

    assert disp.graph["VAL"].status == WorkItemStatus.FAILED
    assert disp.graph[replacement.id].status == WorkItemStatus.READY


@pytest.mark.asyncio
async def test_complete_applies_submitted_replan_with_replace_failed_validator_flag():
    disp = _make_dispatcher()
    developer = _wi("DEV")
    validator = _wi("VAL", deps=["DEV"])
    await disp.add_work_item(developer)
    await disp.add_work_item(validator)

    assert await disp.pop_ready() == "DEV"
    await disp.mark_running("DEV", "AR-dev")
    await disp.complete("DEV", AgentResult(artifact={"out": 1}, summary="done"))

    assert await disp.pop_ready() == "VAL"
    await disp.mark_running("VAL", "AR-val")
    replanner = await disp.request_replan(
        "VAL",
        ReplanRequest(reason="adjacent deterministic failures", context="traceback"),
    )

    assert await disp.pop_ready() == replanner.id
    await disp.mark_running(replanner.id, "AR-replan")
    new_items = await disp.complete(
        replanner.id,
        AgentResult(
            artifact=None,
            summary="replanned",
            submitted_replan=ReplanPlan(
                add_items=[
                    ReplanItemSpec(agent_name="developer", local_id="fix"),
                    ReplanItemSpec(
                        agent_name="validator",
                        local_id="val-replacement",
                        deps=["fix"],
                    ),
                ],
                replace_failed_validator=True,
            ),
        ),
    )

    assert len(new_items) == 0
    assert disp.graph["VAL"].status == WorkItemStatus.FAILED

    fix = next(wi for wi in disp.graph.values() if wi.local_id == "fix")
    replacement = next(wi for wi in disp.graph.values() if wi.local_id == "val-replacement")
    assert replacement.deps == [fix.id]
    assert replacement.status == WorkItemStatus.PENDING


@pytest.mark.asyncio
async def test_checkpoint_ring_buffer_drops_oldest():
    disp = _make_dispatcher()
    disp._checkpoints.clear()  # start empty
    for _ in range(12):  # default maxlen is 10
        await disp.checkpoint(label=None, project_context=None)
    assert len(disp.list_checkpoints()) == 10
