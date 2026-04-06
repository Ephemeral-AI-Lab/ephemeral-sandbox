"""Token usage tracking store."""

from __future__ import annotations

import logging

from sqlalchemy import func
from sqlalchemy.orm import Session, sessionmaker

from token_tracker.models import TokenUsageRecord

logger = logging.getLogger(__name__)


class UsageStore:
    """Records and queries token consumption."""

    def __init__(self) -> None:
        self._session_factory: sessionmaker[Session] | None = None

    def initialize(self, session_factory: sessionmaker[Session]) -> None:
        self._session_factory = session_factory
        logger.info("UsageStore initialised")

    @property
    def _sf(self) -> sessionmaker[Session]:
        if self._session_factory is None:
            raise RuntimeError("UsageStore not initialised")
        return self._session_factory

    def record(
        self,
        *,
        session_id: str,
        agent_name: str,
        model_id: str,
        prompt_tokens: int = 0,
        completion_tokens: int = 0,
    ) -> TokenUsageRecord:
        with self._sf() as db:
            rec = TokenUsageRecord(
                session_id=session_id,
                agent_name=agent_name,
                model_id=model_id,
                prompt_tokens=prompt_tokens,
                completion_tokens=completion_tokens,
                total_tokens=prompt_tokens + completion_tokens,
            )
            db.add(rec)
            db.commit()
            db.refresh(rec)
            return rec

    def get_session_usage(self, session_id: str) -> dict:
        """Aggregate token usage for a session."""
        with self._sf() as db:
            row = (
                db.query(
                    func.coalesce(func.sum(TokenUsageRecord.prompt_tokens), 0),
                    func.coalesce(func.sum(TokenUsageRecord.completion_tokens), 0),
                    func.coalesce(func.sum(TokenUsageRecord.total_tokens), 0),
                    func.count(TokenUsageRecord.id),
                )
                .filter(TokenUsageRecord.session_id == session_id)
                .one()
            )
            return {
                "session_id": session_id,
                "prompt_tokens": row[0],
                "completion_tokens": row[1],
                "total_tokens": row[2],
                "call_count": row[3],
            }

    def get_usage_by_model(self, session_id: str | None = None) -> list[dict]:
        """Break down usage by model, optionally filtered by session."""
        with self._sf() as db:
            q = db.query(
                TokenUsageRecord.model_id,
                func.sum(TokenUsageRecord.prompt_tokens),
                func.sum(TokenUsageRecord.completion_tokens),
                func.sum(TokenUsageRecord.total_tokens),
                func.count(TokenUsageRecord.id),
            ).group_by(TokenUsageRecord.model_id)
            if session_id:
                q = q.filter(TokenUsageRecord.session_id == session_id)
            return [
                {
                    "model_id": row[0],
                    "prompt_tokens": row[1],
                    "completion_tokens": row[2],
                    "total_tokens": row[3],
                    "call_count": row[4],
                }
                for row in q.all()
            ]
