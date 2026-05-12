"""Scenario protocol + scenario registry.

Composite scenarios live at the top level for historical reasons. Focused
scenarios are organized by concern under subpackages — see
``docs/wiki/live-e2e-scenario-suite-design.md`` for the full taxonomy.
"""

from __future__ import annotations

from live_e2e.scenarios.base import Scenario
from live_e2e.scenarios.capacity import FullSystemCapacityMatrix
from live_e2e.scenarios.correctness_testing import CorrectnessTesting
from live_e2e.scenarios.full_case_user_input import FullCaseUserInput
from live_e2e.scenarios.full_stack_adversarial import FullStackAdversarial
from live_e2e.scenarios.pipeline import (
    AttemptBudgetExhausted,
    AttemptRetryEvaluatorFailure,
    AttemptRetryGeneratorFailure,
    AttemptRetryPlannerFailure,
    DependencyBlockedDescendants,
    DependencyDagDiamond,
    DependencyDagMixed,
    DependencyDagParallel,
    DependencyDagSerial,
    EpisodicContinuation,
    GeneratorFailureQuiescence,
    InitialMission,
    NestedMission,
    NestedMissionFailure,
    PartialParentPlannerFullOnly,
)
from live_e2e.scenarios.planner_validation import (
    PlannerCycleInDeps,
    PlannerDuplicateLocalId,
    PlannerEmptyTasks,
    PlannerPartialWithoutContinuationGoal,
    PlannerUnknownAgentName,
    PlannerUnknownDep,
)
from live_e2e.scenarios.sandbox import (
    AutoSquashCommitResume,
    ComplexProjectBuild,
    ComplexProjectBuildShellEditLsp,
    ComplexProjectBuildShellEditLspSmoke,
    ComplexProjectBuildSmoke,
    OccConcurrentConflicts,
)

SCENARIO_REGISTRY: dict[str, type[Scenario]] = {
    # Composite end-to-end scenarios.
    "correctness_testing": CorrectnessTesting,
    "full_case_user_input": FullCaseUserInput,
    "full_stack_adversarial": FullStackAdversarial,
    # Focused pipeline scenarios.
    "pipeline.initial_mission": InitialMission,
    "pipeline.episodic_continuation": EpisodicContinuation,
    "pipeline.attempt_retry_evaluator_failure": AttemptRetryEvaluatorFailure,
    "pipeline.attempt_retry_generator_failure": AttemptRetryGeneratorFailure,
    "pipeline.attempt_retry_planner_failure": AttemptRetryPlannerFailure,
    "pipeline.dependency_blocked_descendants": DependencyBlockedDescendants,
    "pipeline.dependency_dag_diamond": DependencyDagDiamond,
    "pipeline.dependency_dag_serial": DependencyDagSerial,
    "pipeline.dependency_dag_mixed": DependencyDagMixed,
    "pipeline.dependency_dag_parallel": DependencyDagParallel,
    "pipeline.generator_failure_quiescence": GeneratorFailureQuiescence,
    "pipeline.attempt_budget_exhausted": AttemptBudgetExhausted,
    "pipeline.nested_mission": NestedMission,
    "pipeline.nested_mission_failure": NestedMissionFailure,
    "pipeline.partial_parent_planner_full_only": PartialParentPlannerFullOnly,
    # Composite capacity scenarios.
    "capacity.full_system_capacity_matrix": FullSystemCapacityMatrix,
    # Focused sandbox scenarios.
    "sandbox.auto_squash_commit_resume": AutoSquashCommitResume,
    "sandbox.complex_project_build": ComplexProjectBuild,
    "sandbox.complex_project_build_shell_edit_lsp": ComplexProjectBuildShellEditLsp,
    "sandbox.complex_project_build_shell_edit_lsp_smoke": (
        ComplexProjectBuildShellEditLspSmoke
    ),
    "sandbox.complex_project_build_smoke": ComplexProjectBuildSmoke,
    "sandbox.occ_concurrent_conflicts": OccConcurrentConflicts,
    # Focused planner-validation scenarios.
    "planner_validation.cycle_in_deps": PlannerCycleInDeps,
    "planner_validation.duplicate_local_id": PlannerDuplicateLocalId,
    "planner_validation.empty_tasks": PlannerEmptyTasks,
    "planner_validation.partial_without_continuation_goal": (
        PlannerPartialWithoutContinuationGoal
    ),
    "planner_validation.unknown_agent_name": PlannerUnknownAgentName,
    "planner_validation.unknown_dep": PlannerUnknownDep,
}

__all__ = [
    "AttemptBudgetExhausted",
    "AttemptRetryEvaluatorFailure",
    "AttemptRetryGeneratorFailure",
    "AttemptRetryPlannerFailure",
    "AutoSquashCommitResume",
    "ComplexProjectBuild",
    "ComplexProjectBuildShellEditLsp",
    "ComplexProjectBuildShellEditLspSmoke",
    "ComplexProjectBuildSmoke",
    "CorrectnessTesting",
    "DependencyBlockedDescendants",
    "DependencyDagDiamond",
    "DependencyDagMixed",
    "DependencyDagParallel",
    "DependencyDagSerial",
    "EpisodicContinuation",
    "FullCaseUserInput",
    "FullSystemCapacityMatrix",
    "FullStackAdversarial",
    "GeneratorFailureQuiescence",
    "InitialMission",
    "NestedMission",
    "NestedMissionFailure",
    "PartialParentPlannerFullOnly",
    "OccConcurrentConflicts",
    "PlannerCycleInDeps",
    "PlannerDuplicateLocalId",
    "PlannerEmptyTasks",
    "PlannerPartialWithoutContinuationGoal",
    "PlannerUnknownAgentName",
    "PlannerUnknownDep",
    "SCENARIO_REGISTRY",
]
