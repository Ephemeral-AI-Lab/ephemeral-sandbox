"""Task-center pipeline state-machine scenarios.

Drive the orchestrator/dispatcher/episode-manager/mission-handler control
flow with the lightest possible executor action (`preflight` or `fail`).
Failures here mean a regression in `task_center/` proper. See
``docs/wiki/live-e2e-scenario-suite-design.md`` for the full coverage matrix.

Implemented (reference scenarios):
- :class:`AttemptBudgetExhausted`
- :class:`AttemptRetryEvaluatorFailure`
- :class:`AttemptRetryGeneratorFailure`
- :class:`AttemptRetryPlannerFailure`
- :class:`DependencyBlockedDescendants`
- :class:`DependencyDagDiamond`
- :class:`DependencyDagMixed`
- :class:`DependencyDagParallel`
- :class:`DependencyDagSerial`
- :class:`EpisodicContinuation`
- :class:`GeneratorFailureQuiescence`
- :class:`InitialMission`
- :class:`NestedMission`
- :class:`NestedMissionFailure`
- :class:`PartialParentPlannerFullOnly`
"""

from __future__ import annotations

from live_e2e.scenarios.pipeline.attempt_budget_exhausted import (
    AttemptBudgetExhausted,
)
from live_e2e.scenarios.pipeline.attempt_retry_evaluator_failure import (
    AttemptRetryEvaluatorFailure,
)
from live_e2e.scenarios.pipeline.attempt_retry_generator_failure import (
    AttemptRetryGeneratorFailure,
)
from live_e2e.scenarios.pipeline.attempt_retry_planner_failure import (
    AttemptRetryPlannerFailure,
)
from live_e2e.scenarios.pipeline.dependency_blocked_descendants import (
    DependencyBlockedDescendants,
)
from live_e2e.scenarios.pipeline.dependency_dag_diamond import (
    DependencyDagDiamond,
)
from live_e2e.scenarios.pipeline.dependency_dag_mixed import (
    DependencyDagMixed,
)
from live_e2e.scenarios.pipeline.dependency_dag_parallel import (
    DependencyDagParallel,
)
from live_e2e.scenarios.pipeline.dependency_dag_serial import (
    DependencyDagSerial,
)
from live_e2e.scenarios.pipeline.episodic_continuation import (
    EpisodicContinuation,
)
from live_e2e.scenarios.pipeline.generator_failure_quiescence import (
    GeneratorFailureQuiescence,
)
from live_e2e.scenarios.pipeline.initial_mission import InitialMission
from live_e2e.scenarios.pipeline.nested_mission import (
    NestedMission,
    NestedMissionFailure,
)
from live_e2e.scenarios.pipeline.partial_parent_planner_full_only import (
    PartialParentPlannerFullOnly,
)

__all__ = [
    "AttemptBudgetExhausted",
    "AttemptRetryEvaluatorFailure",
    "AttemptRetryGeneratorFailure",
    "AttemptRetryPlannerFailure",
    "DependencyBlockedDescendants",
    "DependencyDagDiamond",
    "DependencyDagMixed",
    "DependencyDagParallel",
    "DependencyDagSerial",
    "EpisodicContinuation",
    "GeneratorFailureQuiescence",
    "InitialMission",
    "NestedMission",
    "NestedMissionFailure",
    "PartialParentPlannerFullOnly",
]
