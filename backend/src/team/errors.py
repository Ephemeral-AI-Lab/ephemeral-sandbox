"""Exceptions raised by team-mode components."""

from __future__ import annotations


class InvalidPlan(Exception):
    """Raised when validation rejects a submitted Plan."""


class CheckpointNotFound(Exception):
    """Raised when a checkpoint id is not known to the Dispatcher."""


class BudgetExceeded(Exception):
    """Raised when adding a Task would exceed a configured budget."""


class GraphInvariantViolation(RuntimeError):
    """Raised when persisted task graph state violates scheduler invariants."""
