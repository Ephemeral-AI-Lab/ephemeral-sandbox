"""Posthook toolkit — terminal submission actions for team-mode agents."""

from __future__ import annotations

import time
import uuid
from typing import Any

from pydantic import BaseModel, Field

from agents.registry import get_definition
from team.planning.validation import validate_plan
from tools.context.toolkit import PostNoteTool
from tools.core.base import BaseTool, BaseToolkit, ToolExecutionContext, ToolResult


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


async def _check_context_freshness(context: ToolExecutionContext) -> str:
    """Return a warning string if context is stale, otherwise empty string."""
    from tools.context.freshness import check_freshness

    report = await check_freshness(context)
    if not report.stale:
        return ""

    return (
        f"\n\n[FRESHNESS WARNING] Your context may be stale since task started. "
        f"Scope changes by others: {report.scope_changes_by_others}, "
        f"New dependency notes: {report.new_dep_notes}, "
        f"New sibling completions: {report.new_sibling_completions}. "
        f"Consider re-reading affected files before submitting."
    )


async def _freshness_submission_gate(context: ToolExecutionContext, *, action: str) -> ToolResult | None:
    """Reject terminal submissions when the task context has gone stale."""
    freshness_warning = await _check_context_freshness(context)
    if not freshness_warning:
        return None
    return ToolResult(
        output=(
            f"Error: `{action}` is blocked because your task context changed since the "
            "last acknowledged baseline. Call `context_changed_since()` now, refresh with "
            "`read_notes(...)` or targeted rereads if needed, then either retry the "
            f"submission or call `request_replan()`. {freshness_warning.strip()}"
        ),
        is_error=True,
    )


async def _accept_replan_submission(
    context: ToolExecutionContext,
    *,
    add_tasks: list[dict],
    cancel_ids: list[str],
    note_content: str,
) -> ToolResult:
    from team.models import ReplanPlan

    replan = ReplanPlan.from_dict({"add_tasks": add_tasks, "cancel_ids": cancel_ids})
    freshness_gate = await _freshness_submission_gate(context, action="replan_submission()")
    if freshness_gate is not None:
        return freshness_gate
    await _post_submission_note(context, content=note_content, tags=["refactor"])
    return ToolResult(
        output=f"Replan accepted ({len(replan.add_tasks)} new tasks, {len(replan.cancel_ids)} cancelled).",
        metadata={"resolved_replan": replan},
    )


def _resolve_agent_name(agent_value: str, roster: dict[str, list[str]]) -> str:
    candidate = agent_value.strip()
    if not candidate:
        return candidate
    if get_definition(candidate) is not None:
        return candidate
    role_matches = roster.get(candidate)
    if role_matches:
        return str(role_matches[0])
    return candidate


def _resolve_plan_tasks(
    raw_tasks: list[dict[str, Any]],
    roster: dict[str, list[str]],
) -> list[dict[str, Any]]:
    resolved: list[dict[str, Any]] = []
    for item in raw_tasks:
        data = dict(item)
        data["agent"] = _resolve_agent_name(str(data.get("agent") or ""), roster)
        resolved.append(data)
    return resolved


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


def _roster_from_context(context: ToolExecutionContext) -> dict[str, list[str]]:
    roster = context.metadata.get("roster")
    if not isinstance(roster, dict):
        return {}
    return {
        str(role): [str(agent_name) for agent_name in agent_names if isinstance(agent_name, str)]
        for role, agent_names in roster.items()
        if isinstance(agent_names, list)
    }


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
        task_text = str(item.get("task") or "")
        size = len(task_text.encode("utf-8"))
        if size > max_note_bytes:
            issues.append(
                f"task '{task_id}' is {size} bytes, exceeds max_note_bytes={max_note_bytes}"
            )
    return issues


def _coordination_warnings(context: ToolExecutionContext) -> list[str]:
    raw = context.metadata.get("coordination_warnings")
    if not isinstance(raw, list):
        return []
    seen: set[str] = set()
    messages: list[str] = []
    for item in raw:
        if isinstance(item, dict):
            message = str(item.get("message") or "").strip()
        else:
            message = str(item or "").strip()
        if not message or message in seen:
            continue
        seen.add(message)
        messages.append(message)
    return messages


