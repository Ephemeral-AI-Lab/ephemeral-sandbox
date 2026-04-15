"""Unit tests for team.models — core dataclasses and enums."""

from __future__ import annotations

import pytest

from team.models import (
    AgentResult,
    BudgetConfig,
    BudgetState,
    Note,
    Plan,
    ReplanPlan,
    ReplanRequest,
    RetryRequest,
    SubmittedSummary,
    Task,
    TaskDefinition,
    TaskStatus,
    TERMINAL_STATUSES,
)
from config.defaults import (
    DEFAULT_MAX_TASKS,
    DEFAULT_MAX_DEPTH,
    DEFAULT_MAX_PLAN_SIZE,
    DEFAULT_MAX_RETRIES_PER_ITEM,
    DEFAULT_MAX_REPLANS_PER_RUN,
    DEFAULT_MAX_NOTE_BYTES,
    DEFAULT_MAX_TOTAL_NOTE_BYTES,
)


# ---------------------------------------------------------------------------
# Note
# ---------------------------------------------------------------------------


def test_note_creation_with_all_fields():
    note = Note(
        id="n1",
        task_id="task-1",
        agent_name="developer",
        content="some output",
        timestamp=1000.0,
        paths=["src/auth/session.py"],
        parent_note_id="n0",
    )
    assert note.id == "n1"
    assert note.task_id == "task-1"
    assert note.agent_name == "developer"
    assert note.content == "some output"
    assert note.timestamp == 1000.0
    assert note.paths == ["src/auth/session.py"]
    assert note.parent_note_id == "n0"


def test_note_defaults():
    note = Note(id="n2", task_id="t", agent_name="a", content="c", timestamp=0.0)
    assert note.paths == []
    assert note.parent_note_id is None


# ---------------------------------------------------------------------------
# TaskDefinition
# ---------------------------------------------------------------------------


def test_taskspec_creation_with_required_fields():
    spec = TaskDefinition(id="t1", objective="do work", agent="developer")
    assert spec.id == "t1"
    assert spec.objective == "do work"
    assert spec.agent == "developer"


def test_taskspec_defaults():
    spec = TaskDefinition(id="t1", objective="do work", agent="developer")
    assert spec.deps == []
    assert spec.scope_paths == []
    assert spec.cascade_policy == "cancel"


def test_taskspec_with_all_fields():
    spec = TaskDefinition(
        id="t2",
        objective="verify",
        agent="validator",
        deps=["t1"],
        scope_paths=["src/auth/"],
        cascade_policy="propagate",
    )
    assert spec.deps == ["t1"]
    assert spec.scope_paths == ["src/auth/"]
    assert spec.cascade_policy == "propagate"


def test_plan_from_dict_reports_invalid_task_index_for_missing_id():
    with pytest.raises(ValueError, match=r"tasks\[1\]: TaskDefinition requires a non-empty 'id'"):
        Plan.from_dict(
            {
                "tasks": [
                    {"id": "ok", "objective": "do work", "agent": "developer"},
                    {"objective": "missing id", "agent": "developer"},
                ]
            }
        )


def test_replan_from_dict_reports_non_object_index():
    with pytest.raises(ValueError, match=r"add_tasks\[1\] must be an object"):
        ReplanPlan.from_dict(
            {
                "add_tasks": [
                    {"id": "ok", "objective": "do work", "agent": "developer"},
                    "not-a-task",
                ]
            }
        )


def test_plan_from_dict_rejects_legacy_task_field():
    with pytest.raises(
        ValueError,
        match=r"tasks\[0\]: TaskDefinition 't1' uses legacy 'task'; use 'objective'",
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
        agent_name="developer",
        status=TaskStatus.PENDING,
        objective="implement feature",
    )
    assert task.id == "x"
    assert task.team_run_id == "run-1"
    assert task.agent_name == "developer"
    assert task.status == TaskStatus.PENDING
    assert task.objective == "implement feature"


