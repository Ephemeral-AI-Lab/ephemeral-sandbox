from __future__ import annotations

import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from pydantic import ValidationError

from agents.registry import get_definition
from team.builtins import register_all as register_team_builtins
from .helpers import structured_spec as _spec
from team.models import BudgetConfig, BudgetState, Task, TaskStatus
from team.runtime.context_builder import build_query_context, build_task_metadata
from tools.core.base import ToolExecutionContext
from tools.submission.toolkit import SubmitPlanTool, SubmitReplanTool
from prompt.external_trigger_prompts import build_parent_summary_prompt as _build_parent_summary_prompt


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
    audit_notes = [
        n for n in task_center.posted if "architecture" in (n.tags or [])
    ]
    assert len(audit_notes) == 1
    assert "Submitted plan with 2 task(s)." in audit_notes[0].content
    assert "Tasks:" in audit_notes[0].content
    assert "- impl (developer): Implement API; scope=src/api.py" in audit_notes[0].content
    assert (
        "- review (validator): Validate API changes; deps=impl; scope=src/api.py"
        in audit_notes[0].content
    )


@pytest.mark.asyncio
async def test_submit_plan_rejects_empty_plan_with_arbiter_scope_change_context():
    task_center = _AsyncTaskCenterStub()
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "planner-task",
            "agent_name": "team_planner",
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
        SubmitPlanTool.input_model(
            new_tasks=[],
        ),
        ctx,
    )

    assert result.is_error is True
    assert "plan has no tasks" in result.output


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
            ],
        )


def test_submit_plan_rejects_legacy_output_field():
    """The legacy `output` prose field is gone; extra='forbid' must reject it."""
    with pytest.raises(ValidationError):
        SubmitPlanTool.input_model(new_tasks=[], output="legacy prose")


def test_submit_plan_schema_keeps_new_tasks_and_drops_prose_fields():
    tool = SubmitPlanTool()
    schema = tool.to_api_schema()

    assert "implementation owner paths" in schema["description"]
    assert "including validators" in schema["description"]
    assert "distinct verification lane" in schema["description"]
    assert "non-empty repo-relative scope_paths" in schema["description"]
    assert "Do not tell children to `cd /testbed`" in schema["description"]
    assert "not `/testbed/...` prefixes" in schema["description"]
    assert "new_tasks" in schema["input_schema"]["properties"]
    assert "output" not in schema["input_schema"]["properties"]
    assert "summary" not in schema["input_schema"]["properties"]
    spec_desc = schema["input_schema"]["$defs"]["NewTaskSpec"]["properties"]["spec"][
        "description"
    ]
    assert "1. Goal" in spec_desc
    assert "2. Task Details" in spec_desc
    assert "3. Acceptance Criteria" in spec_desc
    assert "2. Environment" not in spec_desc
    scope_desc = schema["input_schema"]["$defs"]["NewTaskSpec"]["properties"]["scope_paths"][
        "description"
    ]
    assert "implementation owner paths" in scope_desc
    assert "repo-relative implementation owner paths" in scope_desc
    assert "not `/testbed/...` prefixes" in scope_desc
    assert "For validators, use the production paths being verified" in scope_desc
    assert "test files and test directories are rejected as scope_paths" in scope_desc

    payload = tool.input_model(
        new_tasks=[
            {
                "id": "dev-owner",
                "description": "Repair owner",
                "name": "developer",
                "spec": _spec("Repair the production owner."),
                "scope_paths": ["pkg/owner.py"],
            }
        ],
    )
    assert payload.new_tasks[0].scope_paths == ["pkg/owner.py"]


