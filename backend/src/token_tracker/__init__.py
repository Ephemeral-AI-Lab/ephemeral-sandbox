"""Token tracker module for recording and querying token usage.

Architecture:
    - TokenUsageRecord: SQLAlchemy model for persisted usage data
    - UsageStore: Database operations for recording/querying usage
    - TokenTracker: High-level facade combining model + store operations
"""

from __future__ import annotations

from token_tracker.models import TokenUsageRecord
from token_tracker.store import UsageStore


class TokenTracker:
    """High-level token usage tracker.

    Combines UsageStore (database operations) with TokenUsageRecord model
    into a single interface for recording and querying token consumption.
    """

    def __init__(self) -> None:
        self._store = UsageStore()

    @property
    def store(self) -> UsageStore:
        """Direct access to underlying UsageStore for compatibility."""
        return self._store

    def initialize(self, session_factory) -> None:
        """Initialize the underlying store with a session factory."""
        self._store.initialize(session_factory)

    def record(
        self,
        *,
        session_id: str,
        agent_name: str,
        model_id: str,
        prompt_tokens: int = 0,
        completion_tokens: int = 0,
    ) -> TokenUsageRecord:
        """Record token usage for an agent call.

        Args:
            session_id: Unique session identifier
            agent_name: Name of the agent that made the call
            model_id: Model identifier (e.g., "claude-3-5-sonnet")
            prompt_tokens: Number of input tokens consumed
            completion_tokens: Number of output tokens generated

        Returns:
            The created TokenUsageRecord
        """
        return self._store.record(
            session_id=session_id,
            agent_name=agent_name,
            model_id=model_id,
            prompt_tokens=prompt_tokens,
            completion_tokens=completion_tokens,
        )

    def get_session_usage(self, session_id: str) -> dict:
        """Get aggregated usage for a session."""
        return self._store.get_session_usage(session_id)

    def get_usage_by_model(self, session_id: str | None = None) -> list[dict]:
        """Get usage breakdown by model, optionally filtered by session."""
        return self._store.get_usage_by_model(session_id)


__all__ = [
    "TokenTracker",
    "TokenUsageRecord",
    "UsageStore",
]
