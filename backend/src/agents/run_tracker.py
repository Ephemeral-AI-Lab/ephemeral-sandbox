"""Centralised agent_run persistence.

:class:`AgentRunTracker` wraps the minimal ``agent_runs`` row for one
TaskCenter task. Direct eval-agent invocations pass ``task_id=None`` and are
not persisted.

Lifecycle:

    tracker = AgentRunTracker.create(
        task_id=..., agent_name=...,
    )
    ... run the agent, stream events ...
    tracker.finish(
        display_messages=...,
        terminal_tool_result=...,
        token_count=...,
        error=...,
    )

When persistence is unavailable, :attr:`run_id` is ``None`` and every
subsequent call on the tracker is a no-op.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Any
from uuid import uuid4

from message.messages import ConversationMessage

logger = logging.getLogger(__name__)

_AUTO_RUN_ID_HEX_LEN = 16


def _get_agent_run_store() -> Any | None:
    """Return the live agent_run_store if it is importable and ready."""
    try:
        from server.app_factory import agent_run_store
    except Exception as exc:
        logger.debug("agent_run_store import failed: %s", exc)
        return None
    if not agent_run_store.is_ready:
        return None
    return agent_run_store


@dataclass
class AgentRunTracker:
    """Handle wrapping a persisted ``agent_run`` row.

    ``run_id`` is ``None`` when persistence is unavailable; all methods
    handle that case by short-circuiting to a no-op so call sites never
    need to branch on a None run id themselves.
    """

    run_id: str | None
    agent_name: str
    _finished: bool = field(default=False, init=False)

    @classmethod
    def create(
        cls,
        *,
        task_id: str | None,
        agent_name: str,
        run_id: str | None = None,
    ) -> AgentRunTracker:
        """Create a persisted run row and return a tracker wrapping it.

        Returns a no-op tracker (``run_id=None``) if the store is not
        ready, the task_id is missing, or the create call raises.
        """
        if not task_id:
            return cls(run_id=None, agent_name=agent_name)

        store = _get_agent_run_store()
        if store is None:
            return cls(run_id=None, agent_name=agent_name)

        resolved_run_id = run_id or uuid4().hex[:_AUTO_RUN_ID_HEX_LEN]
        try:
            store.create_run(
                run_id=resolved_run_id,
                task_id=task_id,
                agent_name=agent_name,
            )
        except Exception:
            logger.warning(
                "AgentRunTracker.create: failed to persist agent_run row", exc_info=True
            )
            return cls(run_id=None, agent_name=agent_name)
        return cls(run_id=resolved_run_id, agent_name=agent_name)

    def finish(
        self,
        *,
        display_messages: list[ConversationMessage] | None = None,
        terminal_tool_result: dict[str, Any] | None = None,
        token_count: int = 0,
        error: str | None = None,
    ) -> None:
        """Finalise the run row. No-op when persistence is unavailable."""
        if self.run_id is None or self._finished:
            return
        store = _get_agent_run_store()
        if store is None:
            return
        try:
            message_history: list[dict[str, Any]] | None = None
            if display_messages is not None:
                message_history = [m.model_dump(mode="json") for m in display_messages]

            store.finish_run(
                self.run_id,
                message_history=message_history,
                terminal_tool_result=terminal_tool_result,
                token_count=token_count,
                error=error,
            )
        except Exception:
            logger.warning(
                "AgentRunTracker.finish: failed to finalise agent_run row", exc_info=True
            )
        finally:
            self._finished = True