def test_submit_replan_schema_keeps_new_tasks_and_drops_prose_fields():
    tool = SubmitReplanTool()
    schema = tool.to_api_schema()

    assert "new_tasks" in schema["input_schema"]["properties"]
    assert "cancel_ids" in schema["input_schema"]["required"]
    assert "non-empty new_tasks" in schema["description"]
    assert "use cancel_ids=[] when none should be cancelled" in schema["description"]
    assert "repo-relative production scope_paths" in schema["description"]
    assert "validator tasks are present" in schema["description"]
    assert "numbered colon labels" in schema["description"]
    assert "Test files and test directories are rejected as scope_paths" in schema["description"]
    name_schema = schema["input_schema"]["$defs"]["NewTaskSpec"]["properties"]["name"]
    assert name_schema["enum"] == ["developer", "validator"]
    assert "team_planner" not in name_schema["enum"]
    assert "summary" not in schema["input_schema"]["properties"]
    assert "output" not in schema["input_schema"]["properties"]

    with pytest.raises(ValidationError):
        SubmitReplanTool.input_model(output="legacy rationale")
    with pytest.raises(ValidationError):
        SubmitReplanTool.input_model(summary="legacy summary")
    with pytest.raises(ValidationError):
        SubmitReplanTool.input_model(
            new_tasks=[
                {
                    "id": "repair-owner",
                    "description": "Repair owner",
                    "name": "developer",
                    "spec": _spec("Repair the production owner."),
                    "deps": [],
                    "scope_paths": ["pkg/owner.py"],
                }
            ],
        )


@pytest.mark.asyncio
async def test_submit_replan_rejects_empty_new_tasks_with_deeper_diagnosis_prompt():
    task_center = _AsyncTaskCenterStub()
    task_center.graph["replanner-task"] = Task(
        id="replanner-task",
        team_run_id="run-1",
        agent_name="team_replanner",
        status=TaskStatus.RUNNING,
        objective="recover failed work",
        parent_id="root",
    )
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replanner-task",
            "agent_name": "team_replanner",
            "role": "replanner",
        },
    )

    result = await SubmitReplanTool().execute(
        SubmitReplanTool.input_model(new_tasks=[], cancel_ids=[]),
        ctx,
    )

    assert result.is_error is True
    assert "submit_replan requires at least one corrective new_task" in result.output
    assert "look deeper into the issues and come back" in result.output
    assert ctx.metadata.get("resolved_plan") is None
    assert task_center.posted == []


@pytest.mark.asyncio
async def test_submit_plan_posts_structured_initial_planned_tasks_note():
    """After validation, submit_plan attaches the structured JSON payload as a
    note tagged `initial_planned_tasks` on the parent task so downstream
    readers can retrieve it via read_task_details."""
    task_center = _AsyncTaskCenterStub()
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "planner-task",
            "agent_name": "team_planner",
            "max_plan_size": 8,
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
            ],
        ),
        ctx,
    )
    assert result.is_error is False, result.output

    tagged = [
        n for n in task_center.posted if "initial_planned_tasks" in (n.tags or [])
    ]
    assert len(tagged) == 1
    note = tagged[0]
    assert note.task_id == "planner-task"
    payload = json.loads(note.content)
    assert isinstance(payload, list)
    assert payload[0]["id"] == "impl"
    assert payload[0]["agent"] == "developer"
    assert payload[0]["scope_paths"] == ["src/api.py"]
    assert "src/api.py" in (note.paths or [])


@pytest.mark.asyncio
async def test_submit_replan_posts_structured_initial_replanned_tasks_note():
    task_center = _AsyncTaskCenterStub()
    parent = Task(
        id="replan-1",
        team_run_id="run-1",
        agent_name="replanner",
        status=TaskStatus.RUNNING,
        objective="replan",
        scope_paths=[],
        parent_id="root",
    )
    task_center.graph["replan-1"] = parent
    task_center.graph["root"] = Task(
        id="root",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED,
        objective="root",
    )
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": task_center,
            "work_item_id": "replan-1",
            "agent_name": "replanner",
            "max_plan_size": 8,
            "max_note_bytes": 10_000,
        },
    )

    tool = SubmitReplanTool()
    result = await tool.execute(
        tool.input_model(
            new_tasks=[
                {
                    "id": "fix-api",
                    "description": "Fix broken API",
                    "spec": _spec("Repair the API."),
                    "name": "developer",
                    "scope_paths": ["src/api.py"],
                },
            ],
            cancel_ids=[],
        ),
        ctx,
    )
    assert result.is_error is False, result.output

    tagged = [
        n for n in task_center.posted if "initial_replanned_tasks" in (n.tags or [])
    ]
    assert len(tagged) == 1
    note = tagged[0]
    assert note.task_id == "replan-1"
    payload = json.loads(note.content)
    assert isinstance(payload, list)
    assert payload[0]["id"] == "fix-api"
    assert payload[0]["parent_id"] == "replan-1"


