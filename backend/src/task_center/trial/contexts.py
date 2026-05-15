"""Role-narrow dependency Protocol for :class:`LaunchBuilder`.

The launcher only needs ``mission_store`` + ``episode_store`` + the
``run_id_for_attempt`` / ``require_composer`` methods. Declaring this
narrow Protocol lets the launcher accept any structurally compatible
context — concrete :class:`TrialDeps` satisfies it.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol

from task_center._core.persistence import (
    GoalStoreProtocol,
    IterationStoreProtocol,
)

if TYPE_CHECKING:  # pragma: no cover - typing-only
    from task_center.trial.state import Trial
    from task_center.context_engine.core import ContextComposer


class LaunchCtx(Protocol):
    """Dependencies for :class:`LaunchBuilder` — composer access + stores."""

    mission_store: GoalStoreProtocol
    episode_store: IterationStoreProtocol

    def run_id_for_attempt(self, attempt: Trial) -> str: ...

    def require_composer(self) -> ContextComposer: ...


__all__ = ["LaunchCtx"]
