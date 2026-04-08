"""Tier 3 — ChangeLog of file mutations across WorkItems in one TeamRun."""

from __future__ import annotations

import threading
from dataclasses import dataclass, field
from datetime import datetime
from typing import TYPE_CHECKING, Any

from team.types import _utcnow

if TYPE_CHECKING:
    from team.run import TeamRun


# File-editing tool names we treat as "file change" events.
FILE_EDIT_TOOL_NAMES: frozenset[str] = frozenset(
    {
        "str_replace_based_edit_tool",
        "create_file",
        "write_file",
        "edit_file",
        "str_replace_editor",
    }
)


@dataclass
class ChangeLogEntry:
    work_item_id: str
    agent_run_id: str | None
    filepath: str
    timestamp: datetime = field(default_factory=_utcnow)


class ChangeLog:
    """Append-only file-change tracker scoped to a single TeamRun."""

    def __init__(self) -> None:
        self._entries: list[ChangeLogEntry] = []
        self._lock = threading.Lock()

    def append(self, entry: ChangeLogEntry) -> None:
        with self._lock:
            self._entries.append(entry)

    def since(
        self,
        ts: datetime | None,
        exclude_work_item_id: str | None = None,
    ) -> list[ChangeLogEntry]:
        with self._lock:
            return [
                e
                for e in self._entries
                if (ts is None or e.timestamp >= ts)
                and (exclude_work_item_id is None or e.work_item_id != exclude_work_item_id)
            ]

    def all(self) -> list[ChangeLogEntry]:
        with self._lock:
            return list(self._entries)

    def restore(self, entries: list[ChangeLogEntry]) -> None:
        with self._lock:
            self._entries = list(entries)


# ---------------------------------------------------------------------------
# Process-wide registry of active TeamRuns for hook routing
# ---------------------------------------------------------------------------

_active_team_runs: dict[str, "TeamRun"] = {}
_registry_lock = threading.Lock()


def register_team_run(team_run: "TeamRun") -> None:
    with _registry_lock:
        _active_team_runs[team_run.id] = team_run


def unregister_team_run(team_run_id: str) -> None:
    with _registry_lock:
        _active_team_runs.pop(team_run_id, None)


def get_active_team_run(team_run_id: str) -> "TeamRun | None":
    with _registry_lock:
        return _active_team_runs.get(team_run_id)


def record_file_edit_from_hook_payload(payload: dict[str, Any]) -> bool:
    """POST_TOOL_USE hook subscriber entry point."""
    tool_name = payload.get("tool_name")
    if tool_name not in FILE_EDIT_TOOL_NAMES:
        return False
    team_ctx = payload.get("team_context") or {}
    team_run_id = team_ctx.get("team_run_id")
    if not team_run_id:
        return False
    team_run = get_active_team_run(team_run_id)
    if team_run is None:
        return False

    tool_input = payload.get("tool_input") or {}
    filepath = (
        tool_input.get("path")
        or tool_input.get("file_path")
        or tool_input.get("filename")
        or ""
    )
    if not filepath:
        return False

    team_run.change_log.append(
        ChangeLogEntry(
            work_item_id=team_ctx.get("work_item_id", ""),
            agent_run_id=team_ctx.get("agent_run_id"),
            filepath=str(filepath),
        )
    )
    return True