@pytest.mark.asyncio
async def test_submit_plan_accepts_long_description_without_truncating():
    ctx = ToolExecutionContext(
        cwd="/tmp",
        metadata={
            "task_center": _AsyncTaskCenterStub(),
            "work_item_id": "planner-task",
            "agent_name": "team_planner",
            "max_plan_size": 8,
        },
    )

    result = await SubmitPlanTool().execute(
        SubmitPlanTool.input_model(
            new_tasks=[
                {
                    "id": "long-description",
                    "description": (
                        "one two three four five six seven eight nine ten eleven twelve thirteen "
                        "fourteen fifteen sixteen seventeen eighteen nineteen twenty twentyone"
                    ),
                    "spec": _spec("Implement the API."),
                    "name": "developer",
                    "scope_paths": ["src/api.py"],
                }
            ],
        ),
        ctx,
    )

    assert result.is_error is False, result.output
    assert "one two three four five six seven eight nine ten eleven twelve thirteen" in result.output
    assert "twenty twentyone" in result.output


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
            ],
        ),
        ctx,
    )

    assert result.is_error is True
    assert "max_note_bytes" in result.output
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
            ],
        ),
        ctx,
    )

    assert result.is_error is True
    assert "missing spec section(s): Task Details, Acceptance Criteria" in result.output
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
    audit_notes = [
        n for n in task_center.posted if "refactor" in (n.tags or [])
    ]
    assert len(audit_notes) == 1
    assert "Corrective tasks:" in audit_notes[0].content
    assert "- repair (developer): Repair implementation; scope=src/api.py" in (
        audit_notes[0].content
    )
    assert "Cancelled siblings: stale" in audit_notes[0].content


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
            ],
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
            ],
            cancel_ids=[],
        ),
        ctx,
    )

    assert result.is_error is True
    assert "submitted plans cannot include replanner agent" in result.output


@pytest.mark.asyncio
async def test_submit_replan_rejects_planner_agent_targets():
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
                    "id": "bad-planner",
                    "description": "Spawn planner",
                    "spec": _spec("Try to delegate replanning to a planner."),
                    "name": "team_planner",
                    "scope_paths": ["src/api.py"],
                }
            ],
            cancel_ids=[],
        ),
        ctx,
    )

    assert result.is_error is True
    assert "submit_replan can only create developer or validator tasks" in result.output
    assert "agent 'team_planner'" in result.output


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
            ],
            cancel_ids=[],
        ),
        ctx,
    )

    assert result.is_error is True
    assert "submitted plans cannot target 'subagent'-typed agent 'scout'" in result.output


@pytest.mark.asyncio
async def test_submit_replan_requires_diagnostics_decision_for_unresolved_blocker():
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
                    "id": "missing-decision",
                    "description": "Repair unresolved blocker",
                    "spec": _spec(
                        "Repair unresolved blocker evidence.",
                        task_details=(
                            "Classification: unresolved_blocker. "
                            "Repair the production dispatch path."
                        ),
                    ),
                    "name": "developer",
                    "scope_paths": ["src/api.py"],
                }
            ],
            cancel_ids=[],
        ),
        ctx,
    )

    assert result.is_error is True
    assert (
        "unresolved_blocker requires Diagnostics decision: "
        "trivial_direct_replan or deep_diagnostics"
        in result.output
    )
    assert ctx.metadata.get("resolved_plan") is None


@pytest.mark.asyncio
async def test_submit_replan_accepts_repair_at_replanner_depth_limit():
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
                    "id": "same-depth-repair",
                    "description": "Repair at limit",
                    "spec": _spec("Repair at the replanner depth limit."),
                    "name": "developer",
                    "scope_paths": ["src/api.py"],
                }
            ],
            cancel_ids=[],
        ),
        ctx,
    )

    assert result.is_error is False, result.output
    replan = ctx.metadata["resolved_plan"]
    assert [task.id for task in replan.add_tasks] == ["same-depth-repair"]
    assert [task.parent_id for task in replan.add_tasks] == ["replanner-task"]


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
            ],
            cancel_ids=[],
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
            ],
            cancel_ids=[],
        ),
        ctx,
    )

    assert result.is_error is True
    assert "replan would exceed max_tasks=1" in result.output


def test_submit_replan_rejects_removed_expected_projection_argument():
    with pytest.raises(ValidationError):
        SubmitReplanTool.input_model(expected_projection={"root_parent_id": "parent"})


