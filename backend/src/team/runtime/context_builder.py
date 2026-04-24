"""Production context builder for the team Executor.

Assembles a TeamAgentContext for a Task using TaskCenter's context builder.
"""

from __future__ import annotations

import time
from functools import lru_cache
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Iterable

from message import ConversationMessage
from prompt.user_prompt_templates import render_user_prompt_template
from team.models import Task
from tools.core.runtime import ExecutionMetadata

if TYPE_CHECKING:
    from agents.types import AgentDefinition
    from team.runtime.team_run import TeamRun


def _resolve_terminal_tools(defn: "AgentDefinition") -> set[str]:
    return {
        str(name).strip()
        for name in getattr(defn, "terminal_tools", []) or []
        if str(name).strip()
    }


@dataclass
class TeamAgentContext:
    """Canonical team-runtime context for work runners."""

    user_message: str = ""
    initial_messages: list[ConversationMessage] = field(default_factory=list)
    tool_metadata: ExecutionMetadata = field(default_factory=ExecutionMetadata)

    def __post_init__(self) -> None:
        if isinstance(self.tool_metadata, dict):
            meta = ExecutionMetadata()
            meta.update(self.tool_metadata)
            self.tool_metadata = meta


def build_task_metadata(team_run: "TeamRun", task: Task) -> ExecutionMetadata:
    """Build the canonical routing metadata for a team task."""
    meta = ExecutionMetadata(
        team_run_id=team_run.id,
        work_item_id=task.id,
        agent_run_id=task.agent_run_id,
        agent_name=task.agent_name,
        sandbox_id=getattr(team_run, "sandbox_id", "") or "",
    )
    meta["work_item_started_at"] = time.time()
    meta["task_deps"] = list(task.deps)
    meta["task_parent_id"] = task.parent_id
    meta["task_depth"] = task.depth
    repo_root = str(getattr(getattr(team_run, "project_context", None), "repo_root", "") or "")
    if repo_root:
        meta["repo_root"] = repo_root
        meta["exec_cwd"] = repo_root
        meta["ci_workspace_root"] = repo_root
    for key, value in getattr(team_run, "coordination_metadata", {}).items():
        meta[key] = value
    if task.scope_paths:
        meta["write_scope"] = task.scope_paths

    meta["task_center"] = team_run.task_center
    arbiter = getattr(team_run, "arbiter", None)
    if arbiter is not None:
        meta["arbiter"] = arbiter

    budgets = getattr(team_run, "budgets", None)
    if budgets is not None:
        meta["max_tasks"] = budgets.max_tasks
        meta["max_depth"] = budgets.max_depth
        meta["max_plan_size"] = budgets.max_plan_size
        meta["max_replans_per_run"] = budgets.max_replans_per_run
        meta["max_note_bytes"] = budgets.max_note_bytes
        meta["max_total_note_bytes"] = budgets.max_total_note_bytes
    budget_state = getattr(team_run, "budget_state", None)
    if budget_state is not None:
        meta["tasks_used"] = budget_state.tasks_used
        meta["note_bytes_used"] = budget_state.note_bytes_used
        meta["replans_used"] = budget_state.replans_used

    _populate_plan_submission_context(meta, team_run, task)

    return meta


def _populate_plan_submission_context(
    meta: ExecutionMetadata, team_run: "TeamRun", task: Task,
) -> None:
    roster = getattr(team_run, "roster", None)
    if isinstance(roster, dict):
        meta["roster"] = {str(role): list(names) for role, names in roster.items()}
        agent_names: set[str] = set()
        for names in roster.values():
            if isinstance(names, list):
                agent_names.update(str(n) for n in names)
        if agent_names:
            meta["roster_agent_names"] = agent_names

    try:
        from benchmarks.sweevo.plan_normalization import extract_benchmark_targets_from_team_run
        test_ids, test_files = extract_benchmark_targets_from_team_run(team_run.id)
        if test_ids:
            meta["benchmark_test_ids"] = test_ids
        if test_files:
            meta["benchmark_test_files"] = test_files
    except ImportError:
        pass


def build_initial_messages(task: Task) -> list[ConversationMessage]:
    return []


def _template_name_for_task(
    defn: "AgentDefinition | None", team_run: "TeamRun", task: Task,
) -> str | None:
    role = str(getattr(defn, "role", "") or "").strip()
    agent_name = str(getattr(task, "agent_name", "") or "").strip()

    if agent_name == "root_planner":
        return "root_task_planner"
    if role == "planner" or agent_name == "team_planner":
        return "task_planner"
    if role == "replanner" or agent_name == "team_replanner":
        return "task_replanner"
    if role == "developer" or agent_name == "developer":
        return "developer"
    if role == "reviewer" or agent_name == "validator":
        return "validator"
    return None


