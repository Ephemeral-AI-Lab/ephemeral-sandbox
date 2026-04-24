"""BudgetManager — task/replan budget tracking and enforcement.

Extracted from TaskCenter. Owns BudgetConfig + BudgetState and emits
budget_update events on change.
"""

from __future__ import annotations

import logging
from typing import Callable

from team.core.errors import BudgetExceeded
from team.core.models import BudgetConfig, BudgetState
from team.persistence.events import TeamRunEvent, make_budget_update

logger = logging.getLogger(__name__)


class BudgetManager:
    """Owns BudgetConfig + BudgetState and emits budget_update events."""

    def __init__(
        self,
        *,
        team_run_id: str,
        budgets: BudgetConfig,
        budget_state: BudgetState,
        emit_cb: Callable[[TeamRunEvent], None],
    ) -> None:
        self._team_run_id = team_run_id
        self.budgets = budgets
        self.budget_state = budget_state
        self._emit_cb = emit_cb

    def emit_update(self) -> None:
        self._emit_cb(
            make_budget_update(
                self._team_run_id,
                tasks_used=self.budget_state.tasks_used,
                replans_used=self.budget_state.replans_used,
            )
        )

    def has_capacity_for(self, n: int) -> bool:
        return self.budget_state.tasks_used + n <= self.budgets.max_tasks

    def require_capacity_for(self, n: int = 1, msg: str | None = None) -> None:
        if self.budget_state.tasks_used + n > self.budgets.max_tasks:
            raise BudgetExceeded(msg or f"max_tasks={self.budgets.max_tasks} reached")

    def charge_tasks(self, n: int = 1) -> None:
        self.budget_state.tasks_used += n
        self.emit_update()

    def add_tasks_used(self, n: int = 1) -> None:
        """Mutate counter without emitting; caller is responsible for emit_update()."""
        self.budget_state.tasks_used += n

    def require_replan_capacity(self) -> None:
        if self.budget_state.replans_used >= self.budgets.max_replans_per_run:
            raise BudgetExceeded("max_replans_per_run reached")

    def bump_replan_counters(self) -> None:
        """Charge a replan (one task + one replan) without emitting."""
        self.budget_state.tasks_used += 1
        self.budget_state.replans_used += 1

    def within_depth_limit(self, new_depth: int) -> bool:
        return new_depth <= self.budgets.max_depth
