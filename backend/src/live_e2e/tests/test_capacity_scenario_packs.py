"""Offline conformance tests for the capacity-suite scenario-pack catalog."""

from __future__ import annotations

import re
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from live_e2e.scenarios import SCENARIO_REGISTRY
from live_e2e.scenarios.base import ScenarioContext
from live_e2e.scenarios.capacity.pack_catalog import CAPACITY_PACK_SPECS, names

_REPO_ROOT = Path(__file__).resolve().parents[4]
_SCENARIO_PACK_DOC = (
    _REPO_ROOT / "docs" / "wiki" / "live-e2e-capacity-suite-scenario-packs.md"
)
_SCENARIO_HEADING = re.compile(r"^### `([^`]+)`$", re.MULTILINE)


def test_capacity_pack_catalog_matches_docs_matrix() -> None:
    documented_names = set(_SCENARIO_HEADING.findall(_SCENARIO_PACK_DOC.read_text()))
    assert documented_names == names()


def test_capacity_pack_catalog_has_no_duplicate_names() -> None:
    scenario_names = [spec.name for spec in CAPACITY_PACK_SPECS]
    assert len(scenario_names) == len(set(scenario_names))


def test_capacity_pack_specs_have_existing_implementation_anchors() -> None:
    for spec in CAPACITY_PACK_SPECS:
        assert spec.implementation_anchor, f"{spec.name} has no implementation anchor"
        if spec.registry_name is not None:
            assert spec.registry_name in SCENARIO_REGISTRY, spec
        if spec.superseded_by is not None:
            assert spec.superseded_by in SCENARIO_REGISTRY, spec
        if spec.test_path is not None:
            assert (_REPO_ROOT / spec.test_path).exists(), spec


def test_pipeline_capacity_scenarios_encode_expected_graph_shapes() -> None:
    root_ctx = _ctx()
    recursive_ctx = _ctx(recursive=True)

    parallel = _planner_args("pipeline.dependency_dag_parallel", root_ctx)
    assert _deps_by_id(parallel) == {
        "a": (),
        "b": (),
        "c": (),
        "d": ("a", "b", "c"),
    }

    diamond = _planner_args("pipeline.dependency_dag_diamond", root_ctx)
    assert _deps_by_id(diamond) == {
        "a": (),
        "b": ("a",),
        "c": ("a",),
        "d": ("b", "c"),
    }

    blocked = _planner_args("pipeline.dependency_blocked_descendants", root_ctx)
    assert _deps_by_id(blocked) == {
        "a": (),
        "b": ("a",),
        "c": ("a",),
        "d": ("b", "c"),
    }
    assert "ACTION fail_root" in blocked["task_specs"]["a"]

    retry_planner_1 = _planner_args(
        "pipeline.attempt_retry_planner_failure",
        _ctx(attempt_no=1),
    )
    retry_planner_2 = _planner_args(
        "pipeline.attempt_retry_planner_failure",
        _ctx(attempt_no=2),
    )
    assert _deps_by_id(retry_planner_1) == {"a": ("missing",)}
    assert _deps_by_id(retry_planner_2) == {"preflight": ()}

    nested_root = _planner_args("pipeline.nested_mission", root_ctx)
    nested_child = _planner_args("pipeline.nested_mission", recursive_ctx)
    assert _deps_by_id(nested_root) == {
        "delegate_child": (),
        "recursive_return_guard": ("delegate_child",),
        "parent_reconciliation": ("recursive_return_guard",),
    }
    assert _deps_by_id(nested_child) == {
        "child_a": (),
        "child_b": ("child_a",),
    }


def test_planner_validation_capacity_scenarios_encode_rejection_cases() -> None:
    unknown_dep = _planner_args("planner_validation.unknown_dep", _ctx())
    assert _deps_by_id(unknown_dep)["b"] == ("z",)

    cycle = _planner_args("planner_validation.cycle_in_deps", _ctx())
    assert _deps_by_id(cycle) == {"a": ("b",), "b": ("a",)}

    missing_goal = SCENARIO_REGISTRY[
        "planner_validation.partial_without_continuation_goal"
    ]().planner_response(_ctx())
    assert missing_goal.tool.name == "submit_partial_plan"
    assert "continuation_goal" not in missing_goal.args

    unknown_agent = _planner_args("planner_validation.unknown_agent_name", _ctx())
    assert unknown_agent["tasks"][0]["agent_name"] == "missing_generator_agent"

    empty = _planner_args("planner_validation.empty_tasks", _ctx())
    assert empty["tasks"] == []
    assert empty["task_specs"] == {}


def _planner_args(name: str, ctx: ScenarioContext) -> dict[str, Any]:
    return dict(SCENARIO_REGISTRY[name]().planner_response(ctx).args)


def _deps_by_id(plan: dict[str, Any]) -> dict[str, tuple[str, ...]]:
    return {str(task["id"]): tuple(task.get("deps") or ()) for task in plan["tasks"]}


def _ctx(*, attempt_no: int = 1, episode_no: int = 1, recursive: bool = False) -> ScenarioContext:
    requested_by = "parent-task-id" if recursive else "task-center-run:entry"
    return ScenarioContext(
        attempt=SimpleNamespace(
            attempt_sequence_no=attempt_no,
            evaluation_criteria=("criterion",),
            id=f"attempt-{attempt_no}",
        ),
        episode=SimpleNamespace(sequence_no=episode_no, mission_id="mission-id"),
        mission=SimpleNamespace(requested_by_task_id=requested_by),
        prompt="capacity scenario pack offline test",
        metadata={},
        audit_recorder=None,
        mutable_state=None,
        task_id="task-id",
        agent_name="executor",
        rendered_prompt="",
    )
