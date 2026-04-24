"""Submission tools — terminal actions for team-mode agents.

Tools write structured data to ``context.metadata``; the executor reads
it after the runner returns.

Tool surface:
  - submit_plan          (terminal)     — commit child plan to task model
  - submit_replan        (terminal)     — commit corrective replan to task model
  - submit_task_success  (terminal)     — report successful completion with a summary
  - request_replan       (terminal)     — request a replan with a reason
"""

from __future__ import annotations

import logging
import re
from typing import Any

from pydantic import BaseModel, ConfigDict, Field, field_validator

from agents.registry import get_definition
from team.core.models import TaskSpec
from team.planning.validation import validate_plan
from tools.core.base import BaseTool, ToolExecutionContext, ToolResult

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


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


_UNRESOLVED_BLOCKER_RE = re.compile(
    r"(?i)\bClassification\s*:\s*unresolved_blocker\b"
)
_DIAGNOSTICS_DECISION_RE = re.compile(
    r"(?i)\bDiagnostics decision\s*:\s*"
    r"(?:trivial_direct_replan|deep_diagnostics)\b"
)

def _replan_spec_contract_errors(spec: TaskSpec) -> list[str]:
    if (
        _UNRESOLVED_BLOCKER_RE.search(spec.detail)
        and not _DIAGNOSTICS_DECISION_RE.search(spec.detail)
    ):
        return [
            "unresolved_blocker requires Diagnostics decision: "
            "trivial_direct_replan or deep_diagnostics"
        ]
    return []


def _replan_agent_target_issues(tasks: list[Any]) -> list[dict[str, str]]:
    """Restrict replanner-authored corrective children to terminal work lanes."""
    issues: list[dict[str, str]] = []
    allowed_roles = {"developer", "reviewer"}
    for idx, item in enumerate(tasks):
        agent = str(getattr(item, "agent", "") or "")
        agent_def = get_definition(agent)
        if agent_def is None or agent_def.role in allowed_roles:
            continue
        issues.append(
            {
                "field": f"tasks[{idx}].agent",
                "msg": (
                    "submit_replan can only create developer or validator tasks; "
                    f"task '{item.id}' resolved to {agent_def.role!r} agent '{agent}'"
                ),
            }
        )
    return issues


# ---------------------------------------------------------------------------
# SubmitTaskSuccessTool / RequestReplanTool — terminal for non-planner agents
# ---------------------------------------------------------------------------


class SubmitTaskSuccessInput(BaseModel):
    summary: str = Field(
        ...,
        min_length=1,
        description=(
            "Evidence-rich success summary for the task details. Developers: "
            "state the concrete API or behavior delta, verification commands and outcomes "
            "observed after the final edit, and known gaps or deferred items. Validators: "
            "list each acceptance criterion with pass/fail plus the command, probe, "
            "exit code, or key assertion used. Do not submit placeholders such as "
            "'task completed', 'all checks passed', or a filename-only list."
        ),
    )

    @field_validator("summary")
    @classmethod
    def _summary_must_not_be_blank(cls, value: str) -> str:
        if not value.strip():
            raise ValueError("summary must contain non-whitespace text")
        return value


class SubmitTaskSuccessOutput(BaseModel):
    task_id: str = Field(..., description="Runtime-stamped task id.")
    agent_name: str = Field(..., description="Runtime-stamped submitting agent name.")
    summary: str = Field(..., description="Submitted success summary.")


class SubmitTaskSuccessTool(BaseTool):
    name = "submit_task_success"
    description = (
        "Submits a terminal success summary for the current task and ends "
        "the agent loop."
    )
    short_description = "Report task success."
    input_model = SubmitTaskSuccessInput
    output_model = SubmitTaskSuccessOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitTaskSuccessInput)

        context.metadata["task_summary"] = arguments.summary
        context.metadata["task_summary_type"] = "success"

        payload = SubmitTaskSuccessOutput(
            task_id=str(context.metadata.get("work_item_id") or ""),
            agent_name=str(context.metadata.get("agent_name") or ""),
            summary=arguments.summary,
        )
        return ToolResult(output=payload.model_dump_json())


class RequestReplanInput(BaseModel):
    reason: str = Field(
        ...,
        min_length=1,
        description=(
            "Evidence-rich replan request for the task details. Start with "
            "a replan trigger, exactly one of scope_expansion, "
            "wrong_owner_or_role, or unresolved_blocker; then include blocking "
            "evidence, failing command or tool result, affected paths or owners, "
            "and why a different owner, scope, sequence, or budget is needed."
        ),
    )

    @field_validator("reason")
    @classmethod
    def _reason_must_not_be_blank(cls, value: str) -> str:
        if not value.strip():
            raise ValueError("reason must contain non-whitespace text")
        return value


class RequestReplanOutput(BaseModel):
    task_id: str = Field(..., description="Runtime-stamped task id.")
    agent_name: str = Field(..., description="Runtime-stamped submitting agent name.")
    reason: str = Field(..., description="Submitted replan reason.")


