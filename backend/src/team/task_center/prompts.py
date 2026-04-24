"""TaskContextBuilder — agent prompt context assembly for team tasks."""

from __future__ import annotations

import logging
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import TYPE_CHECKING

from team.core.models import Task, render_task_spec

if TYPE_CHECKING:
    from team.persistence.task_store import TaskStore

logger = logging.getLogger("team.task_center")


@dataclass(frozen=True)
class UserPromptContextParts:
    """Rendered context fragments used by markdown user prompt templates."""

    task_spec: str
    scope_paths: str = ""


class TaskContextBuilder:
    """Build injected context for a task from graph state and edits."""

    def __init__(
        self,
        *,
        team_run_id: str,
        get_task_fn: Callable[[str], Awaitable[Task | None]] | None = None,
        task_store: "TaskStore | None" = None,
    ) -> None:
        self._team_run_id = team_run_id
        self._get_task_fn = get_task_fn
        self._task_store = task_store

    @staticmethod
    def _is_replanner_task(task: Task) -> bool:
        agent_name = str(task.definition.agent or "").strip()
        if agent_name == "team_replanner":
            return True
        try:
            from agents.registry import get_role

            return get_role(agent_name) == "replanner"
        except Exception:
            logger.debug(
                "Failed to resolve agent role for replanner context: %s",
                agent_name,
                exc_info=True,
            )
            return False

    @staticmethod
    def _should_include_replanner_root_cause_trace(task: Task) -> bool:
        return bool(
            task.fired_by_task_id
            and TaskContextBuilder._is_replanner_task(task)
        )

    async def get_task(self, task_id: str) -> Task | None:
        if self._get_task_fn is not None:
            return await self._get_task_fn(task_id)
        if self._task_store is None:
            return None
        rec = await self._task_store.get_record(task_id)
        if rec is None:
            return None
        from team.persistence.task_store import record_to_task

        return record_to_task(rec)

    async def _tasks_depending_on(self, dep_id: str) -> list[Task]:
        graph = getattr(self._task_store, "graph", None)
        if isinstance(graph, dict) and graph:
            return [
                task
                for task in graph.values()
                if dep_id in [str(item) for item in (task.definition.deps or [])]
            ]
        if self._task_store is None or not hasattr(self._task_store, "get_all_tasks"):
            return []
        try:
            from team.persistence.task_store import record_to_task

            records = await self._task_store.get_all_tasks()
            return [
                record_to_task(record)
                for record in records
                if dep_id in [str(item) for item in (record.deps or [])]
            ]
        except Exception:
            logger.debug("Failed to read dependent tasks for %s", dep_id, exc_info=True)
            return []

    async def _replanner_root_cause_trace(self, task: Task) -> str | None:
        original_id = task.fired_by_task_id
        if not original_id:
            return None
        if not self._should_include_replanner_root_cause_trace(task):
            return None

        original = await self.get_task(original_id)
        lines = ["## Replan root cause trace", f"Original task: {original_id}"]
        if original is not None:
            orig_defn = original.definition
            lines.extend(
                [
                    f"Original agent: {orig_defn.agent}",
                    f"Original status: {original.status.value}",
                    f"Failed reason: {original.failure_reason or 'unknown'}",
                    "",
                    "### Original task spec",
                    render_task_spec(orig_defn.spec),
                ]
            )
            if orig_defn.description:
                lines.extend(["", "### Original description", orig_defn.description])
            lines.append("")
            lines.append(
                "Original scope paths: "
                + (", ".join(orig_defn.scope_paths) if orig_defn.scope_paths else "(none)")
            )
            lines.append(
                "Original deps: " + (", ".join(orig_defn.deps) if orig_defn.deps else "(none)")
            )
        else:
            lines.append("Failed reason: unknown")

        dependents = await self._tasks_depending_on(task.id)
        dependents = [item for item in dependents if item.id != task.id]
        if dependents:
            lines.extend(["", "### Downstream dependents rewired to this replanner"])
            for dependent in sorted(dependents, key=lambda item: item.id):
                dep_deps = dependent.definition.deps
                deps = ", ".join(dep_deps) if dep_deps else "(none)"
                lines.append(f"- {dependent.id} ({dependent.status.value}); deps: {deps}")
        else:
            lines.extend(["", "### Downstream dependents rewired to this replanner", "(none)"])

        return "\n".join(lines)

    async def context_for(
        self,
        task: Task,
        *,
        max_context_bytes: int = 200_000,
    ) -> str:
        """Build the injected context string for a task."""
        budget = max_context_bytes
        sections: list[str] = []
        defn = task.definition

        task_section = f"## Your task\n{render_task_spec(defn.spec)}"
        if defn.scope_paths:
            task_section += f"\n\nScope: {', '.join(defn.scope_paths)}"
        sections.append(task_section)
        budget -= len(task_section.encode())

        if self._should_include_replanner_root_cause_trace(task) and budget > 0:
            sec = await self._replanner_root_cause_trace(task)
            if sec:
                b = len(sec.encode())
                if b <= budget:
                    sections.append(sec)
                    budget -= b
                else:
                    safe = max(0, budget - len("\n...[truncated]".encode()))
                    sections.append(
                        sec.encode()[:safe].decode("utf-8", errors="ignore") + "\n...[truncated]"
                    )
                    budget = 0

        return "\n\n".join(sections)

    async def template_context_for(
        self,
        task: Task,
        *,
        max_context_bytes: int = 200_000,
    ) -> UserPromptContextParts:
        """Build context fragments for markdown-backed user prompt templates."""
        budget = max_context_bytes
        defn = task.definition
        task_spec = render_task_spec(defn.spec).strip()
        budget -= len(task_spec.encode())

        scope_paths = "\n".join(f"- {path}" for path in defn.scope_paths)
        budget -= len(scope_paths.encode())

        return UserPromptContextParts(
            task_spec=task_spec,
            scope_paths=scope_paths,
        )
