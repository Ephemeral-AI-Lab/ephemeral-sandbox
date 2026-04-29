"""Task and Status — the core data model for the GAN-style task graph.

Each task is a node owned by an executor, planner, or evaluator. Tasks belong
to at most one ``HarnessGraph`` via ``task_center_harness_graph_id``; only the
root executor has no harness graph. Dependencies between executor tasks are
represented by ``needs``.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Literal


TaskId = str
HarnessGraphId = str


class Status(str, Enum):
    PENDING = "pending"
    READY = "ready"
    RUNNING = "running"
    HANDOFF = "handoff"
    DONE = "done"
    FAILED = "failed"
    # Stage 2 of the four-role roadmap: a verifier that emitted
    # ``submit_verification_failure`` enters FIXING while a fix-executor
    # is in flight (Stage 6). Stage 2 itself does not transition into
    # FIXING — that wiring lands with the fix-executor primitive.
    FIXING = "fixing"


TaskRole = Literal["executor", "planner", "verifier", "evaluator"]

SummaryKind = Literal[
    "handoff",
    "success",
    "failure",
    "evaluation_failure",
    "dependency_blocked",
    "child_success",
    "child_failure",
    # Stage 5: appended onto a partial-plan graph's root_task when the
    # segment evaluator approves; the chain continues with a new graph.
    "segment_success",
]


@dataclass
class TaskSummary:
    """Append-only summary entry attached to a task."""

    kind: SummaryKind
    text: str
    source_task_id: TaskId
    created_at: float = field(default_factory=time.time)


@dataclass
class Task:
    """A single node in the task graph."""

    id: TaskId
    role: TaskRole
    input: str
    status: Status
    task_center_harness_graph_id: HarnessGraphId | None = None
    needs: frozenset[TaskId] = field(default_factory=frozenset)
    summaries: list[TaskSummary] = field(default_factory=list)
    created_at: float = field(default_factory=time.time)