class RequestReplanTool(BaseTool):
    name = "request_replan"
    description = (
        "Submits a terminal replan request for the current task and ends "
        "the agent loop."
    )
    short_description = "Request a replan."
    input_model = RequestReplanInput
    output_model = RequestReplanOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, RequestReplanInput)

        context.metadata["task_summary"] = arguments.reason
        context.metadata["task_summary_type"] = "request_replan"

        payload = RequestReplanOutput(
            task_id=str(context.metadata.get("work_item_id") or ""),
            agent_name=str(context.metadata.get("agent_name") or ""),
            reason=arguments.reason,
        )
        return ToolResult(output=payload.model_dump_json())


# ---------------------------------------------------------------------------
# Planning tool input models
# ---------------------------------------------------------------------------


class NewTaskDefinition(BaseModel):
    """Full definition for a task the agent is creating."""

    model_config = ConfigDict(extra="forbid")

    id: str = Field(..., description="Unique ID for the new task")
    name: str = Field(
        ...,
        description="Agent name or role hint (e.g. 'developer', 'validator')",
    )
    spec: TaskSpec = Field(
        ...,
        description=(
            "Required structured task spec object with goal, detail, and "
            "acceptance_criteria. The detail field is the agent's full briefing; "
            "acceptance_criteria should name concrete commands, expected evidence, "
            "or specific pytest ids where applicable."
        ),
    )
    deps: list[str] = Field(default_factory=list, description="Task IDs this depends on")
    scope_paths: list[str] = Field(
        default_factory=list,
        description=(
            "File/dir hints for coordination and file-note lookup. For coding/planning lanes, "
            "use repo-relative implementation owner paths, not `/testbed/...` "
            "prefixes. For validators, use the production paths being verified. "
            "Every task should provide at least one path. Keep verification-only "
            "test targets in spec.acceptance_criteria; test files and test directories are rejected "
            "as scope_paths."
        ),
    )


class ResolvedTaskOutput(BaseModel):
    id: str = Field(..., description="Task id submitted by the planner.")
    spec: TaskSpec = Field(..., description="Full structured task spec.")
    agent: str = Field(..., description="Resolved exact agent name.")
    deps: list[str] = Field(default_factory=list, description="Task ids this task depends on.")
    scope_paths: list[str] = Field(default_factory=list, description="Scope paths for this task.")
    parent_id: str | None = Field(
        default=None,
        description="Parent task id when stamped by a replan submission.",
    )


class SubmitPlanInput(BaseModel):
    """Planner submission payload.

    Declares the initial child task structure only. A system-generated
    outcome summary is produced after the children terminate — planners do
    NOT author prose here.
    """

    model_config = ConfigDict(extra="forbid")

    new_tasks: list[NewTaskDefinition] = Field(
        default_factory=list,
        description=(
            "Structured JSON array of initial child tasks. Each entry is a "
            "NewTaskDefinition with id, name, spec, deps, and "
            "non-empty repo-relative scope_paths, including validators. The outcome "
            "summary is generated by the system after children complete; do not author prose."
        ),
    )


