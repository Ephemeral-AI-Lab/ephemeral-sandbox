"""Event schema for TeamRun persistence.

Events are append-only and self-describing. ``TeamRunEvent.to_json`` /
``from_json`` form the wire format shared by every ``TeamRunStore``
implementation (jsonl file, SQL row, in-memory null sink).
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from typing import Any, Literal

EventKind = Literal[
    "team_run_created",
    "team_run_status",
    "task_added",
    "task_status",
    "note_posted",
    "budget_update",
    "checkpoint_taken",
    "checkpoint_repo_state",
    "file_changed",
]


def _utcnow_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


@dataclass
class TeamRunEvent:
    team_run_id: str
    kind: EventKind
    data: dict[str, Any] = field(default_factory=dict)
    ts: str = field(default_factory=_utcnow_iso)
    seq: int = 0

    def to_json(self) -> dict[str, Any]:
        return asdict(self)

    @classmethod
    def from_json(cls, obj: dict[str, Any]) -> "TeamRunEvent":
        return cls(
            team_run_id=obj["team_run_id"],
            kind=obj["kind"],
            data=dict(obj.get("data") or {}),
            ts=obj.get("ts") or _utcnow_iso(),
            seq=int(obj.get("seq") or 0),
        )


# ---- canonical payload builders -----------------------------------------


def make_team_run_created(
    team_run_id: str, *, session_id: str, user_request: str,
    goal: str | None, repo_root: str | None, sandbox_id: str | None = None,
    budgets: dict[str, Any], roster: dict[str, list[str]] | None = None,
) -> TeamRunEvent:
    data: dict[str, Any] = {
        "session_id": session_id, "user_request": user_request,
        "goal": goal, "repo_root": repo_root, "sandbox_id": sandbox_id,
        "budgets": budgets,
    }
    if roster:
        data["roster"] = roster
    return TeamRunEvent(team_run_id=team_run_id, kind="team_run_created", data=data)


def make_team_run_status(team_run_id: str, status: str, **fields: Any) -> TeamRunEvent:
    payload: dict[str, Any] = {"status": status}
    payload.update(fields)
    return TeamRunEvent(team_run_id=team_run_id, kind="team_run_status", data=payload)


def make_task_added(team_run_id: str, task: dict[str, Any]) -> TeamRunEvent:
    return TeamRunEvent(team_run_id=team_run_id, kind="task_added", data={"task": task})


def make_task_status(
    team_run_id: str, task_id: str, status: str, **fields: Any,
) -> TeamRunEvent:
    payload: dict[str, Any] = {"task_id": task_id, "status": status}
    payload.update(fields)
    return TeamRunEvent(team_run_id=team_run_id, kind="task_status", data=payload)


def make_note_posted(
    team_run_id: str,
    *,
    task_id: str,
    agent_name: str,
    auto: bool,
    scope_paths: list[str] | None,
    content_preview: str,
    content_bytes: int,
) -> TeamRunEvent:
    return TeamRunEvent(
        team_run_id=team_run_id,
        kind="note_posted",
        data={
            "task_id": task_id,
            "agent_name": agent_name,
            "auto": auto,
            "scope_paths": list(scope_paths or []),
            "content_preview": content_preview,
            "content_bytes": content_bytes,
        },
    )


def make_budget_update(
    team_run_id: str, *, tasks_used: int, note_bytes_used: int, replans_used: int = 0,
) -> TeamRunEvent:
    return TeamRunEvent(
        team_run_id=team_run_id, kind="budget_update",
        data={"tasks_used": tasks_used, "note_bytes_used": note_bytes_used, "replans_used": replans_used},
    )


def make_checkpoint_taken(
    team_run_id: str, *, checkpoint_id: str, sequence: int, label: str | None,
) -> TeamRunEvent:
    return TeamRunEvent(
        team_run_id=team_run_id, kind="checkpoint_taken",
        data={"checkpoint_id": checkpoint_id, "sequence": sequence, "label": label},
    )


def make_checkpoint_repo_state(
    team_run_id: str, *, checkpoint_id: str, repo_patch: str,
) -> TeamRunEvent:
    return TeamRunEvent(
        team_run_id=team_run_id, kind="checkpoint_repo_state",
        data={"checkpoint_id": checkpoint_id, "repo_patch": repo_patch,
              "repo_patch_bytes": len(repo_patch.encode("utf-8"))},
    )


def make_file_changed(
    team_run_id: str, *, task_id: str | None, path: str, op: str,
) -> TeamRunEvent:
    return TeamRunEvent(
        team_run_id=team_run_id, kind="file_changed",
        data={"task_id": task_id, "path": path, "op": op},
    )


# ---- serialisation helpers -----------------------------------------------


def task_to_dict(task: Any) -> dict[str, Any]:
    """Serialise a ``Task`` dataclass to a JSON-safe dict."""
    from team.models import Task
    assert isinstance(task, Task)
    return {
        "id": task.id, "team_run_id": task.team_run_id,
        "agent_name": task.agent_name, "status": task.status.value,
        "task": task.task, "deps": list(task.deps),
        "scope_paths": list(task.scope_paths),
        "cascade_policy": task.cascade_policy, "parent_id": task.parent_id,
        "root_id": task.root_id, "depth": task.depth,
        "retry_count": task.retry_count, "max_retries": task.max_retries,
        "agent_run_id": task.agent_run_id,
        "created_at": task.created_at.isoformat() if task.created_at else None,
        "started_at": task.started_at.isoformat() if task.started_at else None,
        "finished_at": task.finished_at.isoformat() if task.finished_at else None,
        "failure_reason": task.failure_reason,
    }
