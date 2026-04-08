"""Unit tests for team.models, team.planning.validation, team.artifacts.store."""

from __future__ import annotations

import pytest

from team.artifacts.store import InMemoryArtifactStore
from team.errors import ArtifactTooLarge, InvalidPlan
from team.models import (
    BudgetConfig,
    BudgetState,
    Plan,
    WorkItem,
    WorkItemKind,
    WorkItemSpec,
    WorkItemStatus,
)
from team.planning.validation import validate_plan_phase_a, validate_plan_phase_b


# ---------- Plan construction ------------------------------------------------


def test_plan_from_dict_roundtrip():
    data = {
        "items": [
            {"agent_name": "a", "payload": {"x": 1}, "local_id": "w1"},
            {"agent_name": "b", "deps": ["w1"], "local_id": "w2"},
        ],
        "rationale": "why",
    }
    plan = Plan.from_dict(data)
    assert len(plan.items) == 2
    assert plan.items[0].agent_name == "a"
    assert plan.items[1].deps == ["w1"]
    assert plan.rationale == "why"


# ---------- Phase A ----------------------------------------------------------


def _patch_registry(monkeypatch, known_agents):
    from team.planning import validation

    monkeypatch.setattr(
        validation, "_agent_exists", lambda name: name in known_agents
    )


def test_phase_a_empty_plan(monkeypatch):
    _patch_registry(monkeypatch, {"a"})
    issues = validate_plan_phase_a(Plan(items=[]))
    assert any("no items" in i["msg"] for i in issues)


def test_phase_a_size_limit(monkeypatch):
    _patch_registry(monkeypatch, {"a"})
    plan = Plan(items=[WorkItemSpec(agent_name="a", local_id=f"w{i}") for i in range(51)])
    issues = validate_plan_phase_a(plan, max_plan_size=50)
    assert any("max_plan_size" in i["msg"] for i in issues)


def test_phase_a_duplicate_local_id(monkeypatch):
    _patch_registry(monkeypatch, {"a"})
    plan = Plan(
        items=[
            WorkItemSpec(agent_name="a", local_id="w1"),
            WorkItemSpec(agent_name="a", local_id="w1"),
        ]
    )
    issues = validate_plan_phase_a(plan)
    assert any("duplicate" in i["msg"] for i in issues)


def test_phase_a_unknown_agent(monkeypatch):
    _patch_registry(monkeypatch, {"a"})
    plan = Plan(items=[WorkItemSpec(agent_name="ghost", local_id="w1")])
    issues = validate_plan_phase_a(plan)
    assert any("unknown agent" in i["msg"] for i in issues)


def test_phase_a_internal_cycle(monkeypatch):
    _patch_registry(monkeypatch, {"a"})
    plan = Plan(
        items=[
            WorkItemSpec(agent_name="a", local_id="w1", deps=["w2"]),
            WorkItemSpec(agent_name="a", local_id="w2", deps=["w1"]),
        ]
    )
    issues = validate_plan_phase_a(plan)
    assert any("cycle" in i["msg"] for i in issues)


def test_phase_a_valid_plan(monkeypatch):
    _patch_registry(monkeypatch, {"a", "b"})
    plan = Plan(
        items=[
            WorkItemSpec(agent_name="a", local_id="w1"),
            WorkItemSpec(agent_name="b", local_id="w2", deps=["w1"]),
        ]
    )
    assert validate_plan_phase_a(plan) == []


# ---------- Phase B ----------------------------------------------------------


def _parent_wi(team_run_id="T1"):
    return WorkItem(
        id="PARENT",
        team_run_id=team_run_id,
        agent_name="planner",
        status=WorkItemStatus.RUNNING,
        kind=WorkItemKind.EXPANDABLE,
        root_id="PARENT",
        depth=0,
    )


def test_phase_b_resolves_local_ids(monkeypatch):
    _patch_registry(monkeypatch, {"a"})
    counter = {"n": 0}

    def fresh_id():
        counter["n"] += 1
        return f"NEW{counter['n']}"

    plan = Plan(
        items=[
            WorkItemSpec(agent_name="a", local_id="w1"),
            WorkItemSpec(agent_name="a", local_id="w2", deps=["w1"]),
        ]
    )
    parent = _parent_wi()
    existing = {parent.id: parent}
    new_items = validate_plan_phase_b(
        existing, plan, "T1", parent, new_id_factory=fresh_id, max_depth=5
    )
    assert len(new_items) == 2
    assert new_items[1].deps == [new_items[0].id]
    assert all(wi.depth == 1 for wi in new_items)
    assert all(wi.parent_id == "PARENT" for wi in new_items)