@pytest.mark.asyncio
async def test_parent_summary_task_is_injected_on_awaiting_summary():
    """When a planner parent transitions to EAS, a dispatchable summary task
    is injected instead of firing an external callback.

    The injected task carries ``fired_by_task_id = parent.id`` and the
    ``parent_summarizer`` agent role, so the normal Executor picks it up and
    runs the summarizer through the standard team-agent path.
    """
    from team.task_center import TaskCenter

    class _FakeSessionFactory:
        def __call__(self):
            class _Ctx:
                async def __aenter__(self_inner):
                    return None

                async def __aexit__(self_inner, *a):
                    return False

            return _Ctx()

    tc = TaskCenter(
        session_factory=_FakeSessionFactory(),
        team_run_id="run-1",
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
    )

    parent = Task(
        id="planner-parent",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED,
        objective="plan",
        depth=0,
        root_id="planner-parent",
    )
    tc.store._tg.tasks[parent.id] = parent

    async def _fake_maybe_promote(child_id: str):
        parent.status = TaskStatus.EXPANDED_AWAITING_SUMMARY
        return [], ["planner-parent"]

    inserted: list[Task] = []

    async def _fake_insert_parent_summary_task(
        *, parent_task: Task, summarizer_agent: str, objective: str
    ) -> tuple[Task, bool]:
        summary_task = Task(
            id="summary-1",
            team_run_id="run-1",
            agent_name=summarizer_agent,
            status=TaskStatus.READY,
            objective=objective,
            parent_id=parent_task.id,
            depth=parent_task.depth + 1,
            root_id=parent_task.root_id,
            fired_by_task_id=parent_task.id,
        )
        tc.store._tg.tasks[summary_task.id] = summary_task
        inserted.append(summary_task)
        return summary_task, True

    tc.store.maybe_promote_expanded_parent = _fake_maybe_promote  # type: ignore[assignment]
    tc.store.mark_done = AsyncMock(return_value=[])  # type: ignore[assignment]
    tc.store.insert_parent_summary_task = _fake_insert_parent_summary_task  # type: ignore[assignment]

    await tc._mark_done_emit_promotions("child-id")

    assert len(inserted) == 1, f"expected one injected summary task, got {inserted!r}"
    summary = inserted[0]
    assert summary.agent_name == "parent_summarizer"
    assert summary.fired_by_task_id == "planner-parent"
    assert summary.parent_id == "planner-parent"
    assert summary.status == TaskStatus.READY
    assert "planner-parent" in summary.objective
    assert parent.status == TaskStatus.EXPANDED_AWAITING_SUMMARY


@pytest.mark.asyncio
async def test_parent_summary_task_recovery_reuses_live_sidecar_without_budget():
    from team.task_center import TaskCenter

    class _FakeSessionFactory:
        def __call__(self):
            class _Ctx:
                async def __aenter__(self_inner):
                    return None

                async def __aexit__(self_inner, *a):
                    return False

            return _Ctx()

    budget_state = BudgetState(tasks_used=0)
    tc = TaskCenter(
        session_factory=_FakeSessionFactory(),
        team_run_id="run-1",
        budgets=BudgetConfig(max_tasks=0),
        budget_state=budget_state,
    )
    parent = Task(
        id="planner-parent",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED_AWAITING_SUMMARY,
        objective="plan",
        depth=0,
        root_id="planner-parent",
    )
    existing_summary = Task(
        id="summary-existing",
        team_run_id="run-1",
        agent_name="parent_summarizer",
        status=TaskStatus.READY,
        objective="Summarize planner-parent",
        parent_id="planner-parent",
        depth=1,
        root_id="planner-parent",
        fired_by_task_id="planner-parent",
    )
    tc.store._tg.tasks[parent.id] = parent
    tc.store._tg.tasks[existing_summary.id] = existing_summary

    async def _unexpected_insert(*_args, **_kwargs):
        raise AssertionError("existing parent-summary sidecar should be reused")

    tc.store.insert_parent_summary_task = _unexpected_insert  # type: ignore[assignment]

    await tc._ensure_parent_summary_task(parent.id)

    assert budget_state.tasks_used == 0


