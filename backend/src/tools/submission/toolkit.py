"""Submission toolkit — terminal actions for team-mode agents.

Tools write structured data to ``context.metadata``; the executor reads
it after the runner returns.

Tool surface:
  - draft_task_plan      (non-terminal) — validate proposed plan, render ASCII diff
  - submit_task_plan     (terminal)     — commit plan to task model
  - declare_blocker      (terminal)     — escalate to conductor
  - submit_task_summary  (terminal)     — submit success/fail outcome
"""

from __future__ import annotations

import logging
import time
import uuid
from typing import Any, Literal

from pydantic import BaseModel, Field

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
    from tools.context.freshness import check_freshness

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
            "last acknowledged baseline. Call `context_changed_since()` now, refresh with "
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


_EXISTING_TASKS_UNSUPPORTED = (
    "existing_tasks rewiring is not supported yet; use remove_tasks + new_tasks "
    "to replace stale siblings instead."
)


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


class ExistingTaskRef(BaseModel):
    """Reference to a task already in the graph. Only deps can be rewired."""

    id: str = Field(..., description="Must match an existing task ID")
    deps: list[str] = Field(default_factory=list, description="Updated dependency list")


class NewTaskSpec(BaseModel):
    """Full spec for a task the agent is creating."""

    id: str = Field(..., description="Unique ID for the new task")
    name: str = Field(
        ...,
        description="Agent name or role hint (e.g. 'developer', 'validator')",
    )
    objective: str = Field(
        ...,
        description="Prose instruction — the agent's sole briefing",
    )
    deps: list[str] = Field(default_factory=list, description="Task IDs this depends on")
    scope_paths: list[str] = Field(
        default_factory=list,
        description="File/dir hints for OCC and note scoping",
    )


class DraftTaskPlanInput(BaseModel):
    existing_tasks: list[ExistingTaskRef] = Field(
        default_factory=list,
        description=(
            "Reserved for future sibling dependency rewrites. "
            "The current runtime rejects non-empty values."
        ),
    )
    new_tasks: list[NewTaskSpec] = Field(
        default_factory=list,
        description="New tasks to create",
    )
    remove_tasks: list[str] = Field(
        default_factory=list,
        description="Task IDs to cancel (siblings + descendants)",
    )


class SubmitTaskPlanInput(BaseModel):
    """Same schema as DraftTaskPlanInput."""

    existing_tasks: list[ExistingTaskRef] = Field(
        default_factory=list,
        description=(
            "Reserved for future sibling dependency rewrites. "
            "The current runtime rejects non-empty values."
        ),
    )
    new_tasks: list[NewTaskSpec] = Field(
        default_factory=list,
        description="New tasks to create",
    )
    remove_tasks: list[str] = Field(
        default_factory=list,
        description="Task IDs to cancel (siblings + descendants)",
    )


# ---------------------------------------------------------------------------
# Shared planning helpers
# ---------------------------------------------------------------------------


def _get_graph(context: ToolExecutionContext) -> dict[str, Any] | None:
    tc = context.metadata.get("task_center")
    graph = getattr(tc, "graph", None) if tc is not None else None
    return graph if isinstance(graph, dict) else None


def _get_sibling_ids(graph: dict[str, Any], parent_id: str | None) -> set[str]:
    """Return IDs of tasks sharing the same parent."""
    return {t.id for t in graph.values() if getattr(t, "parent_id", None) == parent_id}


