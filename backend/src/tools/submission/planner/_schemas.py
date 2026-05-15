"""Planner submission schemas and validation helpers."""

from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, field_validator

from agents import AgentKind, get_definition
from task_center import (
    PlannedGeneratorTask,
    PlannerSubmission,
    TaskCenterInvariantViolation,
    ordered_generator_tasks,
)
from tools.submission.context import TrialSubmissionContext


class PlanTaskInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str = Field(..., min_length=1)
    agent_name: str = Field(..., min_length=1)
    deps: list[str] = Field(default_factory=list)

    @field_validator("id")
    @classmethod
    def _validate_id(cls, value: str) -> str:
        return validate_nonblank(value, "id")

    @field_validator("agent_name")
    @classmethod
    def _validate_agent_name(cls, value: str) -> str:
        return validate_nonblank(value, "agent_name")

    @field_validator("deps")
    @classmethod
    def _validate_deps(cls, value: list[str]) -> list[str]:
        for dep in value:
            validate_nonblank(dep, "deps")
        return value


class PlannerSubmissionBaseInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    task_specification: str = Field(..., min_length=1)
    evaluation_criteria: list[str] = Field(..., min_length=1)
    tasks: list[PlanTaskInput] = Field(..., min_length=1)
    task_specs: dict[str, str] = Field(..., min_length=1)

    @field_validator("task_specification")
    @classmethod
    def _validate_task_specification(cls, value: str) -> str:
        return validate_nonblank(value, "task_specification")

    @field_validator("evaluation_criteria")
    @classmethod
    def _validate_evaluation_criteria(cls, value: list[str]) -> list[str]:
        for criterion in value:
            validate_nonblank(criterion, "evaluation_criteria")
        return value

    @field_validator("task_specs")
    @classmethod
    def _validate_task_specs(cls, value: dict[str, str]) -> dict[str, str]:
        for key, spec in value.items():
            validate_nonblank(key, "task_specs key")
            validate_nonblank(spec, f"task spec for {key!r}")
        return value


def validate_nonblank(value: str, field_name: str) -> str:
    if not value or value.isspace():
        raise ValueError(f"{field_name} must be nonblank")
    return value


def _is_generator_capable_agent(agent_name: str) -> bool:
    """Gate for ``agent_name`` values a planner may submit as a generator task.

    Defense-in-depth: requires BOTH a positive ``dispatchable_by_planner`` flag
    on the registered definition AND an executor / verifier ``agent_kind``.
    The literal-name fast-path was deleted in Stage 6 of the agent-kind plan;
    the prior shortcut masked entry_executor's structurally-executor agent_kind
    and allowed planners to submit it.
    """
    definition = get_definition(agent_name)
    if definition is None:
        return False
    return definition.dispatchable_by_planner and definition.agent_kind in {
        AgentKind.EXECUTOR,
        AgentKind.VERIFIER,
    }


def build_planner_submission(
    *,
    submission_context: TrialSubmissionContext,
    kind: Literal["full", "partial"],
    task_specification: str,
    evaluation_criteria: list[str],
    tasks: list[PlanTaskInput],
    task_specs: dict[str, str],
    continuation_goal: str | None,
) -> tuple[PlannerSubmission | None, str | None]:
    task_id = submission_context.task_center_task_id
    if task_id != submission_context.attempt.planner_task_id:
        return None, "Current TaskCenter task is not this attempt's planner task."

    seen: set[str] = set()
    for task in tasks:
        if task.id in seen:
            return None, f"Plan contains duplicate task id {task.id!r}."
        seen.add(task.id)
        if not _is_generator_capable_agent(task.agent_name):
            return None, f"Unknown generator agent {task.agent_name!r}."

    task_ids = {task.id for task in tasks}
    spec_ids = set(task_specs)
    missing_specs = sorted(task_ids - spec_ids)
    if missing_specs:
        return None, f"Missing task_specs for {', '.join(missing_specs)}."
    extra_specs = sorted(spec_ids - task_ids)
    if extra_specs:
        return None, f"task_specs contains unknown ids {', '.join(extra_specs)}."

    for task_id_for_spec, spec in task_specs.items():
        if not spec or spec.isspace():
            return None, f"Task spec for {task_id_for_spec!r} is blank."

    planned = tuple(
        PlannedGeneratorTask(
            local_id=task.id,
            agent_name=task.agent_name,
            deps=tuple(task.deps),
            task_spec=task_specs[task.id],
        )
        for task in tasks
    )
    try:
        planned = ordered_generator_tasks(planned)
    except TaskCenterInvariantViolation as exc:
        message = str(exc)
        if "unknown deps" in message:
            return None, message
        if "dependency cycle" in message:
            return None, "Plan contains a dependency cycle."
        return None, message

    return (
        PlannerSubmission(
            attempt_id=submission_context.attempt.id,
            planner_task_id=task_id,
            kind=kind,
            task_specification=task_specification,
            evaluation_criteria=tuple(evaluation_criteria),
            tasks=planned,
            continuation_goal=continuation_goal,
            summary=f"Accepted {kind} planner submission.",
        ),
        None,
    )