@pytest.mark.asyncio
async def test_parent_summary_task_creation_does_not_consume_user_task_budget():
    from team.task_center import TaskCenter

    class _FakeSessionFactory:
        def __call__(self):
            class _Ctx:
                async def __aenter__(self_inner):
                    return None

                async def __aexit__(self_inner, *a):
                    return False

            return _Ctx()

    budget_state = BudgetState(tasks_used=0)
    tc = TaskCenter(
        session_factory=_FakeSessionFactory(),
        team_run_id="run-1",
        budgets=BudgetConfig(max_tasks=0),
        budget_state=budget_state,
    )
    parent = Task(
        id="planner-parent",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED_AWAITING_SUMMARY,
        objective="plan",
        depth=0,
        root_id="planner-parent",
    )
    tc.store._tg.tasks[parent.id] = parent
    inserted: list[Task] = []

    async def _fake_insert_parent_summary_task(
        *, parent_task: Task, summarizer_agent: str, objective: str
    ) -> tuple[Task, bool]:
        summary_task = Task(
            id="summary-created",
            team_run_id="run-1",
            agent_name=summarizer_agent,
            status=TaskStatus.READY,
            objective=objective,
            parent_id=parent_task.id,
            depth=parent_task.depth + 1,
            root_id=parent_task.root_id,
            fired_by_task_id=parent_task.id,
        )
        tc.store._tg.tasks[summary_task.id] = summary_task
        inserted.append(summary_task)
        return summary_task, True

    tc.store.insert_parent_summary_task = _fake_insert_parent_summary_task  # type: ignore[assignment]

    await tc._ensure_parent_summary_task(parent.id)

    assert [task.id for task in inserted] == ["summary-created"]
    assert budget_state.tasks_used == 0


@pytest.mark.asyncio
async def test_parent_summary_task_completion_finalizes_parent_and_posts_note():
    """Completing a parent_summarizer summary task should post an
    implementation/parent_summary note against the parent id and transition
    the EAS parent to DONE.
    """
    from team.models import AgentResult
    from team.task_center import TaskCenter

    class _FakeSessionFactory:
        def __call__(self):
            class _Ctx:
                async def __aenter__(self_inner):
                    return None

                async def __aexit__(self_inner, *a):
                    return False

            return _Ctx()

    tc = TaskCenter(
        session_factory=_FakeSessionFactory(),
        team_run_id="run-1",
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
    )

    parent = Task(
        id="planner-parent",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED_AWAITING_SUMMARY,
        objective="plan",
        depth=0,
        root_id="planner-parent",
        scope_paths=["src/foo.py"],
    )
    summary_task = Task(
        id="summary-1",
        team_run_id="run-1",
        agent_name="parent_summarizer",
        status=TaskStatus.RUNNING,
        objective="Summarize planner-parent",
        parent_id="planner-parent",
        depth=1,
        root_id="planner-parent",
        fired_by_task_id="planner-parent",
    )
    tc.store._tg.tasks[parent.id] = parent
    tc.store._tg.tasks[summary_task.id] = summary_task

    async def _fake_get_record(task_id: str):
        if task_id == summary_task.id:
            return SimpleNamespace(
                id=task_id,
                status="running",
                agent_name="parent_summarizer",
            )
        if task_id == parent.id:
            return SimpleNamespace(
                id=task_id,
                status="expanded_awaiting_summary",
                agent_name="team_planner",
            )
        return None

    async def _fake_mark_done(task_id: str):
        if task_id == parent.id:
            parent.status = TaskStatus.DONE
        elif task_id == summary_task.id:
            summary_task.status = TaskStatus.DONE
        return []

    async def _fake_maybe_promote(child_id: str):
        return [], []

    async def _noop_refresh():
        return tc.store._tg.tasks

    tc.store.get_record = _fake_get_record  # type: ignore[assignment]
    tc.store.mark_done = _fake_mark_done  # type: ignore[assignment]
    tc.store.maybe_promote_expanded_parent = _fake_maybe_promote  # type: ignore[assignment]
    tc.store.finalize_parent_summary = _fake_mark_done  # type: ignore[assignment]
    tc.store.refresh_graph = _noop_refresh  # type: ignore[assignment]

    result = AgentResult(summary="header\n- child-1 (dev, done): landed X")
    await tc.complete_task(summary_task.id, result)

    assert parent.status == TaskStatus.DONE
    parent_summary_notes = [
        n for n in tc.notes.snapshot()
        if n.task_id == parent.id and "parent_summary" in (n.tags or [])
    ]
    assert len(parent_summary_notes) == 1
    assert parent_summary_notes[0].agent_name == "parent_summarizer"
    assert parent_summary_notes[0].content == result.summary


