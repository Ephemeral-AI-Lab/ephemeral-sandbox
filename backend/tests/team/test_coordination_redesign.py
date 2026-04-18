from __future__ import annotations

import json
from types import SimpleNamespace

import pytest
from pydantic import ValidationError

from agents.registry import get_definition
from team.builtins import register_all as register_team_builtins
from .helpers import structured_spec as _spec
from team.models import BudgetConfig, BudgetState, Task, TaskStatus
from team.runtime.context_builder import build_query_context, build_task_metadata
from tools.core.base import ToolExecutionContext
from tools.submission.toolkit import SubmitPlanTool, SubmitReplanTool


if get_definition("developer") is None:
    register_team_builtins()


class _AsyncTaskCenterStub:
    def __init__(self) -> None:
        self.posted: list = []
        self.notes = self  # production code calls tc.notes.post(note)
        self.context = self  # production code calls tc.context.context_for(task)
        self.graph: dict[str, Task] = {}

    async def post(self, note) -> None:
        self.posted.append(note)

    async def context_for(self, task: Task) -> str:
        return f"## Task\n{task.objective}"


class _AsyncDispatcherStub:
    def __init__(self, known_ids: set[str] | None = None) -> None:
        self._known_ids = known_ids or set()

    async def known_task_ids(self) -> set[str]:
        return set(self._known_ids)


def test_build_task_metadata_enables_team_runtime_flags():
    task = Task(
        id="task-1",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.PENDING,
        objective="implement auth",
        deps=["dep-1", "dep-2"],
        scope_paths=["src/auth"],
        depth=2,
    )
    team_run = SimpleNamespace(
        id="run-1",
        sandbox_id="sbx-1",
        project_context=SimpleNamespace(repo_root="/repo"),
        coordination_metadata={"require_declared_shell_outputs": True},
        task_center=object(),
        arbiter=None,
        budgets=BudgetConfig(max_tasks=12, max_depth=4, max_plan_size=6, max_note_bytes=2048),
        budget_state=BudgetState(tasks_used=3, note_bytes_used=128, replans_used=1),
        root_task_id="root-1",
        roster={"developer": ["developer"]},
    )

    meta = build_task_metadata(team_run, task)

    assert meta["task_deps"] == ["dep-1", "dep-2"]
    assert meta["task_parent_id"] is None
    assert meta["task_depth"] == 2
    assert meta["task_center"] is team_run.task_center
    assert meta["max_plan_size"] == 6
    assert meta["max_tasks"] == 12
    assert meta["max_depth"] == 4
    assert meta["max_note_bytes"] == 2048
    assert meta["tasks_used"] == 3
    assert meta["note_bytes_used"] == 128
    assert meta["replans_used"] == 1


@pytest.mark.asyncio
async def test_submit_plan_resolves_roster_role_hints():
    task_center = _AsyncTaskCenterStub()
    dispatcher = _AsyncDispatcherStub()
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "task_center_ref": dispatcher,
            "work_item_id": "planner-task",
            "agent_name": "team_planner",
            "allow_empty_plan": False,
            "roster": {"reviewer": ["validator"]},
            "max_plan_size": 8,
            "max_tasks": 20,
            "tasks_used": 1,
            "max_depth": 4,
            "task_depth": 0,
            "max_note_bytes": 10_000,
        },
    )

    tool = SubmitPlanTool()
    result = await tool.execute(
        tool.input_model(
            new_tasks=[
                {
                    "id": "impl",
                    "description": "Implement API",
                    "spec": _spec("Implement the API."),
                    "name": "developer",
                    "scope_paths": ["src/api.py"],
                },
                {
                    "id": "review",
                    "description": "Validate API changes",
                    "spec": _spec("Validate the API changes."),
                    "name": "reviewer",
                    "deps": ["impl"],
                    "scope_paths": ["src/api.py"],
                },
            ],
        ),
        ctx,
    )

    assert result.is_error is False, result.output
    payload = json.loads(result.output)
    assert payload["task_id"] == "planner-task"
    assert payload["agent_name"] == "team_planner"
    assert len(payload["new_tasks"]) == 2
    assert payload["new_tasks"][1]["agent"] == "validator"
    assert payload["new_tasks"][0]["description"] == "Implement API"
    resolved_plan = ctx.metadata.get("resolved_plan")
    assert resolved_plan is not None
    assert resolved_plan.tasks[0].description == "Implement API"
    assert resolved_plan.tasks[1].agent == "validator"
    assert len(task_center.posted) == 1
    assert "Submitted plan with 2 task(s)." in task_center.posted[0].content


