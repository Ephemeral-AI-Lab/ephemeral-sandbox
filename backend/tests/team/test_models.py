"""Unit tests for team.core.models — core dataclasses and enums."""

from __future__ import annotations

import pytest

from team.core.models import (
    BudgetConfig,
    BudgetState,
    Note,
    Plan,
    ReplanPlan,
    SubmittedSummary,
    Task,
    TaskDefinition,
    TaskSpec,
    TaskStatus,
    TaskStatusUpdate,
    TERMINAL_STATUSES,
)
from config.defaults import (
    DEFAULT_MAX_TASKS,
    DEFAULT_MAX_DEPTH,
    DEFAULT_MAX_PLAN_SIZE,
    DEFAULT_MAX_REPLANS_PER_RUN,
)


def _spec(goal: str = "do work") -> dict[str, str]:
    return {
        "goal": goal,
        "detail": f"Detail for {goal}",
        "acceptance_criteria": f"Acceptance for {goal}",
    }


# ---------------------------------------------------------------------------
# Note
# ---------------------------------------------------------------------------


def test_note_creation_with_all_fields():
    note = Note(
        id="n1",
        agent_name="developer",
        content="some output",
        timestamp=1000.0,
        paths=["src/auth/session.py"],
    )
    assert note.id == "n1"
    assert note.agent_name == "developer"
    assert note.content == "some output"
    assert note.timestamp == 1000.0
    assert note.paths == ["src/auth/session.py"]


def test_note_defaults():
    note = Note(id="n2", agent_name="a", content="c", timestamp=0.0)
    assert note.paths == []
    assert not hasattr(note, "task_id")
    assert not hasattr(note, "tags")
    assert not hasattr(note, "parent_note_id")


# ---------------------------------------------------------------------------
# TaskDefinition
# ---------------------------------------------------------------------------


def test_taskspec_creation_with_required_fields():
    spec = TaskSpec(goal="do work", detail="own src/app.py", acceptance_criteria="run tests")
    assert spec.goal == "do work"
    assert spec.detail == "own src/app.py"
    assert spec.acceptance_criteria == "run tests"


def test_task_definition_creation_with_required_fields():
    task_def = TaskDefinition(id="t1", spec=_spec("do work"), agent="developer")
    assert task_def.id == "t1"
    assert task_def.spec.goal == "do work"
    assert task_def.agent == "developer"


def test_taskspec_defaults():
    spec = TaskDefinition(id="t1", spec=_spec("do work"), agent="developer")
    assert spec.deps == []
    assert spec.scope_paths == []


def test_taskspec_with_all_fields():
    spec = TaskDefinition(
        id="t2",
        spec=_spec("verify"),
        agent="validator",
        deps=["t1"],
        scope_paths=["src/auth/"],
    )
    assert spec.deps == ["t1"]
    assert spec.scope_paths == ["src/auth/"]


def test_plan_from_dict_reports_invalid_task_index_for_missing_id():
    with pytest.raises(ValueError, match=r"tasks\[1\]: TaskDefinition requires a non-empty 'id'"):
        Plan.from_dict(
            {
                "tasks": [
                    {"id": "ok", "spec": _spec("do work"), "agent": "developer"},
                    {"spec": _spec("missing id"), "agent": "developer"},
                ]
            }
        )


def test_replan_from_dict_reports_non_object_index():
    with pytest.raises(ValueError, match=r"add_tasks\[1\] must be an object"):
        ReplanPlan.from_dict(
            {
                "add_tasks": [
                    {"id": "ok", "spec": _spec("do work"), "agent": "developer"},
                    "not-a-task",
                ]
            }
        )


def test_plan_from_dict_requires_spec():
    with pytest.raises(
        ValueError,
        match=r"tasks\[0\]: TaskDefinition 't1' requires a non-empty 'spec'",
    ):
        Plan.from_dict(
            {
                "tasks": [
                    {"id": "t1", "task": "do work", "agent": "developer"},
                ]
            }
        )


# ---------------------------------------------------------------------------
# Task
# ---------------------------------------------------------------------------