# ---------------------------------------------------------------------------
# SubmitPlanTool
# ---------------------------------------------------------------------------


class SubmitPlanInput(BaseModel):
    tasks: list[dict] = Field(
        ...,
        description=(
            "List of TaskSpec dicts. Each must have: "
            "id (unique string), task (prose instruction — this is the agent's sole briefing), "
            "agent (agent name or role hint, e.g. 'developer', 'team_planner', 'validator'), "
            "deps (list of task ids this depends on, default []), "
            "scope_paths (file/dir hints for OCC and note scoping, default []), "
            "cascade_policy ('cancel' | 'retry_first' | 'continue', default 'cancel')."
        ),
    )
    rationale: str | None = Field(
        default=None,
        description="Why this decomposition was chosen — helps replanners if tasks fail",
    )


class SubmitPlanTool(BaseTool):
    name = "submit_plan"
    description = (
        "Submit a plan decomposition. Terminal action for planners. "
        "Each task's 'task' field is the agent's sole briefing — write clear, "
        "actionable prose. Items targeting a planner-role agent are expandable "
        "(further decomposed); all others are atomic."
    )
    input_model = SubmitPlanInput
    tool_types = frozenset({"post_run"})

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitPlanInput)
        from team.models import Plan

        roster = _roster_from_context(context)
        resolved_tasks = _resolve_plan_tasks(arguments.tasks, roster)
        try:
            plan = Plan.from_dict({"tasks": resolved_tasks, "rationale": arguments.rationale})
        except (TypeError, ValueError) as exc:
            return ToolResult(output=f"Error: invalid plan payload: {exc}", is_error=True)

        allow_empty = bool(context.metadata.get("allow_empty_plan"))
        max_plan_size = int(context.metadata.get("max_plan_size", 50) or 50)
        known_external_dep_ids = await _known_external_dep_ids(context)
        issues = validate_plan(
            plan,
            max_plan_size=max_plan_size,
            allow_empty=allow_empty,
            known_external_deps=known_external_dep_ids,
        )

        max_tasks = int(context.metadata.get("max_tasks", 0) or 0)
        tasks_used = int(context.metadata.get("tasks_used", 0) or 0)
        if max_tasks and tasks_used + len(plan.tasks) > max_tasks:
            issues.append(
                {
                    "field": "tasks",
                    "msg": f"plan would exceed max_tasks={max_tasks} (used={tasks_used}, adding={len(plan.tasks)})",
                }
            )
        max_depth = int(context.metadata.get("max_depth", 0) or 0)
        task_depth = int(context.metadata.get("task_depth", 0) or 0)
        if max_depth and plan.tasks and (task_depth + 1) > max_depth:
            issues.append(
                {
                    "field": "tasks",
                    "msg": f"plan would exceed max_depth={max_depth} from current depth={task_depth}",
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
        if arguments.rationale:
            summary += f"\nRationale: {arguments.rationale.strip()}"
        await _post_submission_note(context, content=summary, tags=["architecture"])
        return ToolResult(
            output=f"Plan accepted ({len(plan.tasks)} tasks).",
            metadata={"resolved_plan": plan},
        )


# ---------------------------------------------------------------------------
# RequestReplanTool
# ---------------------------------------------------------------------------


class RequestReplanInput(BaseModel):
    reason: str = Field(
        ...,
        description=(
            "Why replan is needed. Describe the structural problem — e.g. "
            "'auth spans 3 services, need separate tasks' or 'wrong owner file, "
            "actual owner is src/utils.py not src/helpers.py'."
        ),
    )
    suggestion: str | None = Field(
        default=None,
        description="Optional suggestion for the replanner on how to restructure the work",
    )


class RequestReplanTool(BaseTool):
    name = "request_replan"
    description = (
        "Request a replan of the current task scope. Use when the task itself is "
        "mis-scoped — wrong files, scope too broad, missing dependencies, or the "
        "decomposition needs restructuring. A replanner agent will create corrective "
        "sibling tasks."
    )
    input_model = RequestReplanInput
    tool_types = frozenset({"post_run"})

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, RequestReplanInput)
        note = f"Requested replan: {arguments.reason}"
        if arguments.suggestion:
            note += f"\nSuggestion: {arguments.suggestion}"
        await _post_submission_note(context, content=note, tags=["warning"])
        return ToolResult(output="Replan requested.")


# ---------------------------------------------------------------------------
# Replanner tools — add_tasks / declare_blocker / cancel_and_redraft
# ---------------------------------------------------------------------------


class SubmitReplanInput(BaseModel):
    add_tasks: list[dict] = Field(
        default_factory=list,
        description=(
            "New TaskSpec dicts inserted at the current DAG level as corrective siblings. "
            "Same shape as submit_plan tasks: id, task (prose), agent, deps, scope_paths, "
            "cascade_policy. Plan at this level only — assign agent='team_planner' for "
            "tasks that need further decomposition into subtrees."
        ),
    )
    cancel_ids: list[str] = Field(
        default_factory=list,
        description=(
            "Task IDs to cancel. Cancelling a node cancels its entire subtree. "
            "Targets can be atomic or expandable, running or pending."
        ),
    )


class AddTasksTool(BaseTool):
    name = "add_tasks"
    description = (
        "Add corrective sibling tasks without cancelling existing work. "
        "Use for isolated failures, transient retries, or follow-up work "
        "when other siblings can continue unchanged."
    )
    input_model = SubmitReplanInput
    tool_types = frozenset({"post_run"})

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitReplanInput)
        return await _accept_replan_submission(
            context,
            add_tasks=arguments.add_tasks,
            cancel_ids=[],
            note_content=f"Replanner added {len(arguments.add_tasks)} corrective task(s).",
        )


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
    input_model = DeclareBlockerInput
    tool_types = frozenset({"post_run"})

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, DeclareBlockerInput)
        freshness_gate = await _freshness_submission_gate(context, action="declare_blocker()")
        if freshness_gate is not None:
            return freshness_gate
        note = f"Declared blocker on {', '.join(arguments.root_cause_paths)}: {arguments.reason}"
        if arguments.suggestion:
            note += f"\nSuggestion: {arguments.suggestion}"
        await _post_submission_note(context, content=note, tags=["blocker"])
        return ToolResult(output="Blocker declared.")


