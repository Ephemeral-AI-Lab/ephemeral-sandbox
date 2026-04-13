"""Shared context-freshness check used by both context tools and submission pre-checks."""

from __future__ import annotations

from dataclasses import dataclass

from team._path_utils import scope_paths_overlap
from tools.core.base import ToolExecutionContext


@dataclass
class FreshnessReport:
    """Result of a context freshness check."""

    scope_changes_by_others: int = 0
    new_dep_notes: int = 0
    new_sibling_completions: int = 0

    @property
    def stale(self) -> bool:
        return (
            self.scope_changes_by_others > 0
            or self.new_dep_notes > 0
            or self.new_sibling_completions > 0
        )


async def check_freshness(context: ToolExecutionContext) -> FreshnessReport:
    """Check if an agent's context has gone stale since its task started.

    Examines three signals:
    1. Scope changes by other agents (via file_change_store)
    2. New notes from dependency tasks (via Task Center)
    3. New sibling task completions (via dispatcher)
    """
    # Use the most recent freshness-check timestamp if available, so we
    # only report NEW changes since the agent last acknowledged staleness.
    # Falls back to work_item_started_at for the first check.
    since = (
        context.metadata.get("freshness_checked_at")
        or context.metadata.get("work_item_started_at", 0)
    )
    task_id = context.metadata.get("work_item_id", "")
    agent_run_id = context.metadata.get("agent_run_id", "")

    scope_changes = 0
    new_dep_notes = 0
    new_sibling_completions = 0

    file_change_store = context.metadata.get("file_change_store")
    scope_paths = context.metadata.get("write_scope") or []
    if (
        file_change_store is not None
        and getattr(file_change_store, "initialized", False)
        and scope_paths
    ):
        changes = file_change_store.changes_since(since)
        scope_changes = sum(
            1
            for e in changes
            if e.agent_run_id != agent_run_id
            and any(e.file_path.startswith(p.rstrip("/")) for p in scope_paths)
        )

    tc = context.metadata.get("task_center")
    dispatcher = context.metadata.get("dispatcher")
    if tc is not None:
        task_deps = set(context.metadata.get("task_deps", []))
        if task_deps:
            dep_notes = await tc.read(authors=list(task_deps), since=since)
            new_dep_notes = len(dep_notes)
    if dispatcher is not None and hasattr(dispatcher, "done_sibling_ids"):
        sibling_ids = await dispatcher.done_sibling_ids(
            task_id=task_id,
            parent_id=context.metadata.get("task_parent_id"),
            since=since,
        )
        if sibling_ids and scope_paths and hasattr(dispatcher, "get_task_by_id"):
            relevant = 0
            for sibling_id in sibling_ids:
                sibling = await dispatcher.get_task_by_id(sibling_id)
                sibling_scopes = list(getattr(sibling, "scope_paths", None) or [])
                if not sibling_scopes or any(
                    scope_paths_overlap(scope, sibling_scope)
                    for scope in scope_paths
                    for sibling_scope in sibling_scopes
                ):
                    relevant += 1
            new_sibling_completions = relevant
        else:
            new_sibling_completions = len(sibling_ids)

    return FreshnessReport(
        scope_changes_by_others=scope_changes,
        new_dep_notes=new_dep_notes,
        new_sibling_completions=new_sibling_completions,
    )