@pytest.mark.asyncio
async def test_parent_summary_note_post_failure_fails_parent_and_fires_fail_fast():
    from team.models import AgentResult
    from team.task_center import TaskCenter

    class _FakeSessionFactory:
        def __call__(self):
            class _Ctx:
                async def __aenter__(self_inner):
                    return None

                async def __aexit__(self_inner, *a):
                    return False

            return _Ctx()

    tc = TaskCenter(
        session_factory=_FakeSessionFactory(),
        team_run_id="run-1",
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
    )
    fail_fast_reasons: list[str] = []

    async def _fake_fail_fast(reason: str) -> None:
        fail_fast_reasons.append(reason)

    tc.set_fail_fast_callback(_fake_fail_fast)

    parent = Task(
        id="planner-parent",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED_AWAITING_SUMMARY,
        objective="plan",
        depth=0,
        root_id="planner-parent",
    )
    summary_task = Task(
        id="summary-1",
        team_run_id="run-1",
        agent_name="parent_summarizer",
        status=TaskStatus.RUNNING,
        objective="Summarize planner-parent",
        parent_id="planner-parent",
        depth=1,
        root_id="planner-parent",
        fired_by_task_id="planner-parent",
    )
    tc.store._tg.tasks[parent.id] = parent
    tc.store._tg.tasks[summary_task.id] = summary_task

    async def _fake_get_record(task_id: str):
        if task_id == summary_task.id:
            return SimpleNamespace(
                id=task_id,
                status="running",
                agent_name="parent_summarizer",
            )
        if task_id == parent.id:
            return SimpleNamespace(
                id=task_id,
                status="expanded_awaiting_summary",
                agent_name="team_planner",
            )
        return None

    async def _fake_mark_done(task_id: str):
        if task_id == parent.id:
            parent.status = TaskStatus.DONE
        elif task_id == summary_task.id:
            summary_task.status = TaskStatus.DONE
        return []

    async def _fake_maybe_promote(child_id: str):
        return [], []

    async def _unexpected_finalize(task_id: str):
        raise AssertionError(f"parent finalized without durable summary: {task_id}")

    async def _fake_mark_terminal(task_id: str, status: str, reason: str):
        if task_id == parent.id and status == "failed":
            parent.status = TaskStatus.FAILED
            parent.failure_reason = reason

    async def _noop_refresh():
        return tc.store._tg.tasks

    async def _failing_note_post(_note):
        raise RuntimeError("note store unavailable")

    tc.store.get_record = _fake_get_record  # type: ignore[assignment]
    tc.store.mark_done = _fake_mark_done  # type: ignore[assignment]
    tc.store.mark_terminal = _fake_mark_terminal  # type: ignore[assignment]
    tc.store.maybe_promote_expanded_parent = _fake_maybe_promote  # type: ignore[assignment]
    tc.store.finalize_parent_summary = _unexpected_finalize  # type: ignore[assignment]
    tc.store.refresh_graph = _noop_refresh  # type: ignore[assignment]
    tc.notes.post = _failing_note_post  # type: ignore[assignment]

    await tc.complete_task(summary_task.id, AgentResult(summary="child roll-up"))

    assert summary_task.status == TaskStatus.DONE
    assert parent.status == TaskStatus.FAILED
    assert parent.failure_reason == "parent_summary_note_post_failed"
    assert fail_fast_reasons == ["parent_summary_note_post_failed"]


