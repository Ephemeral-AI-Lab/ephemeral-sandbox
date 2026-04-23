"""TaskContextBuilder — agent prompt context assembly for team tasks."""

from __future__ import annotations

import logging
import time
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from code_intelligence.editing.change_labels import change_actor_label
from team._path_utils import ScopePath
from team.models import Note, Task
from team.note_manager import NoteManager

if TYPE_CHECKING:
    from team.persistence.task_store import TaskStore

logger = logging.getLogger("team.task_center")


@dataclass(frozen=True)
class UserPromptContextParts:
    """Rendered context fragments used by markdown user prompt templates."""

    task_spec: str
    scope_paths: str = ""
    context_from_dependencies: str = ""
    recent_scope_changes: str = ""
    parent_context: str = ""
    failure_context: str = ""


class TaskContextBuilder:
    """Build injected context for a task from notes, graph state, and edits."""

    def __init__(
        self,
        *,
        team_run_id: str,
        notes: NoteManager,
        get_task_fn: Callable[[str], Awaitable[Task | None]] | None = None,
        task_store: "TaskStore | None" = None,
        arbiter: Any = None,
    ) -> None:
        self._team_run_id = team_run_id
        self._notes = notes
        self._get_task_fn = get_task_fn
        self._task_store = task_store
        self._arbiter = arbiter

    @staticmethod
    def _render_notes(header: str, notes: list[Note]) -> str:
        lines = [f"## {header}"]
        for n in notes:
            lines.append(f"### {n.agent_name} ({n.task_id})")
            lines.append(n.content)
        return "\n".join(lines)

    @staticmethod
    def _render_notes_body(notes: list[Note]) -> str:
        lines: list[str] = []
        for note in notes:
            lines.append(f"### {note.agent_name} ({note.task_id})")
            lines.append(note.content)
        return "\n".join(lines)

    @staticmethod
    def _preferred_notes_per_task(notes: list[Note]) -> list[Note]:
        preferred: dict[str, Note] = {}
        for note in notes:
            preferred[note.task_id] = note
        return list(preferred.values())

    @staticmethod
    def _scope_relevant_notes(notes: list[Note], scope_paths: list[str]) -> list[Note]:
        if not scope_paths:
            return []
        return [
            note
            for note in notes
            if note.paths and ScopePath.matches_scopes(note.paths, scope_paths)
        ]

    @staticmethod
    def _parent_context_notes(notes: list[Note], scope_paths: list[str]) -> list[Note]:
        scoped = TaskContextBuilder._scope_relevant_notes(notes, scope_paths)
        return TaskContextBuilder._preferred_notes_per_task(scoped or notes)

    @staticmethod
    def _is_replanner_task(task: Task) -> bool:
        agent_name = str(task.agent_name or "").strip()
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
    def _should_include_replanner_failure_context(task: Task) -> bool:
        return bool(
            task.fired_by_task_id
            and TaskContextBuilder._is_replanner_task(task)
        )

    @staticmethod
    def _truncate_section(header: str, notes: list[Note], budget: int) -> str:
        sep = "\n"
        header_line = f"## {header}"
        remaining = budget - len(header_line.encode()) - len(sep.encode())
        lines = [header_line]
        for n in notes:
            entry = f"### {n.agent_name} ({n.task_id})\n{n.content}"
            cost = len(entry.encode()) + len(sep.encode())
            if cost <= remaining:
                lines.append(entry)
                remaining -= cost
                continue
            safe = max(0, remaining - len(sep.encode()) - len("\n...[truncated]".encode()))
            lines.append(entry.encode()[:safe].decode("utf-8", errors="ignore") + "\n...[truncated]")
            break
        return sep.join(lines)

    @staticmethod
    def _truncate_text(text: str, budget: int) -> str:
        if len(text.encode()) <= budget:
            return text
        suffix = "\n...[truncated]"
        safe = max(0, budget - len(suffix.encode()))
        return text.encode()[:safe].decode("utf-8", errors="ignore") + suffix

    @staticmethod
    def _truncate_notes_body(notes: list[Note], budget: int) -> str:
        sep = "\n"
        remaining = budget
        lines: list[str] = []
        for note in notes:
            entry = f"### {note.agent_name} ({note.task_id})\n{note.content}"
            cost = len(entry.encode()) + len(sep.encode())
            if cost <= remaining:
                lines.append(entry)
                remaining -= cost
                continue
            lines.append(TaskContextBuilder._truncate_text(entry, remaining))
            break
        return sep.join(lines)

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

    async def _parent_chain_ids(self, task: Task) -> list[str]:
        """Walk up the parent chain collecting all ancestor task IDs."""
        if task.parent_id is None:
            return []
        parent_ids: list[str] = []
        seen: set[str] = set()
        current_id: str | None = task.parent_id
        while current_id and current_id not in seen:
            parent_ids.append(current_id)
            seen.add(current_id)
            parent = await self.get_task(current_id)
            current_id = parent.parent_id if parent is not None else None
        return parent_ids

    async def _tasks_depending_on(self, dep_id: str) -> list[Task]:
        graph = getattr(self._task_store, "graph", None)
        if isinstance(graph, dict) and graph:
            return [
                task
                for task in graph.values()
                if dep_id in [str(item) for item in (task.deps or [])]
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

    async def _replanner_failure_context(self, task: Task) -> str | None:
        original_id = task.fired_by_task_id
        if not original_id:
            return None
        if not self._should_include_replanner_failure_context(task):
            return None

        original = await self.get_task(original_id)
        lines = ["## Replan root cause trace", f"Original task: {original_id}"]
        if original is not None:
            lines.extend(
                [
                    f"Original agent: {original.agent_name}",
                    f"Original status: {original.status.value}",
                    f"Failed reason: {original.failure_reason or 'unknown'}",
                    "",
                    "### Original task spec",
                    original.objective,
                ]
            )
            if original.description:
                lines.extend(["", "### Original description", original.description])
            lines.append("")
            lines.append(
                "Original scope paths: "
                + (", ".join(original.scope_paths) if original.scope_paths else "(none)")
            )
            lines.append(
                "Original deps: " + (", ".join(original.deps) if original.deps else "(none)")
            )
        else:
            lines.append("Failed reason: unknown")

        failed_notes = await self._notes.read(authors=[original_id])
        if failed_notes:
            lines.extend(["", "### Failed task notes"])
            for note in failed_notes[-3:]:
                lines.append(f"- {note.agent_name}: {note.content}")

        original_deps = list(original.deps) if original is not None else []
        if original_deps:
            dep_notes = await self._notes.read(authors=original_deps)
            dep_notes = self._preferred_notes_per_task(dep_notes)
            if dep_notes:
                lines.extend(["", "### Original dependency notes"])
                for note in dep_notes:
                    lines.append(f"- {note.task_id} / {note.agent_name}: {note.content}")

        dependents = await self._tasks_depending_on(task.id)
        dependents = [item for item in dependents if item.id != task.id]
        if dependents:
            lines.extend(["", "### Downstream dependents rewired to this replanner"])
            for dependent in sorted(dependents, key=lambda item: item.id):
                deps = ", ".join(dependent.deps) if dependent.deps else "(none)"
                lines.append(f"- {dependent.id} ({dependent.status.value}); deps: {deps}")
        else:
            lines.extend(["", "### Downstream dependents rewired to this replanner", "(none)"])

        return "\n".join(lines)

    async def context_for(
        self,
        task: Task,
        *,
        max_context_bytes: int = 200_000,
        arbiter: Any = None,
    ) -> str:
        """Build the injected context string for a task."""
        if arbiter is None:
            arbiter = self._arbiter

        budget = max_context_bytes
        sections: list[str] = []

        task_section = f"## Your task\n{task.objective}"
        if task.scope_paths:
            task_section += f"\n\nScope: {', '.join(task.scope_paths)}"
        sections.append(task_section)
        budget -= len(task_section.encode())

        if self._should_include_replanner_failure_context(task) and budget > 0:
            sec = await self._replanner_failure_context(task)
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

        if task.deps and budget > 0:
            dep_notes = await self._notes.read(authors=task.deps)
            if dep_notes:
                deduped = self._preferred_notes_per_task(dep_notes)
                sec = self._render_notes("Context from dependencies", deduped)
                b = len(sec.encode())
                if b <= budget:
                    sections.append(sec)
                    budget -= b
                else:
                    sections.append(
                        self._truncate_section("Context from dependencies", deduped, budget)
                    )
                    budget = 0

        history = arbiter
        if (
            history is not None
            and getattr(history, "initialized", False)
            and budget > 0
            and task.scope_paths
        ):
            created_ts = task.created_at.timestamp() if task.created_at else 0.0
            changes = history.changes_since(created_ts, team_run_id=self._team_run_id)
            scoped = [
                e
                for e in changes
                if ScopePath.matches_scopes([str(e.file_path)], task.scope_paths)
            ]
            if scoped:
                now = time.time()
                lines = [
                    f"- {e.file_path} ({e.edit_type} by {change_actor_label(e)}, "
                    f"{int(now - e.created_at.timestamp())}s ago)"
                    for e in scoped
                ]
                sec = "## Recent changes in your scope\n" + "\n".join(lines)
                b = len(sec.encode())
                if b <= budget:
                    sections.append(sec)
                    budget -= b

        if task.parent_id and budget > 0:
            parent_ids = await self._parent_chain_ids(task)
            parent_notes = await self._notes.read(authors=parent_ids)
            if parent_notes:
                deduped = self._parent_context_notes(parent_notes, task.scope_paths)
                sec = self._render_notes("Parent context", deduped)
                b = len(sec.encode())
                if b <= budget:
                    sections.append(sec)
                    budget -= b
                else:
                    sections.append(self._truncate_section("Parent context", deduped, budget))

        return "\n\n".join(sections)

    async def template_context_for(
        self,
        task: Task,
        *,
        max_context_bytes: int = 200_000,
        arbiter: Any = None,
    ) -> UserPromptContextParts:
        """Build context fragments for markdown-backed user prompt templates."""
        if arbiter is None:
            arbiter = self._arbiter

        budget = max_context_bytes
        task_spec = task.objective.strip()
        budget -= len(task_spec.encode())

        scope_paths = "\n".join(f"- {path}" for path in task.scope_paths)
        budget -= len(scope_paths.encode())

        failure_context = ""
        if self._should_include_replanner_failure_context(task) and budget > 0:
            sec = await self._replanner_failure_context(task)
            if sec:
                failure_context = self._truncate_text(sec, budget)
                budget -= len(failure_context.encode())

        context_from_dependencies = ""
        if task.deps and budget > 0:
            dep_notes = await self._notes.read(authors=task.deps)
            if dep_notes:
                deduped = self._preferred_notes_per_task(dep_notes)
                sec = self._render_notes_body(deduped)
                if len(sec.encode()) <= budget:
                    context_from_dependencies = sec
                    budget -= len(sec.encode())
                else:
                    context_from_dependencies = self._truncate_notes_body(deduped, budget)
                    budget = 0

        recent_scope_changes = ""
        history = arbiter
        if (
            history is not None
            and getattr(history, "initialized", False)
            and budget > 0
            and task.scope_paths
        ):
            created_ts = task.created_at.timestamp() if task.created_at else 0.0
            changes = history.changes_since(created_ts, team_run_id=self._team_run_id)
            scoped = [
                e
                for e in changes
                if ScopePath.matches_scopes([str(e.file_path)], task.scope_paths)
            ]
            if scoped:
                now = time.time()
                lines = [
                    f"- {e.file_path} ({e.edit_type} by {change_actor_label(e)}, "
                    f"{int(now - e.created_at.timestamp())}s ago)"
                    for e in scoped
                ]
                sec = "\n".join(lines)
                if len(sec.encode()) <= budget:
                    recent_scope_changes = sec
                    budget -= len(sec.encode())

        parent_context = ""
        if task.parent_id and budget > 0:
            parent_ids = await self._parent_chain_ids(task)
            parent_notes = await self._notes.read(authors=parent_ids)
            if parent_notes:
                deduped = self._parent_context_notes(parent_notes, task.scope_paths)
                sec = self._render_notes_body(deduped)
                if len(sec.encode()) <= budget:
                    parent_context = sec
                else:
                    parent_context = self._truncate_notes_body(deduped, budget)

        return UserPromptContextParts(
            task_spec=task_spec,
            scope_paths=scope_paths,
            context_from_dependencies=context_from_dependencies,
            recent_scope_changes=recent_scope_changes,
            parent_context=parent_context,
            failure_context=failure_context,
        )
