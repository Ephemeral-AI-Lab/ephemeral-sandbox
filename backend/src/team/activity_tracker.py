"""ActivityTracker — edit/turn counting and auto note triggering.

Extracted from TaskCenter. Tracks per-task edit counts, turn counts,
and decides when to fire auto notes.
"""

from __future__ import annotations

import logging
import time
import uuid
from typing import Any, Callable

from team.models import Note, NoteTag

logger = logging.getLogger(__name__)


class ActivityTracker:
    """Tracks edit/turn activity per task and triggers auto notes."""

    def __init__(
        self,
        team_run_id: str,
        note_posted_cb: Callable[[Note], None] | None = None,
        graph_getter: Callable[[], dict[str, Any]] | None = None,
        post_note_cb: Callable[[Note], Any] | None = None,
    ) -> None:
        self._activity_counters: dict[str, dict[str, Any]] = {}
        self._note_inflight: set[str] = set()
        self._note_snapshots: dict[str, dict[str, int]] = {}
        self._team_run_id = team_run_id
        self._note_posted_cb = note_posted_cb
        self._graph_getter = graph_getter
        self._post_note_cb = post_note_cb
        self._skipped_resets: dict[str, int] = {
            "system_or_checkpoint_sender": 0,
            "no_counters": 0,
        }

    def _get_counters(self, task_id: str) -> dict[str, Any]:
        if task_id not in self._activity_counters:
            self._activity_counters[task_id] = {"edits": 0, "turns": 0, "edit_history": []}
        return self._activity_counters[task_id]

    def on_edit(self, task_id: str, file_path: str) -> None:
        c = self._get_counters(task_id)
        c["edits"] += 1
        c["edit_history"].append(file_path)
        c["turns"] = 0

    def on_posthook(self, task_id: str) -> None:
        self._get_counters(task_id)["turns"] = 0

    def tick(self, task_id: str) -> None:
        self._get_counters(task_id)["turns"] += 1

    def on_note_posted(self, note: Note) -> None:
        if note.agent_name in {"system", "checkpoint"}:
            self._skipped_resets["system_or_checkpoint_sender"] += 1
            return
        self._note_inflight.discard(note.task_id)
        if note.task_id not in self._activity_counters:
            self._skipped_resets["no_counters"] += 1
            return
        c = self._activity_counters[note.task_id]
        snapshot = self._note_snapshots.pop(note.task_id, None)
        if snapshot is None:
            self._activity_counters[note.task_id] = {"edits": 0, "turns": 0, "edit_history": []}
            return
        c["edits"] = max(0, c["edits"] - snapshot.get("edits", 0))
        c["turns"] = max(0, c["turns"] - snapshot.get("turns", 0))
        covered_history = snapshot.get("edit_history_len", 0)
        if covered_history > 0:
            c["edit_history"] = c["edit_history"][covered_history:]

    def metrics(self) -> dict[str, int]:
        """Return counters for skipped auto-resets (telemetry)."""
        return dict(self._skipped_resets)

    def should_take_note(self, task_id: str) -> str | None:
        if task_id in self._note_inflight:
            return None
        c = self._get_counters(task_id)
        if c["edits"] >= 5:
            return "edit"
        if c["turns"] >= 15:
            return "turn"
        return None

    @staticmethod
    def _recent_unique_files(edit_history: list[str], *, limit: int = 10) -> list[str]:
        ordered: list[str] = []
        seen: set[str] = set()
        for path in reversed(edit_history):
            if path in seen:
                continue
            seen.add(path)
            ordered.append(path)
            if len(ordered) >= limit:
                break
        ordered.reverse()
        return ordered

    async def check(
        self,
        task_id: str,
        *,
        snapshot: list[dict] | None = None,
        api_client: Any = None,
        model: str | None = None,
    ) -> bool:
        graph = self._graph_getter() if self._graph_getter is not None else {}
        task = graph.get(task_id)
        agent_name = task.agent_name if task else "unknown"
        scope_paths = list(task.scope_paths) if task and task.scope_paths else []
        agent_run_id = task.agent_run_id if task else task_id
        post_note_cb = self._post_note_cb
        trigger = self.should_take_note(task_id)
        if trigger is None:
            return False
        self._note_inflight.add(task_id)
        c = self._get_counters(task_id)
        counter_snapshot = {
            "edits": int(c["edits"]),
            "turns": int(c["turns"]),
            "edit_history_len": len(c["edit_history"]),
        }
        self._note_snapshots[task_id] = counter_snapshot

        logger.info(
            "[activity_tracker] auto-note trigger=%s task=%s agent=%s edits=%d turns=%d scope=%s",
            trigger,
            task_id,
            agent_name,
            counter_snapshot["edits"],
            counter_snapshot["turns"],
            ",".join(scope_paths) if scope_paths else "-",
        )

        content: str | None = None
        note_tags: list[str] | None = None
        posted = False
        try:
            if api_client and snapshot is not None:
                from external_trigger.tc_note import (
                    TC_NOTE_EDIT_PROMPT,
                    TC_NOTE_TURN_PROMPT,
                    run_tc_note,
                )

                prompt = TC_NOTE_EDIT_PROMPT if trigger == "edit" else TC_NOTE_TURN_PROMPT
                try:
                    result = await run_tc_note(
                        task_id=task_id,
                        agent_run_id=agent_run_id or task_id,
                        messages=snapshot or [],
                        prompt=prompt,
                        trigger=trigger,
                        max_tokens=500,
                        model=model,
                        api_client=api_client,
                    )
                except Exception:
                    logger.warning(
                        "[activity_tracker] tc_note generation failed for task=%s trigger=%s; falling back to factual note",
                        task_id,
                        trigger,
                        exc_info=True,
                    )
                else:
                    if result.content:
                        content = result.content
                    if result.tags:
                        note_tags = result.tags
                    if result.paths:
                        scope_paths = list(set(scope_paths + result.paths))

            if content is None:
                if trigger == "edit":
                    files = ", ".join(
                        self._recent_unique_files(
                            c["edit_history"][: counter_snapshot["edit_history_len"]],
                        ),
                    )
                    content = f"Auto note ({counter_snapshot['edits']} edits): {files}"
                else:
                    content = (
                        f"Auto note: {counter_snapshot['turns']} turns without progress note"
                    )

            note = Note(
                id=str(uuid.uuid4()),
                task_id=task_id,
                agent_name=f"{agent_name} (auto)",
                content=content,
                timestamp=time.time(),
                paths=scope_paths,
                tags=note_tags or [NoteTag.IMPLEMENTATION.value],
            )
            if post_note_cb is not None:
                await post_note_cb(note)
            if self._note_posted_cb is not None:
                self._note_posted_cb(note)
            posted = True
            return True
        finally:
            self._note_inflight.discard(task_id)
            if not posted:
                self._note_snapshots.pop(task_id, None)
