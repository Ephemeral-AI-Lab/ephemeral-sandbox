"""Live regression for partial-parent planner terminal routing."""

from __future__ import annotations

import json
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.scenarios import SCENARIO_REGISTRY
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.environments.sweevo_image.fixtures import run_scenario_on_sweevo_image
from task_center_runner.tests._live_config import database_configured


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
async def test_partial_parent_filters_child_planner_to_close_terminal(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    scenario = SCENARIO_REGISTRY[
        "pipeline.deferred_parent_planner_terminal_routing"
    ]()
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
    )

    assert report.task_center_status == "done", report.metrics
    planner_launches = [
        launch.agent_name for launch in report.launches if launch.role == "planner"
    ]
    assert planner_launches == ["planner", "planner", "planner"]
    assert _tool_count(report.tool_calls, "submit_plan_defers_goal") == 1
    assert _tool_count(report.tool_calls, "submit_plan_closes_goal") == 2
    _assert_partial_parent_graph(report.graph_summary)
    _assert_restricted_planner_catalog_was_recorded(report.run_dir)


def _tool_count(tool_calls: list[Any], tool_name: str) -> int:
    return sum(1 for call in tool_calls if call.tool_name == tool_name)


def _assert_partial_parent_graph(graph_summary: dict[str, Any]) -> None:
    goals = graph_summary["goals"]
    assert len(goals) == 2, graph_summary
    root = next(
        goal
        for goal in goals
        if goal.get("origin_kind") == "entry"
    )
    child = next(
        goal
        for goal in goals
        if goal.get("origin_kind") == "task"
    )

    assert len(root["iterations"]) == 2
    assert root["iterations"][0]["attempts"][-1]["deferred_goal_for_next_iteration"]
    assert str(child["requested_by_task_id"]).endswith(":delegate_child")


def _assert_restricted_planner_catalog_was_recorded(run_dir: Path) -> None:
    active_terminal_sets = list(_active_terminal_sets_for(run_dir, "planner"))
    assert ("submit_plan_closes_goal",) in active_terminal_sets

    catalogs = list(_terminal_catalog_rows_for(run_dir, "planner"))
    assert catalogs, f"no planner terminal catalog row in {run_dir}"
    assert any(
        "submit_plan_closes_goal" in catalog
        and "submit_plan_defers_goal" not in catalog
        for catalog in catalogs
    )


def _active_terminal_sets_for(
    run_dir: Path,
    agent_name: str,
) -> Iterator[tuple[str, ...]]:
    for path in run_dir.rglob("message.jsonl"):
        for line in path.read_text(encoding="utf-8").splitlines():
            row = json.loads(line)
            metadata = row.get("metadata") or {}
            if metadata.get("agent_name") != agent_name:
                continue
            active = metadata.get("active_terminals")
            if isinstance(active, list):
                yield tuple(str(name) for name in active)


def _terminal_catalog_rows_for(run_dir: Path, agent_name: str) -> Iterator[str]:
    for path in run_dir.rglob("message.jsonl"):
        for line in path.read_text(encoding="utf-8").splitlines():
            row = json.loads(line)
            metadata = row.get("metadata") or {}
            if metadata.get("agent_name") != agent_name or row.get("role") != "user":
                continue
            text = "\n".join(
                str(block.get("text") or "")
                for block in row.get("content", [])
                if isinstance(block, dict)
            )
            if "<terminal_tool_selection>" not in text:
                continue
            yield text.split("<terminal_tool_selection>\n", 1)[1].split(
                "\n</terminal_tool_selection>",
                1,
            )[0]