def test_task_defaults():
    task = Task(
        id="x",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.PENDING,
        objective="do it",
    )
    assert task.deps == []
    assert task.scope_paths == []
    assert task.cascade_policy == "cancel"
    assert task.parent_id is None
    assert task.root_id == ""
    assert task.depth == 0
    assert task.retry_count == 0
    assert task.max_retries == 2
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
                "objective": "implement login",
                "agent": "developer",
                "deps": [],
                "scope_paths": ["src/auth/"],
                "cascade_policy": "cancel",
            },
            {
                "id": "t2",
                "objective": "verify login",
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
    assert t1.objective == "implement login"
    assert t1.agent == "developer"
    assert t1.deps == []
    assert t1.scope_paths == ["src/auth/"]
    assert t1.cascade_policy == "cancel"

    t2 = plan.tasks[1]
    assert t2.id == "t2"
    assert t2.deps == ["t1"]
    assert t2.cascade_policy == "cancel"  # default when omitted


def test_plan_from_dict_empty_tasks():
    plan = Plan.from_dict({"tasks": []})
    assert plan.tasks == []
    assert plan.rationale is None


def test_plan_from_dict_missing_tasks_key():
    plan = Plan.from_dict({})
    assert plan.tasks == []


def test_plan_from_dict_numeric_ids_coerced_to_str():
    data = {"tasks": [{"id": 42, "objective": "do it", "agent": "developer"}]}
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
                "objective": "fix the bug",
                "agent": "developer",
                "deps": [],
                "scope_paths": ["src/"],
            }
        ],
        "cancel_ids": ["old-task-1", "old-task-2"],
    }
    replan = ReplanPlan.from_dict(data)
    assert len(replan.add_tasks) == 1
    assert replan.add_tasks[0].id == "fix1"
    assert replan.add_tasks[0].objective == "fix the bug"
    assert replan.cancel_ids == ["old-task-1", "old-task-2"]


def test_replan_plan_from_dict_empty():
    replan = ReplanPlan.from_dict({})
    assert replan.add_tasks == []
    assert replan.cancel_ids == []


def test_replan_plan_from_dict_cascade_policy_default():
    data = {"add_tasks": [{"id": "x", "objective": "t", "agent": "developer"}]}
    replan = ReplanPlan.from_dict(data)
    assert replan.add_tasks[0].cascade_policy == "cancel"


# ---------------------------------------------------------------------------
# BudgetConfig
# ---------------------------------------------------------------------------


def test_budget_config_defaults():
    bc = BudgetConfig()
    assert bc.max_tasks == DEFAULT_MAX_TASKS
    assert bc.max_depth == DEFAULT_MAX_DEPTH
    assert bc.max_plan_size == DEFAULT_MAX_PLAN_SIZE
    assert bc.max_retries_per_item == DEFAULT_MAX_RETRIES_PER_ITEM
    assert bc.max_replans_per_run == DEFAULT_MAX_REPLANS_PER_RUN
    assert bc.max_note_bytes == DEFAULT_MAX_NOTE_BYTES
    assert bc.max_total_note_bytes == DEFAULT_MAX_TOTAL_NOTE_BYTES


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
    assert bs.note_bytes_used == 0
    assert bs.replans_used == 0


# ---------------------------------------------------------------------------
# Submission types
# ---------------------------------------------------------------------------


def test_submitted_summary_creation():
    s = SubmittedSummary(summary="all tests passed")
    assert s.summary == "all tests passed"
    assert s.submission_kind == "summary"


def test_retry_request_creation():
    r = RetryRequest(reason="flaky test")
    assert r.reason == "flaky test"
    assert r.submission_kind == "retry"


def test_replan_request_creation_with_suggestion():
    r = ReplanRequest(reason="scope too broad", suggestion="split into 3 tasks")
    assert r.reason == "scope too broad"
    assert r.suggestion == "split into 3 tasks"
    assert r.submission_kind == "replan"


def test_replan_request_no_suggestion():
    r = ReplanRequest(reason="needs rework")
    assert r.suggestion is None


# ---------------------------------------------------------------------------
# AgentResult
# ---------------------------------------------------------------------------


def test_agent_result_with_summary_only():
    result = AgentResult(summary="done")
    assert result.summary == "done"
    assert result.submitted_plan is None
    assert result.submitted_replan is None


def test_agent_result_with_plan():
    plan = Plan(tasks=[])
    result = AgentResult(summary="", submitted_plan=plan)
    assert result.submitted_plan is plan


def test_agent_result_with_replan():
    replan = ReplanPlan()
    result = AgentResult(summary="", submitted_replan=replan)
    assert result.submitted_replan is replan