@pytest.mark.asyncio
async def test_parent_summary_empty_fails_parent_and_fires_fail_fast():
    """Empty summary content → EAS parent → failed + fail_fast invoked."""
    from team.models import AgentResult
    from team.task_center import TaskCenter

    class _FakeSessionFactory:
        def __call__(self):
            class _Ctx:
                async def __aenter__(self_inner):
                    return None

                async def __aexit__(self_inner, *a):
                    return False

            return _Ctx()

    tc = TaskCenter(
        session_factory=_FakeSessionFactory(),
        team_run_id="run-1",
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
    )
    fail_fast_reasons: list[str] = []

    async def _fake_fail_fast(reason: str) -> None:
        fail_fast_reasons.append(reason)

    tc.set_fail_fast_callback(_fake_fail_fast)

    parent = Task(
        id="planner-parent",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED_AWAITING_SUMMARY,
        objective="plan",
        depth=0,
        root_id="planner-parent",
    )
    summary_task = Task(
        id="summary-1",
        team_run_id="run-1",
        agent_name="parent_summarizer",
        status=TaskStatus.RUNNING,
        objective="Summarize planner-parent",
        parent_id="planner-parent",
        depth=1,
        root_id="planner-parent",
        fired_by_task_id="planner-parent",
    )
    tc.store._tg.tasks[parent.id] = parent
    tc.store._tg.tasks[summary_task.id] = summary_task

    async def _fake_get_record(task_id: str):
        if task_id == summary_task.id:
            return SimpleNamespace(
                id=task_id, status="running", agent_name="parent_summarizer"
            )
        if task_id == parent.id:
            return SimpleNamespace(
                id=task_id,
                status="expanded_awaiting_summary",
                agent_name="team_planner",
            )
        return None

    async def _fake_mark_done(task_id: str):
        if task_id == summary_task.id:
            summary_task.status = TaskStatus.DONE
        return []

    async def _fake_mark_terminal(task_id: str, status: str, reason: str):
        if task_id == parent.id and status == "failed":
            parent.status = TaskStatus.FAILED
            parent.failure_reason = reason

    async def _fake_maybe_promote(child_id: str):
        return [], []

    async def _noop_refresh():
        return tc.store._tg.tasks

    tc.store.get_record = _fake_get_record  # type: ignore[assignment]
    tc.store.mark_done = _fake_mark_done  # type: ignore[assignment]
    tc.store.mark_terminal = _fake_mark_terminal  # type: ignore[assignment]
    tc.store.maybe_promote_expanded_parent = _fake_maybe_promote  # type: ignore[assignment]
    tc.store.refresh_graph = _noop_refresh  # type: ignore[assignment]

    await tc.complete_task(summary_task.id, AgentResult(summary=""))

    assert parent.status == TaskStatus.FAILED
    assert parent.failure_reason == "parent_summary_empty"
    assert fail_fast_reasons == ["parent_summary_empty"]
    # No parent_summary note should be posted for an empty summary.
    assert not any(
        "parent_summary" in (n.tags or []) for n in tc.notes.snapshot()
    )


@pytest.mark.asyncio
async def test_parent_summary_leaf_failure_fails_parent_and_fires_fail_fast():
    """Summary task leaf failure → EAS parent → failed + fail_fast invoked."""
    from team.task_center import TaskCenter

    class _FakeSessionFactory:
        def __call__(self):
            class _Ctx:
                async def __aenter__(self_inner):
                    return None

                async def __aexit__(self_inner, *a):
                    return False

            return _Ctx()

    tc = TaskCenter(
        session_factory=_FakeSessionFactory(),
        team_run_id="run-1",
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
    )
    fail_fast_reasons: list[str] = []

    async def _fake_fail_fast(reason: str) -> None:
        fail_fast_reasons.append(reason)

    tc.set_fail_fast_callback(_fake_fail_fast)

    parent = Task(
        id="planner-parent",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED_AWAITING_SUMMARY,
        objective="plan",
        depth=0,
        root_id="planner-parent",
    )
    summary_task = Task(
        id="summary-1",
        team_run_id="run-1",
        agent_name="parent_summarizer",
        status=TaskStatus.RUNNING,
        objective="Summarize planner-parent",
        parent_id="planner-parent",
        depth=1,
        root_id="planner-parent",
        fired_by_task_id="planner-parent",
    )
    tc.store._tg.tasks[parent.id] = parent
    tc.store._tg.tasks[summary_task.id] = summary_task

    async def _fake_store_fail_task(task_id: str, reason: str):
        if task_id == summary_task.id:
            summary_task.status = TaskStatus.FAILED
            summary_task.failure_reason = reason

    async def _fake_get_record(task_id: str):
        if task_id == parent.id:
            return SimpleNamespace(
                id=task_id,
                status="expanded_awaiting_summary",
                agent_name="team_planner",
            )
        return None

    async def _fake_mark_terminal(task_id: str, status: str, reason: str):
        if task_id == parent.id and status == "failed":
            parent.status = TaskStatus.FAILED
            parent.failure_reason = reason

    async def _fake_maybe_promote(child_id: str):
        return [], []

    async def _noop_refresh():
        return tc.store._tg.tasks

    tc.store.fail_task = _fake_store_fail_task  # type: ignore[assignment]
    tc.store.get_record = _fake_get_record  # type: ignore[assignment]
    tc.store.mark_terminal = _fake_mark_terminal  # type: ignore[assignment]
    tc.store.maybe_promote_expanded_parent = _fake_maybe_promote  # type: ignore[assignment]
    tc.store.refresh_graph = _noop_refresh  # type: ignore[assignment]
    tc._transitions._refresh_graph_fn = _noop_refresh  # type: ignore[attr-defined]

    await tc.fail_task(summary_task.id, "worker_crash")

    assert summary_task.status == TaskStatus.FAILED
    assert parent.status == TaskStatus.FAILED
    assert parent.failure_reason == "parent_summary_task_failed"
    assert fail_fast_reasons == ["parent_summary_task_failed"]


