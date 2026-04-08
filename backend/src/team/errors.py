"""Exceptions raised by team-mode components."""

from __future__ import annotations


class InvalidPlan(Exception):
    """Raised when Phase B validation rejects a submitted Plan."""


class ArtifactTooLarge(Exception):
    """Raised when an artifact exceeds the configured size budget."""


class CheckpointNotFound(Exception):
    """Raised when a checkpoint id is not known to the Dispatcher."""


class BudgetExceeded(Exception):
    """Raised when adding a WorkItem would exceed a configured budget."""


class NoPosthookOutput(Exception):
    """Raised when the posthook phase ends without an accepted submission."""