def test_task_creation_with_required_fields():
    task = Task(
        id="x",
        team_run_id="run-1",
        definition=TaskDefinition(id="x", spec=_spec("implement feature"), agent="developer"),
        status=TaskStatus.PENDING,
    )
    assert task.id == "x"
    assert task.team_run_id == "run-1"
    assert task.definition.agent == "developer"
    assert task.status == TaskStatus.PENDING
    assert task.definition.spec.goal == "implement feature"


def test_task_defaults():
    task = Task(
        id="x",
        team_run_id="run-1",
        definition=TaskDefinition(id="x", spec=_spec("do it"), agent="developer"),
        status=TaskStatus.PENDING,
    )
    assert task.definition.deps == []
    assert task.definition.scope_paths == []
    assert task.parent_id is None
    assert task.root_id == ""
    assert task.depth == 0
    assert task.agent_run_id is None
    assert task.started_at is None
    assert task.finished_at is None
    assert task.failure_reason is None
    assert task.created_at is not None


# ---------------------------------------------------------------------------
# TaskStatus enum
# ---------------------------------------------------------------------------


def test_task_status_values():
    assert TaskStatus.PENDING == "pending"
    assert TaskStatus.READY == "ready"
    assert TaskStatus.RUNNING == "running"
    assert TaskStatus.DONE == "done"
    assert TaskStatus.FAILED == "failed"
    assert TaskStatus.CANCELLED == "cancelled"


def test_terminal_statuses_contains_expected_values():
    assert TaskStatus.DONE in TERMINAL_STATUSES
    assert TaskStatus.FAILED in TERMINAL_STATUSES
    assert TaskStatus.CANCELLED in TERMINAL_STATUSES


def test_task_status_of_accepts_enum_and_string_inputs():
    assert TaskStatus.of(TaskStatus.READY) is TaskStatus.READY
    assert TaskStatus.of("ready") is TaskStatus.READY


def test_task_status_of_falls_back_to_default_for_unknown_values():
    assert TaskStatus.of("mystery") is TaskStatus.PENDING
    assert TaskStatus.of("mystery", default=TaskStatus.RUNNING) is TaskStatus.RUNNING
    assert TaskStatus.of(None, default=TaskStatus.FAILED) is TaskStatus.FAILED


def test_terminal_statuses_does_not_contain_non_terminal():
    assert TaskStatus.PENDING not in TERMINAL_STATUSES
    assert TaskStatus.READY not in TERMINAL_STATUSES
    assert TaskStatus.RUNNING not in TERMINAL_STATUSES


# ---------------------------------------------------------------------------
# Plan.from_dict()
# ---------------------------------------------------------------------------


def test_plan_from_dict_round_trip():
    data = {
        "tasks": [
            {
                "id": "t1",
                "spec": _spec("implement login"),
                "agent": "developer",
                "deps": [],
                "scope_paths": ["src/auth/"],
            },
            {
                "id": "t2",
                "spec": _spec("verify login"),
                "agent": "validator",
                "deps": ["t1"],
                "scope_paths": [],
            },
        ],
        "rationale": "auth feature",
    }
    plan = Plan.from_dict(data)
    assert len(plan.tasks) == 2
    assert plan.rationale == "auth feature"

    t1 = plan.tasks[0]
    assert t1.id == "t1"
    assert t1.spec.goal == "implement login"
    assert t1.agent == "developer"
    assert t1.deps == []
    assert t1.scope_paths == ["src/auth/"]

    t2 = plan.tasks[1]
    assert t2.id == "t2"
    assert t2.deps == ["t1"]


def test_plan_from_dict_empty_tasks():
    plan = Plan.from_dict({"tasks": []})
    assert plan.tasks == []
    assert plan.rationale is None


def test_plan_from_dict_missing_tasks_key():
    plan = Plan.from_dict({})
    assert plan.tasks == []


def test_plan_from_dict_numeric_ids_coerced_to_str():
    data = {"tasks": [{"id": 42, "spec": _spec("do it"), "agent": "developer"}]}
    plan = Plan.from_dict(data)
    assert plan.tasks[0].id == "42"


