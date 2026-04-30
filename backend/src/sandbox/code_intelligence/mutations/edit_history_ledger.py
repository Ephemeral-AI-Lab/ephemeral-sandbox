"""In-memory edit-history ledger used behind the Arbiter facade."""

from __future__ import annotations

import time
from collections import Counter
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone


def _utcnow() -> datetime:
    return datetime.now(timezone.utc)


@dataclass
class EditRecord:
    """Durable-ish in-memory record of a file edit."""

    file_path: str
    run_id: str = ""
    agent_run_id: str = ""
    task_id: str = ""
    edit_type: str = "edit"
    old_hash: str = ""
    new_hash: str = ""
    description: str = ""
    created_at: datetime = field(default_factory=_utcnow)
    id: int = 0

    def __repr__(self) -> str:
        return (
            f"<EditRecord {self.file_path!r} "
            f"run={self.run_id!r} task={self.task_id!r} type={self.edit_type!r}>"
        )


@dataclass(frozen=True)
class ContentionHotspot:
    file_path: str
    contributor_count: int
    edit_count: int


class EditHistoryLedger:
    """In-memory queryable edit history."""

    initialized: bool = True

    def __init__(self) -> None:
        self._records: list[EditRecord] = []
        self._next_id = 1

    def record(
        self,
        *,
        run_id: str,
        file_path: str,
        agent_run_id: str = "",
        task_id: str = "",
        edit_type: str = "edit",
        old_hash: str = "",
        new_hash: str = "",
        description: str = "",
    ) -> EditRecord:
        rec = EditRecord(
            id=self._next_id,
            run_id=run_id,
            file_path=file_path,
            agent_run_id=agent_run_id,
            task_id=task_id,
            edit_type=edit_type,
            old_hash=old_hash,
            new_hash=new_hash,
            description=description,
        )
        self._next_id += 1
        self._records.append(rec)
        return rec

    def changes_in_scope(
        self,
        run_id: str,
        scope_prefixes: list[str],
        since: float,
    ) -> list[EditRecord]:
        if not scope_prefixes:
            return []
        cutoff = datetime.fromtimestamp(since, tz=timezone.utc)
        normalized = [p.rstrip("/") for p in scope_prefixes]
        return [
            r for r in self._records
            if r.run_id == run_id
            and r.created_at > cutoff
            and any(r.file_path.startswith(prefix) for prefix in normalized)
        ]

    def external_changes_in_scope(
        self,
        run_id: str,
        scope_prefixes: list[str],
        since: float,
        exclude_run_id: str | None = None,
    ) -> list[EditRecord]:
        changes = self.changes_in_scope(run_id, scope_prefixes, since)
        if exclude_run_id:
            changes = [c for c in changes if c.agent_run_id != exclude_run_id]
        return changes

    def changes_since(
        self,
        since: float,
        run_id: str | None = None,
    ) -> list[EditRecord]:
        cutoff = datetime.fromtimestamp(since, tz=timezone.utc)
        results = [r for r in self._records if r.created_at > cutoff]
        if run_id is not None:
            results = [r for r in results if r.run_id == run_id]
        return results

    def recent_edits(
        self,
        seconds: float = 60.0,
        run_id: str | None = None,
    ) -> list[EditRecord]:
        since = time.time() - seconds
        return self.changes_since(since, run_id=run_id)

    def hotspots(
        self,
        limit: int = 10,
        run_id: str | None = None,
    ) -> list[tuple[str, int]]:
        records = self._records
        if run_id is not None:
            records = [r for r in records if r.run_id == run_id]
        counter: Counter[str] = Counter(r.file_path for r in records)
        return counter.most_common(limit)

    def who_changed(
        self,
        file_path: str,
        run_id: str | None = None,
    ) -> list[EditRecord]:
        results = [r for r in self._records if r.file_path == file_path]
        if run_id is not None:
            results = [r for r in results if r.run_id == run_id]
        return results

    def changes_by_agent_run(
        self,
        run_id: str,
        agent_run_id: str,
    ) -> list[EditRecord]:
        if not agent_run_id:
            return []
        return [
            r for r in self._records
            if r.run_id == run_id and r.agent_run_id == agent_run_id
        ]

    def contention_hotspots(
        self,
        scope_prefixes: list[str] | None = None,
        limit: int = 10,
        days: int = 7,
        run_id: str | None = None,
    ) -> list[ContentionHotspot]:
        cutoff = _utcnow() - timedelta(days=days)
        normalized = [p.rstrip("/") for p in (scope_prefixes or []) if p]
        scoped = [
            r for r in self._records
            if r.created_at > cutoff
            and (run_id is None or r.run_id == run_id)
            and (
                not normalized
                or any(r.file_path.startswith(prefix) for prefix in normalized)
            )
        ]
        contributors_by_file: dict[str, set[str]] = {}
        counts: Counter[str] = Counter()
        for r in scoped:
            contributor = r.task_id or r.agent_run_id
            if not contributor:
                contributor = f"edit:{r.id}"
            contributors_by_file.setdefault(r.file_path, set()).add(contributor)
            counts[r.file_path] += 1
        results = [
            ContentionHotspot(
                file_path=fp,
                contributor_count=len(contributors),
                edit_count=counts[fp],
            )
            for fp, contributors in contributors_by_file.items()
            if len(contributors) > 1
        ]
        results.sort(key=lambda h: (-h.contributor_count, -h.edit_count))
        return results[:limit]