class SubmitPlanOutput(BaseModel):
    task_id: str = Field(..., description="Runtime-stamped planner task id.")
    agent_name: str = Field(..., description="Runtime-stamped planner agent name.")
    new_tasks: list[ResolvedTaskOutput] = Field(
        default_factory=list,
        description="Accepted child tasks with resolved exact agent names.",
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

    new_tasks: list[NewTaskDefinition] = Field(
        default_factory=list,
        description=(
            "Non-empty structured JSON array of corrective tasks to create as direct "
            "children of this replanner. The outcome summary is generated "
            "by the system after children complete; do not author prose. "
            "Each new task should include non-empty repo-relative scope_paths, "
            "including validators."
        ),
    )
    cancel_ids: list[str] = Field(
        ...,
        description=(
            "Required explicit list of direct siblings of the replanner to cancel; "
            "use [] when no sibling should be cancelled. Cascade "
            "propagates to their subtrees and dependents). Exclude the "
            "Failed task id; the original failed request_replan task is "
            "immutable evidence and is finalized by the runtime."
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


_EMPTY_REPLAN_ERROR = (
    "submit_replan requires at least one corrective new_task; "
    "look deeper into the issues and come back with a concrete corrective task."
)


# ---------------------------------------------------------------------------
# Shared planning helpers
# ---------------------------------------------------------------------------


def _get_graph(context: ToolExecutionContext) -> dict[str, Any] | None:
    tc = context.metadata.get("task_center")
    graph = getattr(tc, "graph", None) if tc is not None else None
    return graph if isinstance(graph, dict) else None


def _validate_task_specs(
    specs: list[NewTaskDefinition],
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
        valid_dep_ids=new_ids,
        roster=_roster_from_context(context),
    )


def _validate_submit_replan_input(
    arguments: SubmitReplanInput,
    context: ToolExecutionContext,
) -> list[str]:
    from team.core.models import Plan
    from team.planning.replan_validation import validate_replan_rules

    graph = _get_graph(context)
    current_task_id = str(context.metadata.get("work_item_id") or "")

    result = validate_replan_rules(
        graph=graph,
        replan_task_id=current_task_id,
        cancel_ids=arguments.cancel_ids,
    )
    errors = list(result.errors)
    if not arguments.new_tasks:
        errors.append(_EMPTY_REPLAN_ERROR)
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
    for spec in arguments.new_tasks:
        errors.extend(
            f"task '{spec.id}': {error}"
            for error in _replan_spec_contract_errors(spec.spec)
        )
    if errors:
        return errors

    resolved_tasks = _resolved_task_payloads(
        arguments.new_tasks,
        roster=_roster_from_context(context),
        parent_id=current_task_id,
        include_parent_id=True,
    )
    local_ids = {str(task.get("id")) for task in resolved_tasks}
    local_validation_tasks = [
        {
            **task,
            "deps": [
                dep_id
                for dep_id in task.get("deps", [])
                if str(dep_id) in local_ids
            ],
        }
        for task in resolved_tasks
    ]
    try:
        plan = Plan.from_dict({"tasks": local_validation_tasks})
    except (TypeError, ValueError) as exc:
        return [f"invalid replan payload: {exc}"]

    max_plan_size = _metadata_int(context, "max_plan_size", 50)
    issues = (
        validate_plan(
            plan,
            max_plan_size=max_plan_size,
            extra_validators=[_replan_agent_target_issues],
        )
        if plan.tasks
        else []
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
    if max_depth and plan.tasks and task_depth > max_depth:
        issues.append(
            {
                "field": "tasks",
                "msg": (
                    f"replan would exceed max_depth={max_depth} from current depth={task_depth}"
                ),
            }
        )
    errors.extend(str(issue.get("msg") or "invalid replan") for issue in issues)
    return errors


def _resolved_task_payloads(
    specs: list[NewTaskDefinition],
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
            "spec": spec.spec.to_dict(),
            "agent": resolved_agent,
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
        "Submits initial child tasks for the current planner and ends the "
        "planner loop."
    )
    short_description = "Submit a child plan."
    input_model = SubmitPlanInput
    output_model = SubmitPlanOutput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitPlanInput)
        from team.core.models import Plan

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
            plan = Plan.from_dict({"tasks": resolved_tasks})
        except (TypeError, ValueError) as exc:
            return ToolResult(output=f"Error: invalid plan payload: {exc}", is_error=True)

        max_plan_size = _metadata_int(context, "max_plan_size", 50)
        issues = validate_plan(
            plan,
            max_plan_size=max_plan_size,
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

        if issues:
            message = "; ".join(str(issue.get("msg") or "invalid plan") for issue in issues)
            return ToolResult(output=f"Error: {message}", is_error=True)

        context.metadata["resolved_plan"] = plan
        context.metadata["plan_is_replan"] = False
        payload = SubmitPlanOutput(
            task_id=str(context.metadata.get("work_item_id") or ""),
            agent_name=str(context.metadata.get("agent_name") or ""),
            new_tasks=[
                ResolvedTaskOutput.model_validate(item) for item in resolved_tasks
            ],
        )
        return ToolResult(output=payload.model_dump_json())


# ---------------------------------------------------------------------------
# SubmitReplanTool — commit corrective replan (terminal)
# ---------------------------------------------------------------------------


class SubmitReplanTool(BaseTool):
    name = "submit_replan"
    description = (
        "Submits corrective child tasks and sibling cancellations for the "
        "current replanner."
    )
    short_description = "Submit a corrective replan."
    input_model = SubmitReplanInput
    output_model = SubmitReplanOutput

    def to_api_schema(self) -> dict[str, Any]:
        schema = super().to_api_schema()
        task_spec = (
            schema.get("input_schema", {})
            .get("$defs", {})
            .get("NewTaskDefinition", {})
            .get("properties", {})
            .get("name")
        )
        if isinstance(task_spec, dict):
            task_spec["enum"] = ["developer", "validator"]
            task_spec["description"] = (
                "Replan task agent. Replanners may create only developer "
                "repair tasks or validator verification tasks."
            )
        return schema

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitReplanInput)
        from team.core.models import ReplanPlan

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

        context.metadata["resolved_plan"] = replan
        context.metadata["plan_is_replan"] = True
        payload = SubmitReplanOutput(
            task_id=current_task_id,
            agent_name=str(context.metadata.get("agent_name") or ""),
            new_tasks=[
                ResolvedTaskOutput.model_validate(item) for item in resolved_tasks
            ],
            cancel_ids=list(arguments.cancel_ids),
        )
        return ToolResult(output=payload.model_dump_json())


# ---------------------------------------------------------------------------
# Tool exports
# ---------------------------------------------------------------------------


def make_submission_tools() -> list[BaseTool]:
    """Return terminal submission tools for team-mode agents."""
    return [
        SubmitTaskSuccessTool(),
        RequestReplanTool(),
        SubmitPlanTool(),
        SubmitReplanTool(),
    ]
