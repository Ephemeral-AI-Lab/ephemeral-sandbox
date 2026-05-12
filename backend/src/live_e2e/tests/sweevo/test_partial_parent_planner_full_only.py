"""Live regression for partial-parent planner variant routing."""

from __future__ import annotations

import json
import os
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from benchmarks.sweevo.models import SWEEvoInstance
from live_e2e.scenarios import SCENARIO_REGISTRY
from live_e2e.stores import TaskCenterStoreBundle
from live_e2e.sweevo_adapter import run_sweevo_scenario


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not os.environ.get("EPHEMERALOS_DATABASE_URL"),
    reason="EPHEMERALOS_DATABASE_URL not set - live_e2e requires PostgreSQL",
)
async def test_partial_parent_routes_child_planner_to_full_only_agent_md(
    sweevo_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    scenario = SCENARIO_REGISTRY["pipeline.partial_parent_planner_full_only"]()
    report = await run_sweevo_scenario(
        scenario,
        instance=sweevo_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
    )

    assert report.task_center_status == "done", report.metrics
    planner_launches = [
        launch.agent_name for launch in report.launches if launch.role == "planner"
    ]
    assert planner_launches == [
        "planner",
        "planner_full_only",
        "planner",
    ]
    assert _tool_count(report.tool_calls, "submit_partial_plan") == 1
    assert _tool_count(report.tool_calls, "submit_full_plan") == 2
    _assert_partial_parent_graph(report.graph_summary)
    _assert_full_only_agent_md_was_recorded(report.run_dir)


def _tool_count(tool_calls: list[Any], tool_name: str) -> int:
    return sum(1 for call in tool_calls if call.tool_name == tool_name)


def _assert_partial_parent_graph(graph_summary: dict[str, Any]) -> None:
    missions = graph_summary["missions"]
    assert len(missions) == 2, graph_summary
    root = next(
        mission
        for mission in missions
        if str(mission["requested_by_task_id"]).endswith(":entry")
    )
    child = next(
        mission
        for mission in missions
        if not str(mission["requested_by_task_id"]).endswith(":entry")
    )

    assert len(root["episodes"]) == 2
    assert root["episodes"][0]["attempts"][-1]["continuation_goal"]
    assert str(child["requested_by_task_id"]).endswith(":delegate_child")


def _assert_full_only_agent_md_was_recorded(run_dir: Path) -> None:
    prompts = list(_system_prompts_for(run_dir, "planner_full_only"))
    assert prompts, f"no planner_full_only system prompt in {run_dir}"
    assert any("Partial planning is disabled" in prompt for prompt in prompts)
    assert all("submit_partial_plan" not in prompt for prompt in prompts)


def _system_prompts_for(run_dir: Path, agent_name: str) -> Iterator[str]:
    for path in run_dir.rglob("message.jsonl"):
        for line in path.read_text(encoding="utf-8").splitlines():
            row = json.loads(line)
            metadata = row.get("metadata") or {}
            if metadata.get("agent_name") != agent_name or row.get("role") != "system":
                continue
            yield "\n".join(
                str(block.get("text") or "")
                for block in row.get("content", [])
                if isinstance(block, dict)
            )