# ---------------------------------------------------------------------------
# ReplanPlan.from_dict()
# ---------------------------------------------------------------------------


def test_replan_plan_from_dict_round_trip():
    data = {
        "add_tasks": [
            {
                "id": "fix1",
                "spec": _spec("fix the bug"),
                "agent": "developer",
                "deps": [],
                "scope_paths": ["src/"],
                "parent_id": "parent-task",
            }
        ],
        "cancel_ids": ["old-task-1", "old-task-2"],
    }
    replan = ReplanPlan.from_dict(data)
    assert len(replan.add_tasks) == 1
    assert replan.add_tasks[0].id == "fix1"
    assert replan.add_tasks[0].spec.goal == "fix the bug"
    assert replan.cancel_ids == ["old-task-1", "old-task-2"]


def test_replan_plan_from_dict_empty():
    replan = ReplanPlan.from_dict({})
    assert replan.add_tasks == []
    assert replan.cancel_ids == []


def test_replan_plan_from_dict_defaults():
    data = {"add_tasks": [{"id": "x", "spec": _spec("t"), "agent": "developer"}]}
    replan = ReplanPlan.from_dict(data)
    assert replan.add_tasks[0].deps == []


# ---------------------------------------------------------------------------
# BudgetConfig
# ---------------------------------------------------------------------------


def test_budget_config_defaults():
    bc = BudgetConfig()
    assert bc.max_tasks == DEFAULT_MAX_TASKS
    assert bc.max_depth == DEFAULT_MAX_DEPTH
    assert bc.max_plan_size == DEFAULT_MAX_PLAN_SIZE
    assert bc.max_replans_per_run == DEFAULT_MAX_REPLANS_PER_RUN


def test_budget_config_matches_known_values():
    bc = BudgetConfig()
    assert bc.max_tasks == 50
    assert bc.max_plan_size == 50


def test_budget_config_override():
    bc = BudgetConfig(max_tasks=10, max_depth=2)
    assert bc.max_tasks == 10
    assert bc.max_depth == 2


# ---------------------------------------------------------------------------
# BudgetState
# ---------------------------------------------------------------------------


def test_budget_state_initial_values():
    bs = BudgetState()
    assert bs.tasks_used == 0
    assert bs.replans_used == 0


# ---------------------------------------------------------------------------
# Submission types
# ---------------------------------------------------------------------------


def test_submitted_summary_creation():
    s = SubmittedSummary(summary="all tests passed")
    assert s.summary == "all tests passed"
    assert s.artifact is None


# ---------------------------------------------------------------------------
# TaskStatusUpdate
# ---------------------------------------------------------------------------


def test_task_status_update_done_carries_summary_only():
    update = TaskStatusUpdate(task_id="t1", status=TaskStatus.DONE, summary="all green")
    assert update.status is TaskStatus.DONE
    assert update.summary == "all green"
    assert update.plan is None and update.replan is None


def test_task_status_update_expanded_with_plan():
    plan = Plan(tasks=[])
    update = TaskStatusUpdate(task_id="t1", status=TaskStatus.EXPANDED, plan=plan)
    assert update.plan is plan
    assert update.replan is None


def test_task_status_update_expanded_with_replan():
    replan = ReplanPlan()
    update = TaskStatusUpdate(task_id="t1", status=TaskStatus.EXPANDED, replan=replan)
    assert update.replan is replan
    assert update.plan is None


def test_task_status_update_request_replan_carries_reason():
    update = TaskStatusUpdate(
        task_id="t1", status=TaskStatus.REQUEST_REPLAN, summary="owner mismatch"
    )
    assert update.status is TaskStatus.REQUEST_REPLAN
    assert update.summary == "owner mismatch"


def test_task_status_update_failed_carries_reason():
    update = TaskStatusUpdate(
        task_id="t1", status=TaskStatus.FAILED, summary="runner_exception: x"
    )
    assert update.status is TaskStatus.FAILED
    assert "runner_exception" in update.summary
