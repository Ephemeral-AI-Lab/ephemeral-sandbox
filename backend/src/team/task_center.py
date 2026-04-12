"""Task Center — append-only shared context log.

Replaces ProjectContext, InMemoryArtifactStore, and the 3-tier briefing system.

When a NoteStore is attached, PostgreSQL is the source of truth: writes are
awaited and reads query the store directly so every worker sees the same note
set. The in-memory list is retained only for no-PG mode and local snapshots.
"""

from __future__ import annotations

import logging
import time
import uuid as _uuid
from typing import TYPE_CHECKING, Any

from team.models import Note

if TYPE_CHECKING:
    from code_intelligence.editing.arbiter import Arbiter
    from team.models import Task

logger = logging.getLogger(__name__)


class TaskCenter:
    """Append-only shared context log with optional PG-primary persistence."""

    def __init__(
        self,
        goal: str = "",
        user_request: str = "",
        note_store: Any = None,
        team_run_id: str = "",
    ) -> None:
        self._notes: list[Note] = []
        self.goal = goal
        self.user_request = user_request
        self._note_store = note_store  # NoteStore | NullNoteStore | None
        self._team_run_id = team_run_id

    def _store_backed(self) -> bool:
        return self._note_store is not None and getattr(self._note_store, "initialized", False)

    @staticmethod
    def _matches_scope(note_scopes: list[str], query_scopes: list[str]) -> bool:
        if not note_scopes:
            return True
        normalized_queries = [scope.rstrip("/") for scope in query_scopes]
        return any(
            note_scope.startswith(query_scope)
            for note_scope in note_scopes
            for query_scope in normalized_queries
        )

    @staticmethod
    def _note_from_record(record: Any) -> Note:
        created_at = getattr(record, "created_at", None)
        timestamp = (
            created_at.timestamp()
            if created_at is not None and hasattr(created_at, "timestamp")
            else time.time()
        )
        return Note(
            id=str(record.id),
            task_id=record.task_id,
            agent_name=record.agent_name,
            content=record.content,
            timestamp=timestamp,
            scope_paths=list(record.scope_ltree) if getattr(record, "scope_ltree", None) else [],
        )

    async def post(self, note: Note) -> None:
        """Append a note to the active backing store."""
        if not self._store_backed():
            self._notes.append(note)
            return
        try:
            from team.persistence.task_note_record import TaskNoteRecord

            record = TaskNoteRecord(
                id=_uuid.UUID(note.id) if note.id else _uuid.uuid4(),
                team_run_id=self._team_run_id,
                task_id=note.task_id,
                agent_name=note.agent_name,
                content=note.content,
                scope_ltree=list(note.scope_paths) if note.scope_paths else [],
            )
            await self._note_store.insert(record)
        except Exception:
            logger.debug("Failed to persist note %s", note.id, exc_info=True)
            raise

    async def read(
        self,
        *,
        authors: list[str] | None = None,
        scope_paths: list[str] | None = None,
        since: float | None = None,
        limit: int | None = None,
    ) -> list[Note]:
        """Read notes using the configured backing store."""
        if self._store_backed():
            try:
                records = await self._note_store.query(
                    self._team_run_id,
                    task_ids=authors,
                    scope_paths=scope_paths,
                    since=since,
                    limit=limit,
                )
                return [self._note_from_record(record) for record in records]
            except Exception:
                logger.debug("Failed to read notes from PG", exc_info=True)
                raise

        results = list(self._notes)
        if authors:
            author_set = set(authors)
            results = [n for n in results if n.task_id in author_set]
        if scope_paths:
            results = [n for n in results if self._matches_scope(n.scope_paths, scope_paths)]
        if since is not None:
            results = [n for n in results if n.timestamp >= since]
        if limit is not None and limit > 0:
            results = results[-limit:]
        return results

    async def context_for(
        self,
        task: "Task",
        *,
        arbiter: "Arbiter | None" = None,
        max_context_bytes: int = 200_000,
    ) -> str:
        """Build context string for a task. Fixed priority order:
        task (never trimmed) -> deps -> file changes -> parent chain."""
        budget = max_context_bytes
        sections: list[str] = []

        # Priority 1: The task itself (never trimmed)
        task_section = f"## Your task\n{task.task}"
        if task.scope_paths:
            task_section += f"\n\nScope: {', '.join(task.scope_paths)}"
        sections.append(task_section)
        budget -= len(task_section.encode())

        # Priority 2: Dep notes (direct deps only, not transitive)
        # Deduplicate to latest note per dep — many notes from one dep
        # would bloat context; we only care about the most recent summary
        if task.deps and budget > 0:
            dep_notes = await self.read(authors=task.deps)
            if dep_notes:
                by_dep: dict[str, Note] = {}
                for n in dep_notes:
                    by_dep[n.task_id] = n
                deduped = list(by_dep.values())
                dep_section = self._render_notes("Context from dependencies", deduped)
                dep_bytes = len(dep_section.encode())
                if dep_bytes <= budget:
                    sections.append(dep_section)
                    budget -= dep_bytes
                else:
                    sections.append(
                        self._truncate_section("Context from dependencies", deduped, budget)
                    )
                    budget = 0

        # Priority 3: Recent file changes in scope (ground truth from Arbiter)
        if arbiter is not None and budget > 0 and task.scope_paths:
            created_ts = task.created_at.timestamp() if task.created_at else 0.0
            changes = arbiter.changes_since(created_ts)
            scoped = [
                e
                for e in changes
                if any(e.file_path.startswith(p.rstrip("/")) for p in task.scope_paths)
            ]
            if scoped:
                now = time.time()
                lines = [
                    f"- {e.file_path} ({e.edit_type} by {e.agent_id}, "
                    f"{int(now - e.timestamp)}s ago)"
                    for e in scoped
                ]
                change_section = "## Recent changes in your scope\n" + "\n".join(lines)
                change_bytes = len(change_section.encode())
                if change_bytes <= budget:
                    sections.append(change_section)
                    budget -= change_bytes

        # Priority 4: Parent chain (why this task exists)
        if task.parent_id and budget > 0:
            parent_notes = await self.read(authors=[task.parent_id])
            if parent_notes:
                parent_section = self._render_notes("Parent context", parent_notes)
                parent_bytes = len(parent_section.encode())
                if parent_bytes <= budget:
                    sections.append(parent_section)
                    budget -= parent_bytes
                else:
                    sections.append(self._truncate_section("Parent context", parent_notes, budget))

        return "\n\n".join(sections)

    def snapshot(self) -> list[Note]:
        return list(self._notes)

    def restore(self, notes: list[Note]) -> None:
        self._notes = list(notes)

    def _render_notes(self, header: str, notes: list[Note]) -> str:
        lines = [f"## {header}"]
        for n in notes:
            lines.append(f"### {n.agent_name} ({n.task_id})")
            lines.append(n.content)
        return "\n".join(lines)

    def _truncate_section(self, header: str, notes: list[Note], budget: int) -> str:
        sep = "\n"
        header_line = f"## {header}"
        remaining = budget - len(header_line.encode()) - len(sep.encode())
        lines = [header_line]
        for n in notes:
            entry = f"### {n.agent_name} ({n.task_id})\n{n.content}"
            # Account for the separator that join() will insert before this entry
            entry_cost = len(entry.encode()) + len(sep.encode())
            if entry_cost <= remaining:
                lines.append(entry)
                remaining -= entry_cost
            else:
                safe_bytes = max(
                    0, remaining - len(sep.encode()) - len("\n...[truncated]".encode())
                )
                truncated = entry.encode()[:safe_bytes].decode("utf-8", errors="ignore")
                lines.append(truncated + "\n...[truncated]")
                break
        return sep.join(lines)