class CancelAndRedraftTool(BaseTool):
    name = "cancel_and_redraft"
    description = (
        "Cancel stale sibling nodes (and their subtrees) and replace with corrective "
        "tasks at the current DAG level. Use when stale tasks must be stopped, not "
        "just supplemented. Scope can be narrow (one node) or broad (many)."
    )
    input_model = SubmitReplanInput
    tool_types = frozenset({"post_run"})

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SubmitReplanInput)
        return await _accept_replan_submission(
            context,
            add_tasks=arguments.add_tasks,
            cancel_ids=arguments.cancel_ids,
            note_content=(
                f"Cancelled {len(arguments.cancel_ids)} task(s) and redrafted "
                f"{len(arguments.add_tasks)} replacement task(s)."
            ),
        )


# ---------------------------------------------------------------------------
# PosthookTools
# ---------------------------------------------------------------------------


class PosthookTools(BaseToolkit):
    """Role-aware tool set that exposes the appropriate terminal submission tools."""

    posthook = True

    @classmethod
    def from_context(cls, ctx: object) -> PosthookTools:
        from agents.registry import get_role

        metadata = getattr(ctx, "metadata", None) or getattr(ctx, "tool_metadata", None) or {}
        role = metadata.get("role") if hasattr(metadata, "get") else None
        if not isinstance(role, str) or not role.strip():
            agent_name = str(metadata.get("agent_name") or "") if hasattr(metadata, "get") else ""
            role = get_role(agent_name)

        if role == "planner":
            tools = [SubmitPlanTool()]
        elif role == "replanner":
            tools = [
                AddTasksTool(),
                DeclareBlockerTool(),
                CancelAndRedraftTool(),
            ]
        elif role == "resolver":
            tools = [PostNoteTool(), RequestReplanTool()]
        elif role == "explorer":
            tools = [PostNoteTool()]
        else:
            tools = [PostNoteTool(), RequestReplanTool()]
        return cls(
            name="posthook",
            description="Posthook submission tools for the current agent role.",
            tools=tools,
        )