def _validate_plan_input(
    arguments: DraftTaskPlanInput | SubmitTaskPlanInput,
    context: ToolExecutionContext,
) -> list[str]:
    """Validate the plan input. Returns list of error strings."""
    errors: list[str] = []
    if arguments.existing_tasks:
        return [_EXISTING_TASKS_UNSUPPORTED]

    graph = _get_graph(context)
    role = str(context.metadata.get("role") or "")
    task_id = str(context.metadata.get("work_item_id") or "")

    # Determine parent_id for scope
    parent_id: str | None = None
    if graph is not None:
        own_task = graph.get(task_id)
        if own_task is not None:
            parent_id = getattr(own_task, "parent_id", None)

    # Collect known graph IDs
    graph_ids: set[str] = set(graph.keys()) if graph is not None else set()

    # Sibling IDs for scope checking
    sibling_ids = _get_sibling_ids(graph, parent_id) if graph is not None else set()

    # 0. Reject existing_tasks rewiring — not supported yet
    if arguments.existing_tasks:
        errors.append(_EXISTING_TASKS_UNSUPPORTED)
        return errors  # Full rejection — do not proceed with any other validation

    # 1. Validate new_tasks IDs don't collide with existing
    new_ids: set[str] = set()
    for spec in arguments.new_tasks:
        if spec.id in graph_ids:
            errors.append(f"new task '{spec.id}' collides with existing task")
        if spec.id in new_ids:
            errors.append(f"duplicate new task id '{spec.id}'")
        new_ids.add(spec.id)

    # 2. Validate new task specs
    roster = _roster_from_context(context)
    for spec in arguments.new_tasks:
        # Resolve agent name
        resolved = _resolve_agent_name(spec.name, roster)
        if not resolved:
            errors.append(f"task '{spec.id}': empty agent name")
        elif get_definition(resolved) is None:
            errors.append(f"task '{spec.id}': unknown agent '{resolved}'")
        if not spec.objective:
            errors.append(f"task '{spec.id}': empty objective")

    # 3. Validate remove_tasks exist
    from team.models import TERMINAL_STATUSES

    for rid in arguments.remove_tasks:
        if rid not in graph_ids:
            errors.append(f"remove target '{rid}' not found in graph")
        elif graph is not None:
            target = graph.get(rid)
            if target is not None:
                status = getattr(target, "status", None)
                if status is not None and status in TERMINAL_STATUSES:
                    errors.append(
                        f"remove target '{rid}' is {status.value}; cannot cancel terminal tasks"
                    )

    # 4. Validate deps reference valid IDs
    all_valid_ids = graph_ids | new_ids
    for spec in arguments.new_tasks:
        for dep in spec.deps:
            if dep not in all_valid_ids:
                errors.append(f"new task '{spec.id}': unknown dep '{dep}'")

    # 5. Replanner scope check — all removals within the current sibling layer
    if role == "replanner" and parent_id is not None:
        for rid in arguments.remove_tasks:
            if rid not in sibling_ids:
                errors.append(
                    f"scope violation: remove target '{rid}' is not a sibling "
                    f"(parent_id={parent_id})"
                )

    return errors


def _render_task_line(task: Any, *, is_new: bool = False, is_removed: bool = False) -> str:
    """Render a single task as an ASCII line."""
    status = getattr(task, "status", None)
    status_str = f"[{status.value}]" if status is not None else "[new]"
    agent = getattr(task, "agent_name", None) or getattr(task, "name", "?")
    tid = getattr(task, "id", "?")
    deps = getattr(task, "deps", [])
    dep_str = f" deps=[{', '.join(deps)}]" if deps else ""

    prefix = "  "
    if is_new:
        prefix = "+ "
    elif is_removed:
        prefix = "- "

    return f"{prefix}{tid} {agent} {status_str}{dep_str}"


def _render_ascii_graph(
    title: str,
    siblings: list[Any],
    *,
    new_specs: list[NewTaskSpec] | None = None,
    remove_ids: set[str] | None = None,
) -> str:
    """Render an ASCII representation of the task graph."""
    lines = [f"=== {title} ==="]
    remove_ids = remove_ids or set()
    for t in siblings:
        tid = getattr(t, "id", "")
        is_removed = tid in remove_ids
        lines.append(_render_task_line(t, is_removed=is_removed))
    if new_specs:
        for spec in new_specs:
            dep_str = f" deps=[{', '.join(spec.deps)}]" if spec.deps else ""
            lines.append(f"+ {spec.id} {spec.name} [new]{dep_str}")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# DraftTaskPlanTool — preview tool (non-terminal)
# ---------------------------------------------------------------------------


