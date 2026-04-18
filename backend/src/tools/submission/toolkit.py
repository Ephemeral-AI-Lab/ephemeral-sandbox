"""Submission toolkit — terminal actions for team-mode agents.

Tools write structured data to ``context.metadata``; the executor reads
it after the runner returns.

Tool surface:
  - submit_plan          (terminal)     — commit child plan to task model
  - submit_replan        (terminal)     — commit corrective replan to task model
  - submit_task_summary  (terminal)     — submit success/fail outcome
"""

from __future__ import annotations

import logging
import re
import time
import uuid
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field, field_validator

from agents.registry import get_definition
from team.planning.validation import validate_plan
from tools.core.base import BaseTool, BaseToolkit, ToolExecutionContext, ToolResult

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


async def _post_submission_note(
    context: ToolExecutionContext,
    *,
    content: str,
    scope_paths: list[str] | None = None,
    tags: list[str] | None = None,
) -> None:
    tc = context.metadata.get("task_center")
    if tc is None:
        return
    from team.models import Note

    await tc.notes.post(
        Note(
            id=str(uuid.uuid4()),
            task_id=context.metadata.get("work_item_id", ""),
            agent_name=context.metadata.get("agent_name", ""),
            content=content,
            timestamp=time.time(),
            paths=list(scope_paths or context.metadata.get("write_scope") or []),
            tags=tags or [],
        )
    )


def _resolve_agent_name(agent_value: str, roster: dict[str, list[str]]) -> str:
    candidate = agent_value.strip()
    if not candidate:
        return candidate
    if get_definition(candidate) is not None:
        return candidate
    role_matches = roster.get(candidate)
    if role_matches:
        if len(role_matches) > 1:
            logger.warning(
                "Role '%s' resolved to multiple agents (%s); using first: %s",
                candidate,
                len(role_matches),
                role_matches[0],
            )
        return str(role_matches[0])
    return candidate


def _roster_from_context(context: ToolExecutionContext) -> dict[str, list[str]]:
    roster = context.metadata.get("roster")
    if not isinstance(roster, dict):
        return {}
    return {
        str(role): [str(agent_name) for agent_name in agent_names if isinstance(agent_name, str)]
        for role, agent_names in roster.items()
        if isinstance(agent_names, list)
    }


def _metadata_int(context: ToolExecutionContext, key: str, default: int = 0) -> int:
    value = context.metadata.get(key)
    if value is None or value == "":
        return default
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


async def _known_external_dep_ids(context: ToolExecutionContext) -> set[str] | None:
    known = context.metadata.get("known_external_dep_ids")
    if isinstance(known, set):
        return {str(item) for item in known}
    if isinstance(known, list):
        return {str(item) for item in known}
    tc = context.metadata.get("task_center")
    store = getattr(tc, "store", None) if tc is not None else None
    if store is None or not hasattr(store, "get_task_ids"):
        return None
    return {str(item) for item in await store.get_task_ids()}


_SPEC_SECTIONS = ("Goal", "Environment", "Scope", "Context", "Acceptance Criteria")
_SPEC_SECTION_RE = re.compile(
    r"(?im)^\s*(?:\d+[.)]\s*)?"
    r"(Goal|Environment|Scope|Context|Acceptance Criteria)\s*:\s*\S"
)


def _spec_format_errors(spec_text: str) -> list[str]:
    matches = list(_SPEC_SECTION_RE.finditer(spec_text))
    found = [match.group(1) for match in matches]
    missing = [section for section in _SPEC_SECTIONS if section not in found]
    errors: list[str] = []
    if missing:
        errors.append("missing spec section(s): " + ", ".join(missing))

    positions = {match.group(1): match.start() for match in matches}
    previous = -1
    for section in _SPEC_SECTIONS:
        current = positions.get(section)
        if current is None:
            continue
        if current <= previous:
            errors.append("spec sections must appear in order: " + " -> ".join(_SPEC_SECTIONS))
            break
        previous = current
    return errors


def _note_budget_issues(
    tasks: list[dict[str, Any]],
    *,
    max_note_bytes: int | None,
) -> list[str]:
    if not max_note_bytes or max_note_bytes <= 0:
        return []
    issues: list[str] = []
    for item in tasks:
        task_id = str(item.get("id") or "<unknown>")
        objective = str(item.get("objective") or "")
        size = len(objective.encode("utf-8"))
        if size > max_note_bytes:
            issues.append(
                f"task '{task_id}' is {size} bytes, exceeds max_note_bytes={max_note_bytes}"
            )
    return issues


