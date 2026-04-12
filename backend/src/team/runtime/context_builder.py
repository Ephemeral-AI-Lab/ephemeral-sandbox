"""Production context builder for the team Executor.

Assembles a TeamAgentContext for a Task using TaskCenter.context_for().
"""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from team.models import Task, TaskStatus
from team.runtime.registry import get as _get_team_run
from tools.core.runtime import ExecutionMetadata

if TYPE_CHECKING:
    from agents.types import AgentDefinition
    from team.runtime.team_run import TeamRun

logger = logging.getLogger(__name__)


@dataclass
class TeamAgentContext:
    """Canonical team-runtime context for work runners."""

    user_message: str = ""
    tool_metadata: ExecutionMetadata = field(default_factory=ExecutionMetadata)

    def __post_init__(self) -> None:
        if isinstance(self.tool_metadata, dict):
            meta = ExecutionMetadata()
            meta.update(self.tool_metadata)
            self.tool_metadata = meta


def build_work_item_metadata(team_run: "TeamRun", task: Task) -> ExecutionMetadata:
    """Build the canonical routing metadata for a team task."""
    meta = ExecutionMetadata(
        team_run_id=team_run.id,
        work_item_id=task.id,
        agent_run_id=task.agent_run_id,
        agent_name=task.agent_name,
        sandbox_id=getattr(team_run, "sandbox_id", "") or "",
    )
    meta["work_item_started_at"] = time.time()
    meta["retry_count"] = task.retry_count
    meta["max_retries"] = task.max_retries
    repo_root = str(getattr(getattr(team_run, "project_context", None), "repo_root", "") or "")
    if repo_root:
        meta["daytona_cwd"] = repo_root
        meta["ci_workspace_root"] = repo_root
    for key, value in getattr(team_run, "coordination_metadata", {}).items():
        meta[key] = value
    if task.scope_paths:
        meta["write_scope"] = task.scope_paths

    # Inject shared resources for tools
    meta["task_center"] = team_run.task_center
    ledger = getattr(team_run, "ledger", None)
    if ledger is not None:
        meta["ledger"] = ledger
    file_change_store = getattr(team_run, "file_change_store", None)
    if file_change_store is not None:
        meta["file_change_store"] = file_change_store

    _populate_plan_submission_context(meta, team_run, task)
    return meta


def _populate_plan_submission_context(
    meta: ExecutionMetadata,
    team_run: "TeamRun",
    task: Task,
) -> None:
    """Inject plan-submission context into metadata."""
    root_id = str(getattr(team_run, "root_work_item_id", "") or "")
    is_sub_planner = (
        bool(root_id)
        and task.id != root_id
        and task.agent_name == "team_planner"
    )
    meta["allow_empty_plan"] = is_sub_planner

    graph = getattr(getattr(team_run, "dispatcher", None), "graph", None)
    if isinstance(graph, dict):
        meta["known_external_dep_ids"] = {str(tid) for tid in graph}

    roster = getattr(team_run, "roster", None)
    if isinstance(roster, dict):
        agent_names: set[str] = set()
        for names in roster.values():
            if isinstance(names, list):
                agent_names.update(str(n) for n in names)
        if agent_names:
            meta["roster_agent_names"] = agent_names

    try:
        from benchmarks.sweevo.plan_normalization import (
            extract_benchmark_targets_from_team_run,
        )
        test_ids, test_files = extract_benchmark_targets_from_team_run(team_run.id)
        if test_ids:
            meta["benchmark_test_ids"] = test_ids
        if test_files:
            meta["benchmark_test_files"] = test_files
    except ImportError:
        pass


def build_initial_user_message(team_run: "TeamRun", task: Task) -> str:
    """Build context string for a task via TaskCenter."""
    ledger = getattr(team_run, "ledger", None)
    return team_run.task_center.context_for(task, ledger=ledger)


def build_query_context(
    defn: "AgentDefinition",
    team_run: "TeamRun",
    task: Task,
) -> TeamAgentContext:
    """Default production QueryContextBuilder."""
    from agents.registry import get_definition

    meta = build_work_item_metadata(team_run, task)
    user_message = build_initial_user_message(team_run, task)
    roster = getattr(team_run, "roster", None)
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
    return TeamAgentContext(user_message=user_message, tool_metadata=meta)
