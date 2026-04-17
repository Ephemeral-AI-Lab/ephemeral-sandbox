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

from pydantic import BaseModel, ConfigDict, Field

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


async def _freshness_submission_gate(
    context: ToolExecutionContext, *, action: str
) -> ToolResult | None:
    """Reject terminal submissions when the task context has gone stale."""
    from tools.task_center.freshness import check_freshness

    report = await check_freshness(context)
    if not report.stale:
        task_id = context.metadata.get("work_item_id", "?")
        logger.debug(
            "Freshness check passed for %s [task=%s]",
            action,
            task_id,
        )
        return None
    return ToolResult(
        output=(
            f"Error: `{action}` is blocked because your task context changed since the "
            "last acknowledged baseline. Call `task_center_changed_since()` now, refresh with "
            "`read_task_note(...)` or targeted rereads if needed, then either retry the "
            f"submission or call `submit_task_summary(type='fail')`. "
            f"Scope changes by others: {report.scope_changes_by_others}, "
            f"New dependency notes: {report.new_dep_notes}, "
            f"New sibling completions: {report.new_sibling_completions}."
        ),
        is_error=True,
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


class SubmitTaskSummaryTool(BaseTool):
    name = "submit_task_summary"
    description = (
        "Submit your task outcome. Call with type='success' when work is done, "
        "or type='fail' when the task cannot be completed and needs replanning. "
        "This is your terminal action — the agent loop ends after this call."
    )
    short_description = "Submit task outcome."
    input_model = SubmitTaskSummaryInput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitTaskSummaryInput)

        # Write to metadata for executor to read after runner returns
        context.metadata["task_summary"] = arguments.content
        context.metadata["task_summary_type"] = arguments.type

        # Audit trail note
        tag = "implementation" if arguments.type == "success" else "warning"
        await _post_submission_note(context, content=arguments.content, tags=[tag])
        return ToolResult(output="Summary submitted.")


# ---------------------------------------------------------------------------
# Planning tool input models
# ---------------------------------------------------------------------------


class NewTaskSpec(BaseModel):
    """Full spec for a task the agent is creating."""

    id: str = Field(..., description="Unique ID for the new task")
    name: str = Field(
        ...,
        description="Agent name or role hint (e.g. 'developer', 'validator')",
    )
    spec: str = Field(
        ...,
        description=(
            "Structured task spec — the agent's sole briefing. Must include sections "
            "in order: Goal, Environment, Scope, Context, Acceptance Criteria."
        ),
    )
    deps: list[str] = Field(default_factory=list, description="Task IDs this depends on")
    scope_paths: list[str] = Field(
        default_factory=list,
        description="File/dir hints for OCC and note scoping",
    )


class ReplanTaskSpec(NewTaskSpec):
    """Full spec for a task added by a replanner."""

    parent_id: str | None = Field(
        ...,
        description=(
            "Existing parent task ID where this task should be inserted. Use null only "
            "when inserting at the root parent layer."
        ),
    )


class SubmitPlanInput(BaseModel):
    """Planner submission payload."""

    new_tasks: list[NewTaskSpec] = Field(
        default_factory=list,
        description="New child tasks to create",
    )
    output: str | None = Field(
        default=None,
        description="Optional concise rationale or plan summary.",
    )


class SubmitReplanInput(BaseModel):
    """Corrective replan submission payload."""

    model_config = ConfigDict(extra="forbid")

    new_tasks: list[ReplanTaskSpec] = Field(
        default_factory=list,
        description="New corrective tasks to create",
    )
    cancel_ids: list[str] = Field(
        default_factory=list,
        description="Task IDs to cancel (not-completed roots, with cascade)",
    )


# ---------------------------------------------------------------------------
# Shared planning helpers
# ---------------------------------------------------------------------------


def _get_graph(context: ToolExecutionContext) -> dict[str, Any] | None:
    tc = context.metadata.get("task_center")
    graph = getattr(tc, "graph", None) if tc is not None else None
    return graph if isinstance(graph, dict) else None


def _parent_id_for_current_task(
    graph: dict[str, Any] | None,
    task_id: str,
) -> str | None:
    if graph is None:
        return None
    own_task = graph.get(task_id)
    return getattr(own_task, "parent_id", None) if own_task is not None else None


def _is_under_parent_projection(
    graph: dict[str, Any],
    *,
    task_id: str,
    root_parent_id: str | None,
) -> bool:
    task = graph.get(task_id)
    while task is not None:
        parent_id = getattr(task, "parent_id", None)
        if parent_id == root_parent_id:
            return True
        if parent_id is None:
            return root_parent_id is None
        task = graph.get(parent_id)
    return False