def test_phase_b_cross_run_dep_rejected(monkeypatch):
    _patch_registry(monkeypatch, {"a"})
    other = WorkItem(
        id="OTHER",
        team_run_id="OTHER_RUN",
        agent_name="a",
        status=WorkItemStatus.DONE,
    )
    parent = _parent_wi()
    existing = {parent.id: parent, other.id: other}
    plan = Plan(items=[WorkItemSpec(agent_name="a", deps=["OTHER"])])
    with pytest.raises(InvalidPlan, match="cross-run"):
        validate_plan_phase_b(
            existing, plan, "T1", parent, new_id_factory=lambda: "NEW", max_depth=5
        )


def test_phase_b_dangling_external_dep(monkeypatch):
    _patch_registry(monkeypatch, {"a"})
    parent = _parent_wi()
    plan = Plan(items=[WorkItemSpec(agent_name="a", deps=["nonexistent"])])
    with pytest.raises(InvalidPlan, match="not found"):
        validate_plan_phase_b(
            {parent.id: parent}, plan, "T1", parent, new_id_factory=lambda: "N", max_depth=5
        )


def test_phase_b_depth_exceeded(monkeypatch):
    _patch_registry(monkeypatch, {"a"})
    parent = _parent_wi()
    parent.depth = 5
    plan = Plan(items=[WorkItemSpec(agent_name="a")])
    with pytest.raises(InvalidPlan, match="max_depth"):
        validate_plan_phase_b(
            {parent.id: parent}, plan, "T1", parent, new_id_factory=lambda: "N", max_depth=5
        )


def test_phase_b_rejects_agent_without_supported_kind(monkeypatch):
    from agents.types import AgentDefinition
    from team.planning import validation as _v

    atomic_only = AgentDefinition(
        name="atomic_only", description="d", supported_kinds=["atomic"]
    )
    monkeypatch.setattr(_v, "_get_definition", lambda n: atomic_only if n == "atomic_only" else None)
    parent = _parent_wi()
    plan = Plan(
        items=[
            WorkItemSpec(
                agent_name="atomic_only", local_id="w1", kind=WorkItemKind.EXPANDABLE
            )
        ]
    )
    with pytest.raises(InvalidPlan, match="does not support kind"):
        validate_plan_phase_b(
            {parent.id: parent}, plan, "T1", parent, new_id_factory=lambda: "N", max_depth=5
        )


def test_phase_b_combined_cycle(monkeypatch):
    _patch_registry(monkeypatch, {"a"})
    existing_parent = _parent_wi()
    # Existing WI W0 depends on (future) new item we'll try to emit that then
    # depends back on W0 — combined graph is cyclic.
    w0 = WorkItem(
        id="W0",
        team_run_id="T1",
        agent_name="a",
        status=WorkItemStatus.RUNNING,
        deps=["NEW1"],  # forward-referencing a soon-to-be-created item
    )
    graph = {existing_parent.id: existing_parent, w0.id: w0}
    plan = Plan(items=[WorkItemSpec(agent_name="a", local_id="lid", deps=["W0"])])

    def fresh_id():
        return "NEW1"

    with pytest.raises(InvalidPlan, match="cycle"):
        validate_plan_phase_b(
            graph, plan, "T1", existing_parent, new_id_factory=fresh_id, max_depth=5
        )


# ---------- ArtifactStore ----------------------------------------------------


def test_artifact_store_byte_caps():
    budgets = BudgetConfig(max_artifact_bytes=50, max_total_artifact_bytes=80)
    state = BudgetState()
    store = InMemoryArtifactStore(budgets, state)
    store.save("a", "x" * 40)
    assert state.artifact_bytes_used >= 40
    with pytest.raises(ArtifactTooLarge):
        store.save("b", "x" * 100)  # per-artifact cap
    with pytest.raises(ArtifactTooLarge):
        store.save("c", "x" * 45)  # total cap


def test_artifact_store_replace_releases_old_bytes():
    budgets = BudgetConfig(max_artifact_bytes=1000, max_total_artifact_bytes=200)
    state = BudgetState()
    store = InMemoryArtifactStore(budgets, state)
    store.save("a", "x" * 150)
    store.save("a", "y" * 10)  # replace — should free the 150
    assert state.artifact_bytes_used == 10


def test_artifact_store_snapshot_restore():
    budgets = BudgetConfig()
    state = BudgetState()
    store = InMemoryArtifactStore(budgets, state)
    store.save("a", {"v": 1})
    store.save("b", {"v": 2})
    snap = store.snapshot()
    store.save("a", {"v": 999})
    store.delete("b")
    store.restore(snap)
    assert store.load("a") == {"v": 1}
    assert store.load("b") == {"v": 2}
