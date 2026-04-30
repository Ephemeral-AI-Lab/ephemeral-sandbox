"""Agent run persistence store."""

from __future__ import annotations

from datetime import datetime, UTC
from typing import Any

from db.models.agent_run import AgentRunRecord
from db.stores.base import SyncStoreMixin


def _serialize_run_summary(r: AgentRunRecord) -> dict[str, Any]:
    """Small JSON view of an AgentRunRecord for list endpoints."""
    return {
        "id": r.id,
        "task_id": r.task_id,
        "agent_name": r.agent_name,
        "token_count": r.token_count,
        "error": r.error,
        "created_at": r.created_at.isoformat() if r.created_at else None,
        "finished_at": r.finished_at.isoformat() if r.finished_at else None,
    }


class AgentRunStore(SyncStoreMixin):
    """CRUD operations for agent run records."""

    # -- run CRUD --------------------------------------------------------------

    def create_run(
        self,
        *,
        agent_run_id: str,
        task_id: str,
        agent_name: str,
    ) -> AgentRunRecord:
        """Create a new agent run record for one TaskCenter task."""
        with self._sf() as db:
            record = AgentRunRecord(
                id=agent_run_id,
                task_id=task_id,
                agent_name=agent_name,
                created_at=datetime.now(UTC),
            )
            db.add(record)
            db.commit()
            db.refresh(record)
            return record

    def finish_run(
        self,
        agent_run_id: str,
        *,
        message_history: list[dict[str, Any]] | None = None,
        terminal_tool_result: dict[str, Any] | None = None,
        token_count: int = 0,
        error: str | None = None,
    ) -> AgentRunRecord | None:
        with self._sf() as db:
            record = db.get(AgentRunRecord, agent_run_id)
            if record is None:
                return None
            record.message_history = message_history
            record.terminal_tool_result = terminal_tool_result
            record.token_count = token_count
            record.error = error
            record.finished_at = datetime.now(UTC)
            db.commit()
            db.refresh(record)
            return record

    def get_run(self, agent_run_id: str) -> AgentRunRecord | None:
        with self._sf() as db:
            return db.get(AgentRunRecord, agent_run_id)

    def list_runs_for_tasks(self, task_ids: list[str]) -> list[dict[str, Any]]:
        if not task_ids:
            return []
        with self._sf() as db:
            q = (
                db.query(AgentRunRecord)
                .filter(AgentRunRecord.task_id.in_(task_ids))
                .order_by(AgentRunRecord.created_at.asc())
            )
            return [_serialize_run_summary(r) for r in q.all()]