# ---------------------------------------------------------------------------
# SubmitTaskSummaryTool — terminal for non-planner agents
# ---------------------------------------------------------------------------


class SubmitTaskSummaryInput(BaseModel):
    type: Literal["success", "fail"] = Field(
        ...,
        description=(
            "Outcome type. 'success' = task completed successfully. "
            "'fail' = task cannot be completed (triggers replan)."
        ),
    )
    content: str = Field(
        ...,
        min_length=1,
        description=(
            "Summary of work done. For success: describe what was accomplished "
            "and files changed. For fail: describe what went wrong and why "
            "a replan is needed."
        ),
    )


class SubmitTaskSummaryOutput(BaseModel):
    task_id: str = Field(..., description="Runtime-stamped task id.")
    agent_name: str = Field(..., description="Runtime-stamped submitting agent name.")
    type: Literal["success", "fail"] = Field(..., description="Submitted outcome type.")
    content: str = Field(..., description="Submitted task outcome content.")


class SubmitTaskSummaryTool(BaseTool):
    name = "submit_task_summary"
    description = (
        "Submit your task outcome. Call with type='success' when work is done, "
        "or type='fail' when the task cannot be completed and needs replanning. "
        "This is your terminal action — the agent loop ends after this call."
    )
    short_description = "Submit task outcome."
    input_model = SubmitTaskSummaryInput
    output_model = SubmitTaskSummaryOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitTaskSummaryInput)

        # Write to metadata for executor to read after runner returns
        context.metadata["task_summary"] = arguments.content
        context.metadata["task_summary_type"] = arguments.type

        # Audit trail note
        tag = "implementation" if arguments.type == "success" else "warning"
        await _post_submission_note(context, content=arguments.content, tags=[tag])
        payload = SubmitTaskSummaryOutput(
            task_id=str(context.metadata.get("work_item_id") or ""),
            agent_name=str(context.metadata.get("agent_name") or ""),
            type=arguments.type,
            content=arguments.content,
        )
        return ToolResult(output=payload.model_dump_json())


# ---------------------------------------------------------------------------
# Planning tool input models
# ---------------------------------------------------------------------------


class NewTaskSpec(BaseModel):
    """Full spec for a task the agent is creating."""

    model_config = ConfigDict(extra="forbid")

    id: str = Field(..., description="Unique ID for the new task")
    description: str = Field(
        ...,
        min_length=1,
        description=(
            "Planner-authored short task label, kept under about 10 words. "
            "This is persisted as the task description; put full instructions in spec."
        ),
    )
    name: str = Field(
        ...,
        description="Agent name or role hint (e.g. 'developer', 'validator')",
    )
    spec: str = Field(
        ...,
        description=(
            "Structured task spec — the agent's sole briefing. Must include sections "
            "in order using numbered colon labels, e.g. '1. Goal: ...', "
            "'2. Environment: ...', '3. Scope: ...', '4. Context: ...', "
            "'5. Acceptance Criteria: ...'. Markdown headings like '## Goal' "
            "are not accepted."
        ),
    )
    deps: list[str] = Field(default_factory=list, description="Task IDs this depends on")
    scope_paths: list[str] = Field(
        default_factory=list,
        description="File/dir hints for coordination and note scoping",
    )

    @field_validator("description")
    @classmethod
    def _description_must_not_be_blank(cls, value: str) -> str:
        description = value.strip()
        if not description:
            raise ValueError("description is required")
        return description


class ResolvedTaskOutput(BaseModel):
    id: str = Field(..., description="Task id submitted by the planner.")
    objective: str = Field(..., description="Full structured task objective.")
    agent: str = Field(..., description="Resolved exact agent name.")
    description: str = Field(..., description="Planner-authored short task description.")
    deps: list[str] = Field(default_factory=list, description="Task ids this task depends on.")
    scope_paths: list[str] = Field(default_factory=list, description="Scope paths for this task.")
    parent_id: str | None = Field(
        default=None,
        description="Parent task id when stamped by a replan submission.",
    )


class SubmitPlanInput(BaseModel):
    """Planner submission payload."""

    model_config = ConfigDict(extra="forbid")

    new_tasks: list[NewTaskSpec] = Field(
        default_factory=list,
        description="New child tasks to create",
    )
    output: str | None = Field(
        default=None,
        description="Optional concise rationale or plan summary.",
    )