@pytest.mark.asyncio
async def test_submit_plan_allows_stale_freshness_context():
    task_center = _AsyncTaskCenterStub()
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "planner-task",
            "agent_name": "team_planner",
            "allow_empty_plan": True,
            "work_item_started_at": 1.0,
            "agent_run_id": "run-1",
            "write_scope": ["src/auth/"],
            "arbiter": SimpleNamespace(
                initialized=True,
                changes_since=lambda _since, team_run_id=None: [
                    SimpleNamespace(
                        file_path="src/auth/session.py",
                        agent_run_id="run-2",
                    )
                ],
            ),
        },
    )

    result = await SubmitPlanTool().execute(
        SubmitPlanTool.input_model(new_tasks=[]),
        ctx,
    )

    assert result.is_error is False, result.output
    payload = json.loads(result.output)
    assert payload["task_id"] == "planner-task"
    assert payload["new_tasks"] == []


def test_submit_plan_requires_planner_authored_description():
    with pytest.raises(ValidationError):
        SubmitPlanTool.input_model(
            new_tasks=[
                {
                    "id": "missing-description",
                    "spec": _spec("Implement the API."),
                    "name": "developer",
                    "scope_paths": ["src/api.py"],
                }
            ]
        )


@pytest.mark.asyncio
async def test_submit_plan_rejects_overlong_description_without_truncating():
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": _AsyncTaskCenterStub(),
            "work_item_id": "planner-task",
            "agent_name": "team_planner",
            "allow_empty_plan": False,
            "max_plan_size": 8,
        },
    )

    result = await SubmitPlanTool().execute(
        SubmitPlanTool.input_model(
            new_tasks=[
                {
                    "id": "long-description",
                    "description": (
                        "one two three four five six seven eight nine ten eleven twelve thirteen"
                    ),
                    "spec": _spec("Implement the API."),
                    "name": "developer",
                    "scope_paths": ["src/api.py"],
                }
            ]
        ),
        ctx,
    )

    assert result.is_error is True
    assert "description has 13 words" in result.output


@pytest.mark.asyncio
async def test_submit_plan_rejects_oversize_task_notes():
    task_center = _AsyncTaskCenterStub()
    dispatcher = _AsyncDispatcherStub()
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "task_center_ref": dispatcher,
            "work_item_id": "planner-task",
            "agent_name": "team_planner",
            "allow_empty_plan": False,
            "max_plan_size": 8,
            "max_tasks": 20,
            "tasks_used": 1,
            "max_depth": 4,
            "task_depth": 0,
            "max_note_bytes": 16,
        },
    )

    tool = SubmitPlanTool()
    result = await tool.execute(
        tool.input_model(
            new_tasks=[
                {
                    "id": "oversize",
                    "description": "Oversize API note",
                    "spec": _spec(
                        "This task description is intentionally too large.",
                        environment="This environment text is also intentionally long.",
                    ),
                    "name": "developer",
                    "scope_paths": ["src/api.py"],
                }
            ]
        ),
        ctx,
    )

    assert result.is_error is True
    assert "max_note_bytes" in result.output
    assert ctx.metadata.get("submitted_output") is None
    assert task_center.posted == []


@pytest.mark.asyncio
async def test_submit_plan_rejects_malformed_spec_sections():
    task_center = _AsyncTaskCenterStub()
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "planner-task",
            "agent_name": "team_planner",
            "allow_empty_plan": False,
            "max_plan_size": 8,
        },
    )

    tool = SubmitPlanTool()
    result = await tool.execute(
        tool.input_model(
            new_tasks=[
                {
                    "id": "bad-spec",
                    "description": "Malformed API spec",
                    "spec": "Goal: Implement the API.\nScope: src/api.py",
                    "name": "developer",
                    "scope_paths": ["src/api.py"],
                }
            ]
        ),
        ctx,
    )

    assert result.is_error is True
    assert "missing spec section(s): Environment, Context, Acceptance Criteria" in result.output
    assert ctx.metadata.get("resolved_plan") is None


