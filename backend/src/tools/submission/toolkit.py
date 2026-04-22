"""Submission toolkit — terminal actions for team-mode agents.

Tools write structured data to ``context.metadata``; the executor reads
it after the runner returns.

Tool surface:
  - submit_plan          (terminal)     — commit child plan to task model
  - submit_replan        (terminal)     — commit corrective replan to task model
  - submit_task_summary  (terminal)     — submit success/fail outcome
"""

from __future__ import annotations

import json
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


_SPEC_SECTIONS = ("Goal", "Task Details", "Acceptance Criteria")
_SPEC_SECTION_RE = re.compile(
    r"(?im)^\s*(?:\d+[.)]\s*)?"
    r"(Goal|Task Details|Acceptance Criteria)\s*:\s*\S"
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


def _format_task_summary_lines(tasks: list[dict[str, Any]], *, limit: int = 12) -> str:
    lines: list[str] = []
    for item in tasks[:limit]:
        task_id = str(item.get("id") or "<unknown>")
        agent = str(item.get("agent") or "<unknown>")
        description = str(item.get("description") or "").strip()
        deps = [str(dep) for dep in item.get("deps") or []]
        scopes = [str(path) for path in item.get("scope_paths") or []]

        details: list[str] = []
        if description:
            details.append(description)
        if deps:
            details.append("deps=" + ", ".join(deps))
        if scopes:
            shown_scopes = scopes[:3]
            scope_text = ", ".join(shown_scopes)
            if len(scopes) > len(shown_scopes):
                scope_text += f", +{len(scopes) - len(shown_scopes)} more"
            details.append("scope=" + scope_text)

        suffix = "; ".join(details) if details else "no description"
        lines.append(f"- {task_id} ({agent}): {suffix}")

    if len(tasks) > limit:
        lines.append(f"- ... {len(tasks) - limit} more task(s)")
    return "\n".join(lines)


def _format_plan_note(
    *,
    header: str,
    tasks: list[dict[str, Any]] | None = None,
    task_section_title: str = "Tasks",
    cancel_ids: list[str] | None = None,
) -> str:
    sections = [header]
    if tasks:
        sections.append(f"{task_section_title}:\n{_format_task_summary_lines(tasks)}")
    if cancel_ids:
        sections.append("Cancelled siblings: " + ", ".join(str(item) for item in cancel_ids))
    return "\n\n".join(sections)


# ---------------------------------------------------------------------------
# SubmitTaskSummaryTool — terminal for non-planner agents
# ---------------------------------------------------------------------------


class SubmitTaskSummaryInput(BaseModel):
    type: Literal["success", "request_replan"] = Field(
        ...,
        description=(
            "Outcome type. Use 'success' only when all assigned acceptance "
            "criteria are satisfied by live evidence. Use 'request_replan' "
            "when the task is blocked, still red, assigned to the wrong owner, "
            "or needs a different scope or sequence."
        ),
    )
    content: str = Field(
        ...,
        min_length=1,
        description=(
            "Evidence-rich terminal summary for Task Center notes. Developers: "
            "state the concrete API or behavior delta, verification commands and outcomes "
            "observed after the final edit, and known gaps or deferred items. Validators: list "
            "each acceptance criterion with pass/fail plus the command, probe, "
            "exit code, or key assertion used; on failure include the minimal "
            "repro and hypothesized root cause. For request_replan: start with "
            "a replan trigger, exactly one of scope_expansion, "
            "wrong_owner_or_role, or unresolved_blocker; then include blocking "
            "evidence, failing command or tool result, affected paths or owners, "
            "and why a different owner, scope, sequence, or budget is needed. "
            "Do not submit placeholders such as 'task completed', "
            "'all checks passed', or a filename-only list."
        ),
    )

    @field_validator("content")
    @classmethod
    def _content_must_not_be_blank(cls, value: str) -> str:
        if not value.strip():
            raise ValueError("content must contain non-whitespace text")
        return value


class SubmitTaskSummaryOutput(BaseModel):
    task_id: str = Field(..., description="Runtime-stamped task id.")
    agent_name: str = Field(..., description="Runtime-stamped submitting agent name.")
    type: Literal["success", "request_replan"] = Field(
        ..., description="Submitted outcome type."
    )
    content: str = Field(..., description="Submitted task outcome content.")


class SubmitTaskSummaryTool(BaseTool):
    name = "submit_task_summary"
    description = (
        "Submit the evidence-rich terminal outcome for a developer, validator, "
        "or parent_summarizer task. Use type='success' only for satisfied "
        "acceptance criteria with concrete verification evidence; use "
        "type='request_replan' for blockers, red checks, wrong ownership, or "
        "needed scope/sequence changes, classified as scope_expansion, "
        "wrong_owner_or_role, or unresolved_blocker. This is terminal: the "
        "agent loop ends after this call."
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
            "Concise planner-authored task label. "
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
            "in order using numbered colon labels. Each label must start its own "
            "line and have body text after the colon on that same line, e.g. "
            "'1. Goal: ...\\n2. Task Details: ...\\n"
            "3. Acceptance Criteria: ...'. Markdown headings "
            "like '## Goal', one-line specs with every label, and labels whose "
            "body starts on the next line are not accepted. Acceptance Criteria "
            "should name concrete commands, expected evidence, or specific "
            "pytest ids where applicable."
        ),
    )
    deps: list[str] = Field(default_factory=list, description="Task IDs this depends on")
    scope_paths: list[str] = Field(
        default_factory=list,
        description=(
            "File/dir hints for coordination and note scoping. For coding/planning lanes, "
            "use repo-relative implementation owner paths, not `/testbed/...` "
            "prefixes. For validators, use the production paths being verified. "
            "Every task should provide at least one path. Keep verification-only "
            "test targets in spec unless the task explicitly owns a test-only bug."
        ),
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
    """Planner submission payload.

    Declares the initial child task structure only. A system-generated
    outcome summary is produced after the children terminate — planners do
    NOT author prose here.
    """

    model_config = ConfigDict(extra="forbid")

    new_tasks: list[NewTaskSpec] = Field(
        default_factory=list,
        description=(
            "Structured JSON array of initial child tasks. Each entry is a "
            "NewTaskSpec with id, description, name, spec, deps, and "
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

    new_tasks: list[NewTaskSpec] = Field(
        default_factory=list,
        description=(
            "Structured JSON array of corrective tasks to create as direct "
            "children of this replanner. The outcome summary is generated "
            "by the system after children complete; do not author prose. "
            "Each new task should include non-empty repo-relative scope_paths, "
            "including validators."
        ),
    )
    cancel_ids: list[str] = Field(
        default_factory=list,
        description=(
            "Direct siblings of the replanner to cancel (cascade "
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
        valid_dep_ids=new_ids,
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
    issues = validate_plan(plan, max_plan_size=max_plan_size) if plan.tasks else []

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
        "Submit the initial planned tasks as structured JSON. Provide "
        "new_tasks with id, description, name, spec, deps, and "
        "non-empty repo-relative scope_paths for every task, including validators. A "
        "system-generated summary of what actually happened "
        "is produced after children complete — do NOT write prose. Do not "
        "include output, background, summary, parent_id, or "
        "other fields. Each spec must use numbered colon labels in order, "
        "each at the start of its own line with body text after the colon "
        "on the same line: 1. Goal, 2. Task Details, "
        "3. Acceptance Criteria. "
        "Use validator tasks when a distinct verification lane is useful. "
        "Scope paths name implementation owner paths "
        "for developer/planner lanes and production paths being verified for "
        "validator lanes; use repo-relative paths, not `/testbed/...` prefixes; "
        "put verification-only test targets in spec unless tests are explicitly "
        "the owned bug surface."
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

        note_budget_issues = _note_budget_issues(
            resolved_tasks,
            max_note_bytes=_metadata_int(context, "max_note_bytes"),
        )
        issues.extend({"field": "tasks", "msg": msg} for msg in note_budget_issues)

        if issues:
            message = "; ".join(str(issue.get("msg") or "invalid plan") for issue in issues)
            return ToolResult(output=f"Error: {message}", is_error=True)

        note_content = _format_plan_note(
            header=f"Submitted plan with {len(plan.tasks)} task(s).",
            tasks=resolved_tasks,
        )
        await _post_submission_note(context, content=note_content, tags=["architecture"])

        # Persist the structured task JSON on the parent so
        # `read_task_details(<parent_id>)` surfaces the authoritative
        # initial planning payload (id, description, name, spec, deps,
        # scope_paths) to downstream readers.
        scope_union: list[str] = []
        seen_paths: set[str] = set()
        for item in resolved_tasks:
            for path in item.get("scope_paths") or []:
                path_str = str(path)
                if path_str in seen_paths:
                    continue
                seen_paths.add(path_str)
                scope_union.append(path_str)
        await _post_submission_note(
            context,
            content=json.dumps(resolved_tasks, indent=2),
            scope_paths=scope_union,
            tags=["initial_planned_tasks"],
        )

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
        "Submit the initial replanned tasks as structured JSON. Provide "
        "new_tasks for corrective repair work owned by the "
        "replanner, and cancel_ids for stale direct siblings whose subtrees "
        "should be cancelled by cascade. A system-generated summary of what "
        "actually happened is produced after children complete — do NOT "
        "write prose. Never put the Failed task id or original failed "
        "request_replan task in cancel_ids. Do not include output, "
        "summary, background, parent_id, or other fields. Each new task must "
        "include a short planner-authored description. Each new task spec must use "
        "numbered colon labels in order, each at the start of its own line "
        "with body text after the colon on the same line: 1. Goal, "
        "2. Task Details, 3. Acceptance Criteria. "
        "When validator tasks are present, give them non-empty repo-relative "
        "production scope_paths."
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

        note_content = _format_plan_note(
            header=(
                f"Replanner submitted replan: {len(replan.add_tasks)} new task(s), "
                f"{len(replan.cancel_ids)} cancelled."
            ),
            tasks=resolved_tasks,
            task_section_title="Corrective tasks",
            cancel_ids=list(arguments.cancel_ids),
        )
        await _post_submission_note(context, content=note_content, tags=["refactor"])

        # Persist the structured corrective-task JSON on the parent so
        # `read_task_details(<parent_id>)` surfaces the authoritative
        # initial replanning payload (id, description, name, spec, deps,
        # scope_paths, parent_id) to downstream readers.
        scope_union: list[str] = []
        seen_paths: set[str] = set()
        for item in resolved_tasks:
            for path in item.get("scope_paths") or []:
                path_str = str(path)
                if path_str in seen_paths:
                    continue
                seen_paths.add(path_str)
                scope_union.append(path_str)
        await _post_submission_note(
            context,
            content=json.dumps(resolved_tasks, indent=2),
            scope_paths=scope_union,
            tags=["initial_replanned_tasks"],
        )

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
