"""NoteManager — in-memory file-scoped note lifecycle management.

Notes describe paths and agent observations. Persistence of note events is
delegated to the event store callback.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from typing import TYPE_CHECKING

from team.core.scope import scope_paths_overlap
from team.core.models import Note

if TYPE_CHECKING:
    from team.persistence.events import TeamRunEvent

logger = logging.getLogger("team.task_center")


def _note_preview(content: str, *, limit: int = 240) -> str:
    compact = " ".join(content.split())
    if len(compact) <= limit:
        return compact
    return compact[: limit - 1] + "…"


class NoteManager:
    """In-memory file-scoped note lifecycle management."""

    def __init__(
        self,
        team_run_id: str,
        event_store_cb: Callable[[TeamRunEvent], None] | None = None,
        note_posted_cb: Callable[[Note], None] | None = None,
    ) -> None:
        self._notes: list[Note] = []
        self._team_run_id = team_run_id
        self._event_store_cb = event_store_cb
        self._note_posted_cb = note_posted_cb

    def snapshot(self) -> list[Note]:
        """Return a copy of all notes."""
        return list(self._notes)

    def restore(self, notes: list[Note]) -> None:
        """Replace in-memory notes with the given list."""
        self._notes = list(notes)

    async def post(self, note: Note) -> None:
        """Append a note and emit the posted event."""
        self._notes.append(note)
        preview = _note_preview(note.content)
        logger.info(
            "[task_center] note agent=%s scope=%s preview=%s",
            note.agent_name,
            ",".join(note.paths) if note.paths else "-",
            preview,
        )
        if self._event_store_cb is not None:
            from team.persistence.events import make_note_posted

            self._event_store_cb(
                make_note_posted(
                    self._team_run_id,
                    agent_name=note.agent_name,
                    scope_paths=note.paths,
                    content_preview=preview,
                    content_bytes=len(note.content.encode("utf-8")),
                )
            )
        if self._note_posted_cb is not None:
            self._note_posted_cb(note)

    async def read(
        self,
        *,
        paths: list[str] | None = None,
        keyword: str | None = None,
        since: float | None = None,
        last_n: int | None = None,
    ) -> list[Note]:
        """Filter and return notes by paths, keyword, timestamp, and last_n."""
        results = list(self._notes)
        if paths:
            normalized = [s.rstrip("/") for s in paths if s]
            results = [
                n for n in results
                if n.paths
                and any(scope_paths_overlap(np, qp) for np in n.paths for qp in normalized)
            ]
        if keyword:
            keywords = [k.strip().lower() for k in keyword.split("|") if k.strip()]
            if keywords:
                results = [n for n in results if any(kw in n.content.lower() for kw in keywords)]
        if since is not None:
            results = [n for n in results if n.timestamp >= since]
        if last_n is not None and last_n > 0:
            results = results[-last_n:]
        return results

    def known_paths(self) -> list[str]:
        """Return sorted unique paths across all notes (for validation errors)."""
        return sorted({p for n in self._notes for p in n.paths})