class SubmitPlanOutput(BaseModel):
    task_id: str = Field(..., description="Runtime-stamped planner task id.")
    agent_name: str = Field(..., description="Runtime-stamped planner agent name.")
    new_tasks: list[ResolvedTaskOutput] = Field(
        default_factory=list,
        description="Accepted child tasks with resolved exact agent names.",
    )
    output: str | None = Field(
        default=None,
        description="Planner-provided optional rationale from the submit_plan input.",
    )


class SubmitReplanInput(BaseModel):
    """Corrective replan submission payload.

    The replanner is the recovery gate for downstream tasks rewired from the
    failed worker. All newly authored corrective tasks are direct children of
    the replanner, so the replanner only reaches DONE after its repairs finish.
    It may cancel stale direct siblings; cascade handles their subtrees and
    dependents.
    """

    model_config = ConfigDict(extra="forbid")

    new_tasks: list[NewTaskSpec] = Field(
        default_factory=list,
        description=(
            "New corrective tasks to create as direct children of the replanner."
        ),
    )
    cancel_ids: list[str] = Field(
        default_factory=list,
        description=(
            "Direct siblings of the replanner to cancel (cascade "
            "propagates to their subtrees and dependents). Never include "
            "the original failed request_replan task."
        ),
    )


class SubmitReplanOutput(BaseModel):
    task_id: str = Field(..., description="Runtime-stamped replanner task id.")
    agent_name: str = Field(..., description="Runtime-stamped replanner agent name.")
    new_tasks: list[ResolvedTaskOutput] = Field(
        default_factory=list,
        description="Accepted corrective child tasks with resolved exact agent names.",
    )
    cancel_ids: list[str] = Field(
        default_factory=list,
        description="Accepted sibling task ids to cancel by cascade.",
    )


# ---------------------------------------------------------------------------
# Shared planning helpers
# ---------------------------------------------------------------------------


def _get_graph(context: ToolExecutionContext) -> dict[str, Any] | None:
    tc = context.metadata.get("task_center")
    graph = getattr(tc, "graph", None) if tc is not None else None
    return graph if isinstance(graph, dict) else None


def _validate_task_specs(
    specs: list[NewTaskSpec],
    *,
    graph_ids: set[str],
    valid_dep_ids: set[str],
    roster: dict[str, list[str]],
) -> list[str]:
    errors: list[str] = []
    new_ids: set[str] = set()
    for spec in specs:
        if spec.id in graph_ids:
            errors.append(f"new task '{spec.id}' collides with existing task")
        if spec.id in new_ids:
            errors.append(f"duplicate new task id '{spec.id}'")
        new_ids.add(spec.id)

        resolved = _resolve_agent_name(spec.name, roster)
        if not resolved:
            errors.append(f"task '{spec.id}': empty agent name")
        elif get_definition(resolved) is None:
            errors.append(f"task '{spec.id}': unknown agent '{resolved}'")
        if not spec.spec:
            errors.append(f"task '{spec.id}': empty spec")
        for error in _spec_format_errors(spec.spec):
            errors.append(f"task '{spec.id}': {error}")
        for dep in spec.deps:
            if dep not in valid_dep_ids:
                errors.append(f"new task '{spec.id}': unknown dep '{dep}'")
    return errors


def _validate_submit_plan_input(
    arguments: SubmitPlanInput,
    context: ToolExecutionContext,
) -> list[str]:
    graph = _get_graph(context)
    graph_ids = set(graph.keys()) if graph is not None else set()
    new_ids = {spec.id for spec in arguments.new_tasks}
    return _validate_task_specs(
        arguments.new_tasks,
        graph_ids=graph_ids,
        valid_dep_ids=graph_ids | new_ids,
        roster=_roster_from_context(context),
    )


