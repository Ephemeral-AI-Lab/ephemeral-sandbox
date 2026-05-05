"""TaskSegmentClosureReport — closure signal from manager to handler."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Literal

from task_center.attempt import HarnessGraphFailReason


@dataclass(frozen=True, slots=True)
class AttemptedPlanEntry:
    """One past attempt's structural state. Phase 06 fills the summary fields."""

    harness_graph_id: str
    graph_sequence_no: int
    task_specification: str | None
    evaluation_criteria: tuple[str, ...]
    fail_reason: HarnessGraphFailReason | None
    harness_graph_summary_id: str | None
    failure_landscape: dict[str, Any] | None


@dataclass(frozen=True, slots=True)
class TerminalSuccess:
    kind: Literal["terminal_success"] = "terminal_success"


@dataclass(frozen=True, slots=True)
class SuccessContinue:
    goal: str
    kind: Literal["success_continue"] = "success_continue"


@dataclass(frozen=True, slots=True)
class AttemptPlanFailed:
    failure_summary: str
    attempted_plan_history: tuple[AttemptedPlanEntry, ...]
    kind: Literal["attempt_plan_failed"] = "attempt_plan_failed"


ClosureOutcome = TerminalSuccess | SuccessContinue | AttemptPlanFailed


@dataclass(frozen=True, slots=True)
class TaskSegmentClosureReport:
    task_segment_id: str
    final_harness_graph_id: str
    outcome: ClosureOutcome