class DraftTaskPlanTool(BaseTool):
    name = "draft_task_plan"
    description = (
        "Validate proposed plan and render ASCII before/after graph. "
        "Does NOT write to the task model. Call this first to preview, "
        "then call submit_task_plan to commit."
    )
    short_description = "Validate a draft task plan."
    input_model = DraftTaskPlanInput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, DraftTaskPlanInput)

        # Validate
        errors = _validate_plan_input(arguments, context)
        if errors:
            return ToolResult(
                output="Validation failed:\n" + "\n".join(f"- {e}" for e in errors),
                is_error=True,
            )

        # Fetch current siblings for graph rendering
        graph = _get_graph(context)
        task_id = str(context.metadata.get("work_item_id") or "")
        parent_id: str | None = None
        siblings: list[Any] = []

        if graph is not None:
            own_task = graph.get(task_id)
            if own_task is not None:
                parent_id = getattr(own_task, "parent_id", None)
            siblings = [t for t in graph.values() if getattr(t, "parent_id", None) == parent_id]

        remove_ids = set(arguments.remove_tasks)

        # Render BEFORE graph
        ascii_before = _render_ascii_graph("BEFORE", siblings)

        # Render AFTER graph (filter removed, add new)
        after_siblings = [t for t in siblings if getattr(t, "id", "") not in remove_ids]
        ascii_after = _render_ascii_graph(
            "AFTER",
            after_siblings,
            new_specs=arguments.new_tasks,
        )

        # Diff summary
        diff_lines = ["=== DIFF ==="]
        if arguments.remove_tasks:
            diff_lines.append(f"Remove: {len(arguments.remove_tasks)} task(s)")
        if arguments.new_tasks:
            diff_lines.append(f"Add: {len(arguments.new_tasks)} task(s)")
        ascii_diff = "\n".join(diff_lines)

        # Warnings for destructive actions
        warnings: list[str] = []
        if graph is not None:
            for rid in arguments.remove_tasks:
                target = graph.get(rid)
                if target is None:
                    continue
                status = getattr(target, "status", None)
                if status is not None and status.value == "running":
                    warnings.append(f"Warning: {rid} is RUNNING — will be terminated")
                # Check for descendants
                children = [t for t in graph.values() if getattr(t, "parent_id", None) == rid]
                if children:
                    warnings.append(
                        f"Warning: {rid} has {len(children)} descendants — will cascade cancel"
                    )

        output = f"{ascii_before}\n\n{ascii_after}\n\n{ascii_diff}"
        if warnings:
            output += "\n\n" + "\n".join(warnings)
        output += "\n\nPlan looks valid. Call submit_task_plan to commit."

        return ToolResult(output=output)


# ---------------------------------------------------------------------------
# SubmitTaskPlanTool — commit tool (terminal)
# ---------------------------------------------------------------------------


