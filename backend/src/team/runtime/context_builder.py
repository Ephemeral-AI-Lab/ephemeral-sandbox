"""Production context builder for the team Executor.

Assembles a TeamAgentContext for a Task using TaskCenter.context_for().
"""

from __future__ import annotations

import json
import logging
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from message import ConversationMessage
from team.models import Task
from tools.core.runtime import ExecutionMetadata

if TYPE_CHECKING:
    from agents.types import AgentDefinition
    from team.runtime.team_run import TeamRun

logger = logging.getLogger(__name__)

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
    meta["posthook_enabled"] = True
    meta["team_mode_enabled"] = True
    meta["retry_count"] = task.retry_count
    meta["max_retries"] = task.max_retries
    meta["task_deps"] = list(task.deps)
    meta["task_parent_id"] = task.parent_id
    meta["task_depth"] = task.depth
    repo_root = str(getattr(getattr(team_run, "project_context", None), "repo_root", "") or "")
    if repo_root:
        meta["daytona_cwd"] = repo_root
        meta["ci_workspace_root"] = repo_root
    for key, value in getattr(team_run, "coordination_metadata", {}).items():
        meta[key] = value
    if task.scope_paths:
        meta["write_scope"] = task.scope_paths

    meta["task_center"] = team_run.task_center
    file_change_store = getattr(team_run, "file_change_store", None)
    if file_change_store is not None:
        meta["file_change_store"] = file_change_store

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

    # Inject active blocker info so replanners can decide whether to
    # merge into an existing blocker or create a new one.
    conductor = getattr(team_run, "conductor", None)
    if conductor is not None and conductor.has_active_blocker():
        meta["active_blockers"] = [
            {
                "id": b.id,
                "reason": b.reason,
                "root_cause_paths": b.root_cause_paths,
                "status": b.status.value,
                "initiating_task_id": b.initiating_task_id,
                "fix_task_id": b.fix_task_id,
            }
            for b in conductor.active_blockers()
        ]

    return meta


def _populate_plan_submission_context(
    meta: ExecutionMetadata, team_run: "TeamRun", task: Task,
) -> None:
    root_id = str(getattr(team_run, "root_task_id", "") or "")
    is_sub_planner = (
        bool(root_id) and task.id != root_id and task.agent_name == "team_planner"
    )
    meta["allow_empty_plan"] = is_sub_planner

    graph = getattr(team_run.task_center, "graph", None)
    if isinstance(graph, dict):
        meta["known_external_dep_ids"] = {str(tid) for tid in graph}

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
    checkpoint = getattr(task, "pause_checkpoint", None)
    if not checkpoint:
        return []
    try:
        payload = json.loads(checkpoint)
    except Exception:
        logger.debug("Invalid pause_checkpoint for task %s", task.id, exc_info=True)
        return []
    if not isinstance(payload, list):
        return []
    messages: list[ConversationMessage] = []
    for raw in payload:
        if not isinstance(raw, dict):
            continue
        try:
            messages.append(ConversationMessage.model_validate(raw))
        except Exception:
            logger.debug("Invalid resume message for task %s", task.id, exc_info=True)
            return []
    return messages


async def build_initial_user_message(team_run: "TeamRun", task: Task, prefix: str | None = None) -> str:
    """Build context string for a task via TaskCenter."""
    context = await team_run.task_center.notes.context_for(task)
    # Priority 0: resume message for formerly-paused tasks
    if getattr(task, 'pause_checkpoint', None) and getattr(task, 'pause_verdict', None):
        resume_msg = (
            "## RESUME AFTER BLOCKER FIX\n"
            f"Your task was paused because: {task.pause_verdict}\n"
            "The root cause has been fixed. Continue your work from where you left off."
        )
        context = f"{resume_msg}\n\n{context}" if context else resume_msg
    if prefix:
        return f"{prefix}\n\n{context}" if context else prefix
    return context


async def build_query_context(
    defn: "AgentDefinition", team_run: "TeamRun", task: Task,
) -> TeamAgentContext:
    """Default production QueryContextBuilder."""
    from agents.registry import get_definition

    meta = build_task_metadata(team_run, task)
    meta["role"] = getattr(defn, "role", "")
    posthook_tools = getattr(defn, "posthook", None) or []
    if posthook_tools:
        meta["posthook_tool_names"] = list(posthook_tools)
        meta["posthook_prompt"] = (
            "Your main work is complete. Submit results by calling one of: "
            f"{', '.join(posthook_tools)}. "
            "If using post_note, include paths (files touched) and tags."
        )
    user_message = await build_initial_user_message(team_run, task)
    roster = getattr(team_run, "roster", None)
    if getattr(defn, "role", None) == "replanner" and meta.get("active_blockers"):
        blocker_lines = ["## Active Blockers\n",
                         "The following blockers are currently active for sibling tasks. "
                         "If an active blocker already covers the same root-cause paths, do not "
                         "call `declare_blocker` again. Use `add_tasks(...)` instead, and depend "
                         "on that blocker's `fix_task_id` so the retry runs after the shared fix.\n"]
        for b in meta["active_blockers"]:
            blocker_lines.append(
                f"- **{b['id'][:8]}** ({b['status']}): {b['reason']}\n"
                f"  Root cause: {', '.join(b['root_cause_paths'])}\n"
                f"  Fix task: {b.get('fix_task_id') or 'pending assignment'}"
            )
        blocker_lines.append("")
        user_message = "\n".join(blocker_lines) + "\n" + user_message

    if roster and getattr(defn, "role", None) in ("planner", "replanner"):
        lines = ["## Available Agents\n"]
        for role, agent_names in roster.items():
            lines.append(f"### {role}")
            for name in agent_names:
                agent_defn = get_definition(name)
                desc = agent_defn.description if agent_defn else ""
                lines.append(f"- **{name}**: {desc}")
            lines.append("")
        lines.append(
            "When submitting plan items, use these exact agent names. "
            "`kind` is auto-inferred from the agent's role "
            "(planner → expandable, all others → atomic)."
        )
        user_message = "\n".join(lines) + "\n\n" + user_message
    return TeamAgentContext(
        user_message=user_message,
        initial_messages=build_initial_messages(task),
        tool_metadata=meta,
    )