def _validate_submit_replan_input(
    arguments: SubmitReplanInput,
    context: ToolExecutionContext,
) -> list[str]:
    from team.models import Plan
    from team.planning.replan_validation import validate_replan_rules

    graph = _get_graph(context)
    current_task_id = str(context.metadata.get("work_item_id") or "")

    result = validate_replan_rules(
        graph=graph,
        replan_task_id=current_task_id,
        cancel_ids=arguments.cancel_ids,
    )
    errors = list(result.errors)
    if graph is None:
        return errors

    new_ids = {spec.id for spec in arguments.new_tasks}
    valid_dep_ids = result.allowed_existing_dep_ids | new_ids
    errors.extend(
        _validate_task_specs(
            arguments.new_tasks,
            graph_ids=set(graph.keys()),
            valid_dep_ids=valid_dep_ids,
            roster=_roster_from_context(context),
        )
    )
    if errors:
        return errors

    resolved_tasks = _resolved_task_payloads(
        arguments.new_tasks,
        roster=_roster_from_context(context),
        parent_id=current_task_id,
        include_parent_id=True,
    )
    try:
        plan = Plan.from_dict({"tasks": resolved_tasks})
    except (TypeError, ValueError) as exc:
        return [f"invalid replan payload: {exc}"]

    max_plan_size = _metadata_int(context, "max_plan_size", 50)
    issues = validate_plan(
        plan,
        max_plan_size=max_plan_size,
        allow_empty=True,
        known_external_deps=result.allowed_existing_dep_ids,
    )

    max_tasks = _metadata_int(context, "max_tasks")
    tasks_used = _metadata_int(context, "tasks_used")
    if max_tasks and tasks_used + len(plan.tasks) > max_tasks:
        issues.append(
            {
                "field": "tasks",
                "msg": (
                    f"replan would exceed max_tasks={max_tasks} "
                    f"(used={tasks_used}, adding={len(plan.tasks)})"
                ),
            }
        )
    max_depth = _metadata_int(context, "max_depth")
    task_depth = _metadata_int(context, "task_depth")
    if max_depth and plan.tasks and (task_depth + 1) > max_depth:
        issues.append(
            {
                "field": "tasks",
                "msg": (
                    f"replan would exceed max_depth={max_depth} from current depth={task_depth}"
                ),
            }
        )
    note_budget_issues = _note_budget_issues(
        resolved_tasks,
        max_note_bytes=_metadata_int(context, "max_note_bytes"),
    )
    issues.extend({"field": "tasks", "msg": msg} for msg in note_budget_issues)
    errors.extend(str(issue.get("msg") or "invalid replan") for issue in issues)
    return errors


def _resolved_task_payloads(
    specs: list[NewTaskSpec],
    *,
    roster: dict[str, list[str]],
    parent_id: str | None = None,
    include_parent_id: bool = False,
) -> list[dict[str, Any]]:
    resolved_tasks: list[dict[str, Any]] = []
    for spec in specs:
        resolved_agent = _resolve_agent_name(spec.name, roster)
        payload: dict[str, Any] = {
            "id": spec.id,
            "objective": spec.spec,
            "agent": resolved_agent,
            "description": spec.description,
            "deps": list(spec.deps),
            "scope_paths": list(spec.scope_paths),
        }
        if include_parent_id:
            payload["parent_id"] = parent_id
        resolved_tasks.append(payload)
    return resolved_tasks


# ---------------------------------------------------------------------------
# SubmitPlanTool — commit child plan (terminal)
# ---------------------------------------------------------------------------


class SubmitPlanTool(BaseTool):
    name = "submit_plan"
    description = (
        "Submit a child plan. Provide new_tasks with id, description, name, spec, deps, and "
        "scope_paths. Optional output may hold a concise rationale. Do not include "
        "task_note, background, parent_id, or other fields. Each spec must use "
        "numbered colon labels in order: 1. Goal, 2. Environment, 3. Scope, "
        "4. Context, 5. Acceptance Criteria."
    )
    short_description = "Submit a child plan."
    input_model = SubmitPlanInput
    output_model = SubmitPlanOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitPlanInput)
        from team.models import Plan

        errors = _validate_submit_plan_input(arguments, context)
        if errors:
            return ToolResult(
                output="Validation failed:\n" + "\n".join(f"- {e}" for e in errors),
                is_error=True,
            )

        resolved_tasks = _resolved_task_payloads(
            arguments.new_tasks,
            roster=_roster_from_context(context),
        )
        try:
            plan = Plan.from_dict({"tasks": resolved_tasks, "rationale": arguments.output})
        except (TypeError, ValueError) as exc:
            return ToolResult(output=f"Error: invalid plan payload: {exc}", is_error=True)

        allow_empty = bool(context.metadata.get("allow_empty_plan"))
        max_plan_size = _metadata_int(context, "max_plan_size", 50)
        known_ext_deps = await _known_external_dep_ids(context)
        issues = validate_plan(
            plan,
            max_plan_size=max_plan_size,
            allow_empty=allow_empty,
            known_external_deps=known_ext_deps,
        )

        max_tasks = _metadata_int(context, "max_tasks")
        tasks_used = _metadata_int(context, "tasks_used")
        if max_tasks and tasks_used + len(plan.tasks) > max_tasks:
            issues.append(
                {
                    "field": "tasks",
                    "msg": (
                        f"plan would exceed max_tasks={max_tasks} "
                        f"(used={tasks_used}, adding={len(plan.tasks)})"
                    ),
                }
            )
        max_depth = _metadata_int(context, "max_depth")
        task_depth = _metadata_int(context, "task_depth")
        if max_depth and plan.tasks and (task_depth + 1) > max_depth:
            issues.append(
                {
                    "field": "tasks",
                    "msg": (
                        f"plan would exceed max_depth={max_depth} from current depth={task_depth}"
                    ),
                }
            )

        note_budget_issues = _note_budget_issues(
            resolved_tasks,
            max_note_bytes=_metadata_int(context, "max_note_bytes"),
        )
        issues.extend({"field": "tasks", "msg": msg} for msg in note_budget_issues)

        if issues:
            message = "; ".join(str(issue.get("msg") or "invalid plan") for issue in issues)
            return ToolResult(output=f"Error: {message}", is_error=True)

        summary = f"Submitted plan with {len(plan.tasks)} task(s)."
        if arguments.output:
            summary += f"\n\n{arguments.output}"
        await _post_submission_note(context, content=summary, tags=["architecture"])

        context.metadata["resolved_plan"] = plan
        context.metadata["plan_is_replan"] = False
        payload = SubmitPlanOutput(
            task_id=str(context.metadata.get("work_item_id") or ""),
            agent_name=str(context.metadata.get("agent_name") or ""),
            new_tasks=[ResolvedTaskOutput.model_validate(item) for item in resolved_tasks],
            output=arguments.output,
        )
        return ToolResult(output=payload.model_dump_json())


