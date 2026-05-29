"""Task-center pipeline state-machine scenarios.

Drive the orchestrator/task-dispatcher/iteration-coordinator/goal-lifecycle control
flow with the lightest possible executor action (`preflight` or `fail`).
Failures here mean a regression in `task_center/` proper.

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
- :class:`IterativeDeferral`
- :class:`GeneratorFailureQuiescence`
- :class:`InitialWorkflow`
- :class:`NestedWorkflow`
- :class:`NestedWorkflowFailure`
- :class:`DeferredParentPlannerTerminalRouting`
"""

from __future__ import annotations

from task_center_runner.scenarios.pipeline.attempt_budget_exhausted import (
    AttemptBudgetExhausted,
)
from task_center_runner.scenarios.pipeline.attempt_retry_evaluator_failure import (
    AttemptRetryEvaluatorFailure,
)
from task_center_runner.scenarios.pipeline.attempt_retry_generator_failure import (
    AttemptRetryGeneratorFailure,
)
from task_center_runner.scenarios.pipeline.attempt_retry_planner_failure import (
    AttemptRetryPlannerFailure,
)
from task_center_runner.scenarios.pipeline.dependency_blocked_descendants import (
    DependencyBlockedDescendants,
)
from task_center_runner.scenarios.pipeline.dependency_dag_diamond import (
    DependencyDagDiamond,
)
from task_center_runner.scenarios.pipeline.dependency_dag_mixed import (
    DependencyDagMixed,
)
from task_center_runner.scenarios.pipeline.dependency_dag_parallel import (
    DependencyDagParallel,
)
from task_center_runner.scenarios.pipeline.dependency_dag_serial import (
    DependencyDagSerial,
)
from task_center_runner.scenarios.pipeline.initial_messages_capture import (
    InitialMessagesCapture,
)
from task_center_runner.scenarios.pipeline.iterative_deferral import (
    IterativeDeferral,
)
from task_center_runner.scenarios.pipeline.generator_failure_quiescence import (
    GeneratorFailureQuiescence,
)
from task_center_runner.scenarios.pipeline.initial_workflow import InitialWorkflow
from task_center_runner.scenarios.pipeline.nested_workflow import (
    NestedWorkflow,
    NestedWorkflowFailure,
)
from task_center_runner.scenarios.pipeline.deferred_parent_planner_terminal_routing import (
    DeferredParentPlannerTerminalRouting,
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
    "InitialMessagesCapture",
    "IterativeDeferral",
    "GeneratorFailureQuiescence",
    "InitialWorkflow",
    "NestedWorkflow",
    "NestedWorkflowFailure",
    "DeferredParentPlannerTerminalRouting",
]
