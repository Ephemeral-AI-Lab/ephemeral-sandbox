"""Task and Status — the core data model for the executor-evaluator tree.

Each task is a node owned by either an executor or an evaluator, lives in
exactly one of six statuses, and carries a single immutable ``closes_for``
pointer used for summary propagation along the closure chain.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Literal


TaskId = str


class Status(str, Enum):
    """The six task statuses defined by the architecture.

    Order is significant for tests (``list(Status)`` must match the doc).
    """

    PENDING = "pending"
    READY = "ready"
    RUNNING = "running"
    HANDOFF = "handoff"
    DONE = "done"
    FAILED = "failed"


TaskRole = Literal["executor", "evaluator"]


@dataclass
class Task:
    """A single node in the task graph.

    ``closes_for`` is set once at construction (invariant 11) — any later
    mutation raises ``AttributeError``. All other fields are mutable, but
    status transitions should go through ``TaskGraph.transition`` so the
    graph's invariants are preserved.

    ``mode`` is the agent-mode typestate per
    ``docs/architecture/agent-mode-system-v1.md``. New tasks always start in
    ``"direct"``. The mode-entry tools (``enter_plan_for_handoff``,
    ``enter_prepare_continue_to_work``) flip it to a secondary mode exactly
    once per agent run; the only way back is via the secondary mode's
    terminal tool, which closes the task. The Task itself does not enforce
    one-way semantics — that lives in the entry tool's pre-check.
    """

    id: TaskId
    role: TaskRole
    title: str
    spec: str
    status: Status
    parent_id: TaskId | None = None
    closes_for: TaskId | None = None
    needs: frozenset[TaskId] = field(default_factory=frozenset)
    acceptance_criteria: str | None = None
    handoff_note: str | None = None
    summary: str | None = None
    children: list[TaskId] = field(default_factory=list)
    evaluator_id: TaskId | None = None
    mode: str = "direct"
    created_at: float = field(default_factory=time.time)

    def __post_init__(self) -> None:
        # Mark ``closes_for`` as locked. Any subsequent mutation raises.
        object.__setattr__(self, "_closes_for_locked", True)

    def __setattr__(self, name: str, value: Any) -> None:
        if name == "closes_for" and self.__dict__.get("_closes_for_locked", False):
            current = self.__dict__.get("closes_for")
            if value != current:
                raise AttributeError(
                    f"Task.closes_for is set once at creation "
                    f"(current={current!r}); it cannot be mutated."
                )
        super().__setattr__(name, value)