# ---------------------------------------------------------------------------
# SubmitReplanTool — commit corrective replan (terminal)
# ---------------------------------------------------------------------------


class SubmitReplanTool(BaseTool):
    name = "submit_replan"
    description = (
        "Submit a corrective replan. Provide new_tasks for repair work owned by "
        "the replanner, and cancel_ids for stale direct siblings whose subtrees "
        "should be cancelled by cascade. Never cancel the original failed "
        "request_replan task. Do not include task_note, output, background, "
        "parent_id, or other fields. Each new task must include a short "
        "planner-authored description. Each new task spec must use "
        "numbered colon labels in order: 1. Goal, 2. Environment, 3. Scope, "
        "4. Context, 5. Acceptance Criteria."
    )
    short_description = "Submit a corrective replan."
    input_model = SubmitReplanInput
    output_model = SubmitReplanOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitReplanInput)
        from team.models import ReplanPlan

        errors = _validate_submit_replan_input(arguments, context)
        if errors:
            return ToolResult(
                output="Validation failed:\n" + "\n".join(f"- {e}" for e in errors),
                is_error=True,
            )

        current_task_id = str(context.metadata.get("work_item_id") or "")
        roster = _roster_from_context(context)
        resolved_tasks = _resolved_task_payloads(
            arguments.new_tasks,
            roster=roster,
            parent_id=current_task_id,
            include_parent_id=True,
        )
        try:
            replan = ReplanPlan.from_dict(
                {
                    "add_tasks": resolved_tasks,
                    "cancel_ids": arguments.cancel_ids,
                }
            )
        except (TypeError, ValueError) as exc:
            return ToolResult(output=f"Error: invalid replan payload: {exc}", is_error=True)

        note_content = (
            f"Replanner submitted replan: {len(replan.add_tasks)} new task(s), "
            f"{len(replan.cancel_ids)} cancelled."
        )
        await _post_submission_note(context, content=note_content, tags=["refactor"])

        context.metadata["resolved_plan"] = replan
        context.metadata["plan_is_replan"] = True
        payload = SubmitReplanOutput(
            task_id=current_task_id,
            agent_name=str(context.metadata.get("agent_name") or ""),
            new_tasks=[ResolvedTaskOutput.model_validate(item) for item in resolved_tasks],
            cancel_ids=list(arguments.cancel_ids),
        )
        return ToolResult(output=payload.model_dump_json())


# ---------------------------------------------------------------------------
# SubmissionToolkit
# ---------------------------------------------------------------------------


class SubmissionToolkit(BaseToolkit):
    """Terminal submission tools for team-mode agents.

    Registered in the main tool loop. The query loop's ``terminal_tools``
    set (populated from TeamDefinition) causes the loop to exit when one
    of these tools is called.
    """

    @classmethod
    def from_context(cls, ctx: object) -> SubmissionToolkit:
        return cls(
            name="submission",
            description="Terminal submission tools for team-mode agents.",
            tools=[
                SubmitTaskSummaryTool(),
                SubmitPlanTool(),
                SubmitReplanTool(),
            ],
        )
