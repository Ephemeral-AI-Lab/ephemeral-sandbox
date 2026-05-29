"""ContextScope — identity surface every recipe sees.

The scope carries identity (goal / iteration / attempt / task ids). It
does **not** carry store handles; those live on :class:`ContextEngineDeps`
so recipes can be swapped without touching call sites.

The role-specific factory classmethods (:meth:`for_planner`,
:meth:`for_generator`, etc.) document the required fields per role at the
call site: omitting one raises ``TypeError`` at call time, and strict
mypy will narrow the kwargs to their declared ``str`` types. The engine
still validates via :meth:`assert_fields` so direct ``ContextScope(...)``
construction is also covered at runtime.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

from task_center.context_engine.exceptions import RecipeScopeError

ScopeField = Literal["workflow_id", "iteration_id", "attempt_id", "task_id"]


@dataclass(frozen=True, slots=True)
class ContextScope:
    """Identity surface threaded through resolver + engine + recipes."""

    workflow_id: str | None = None

    # Optional identity fields — recipes declare which of these they need.
    iteration_id: str | None = None
    attempt_id: str | None = None
    task_id: str | None = None

    def assert_fields(self, required: frozenset[str]) -> None:
        """Raise :class:`RecipeScopeError` if any required field is None."""
        missing = sorted(f for f in required if getattr(self, f, None) is None)
        if missing:
            raise RecipeScopeError(f"ContextScope is missing required fields: {missing!r}")

    def require_field(self, field: ScopeField) -> str:
        """Return one required identity field as a non-optional string."""
        value = getattr(self, field)
        if not isinstance(value, str):
            raise RecipeScopeError(f"ContextScope is missing required field: {field!r}")
        return value

    # ---- Role-specific factory shortcuts -------------------------------
    #
    # Each factory takes ONLY the required fields for that recipe role as
    # positional/keyword args. Missing a required field is a static error
    # instead of a runtime assert. The defaults flow through the dataclass
    # for any optional fields the role might inspect.

    @classmethod
    def for_planner(
        cls,
        *,
        workflow_id: str,
        iteration_id: str,
        attempt_id: str,
    ) -> ContextScope:
        """Scope shape required by the planner recipe."""
        return cls(
            workflow_id=workflow_id,
            iteration_id=iteration_id,
            attempt_id=attempt_id,
        )

    @classmethod
    def for_generator(
        cls,
        *,
        workflow_id: str,
        iteration_id: str,
        attempt_id: str,
        task_id: str,
    ) -> ContextScope:
        """Scope shape required by the generator recipe."""
        return cls(
            workflow_id=workflow_id,
            iteration_id=iteration_id,
            attempt_id=attempt_id,
            task_id=task_id,
        )

    @classmethod
    def for_evaluator(
        cls,
        *,
        workflow_id: str,
        iteration_id: str,
        attempt_id: str,
    ) -> ContextScope:
        """Scope shape required by the evaluator recipe."""
        return cls(
            workflow_id=workflow_id,
            iteration_id=iteration_id,
            attempt_id=attempt_id,
        )