def _active_tasks_by_id(graph: dict[str, Any]) -> dict[str, Any]:
    from team.models import TERMINAL_STATUSES

    return {
        task_id: task
        for task_id, task in graph.items()
        if getattr(task, "status", None) not in TERMINAL_STATUSES
    }


def _cascade_ids_for_cancel_root(
    graph: dict[str, Any],
    cancel_root_id: str,
) -> set[str]:
    active = _active_tasks_by_id(graph)
    children_by_parent: dict[str, list[str]] = {}
    dependents_by_dep: dict[str, list[str]] = {}
    for task_id, task in active.items():
        parent_id = getattr(task, "parent_id", None)
        if parent_id:
            children_by_parent.setdefault(str(parent_id), []).append(task_id)
        for dep_id in getattr(task, "deps", []) or []:
            dependents_by_dep.setdefault(str(dep_id), []).append(task_id)

    cascaded: set[str] = set()
    queue = [cancel_root_id]
    while queue:
        current = queue.pop(0)
        for child_id in children_by_parent.get(current, []):
            if child_id not in cascaded:
                cascaded.add(child_id)
                queue.append(child_id)
        for dependent_id in dependents_by_dep.get(current, []):
            dependent = active.get(dependent_id)
            if dependent is None:
                continue
            if dependent_id not in cascaded:
                cascaded.add(dependent_id)
                queue.append(dependent_id)
    cascaded.discard(cancel_root_id)
    return cascaded


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
    from team.models import TERMINAL_STATUSES

    errors: list[str] = []
    graph = _get_graph(context)
    if graph is None:
        return ["submit_replan requires the current task graph for validation"]

    current_task_id = str(context.metadata.get("work_item_id") or "")
    current_task = graph.get(current_task_id)
    origin_task_id = getattr(current_task, "fired_by_task_id", None)
    root_parent_id = _parent_id_for_current_task(graph, current_task_id)
    graph_ids = set(graph.keys())
    new_ids = {spec.id for spec in arguments.new_tasks}
    cancelled_ids = set(arguments.cancel_ids)
    all_cancelled_ids = set(cancelled_ids)

    if current_task_id in cancelled_ids:
        errors.append("replanner cannot cancel itself")
    if origin_task_id and origin_task_id in cancelled_ids:
        errors.append("replanner cannot cancel the original replanning task")

    for cancel_id in arguments.cancel_ids:
        target = graph.get(cancel_id)
        if target is None:
            errors.append(f"cancel target '{cancel_id}' not found in graph")
            continue
        if cancel_id == current_task_id:
            continue
        if not _is_under_parent_projection(
            graph,
            task_id=cancel_id,
            root_parent_id=root_parent_id,
        ):
            errors.append(
                f"cancel target '{cancel_id}' is outside the parent projection "
                f"rooted at {root_parent_id!r}"
            )
        status = getattr(target, "status", None)
        if status is not None and status in TERMINAL_STATUSES:
            errors.append(f"cancel target '{cancel_id}' is {status.value}; cannot cancel")
        all_cancelled_ids.update(_cascade_ids_for_cancel_root(graph, cancel_id))

    allowed_parent_ids: set[str | None] = {root_parent_id, current_task_id}
    for task_id, task in graph.items():
        if task_id in all_cancelled_ids:
            continue
        if task_id == origin_task_id:
            continue
        status = getattr(task, "status", None)
        if status is not None and status in TERMINAL_STATUSES:
            continue
        if _is_under_parent_projection(
            graph,
            task_id=task_id,
            root_parent_id=root_parent_id,
        ):
            allowed_parent_ids.add(task_id)

    for spec in arguments.new_tasks:
        if spec.parent_id not in allowed_parent_ids:
            errors.append(
                f"new task '{spec.id}': parent_id {spec.parent_id!r} is outside "
                f"the parent projection rooted at {root_parent_id!r}"
            )
        if spec.parent_id in all_cancelled_ids:
            errors.append(f"new task '{spec.id}': parent_id '{spec.parent_id}' is cancelled")

    excluded_dep_ids = {current_task_id}
    if origin_task_id:
        excluded_dep_ids.add(origin_task_id)
    valid_dep_ids = (graph_ids - all_cancelled_ids - excluded_dep_ids) | new_ids
    errors.extend(
        _validate_task_specs(
            arguments.new_tasks,
            graph_ids=graph_ids,
            valid_dep_ids=valid_dep_ids,
            roster=_roster_from_context(context),
        )
    )
    return errors