@pytest.mark.asyncio
async def test_submit_replan_accepts_child_repair_and_cancelled_sibling():
    task_center = _AsyncTaskCenterStub()
    task_center.graph = {
        "replanner-task": Task(
            id="replanner-task",
            team_run_id="run-1",
            agent_name="team_replanner",
            status=TaskStatus.READY,
            objective="recover",
            parent_id="parent",
        ),
        "stale": Task(
            id="stale",
            team_run_id="run-1",
            agent_name="developer",
            status=TaskStatus.READY,
            objective="stale work",
            parent_id="parent",
        ),
        "survivor": Task(
            id="survivor",
            team_run_id="run-1",
            agent_name="validator",
            status=TaskStatus.EXPANDED,
            objective="validate",
            deps=[],
            parent_id="parent",
        ),
    }
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replanner-task",
            "agent_name": "team_replanner",
            "role": "replanner",
        },
    )

    tool = SubmitReplanTool()
    result = await tool.execute(
        tool.input_model(
            cancel_ids=["stale"],
            new_tasks=[
                {
                    "id": "repair",
                    "description": "Repair implementation",
                    "spec": _spec("Repair the stale implementation path."),
                    "name": "developer",
                    "scope_paths": ["src/api.py"],
                },
                {
                    "id": "followup",
                    "description": "Follow up repair",
                    "spec": _spec("Follow-up owned by the replanner."),
                    "name": "developer",
                    "deps": ["repair"],
                    "scope_paths": ["src/api.py"],
                },
            ],
        ),
        ctx,
    )

    assert result.is_error is False, result.output
    payload = json.loads(result.output)
    assert payload["task_id"] == "replanner-task"
    assert payload["agent_name"] == "team_replanner"
    assert len(payload["new_tasks"]) == 2
    assert payload["cancel_ids"] == ["stale"]
    replan = ctx.metadata["resolved_plan"]
    assert [task.parent_id for task in replan.add_tasks] == [
        "replanner-task",
        "replanner-task",
    ]


def test_submit_replan_rejects_removed_sibling_arguments():
    with pytest.raises(ValidationError):
        SubmitReplanTool.input_model(new_sibling_tasks=[])

    with pytest.raises(ValidationError):
        SubmitReplanTool.input_model(new_children_tasks=[])

    with pytest.raises(ValidationError):
        SubmitReplanTool.input_model(
            new_tasks=[
                {
                    "id": "legacy-parent",
                    "description": "Legacy parent placement",
                    "spec": _spec("Legacy parent placement is rejected."),
                    "name": "developer",
                    "parent_id": "parent",
                }
            ]
        )


def test_submit_plan_rejects_legacy_parent_id_on_new_tasks():
    with pytest.raises(ValidationError):
        SubmitPlanTool.input_model(
            new_tasks=[
                {
                    "id": "legacy-parent",
                    "description": "Legacy parent placement",
                    "spec": _spec("Planner parent placement is rejected."),
                    "name": "developer",
                    "parent_id": "parent",
                }
            ]
        )


@pytest.mark.asyncio
async def test_submit_replan_rejects_replanner_agent_targets():
    task_center = _AsyncTaskCenterStub()
    task_center.graph = {
        "replanner-task": Task(
            id="replanner-task",
            team_run_id="run-1",
            agent_name="team_replanner",
            status=TaskStatus.READY,
            objective="recover",
            parent_id="parent",
        ),
    }
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replanner-task",
            "agent_name": "team_replanner",
            "role": "replanner",
            "max_plan_size": 8,
        },
    )

    result = await SubmitReplanTool().execute(
        SubmitReplanTool.input_model(
            new_tasks=[
                {
                    "id": "bad-replanner",
                    "description": "Spawn replanner",
                    "spec": _spec("Try to spawn another replanner."),
                    "name": "team_replanner",
                    "scope_paths": ["src/api.py"],
                }
            ]
        ),
        ctx,
    )

    assert result.is_error is True
    assert "submitted plans cannot include replanner agent" in result.output


@pytest.mark.asyncio
async def test_submit_replan_rejects_subagent_targets():
    task_center = _AsyncTaskCenterStub()
    task_center.graph = {
        "replanner-task": Task(
            id="replanner-task",
            team_run_id="run-1",
            agent_name="team_replanner",
            status=TaskStatus.READY,
            objective="recover",
            parent_id="parent",
        ),
    }
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replanner-task",
            "agent_name": "team_replanner",
            "role": "replanner",
            "max_plan_size": 8,
        },
    )

    result = await SubmitReplanTool().execute(
        SubmitReplanTool.input_model(
            new_tasks=[
                {
                    "id": "bad-subagent",
                    "description": "Target subagent",
                    "spec": _spec("Try to target a subagent directly."),
                    "name": "scout",
                    "scope_paths": ["src/api.py"],
                }
            ]
        ),
        ctx,
    )

    assert result.is_error is True
    assert "submitted plans cannot target 'subagent'-typed agent 'scout'" in result.output


@pytest.mark.asyncio
async def test_submit_replan_rejects_depth_overflow():
    task_center = _AsyncTaskCenterStub()
    task_center.graph = {
        "replanner-task": Task(
            id="replanner-task",
            team_run_id="run-1",
            agent_name="team_replanner",
            status=TaskStatus.READY,
            objective="recover",
            parent_id="parent",
            depth=1,
        ),
    }
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replanner-task",
            "agent_name": "team_replanner",
            "role": "replanner",
            "max_depth": 1,
            "task_depth": 1,
        },
    )

    result = await SubmitReplanTool().execute(
        SubmitReplanTool.input_model(
            new_tasks=[
                {
                    "id": "too-deep",
                    "description": "Repair below limit",
                    "spec": _spec("Repair below the depth limit."),
                    "name": "developer",
                    "scope_paths": ["src/api.py"],
                }
            ]
        ),
        ctx,
    )

    assert result.is_error is True
    assert "replan would exceed max_depth=1 from current depth=1" in result.output