class SubmitTaskPlanTool(BaseTool):
    name = "submit_task_plan"
    description = (
        "Submit a plan. Planners: provide new_tasks with the full decomposition. "
        "Replanners: provide new_tasks for corrective tasks and remove_tasks "
        "for task IDs to cancel. existing_tasks rewires are not supported yet. "
        "Each task's 'objective' field is the agent's sole briefing — write "
        "clear, actionable prose."
    )
    short_description = "Submit a task plan."
    input_model = SubmitTaskPlanInput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitTaskPlanInput)
        from team.models import Plan, ReplanPlan

        # 1. Re-validate (same checks as draft_task_plan)
        errors = _validate_plan_input(arguments, context)
        if errors:
            return ToolResult(
                output="Validation failed:\n" + "\n".join(f"- {e}" for e in errors),
                is_error=True,
            )

        roster = _roster_from_context(context)
        role = str(context.metadata.get("role") or "")
        is_replanner = role == "replanner"

        # 2. Convert NewTaskSpec list to TaskDefinition dicts for validation
        resolved_tasks: list[dict[str, Any]] = []
        for spec in arguments.new_tasks:
            resolved_agent = _resolve_agent_name(spec.name, roster)
            # Auto-derive description from objective (first ~10 words)
            words = spec.objective.split()
            description = " ".join(words[:10]) + ("..." if len(words) > 10 else "")
            # Auto-set cascade_policy for validators (must be 'continue')
            agent_def = get_definition(resolved_agent)
            cascade = "continue" if agent_def and agent_def.role == "reviewer" else "cancel"
            resolved_tasks.append(
                {
                    "id": spec.id,
                    "objective": spec.objective,
                    "agent": resolved_agent,
                    "description": description,
                    "deps": list(spec.deps),
                    "scope_paths": list(spec.scope_paths),
                    "cascade_policy": cascade,
                }
            )

        if is_replanner:
            # Replanner path: build ReplanPlan
            try:
                replan = ReplanPlan.from_dict(
                    {
                        "add_tasks": resolved_tasks,
                        "cancel_ids": arguments.remove_tasks,
                    }
                )
            except (TypeError, ValueError) as exc:
                return ToolResult(output=f"Error: invalid replan payload: {exc}", is_error=True)

            freshness_gate = await _freshness_submission_gate(
                context, action="submit_task_plan(replan)"
            )
            if freshness_gate is not None:
                return freshness_gate

            note_content = (
                f"Replanner submitted plan: {len(replan.add_tasks)} new task(s), "
                f"{len(replan.cancel_ids)} cancelled."
            )
            await _post_submission_note(context, content=note_content, tags=["refactor"])

            # Write to metadata for executor
            context.metadata["resolved_plan"] = replan
            context.metadata["plan_is_replan"] = True
            return ToolResult(
                output=(
                    f"Replan accepted ({len(replan.add_tasks)} new tasks, "
                    f"{len(replan.cancel_ids)} cancelled)."
                ),
            )
        else:
            # Planner path: build Plan
            try:
                plan = Plan.from_dict({"tasks": resolved_tasks, "rationale": None})
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
                            f"plan would exceed max_depth={max_depth} "
                            f"from current depth={task_depth}"
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

            freshness_gate = await _freshness_submission_gate(context, action="submit_task_plan()")
            if freshness_gate is not None:
                return freshness_gate

            summary = f"Submitted plan with {len(plan.tasks)} task(s)."
            await _post_submission_note(context, content=summary, tags=["architecture"])

            # Write to metadata for executor
            context.metadata["resolved_plan"] = plan
            context.metadata["plan_is_replan"] = False
            return ToolResult(output=f"Plan accepted ({len(plan.tasks)} tasks).")


# ---------------------------------------------------------------------------
# DeclareBlockerTool — terminal for planners/replanners
# ---------------------------------------------------------------------------


class DeclareBlockerInput(BaseModel):
    root_cause_paths: list[str] = Field(
        ...,
        description="Broken shared files that must be fixed before sibling work can continue.",
        min_length=1,
    )
    reason: str = Field(
        ...,
        description="Why this is a shared blocker rather than an isolated task failure.",
        min_length=1,
    )
    suggestion: str | None = Field(
        default=None,
        description="Optional hint for the resolver about the expected fix direction.",
    )


class DeclareBlockerTool(BaseTool):
    name = "declare_blocker"
    description = (
        "Declare a shared blocker so the conductor can pause affected running siblings, "
        "spawn one resolver, and resume work after the fix lands."
    )
    short_description = "Report a shared blocker."
    input_model = DeclareBlockerInput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, DeclareBlockerInput)
        freshness_gate = await _freshness_submission_gate(context, action="declare_blocker()")
        if freshness_gate is not None:
            return freshness_gate
        context.metadata["blocker_declaration"] = {
            "root_cause_paths": list(arguments.root_cause_paths),
            "reason": arguments.reason,
            "suggestion": arguments.suggestion,
        }
        note = f"Declared blocker on {', '.join(arguments.root_cause_paths)}: {arguments.reason}"
        if arguments.suggestion:
            note += f"\nSuggestion: {arguments.suggestion}"
        await _post_submission_note(context, content=note, tags=["blocker"])
        return ToolResult(output="Blocker declared.")


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
                "Terminal submission tools "
                "(submit_task_summary, submit_task_plan, draft_task_plan, declare_blocker)."
            ),
            tools=[
                SubmitTaskSummaryTool(),
                DraftTaskPlanTool(),
                SubmitTaskPlanTool(),
                DeclareBlockerTool(),
            ],
        )