def test_parent_summary_prompt_lists_completed_children_to_read_first():
    parent = Task(
        id="planner-parent",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED_AWAITING_SUMMARY,
        objective="Plan retry work.",
        depth=0,
        root_id="planner-parent",
    )
    child = Task(
        id="dev-child",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.DONE,
        objective=_spec("Repair retry owner."),
        parent_id="planner-parent",
        root_id="planner-parent",
        depth=1,
        scope_paths=["src/retry.py"],
    )
    prompt = _build_parent_summary_prompt(parent, [child])

    assert "# Parent summarizer task" in prompt
    assert "All direct children of the parent task are terminal" in prompt
    assert "## Parent task id\nplanner-parent" in prompt
    assert "## Terminal direct child task ids to read\n- dev-child" in prompt
    assert 'read_task_details(task_id="planner-parent")' in prompt
    assert "read_task_details(task_id=...)" in prompt
    assert "Only after every listed child has been read" in prompt
    assert "pytest config or warning overrides" in prompt
    assert "`--override-ini`" in prompt
    assert 'submit `type="request_replan"`' in prompt
    assert "This terminal submission is the completion signal for the parent task" in prompt
    assert "## Direct child task details" not in prompt
    assert "## Child terminal notes" not in prompt


@pytest.mark.asyncio
async def test_finalize_parent_awaiting_summary_transitions_to_done():
    """finalize_parent_awaiting_summary should transition EXPANDED_AWAITING_SUMMARY → DONE."""
    from team.task_center import TaskCenter

    class _FakeSessionFactory:
        def __call__(self):
            class _Ctx:
                async def __aenter__(self_inner):
                    return None

                async def __aexit__(self_inner, *a):
                    return False

            return _Ctx()

    tc = TaskCenter(
        session_factory=_FakeSessionFactory(),
        team_run_id="run-1",
        budgets=BudgetConfig(),
        budget_state=BudgetState(),
    )
    parent = Task(
        id="planner-parent",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED_AWAITING_SUMMARY,
        objective="plan",
        depth=0,
        root_id="planner-parent",
    )
    tc.store._tg.tasks[parent.id] = parent

    async def _fake_get_record(task_id):
        return SimpleNamespace(id=task_id, status="expanded_awaiting_summary")

    async def _fake_finalize(parent_id):
        parent.status = TaskStatus.DONE
        return []

    async def _fake_maybe_promote(child_id: str):
        return [], []

    tc.store.get_record = _fake_get_record  # type: ignore[assignment]
    tc.store.finalize_parent_summary = _fake_finalize  # type: ignore[assignment]
    tc.store.maybe_promote_expanded_parent = _fake_maybe_promote  # type: ignore[assignment]

    async def _noop_refresh():
        return tc.store._tg.tasks

    tc.store.refresh_graph = _noop_refresh  # type: ignore[assignment]

    await tc.finalize_parent_awaiting_summary("planner-parent")
    assert parent.status == TaskStatus.DONE


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
async def test_build_query_context_parent_summarizer_terminal_tools():
    task = Task(
        id="summary-task",
        team_run_id="run-1",
        agent_name="parent_summarizer",
        status=TaskStatus.READY,
        objective="summarize parent task",
        fired_by_task_id="planner-parent",
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
        roster={"parent_summarizer": ["parent_summarizer"]},
        team_definition=None,
    )

    ctx = await build_query_context(
        SimpleNamespace(role="parent_summarizer"),
        team_run,
        task,
    )

    assert ctx.tool_metadata["terminal_tools"] == {"submit_task_success"}


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