@pytest.mark.asyncio
async def test_submit_replan_rejects_plan_size_overflow():
    task_center = _AsyncTaskCenterStub()
    task_center.graph = {
        "replanner-task": Task(
            id="replanner-task",
            team_run_id="run-1",
            agent_name="team_replanner",
            status=TaskStatus.READY,
            objective="recover",
            parent_id="parent",
        ),
    }
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replanner-task",
            "agent_name": "team_replanner",
            "role": "replanner",
            "max_plan_size": 1,
        },
    )

    result = await SubmitReplanTool().execute(
        SubmitReplanTool.input_model(
            new_tasks=[
                {
                    "id": "repair-a",
                    "description": "Repair first path",
                    "spec": _spec("Repair one path."),
                    "name": "developer",
                    "scope_paths": ["src/a.py"],
                },
                {
                    "id": "repair-b",
                    "description": "Repair second path",
                    "spec": _spec("Repair another path."),
                    "name": "developer",
                    "scope_paths": ["src/b.py"],
                },
            ]
        ),
        ctx,
    )

    assert result.is_error is True
    assert "exceeds max_plan_size=1" in result.output


@pytest.mark.asyncio
async def test_submit_replan_rejects_task_budget_overflow():
    task_center = _AsyncTaskCenterStub()
    task_center.graph = {
        "replanner-task": Task(
            id="replanner-task",
            team_run_id="run-1",
            agent_name="team_replanner",
            status=TaskStatus.READY,
            objective="recover",
            parent_id="parent",
        ),
    }
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replanner-task",
            "agent_name": "team_replanner",
            "role": "replanner",
            "max_tasks": 1,
            "tasks_used": 1,
        },
    )

    result = await SubmitReplanTool().execute(
        SubmitReplanTool.input_model(
            new_tasks=[
                {
                    "id": "repair",
                    "description": "Repair over budget",
                    "spec": _spec("Repair over the task budget."),
                    "name": "developer",
                    "scope_paths": ["src/api.py"],
                }
            ]
        ),
        ctx,
    )

    assert result.is_error is True
    assert "replan would exceed max_tasks=1" in result.output


def test_submit_replan_rejects_removed_expected_projection_argument():
    with pytest.raises(ValidationError):
        SubmitReplanTool.input_model(expected_projection={"root_parent_id": "parent"})


def test_submit_replan_rejects_removed_output_argument():
    with pytest.raises(ValidationError):
        SubmitReplanTool.input_model(output="replan rationale")


@pytest.mark.asyncio
async def test_build_query_context_planner_terminal_tools():
    task = Task(
        id="planner-task",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.READY,
        objective="plan work",
    )
    task_center = _AsyncTaskCenterStub()
    team_run = SimpleNamespace(
        id="run-1",
        sandbox_id="sbx-1",
        project_context=SimpleNamespace(repo_root="/repo"),
        coordination_metadata={},
        task_center=task_center,
        arbiter=None,
        budgets=None,
        budget_state=None,
        root_task_id="planner-task",
        roster={"planner": ["team_planner"]},
        team_definition=None,
    )

    ctx = await build_query_context(
        SimpleNamespace(role="planner"),
        team_run,
        task,
    )

    assert ctx.tool_metadata["terminal_tools"] == {"submit_plan"}


@pytest.mark.asyncio
async def test_build_query_context_uses_team_terminal_tools_override_for_note_taker():
    task = Task(
        id="note-task",
        team_run_id="run-1",
        agent_name="note_taker",
        status=TaskStatus.READY,
        objective="summarize task progress",
    )
    task_center = _AsyncTaskCenterStub()
    team_run = SimpleNamespace(
        id="run-1",
        sandbox_id="sbx-1",
        project_context=SimpleNamespace(repo_root="/repo"),
        coordination_metadata={},
        task_center=task_center,
        arbiter=None,
        budgets=None,
        budget_state=None,
        root_task_id="planner-task",
        roster={"task_center_note_taker": ["note_taker"]},
        team_definition=SimpleNamespace(terminal_tools={"note_taker": {"submit_task_note"}}),
    )

    ctx = await build_query_context(
        SimpleNamespace(role="note_taker"),
        team_run,
        task,
    )

    assert ctx.tool_metadata["terminal_tools"] == {"submit_task_note"}