def _format_benchmark_targets(team_run: "TeamRun") -> str:
    test_ids: set[str] | list[str] | tuple[str, ...] | None = None
    test_files: set[str] | list[str] | tuple[str, ...] | None = None
    try:
        from benchmarks.sweevo.plan_normalization import extract_benchmark_targets_from_team_run

        test_ids, test_files = extract_benchmark_targets_from_team_run(team_run.id)
    except ImportError:
        pass

    if not test_ids:
        test_ids = getattr(team_run, "coordination_metadata", {}).get("benchmark_test_ids")
    if not test_files:
        test_files = getattr(team_run, "coordination_metadata", {}).get("benchmark_test_files")

    lines: list[str] = []
    if test_ids:
        lines.append("Test ids:")
        lines.extend(f"- {item}" for item in sorted(str(item) for item in test_ids))
    if test_files:
        if lines:
            lines.append("")
        lines.append("Test files:")
        lines.extend(f"- {item}" for item in sorted(str(item) for item in test_files))
    return "\n".join(lines)


@lru_cache(maxsize=1)
def _terminal_tool_descriptions() -> dict[str, str]:
    from tools.submission.toolkit import (
        RequestReplanTool,
        SubmitPlanTool,
        SubmitReplanTool,
        SubmitTaskSuccessTool,
    )

    tools = [
        SubmitPlanTool(),
        SubmitReplanTool(),
        SubmitTaskSuccessTool(),
        RequestReplanTool(),
    ]
    return {
        tool.name: (tool.description or tool.short_description or tool.name).strip()
        for tool in tools
    }


def _format_terminal_tools(terminal_tools: Iterable[str]) -> str:
    names = sorted(str(name).strip() for name in terminal_tools if str(name).strip())
    descriptions = _terminal_tool_descriptions()
    if not names:
        return (
            "- final_response: No terminal tool is configured for this role; finish with "
            "the role's normal final response."
        )
    return "\n".join(
        f"- {name}: {descriptions.get(name, 'Terminal tool configured for this role.')}"
        for name in names
    )


def _planning_depth_variables(team_run: "TeamRun", task: Task) -> dict[str, object]:
    budgets = getattr(team_run, "budgets", None)
    max_depth = getattr(budgets, "max_depth", None)
    if max_depth is None:
        return {}

    current_depth = int(task.depth or 0)
    return {
        "current_depth": current_depth,
        "max_depth": int(max_depth),
        "child_depth": current_depth + 1,
        "grandchild_depth": current_depth + 2,
    }


async def _render_template_user_message(
    team_run: "TeamRun",
    task: Task,
    defn: "AgentDefinition | None",
    terminal_tools: Iterable[str] = (),
) -> str | None:
    template_name = _template_name_for_task(defn, team_run, task)
    if template_name is None:
        return None

    context_builder = getattr(team_run.task_center, "context", None)
    template_context_for = getattr(context_builder, "template_context_for", None)
    if template_context_for is None:
        return None

    parts = await template_context_for(task)
    deps_line = ", ".join(f"`{dep}`" for dep in task.deps if dep)
    parent_id = str(task.parent_id) if task.parent_id else ""
    failed_id = str(task.fired_by_task_id) if task.fired_by_task_id else ""
    variables: dict[str, object] = {
        "task_spec": parts.task_spec,
        "scope_paths": parts.scope_paths,
        "context_from_dependencies": parts.context_from_dependencies,
        "recent_scope_changes": parts.recent_scope_changes,
        "parent_context": parts.parent_context,
        "failure_context": parts.failure_context,
        "user_request": str(getattr(team_run, "user_request", "") or task.objective).strip(),
        "benchmark_targets": _format_benchmark_targets(team_run),
        "terminal_tools": _format_terminal_tools(terminal_tools),
        "your_task_id": str(task.id),
        "your_deps_ids": deps_line,
        "your_parent_task_id": parent_id,
        "your_failed_task_id": failed_id,
    }
    variables.update(_planning_depth_variables(team_run, task))
    return render_user_prompt_template(template_name, variables)


async def build_initial_user_message(
    team_run: "TeamRun",
    task: Task,
    defn: "AgentDefinition | None" = None,
    terminal_tools: Iterable[str] = (),
) -> str:
    """Build the runtime user prompt for a team task."""
    context = await _render_template_user_message(team_run, task, defn, terminal_tools)
    if context is None:
        context = str(await team_run.task_center.context.context_for(task))
    return context


async def build_query_context(
    defn: "AgentDefinition", team_run: "TeamRun", task: Task,
) -> TeamAgentContext:
    """Default production QueryContextBuilder."""
    meta = build_task_metadata(team_run, task)
    meta["role"] = getattr(defn, "role", "")

    terminal_set = _resolve_terminal_tools(defn)
    meta["terminal_tools"] = set(terminal_set)
    user_message = await build_initial_user_message(
        team_run,
        task,
        defn=defn,
        terminal_tools=terminal_set,
    )
    return TeamAgentContext(
        user_message=user_message,
        initial_messages=build_initial_messages(task),
        tool_metadata=meta,
    )