def _resolved_task_payloads(
    specs: list[NewTaskSpec],
    *,
    roster: dict[str, list[str]],
) -> list[dict[str, Any]]:
    resolved_tasks: list[dict[str, Any]] = []
    for spec in specs:
        resolved_agent = _resolve_agent_name(spec.name, roster)
        words = spec.spec.split()
        description = " ".join(words[:10]) + ("..." if len(words) > 10 else "")
        payload: dict[str, Any] = {
            "id": spec.id,
            "objective": spec.spec,
            "agent": resolved_agent,
            "description": description,
            "deps": list(spec.deps),
            "scope_paths": list(spec.scope_paths),
        }
        if isinstance(spec, ReplanTaskSpec):
            payload["parent_id"] = spec.parent_id
        resolved_tasks.append(payload)
    return resolved_tasks


# ---------------------------------------------------------------------------
# SubmitPlanTool — commit child plan (terminal)
# ---------------------------------------------------------------------------


class SubmitPlanTool(BaseTool):
    name = "submit_plan"
    description = (
        "Submit a child plan. Provide new_tasks with id, name, spec, deps, and "
        "scope_paths. Each spec must use sections in order: Goal, Environment, "
        "Scope, Context, Acceptance Criteria."
    )
    short_description = "Submit a child plan."
    input_model = SubmitPlanInput

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
        max_plan_size = int(context.metadata.get("max_plan_size", 50) or 50)
        known_ext_deps = await _known_external_dep_ids(context)
        issues = validate_plan(
            plan,
            max_plan_size=max_plan_size,
            allow_empty=allow_empty,
            known_external_deps=known_ext_deps,
        )

        max_tasks = int(context.metadata.get("max_tasks", 0) or 0)
        tasks_used = int(context.metadata.get("tasks_used", 0) or 0)
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
        max_depth = int(context.metadata.get("max_depth", 0) or 0)
        task_depth = int(context.metadata.get("task_depth", 0) or 0)
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
            max_note_bytes=int(context.metadata.get("max_note_bytes", 0) or 0),
        )
        issues.extend({"field": "tasks", "msg": msg} for msg in note_budget_issues)

        if issues:
            message = "; ".join(str(issue.get("msg") or "invalid plan") for issue in issues)
            return ToolResult(output=f"Error: {message}", is_error=True)

        freshness_gate = await _freshness_submission_gate(context, action="submit_plan()")
        if freshness_gate is not None:
            return freshness_gate

        summary = f"Submitted plan with {len(plan.tasks)} task(s)."
        if arguments.output:
            summary += f"\n\n{arguments.output}"
        await _post_submission_note(context, content=summary, tags=["architecture"])

        context.metadata["resolved_plan"] = plan
        context.metadata["plan_is_replan"] = False
        return ToolResult(output=f"Plan accepted ({len(plan.tasks)} tasks).")


# ---------------------------------------------------------------------------
# SubmitReplanTool — commit corrective replan (terminal)
# ---------------------------------------------------------------------------


class SubmitReplanTool(BaseTool):
    name = "submit_replan"
    description = (
        "Submit a corrective replan. Provide cancel_ids for not-completed tasks "
        "in the allowed parent projection to cancel with cascade, and new_tasks "
        "with id, parent_id, name, spec, deps, and scope_paths."
    )
    short_description = "Submit a corrective replan."
    input_model = SubmitReplanInput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitReplanInput)
        from team.models import ReplanPlan

        errors = _validate_submit_replan_input(arguments, context)
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
            replan = ReplanPlan.from_dict(
                {
                    "add_tasks": resolved_tasks,
                    "cancel_ids": arguments.cancel_ids,
                }
            )
        except (TypeError, ValueError) as exc:
            return ToolResult(output=f"Error: invalid replan payload: {exc}", is_error=True)

        freshness_gate = await _freshness_submission_gate(context, action="submit_replan()")
        if freshness_gate is not None:
            return freshness_gate

        note_content = (
            f"Replanner submitted replan: {len(replan.add_tasks)} new task(s), "
            f"{len(replan.cancel_ids)} cancelled."
        )
        await _post_submission_note(context, content=note_content, tags=["refactor"])

        context.metadata["resolved_plan"] = replan
        context.metadata["plan_is_replan"] = True
        return ToolResult(
            output=(
                f"Replan accepted ({len(replan.add_tasks)} new tasks, "
                f"{len(replan.cancel_ids)} cancelled)."
            ),
        )


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
            description=(
                "Terminal submission tools (submit_task_summary, submit_plan, submit_replan)."
            ),
            tools=[
                SubmitTaskSummaryTool(),
                SubmitPlanTool(),
                SubmitReplanTool(),
            ],
        )
