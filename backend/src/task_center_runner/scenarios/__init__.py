"""Scenario protocol + scenario registry.

Composite scenarios live at the top level for historical reasons. Focused
scenarios are organized by concern under subpackages — see
``docs/wiki/live-e2e-scenario-suite-design.md`` for the full taxonomy.
"""

from __future__ import annotations

from task_center_runner.scenarios.base import Scenario
from task_center_runner.scenarios.capacity import FullSystemCapacityMatrix
from task_center_runner.scenarios.correctness_testing import CorrectnessTesting
from task_center_runner.scenarios.full_case_user_input import FullCaseUserInput
from task_center_runner.scenarios.full_stack_adversarial import FullStackAdversarial
from task_center_runner.scenarios.pipeline import (
    AttemptBudgetExhausted,
    AttemptRetryEvaluatorFailure,
    AttemptRetryGeneratorFailure,
    AttemptRetryPlannerFailure,
    DependencyBlockedDescendants,
    DependencyDagDiamond,
    DependencyDagMixed,
    DependencyDagParallel,
    DependencyDagSerial,
    InitialMessagesCapture,
    IterativeDeferral,
    GeneratorFailureQuiescence,
    InitialGoal,
    NestedGoal,
    NestedGoalFailure,
    DeferredParentPlannerTerminalRouting,
)
from task_center_runner.scenarios.planner_validation import (
    PlannerCycleInDeps,
    PlannerDuplicateLocalId,
    PlannerEmptyTasks,
    PlannerDefersWithoutDeferredGoal,
    PlannerUnknownAgentName,
    PlannerUnknownDep,
)
from task_center_runner.scenarios.sandbox import (
    AutoSquashCommitResume,
    BackgroundEngineRestartNoLeaseLeak,
    BackgroundExitIwsDrainsAgentTasks,
    BackgroundHeartbeatLossReapsOnlyStaleBg,
    BackgroundManySmallWritesDoNotStarveDispatcher,
    BackgroundMixedFgBgSamePathConflict,
    BackgroundShellStop,
    BackgroundShellStopDuringMaintenance,
    BackgroundShellExhaustion,
    BackgroundShellGolden,
    BackgroundShellInterleave,
    BackgroundShellLateCancelRace,
    BackgroundShellPartialWriteCancel,
    ComplexProjectBuild,
    ComplexProjectBuildGrepGlob,
    ComplexProjectBuildGrepGlobSmoke,
    ComplexProjectBuildShellEditLsp,
    ComplexProjectBuildShellEditLspSmoke,
    ComplexProjectBuildSmoke,
    EphemeralWorkspaceAllVerbs,
    EphemeralWorkspaceCancellation,
    EphemeralWorkspaceConcurrentWrites,
    EphemeralWorkspaceO1Disk,
    EphemeralWorkspacePolicy,
    EphemeralWorkspaceSamePathConflict,
    HeavyIoZonedConcurrent,
    HighConcurrencyLayerstackOverlayOcc,
    OccConcurrentConflicts,
    PluginIntentContract,
    PluginIwsPolicy,
    PluginReadOnlyLspRefresh,
    PluginServiceEvict,
    PluginSetupFailure,
    PluginWriteAllowedPublish,
)

SCENARIO_REGISTRY: dict[str, type[Scenario]] = {
    # Composite end-to-end scenarios.
    "correctness_testing": CorrectnessTesting,
    "full_case_user_input": FullCaseUserInput,
    "full_stack_adversarial": FullStackAdversarial,
    # Focused pipeline scenarios.
    "pipeline.initial_goal": InitialGoal,
    "pipeline.initial_messages_capture": InitialMessagesCapture,
    "pipeline.iterative_deferral": IterativeDeferral,
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
    "pipeline.nested_goal": NestedGoal,
    "pipeline.nested_goal_failure": NestedGoalFailure,
    "pipeline.deferred_parent_planner_terminal_routing": (
        DeferredParentPlannerTerminalRouting
    ),
    # Composite capacity scenarios.
    "capacity.full_system_capacity_matrix": FullSystemCapacityMatrix,
    # Focused sandbox scenarios.
    "sandbox.auto_squash_commit_resume": AutoSquashCommitResume,
    "sandbox.background_shell_stop": BackgroundShellStop,
    "sandbox.background_shell_stop_during_maintenance": (
        BackgroundShellStopDuringMaintenance
    ),
    "sandbox.background_shell_exhaustion": BackgroundShellExhaustion,
    "sandbox.background_shell_golden": BackgroundShellGolden,
    "sandbox.background_shell_interleave": BackgroundShellInterleave,
    "sandbox.background_shell_late_cancel_race": BackgroundShellLateCancelRace,
    "sandbox.background_shell_partial_write_cancel": (
        BackgroundShellPartialWriteCancel
    ),
    "sandbox.background_mixed_fg_bg_same_path_conflict": (
        BackgroundMixedFgBgSamePathConflict
    ),
    "sandbox.background_heartbeat_loss_reaps_only_stale_bg": (
        BackgroundHeartbeatLossReapsOnlyStaleBg
    ),
    "sandbox.background_exit_iws_drains_agent_tasks": (
        BackgroundExitIwsDrainsAgentTasks
    ),
    "sandbox.background_engine_restart_no_lease_leak": (
        BackgroundEngineRestartNoLeaseLeak
    ),
    "sandbox.background_many_small_writes_do_not_starve_dispatcher": (
        BackgroundManySmallWritesDoNotStarveDispatcher
    ),
    "sandbox.complex_project_build": ComplexProjectBuild,
    "sandbox.complex_project_build_grep_glob": ComplexProjectBuildGrepGlob,
    "sandbox.complex_project_build_grep_glob_smoke": ComplexProjectBuildGrepGlobSmoke,
    "sandbox.complex_project_build_shell_edit_lsp": ComplexProjectBuildShellEditLsp,
    "sandbox.complex_project_build_shell_edit_lsp_smoke": (
        ComplexProjectBuildShellEditLspSmoke
    ),
    "sandbox.complex_project_build_smoke": ComplexProjectBuildSmoke,
    "sandbox.ephemeral_workspace_all_verbs": EphemeralWorkspaceAllVerbs,
    "sandbox.ephemeral_workspace_concurrent_writes": (
        EphemeralWorkspaceConcurrentWrites
    ),
    "sandbox.ephemeral_workspace_same_path_conflict": (
        EphemeralWorkspaceSamePathConflict
    ),
    "sandbox.ephemeral_workspace_policy": EphemeralWorkspacePolicy,
    "sandbox.ephemeral_workspace_cancellation": EphemeralWorkspaceCancellation,
    "sandbox.ephemeral_workspace_o1_disk": EphemeralWorkspaceO1Disk,
    "sandbox.heavy_io_zoned_concurrent": HeavyIoZonedConcurrent,
    "sandbox.high_concurrency_layerstack_overlay_occ": (
        HighConcurrencyLayerstackOverlayOcc
    ),
    "sandbox.occ_concurrent_conflicts": OccConcurrentConflicts,
    "sandbox.plugin_read_only_lsp_refresh": PluginReadOnlyLspRefresh,
    "sandbox.plugin_write_allowed_publish": PluginWriteAllowedPublish,
    "sandbox.plugin_intent_contract": PluginIntentContract,
    "sandbox.plugin_iws_policy": PluginIwsPolicy,
    "sandbox.plugin_setup_failure": PluginSetupFailure,
    "sandbox.plugin_service_evict": PluginServiceEvict,
    # Focused planner-validation scenarios.
    "planner_validation.cycle_in_deps": PlannerCycleInDeps,
    "planner_validation.duplicate_local_id": PlannerDuplicateLocalId,
    "planner_validation.empty_tasks": PlannerEmptyTasks,
    "planner_validation.defers_without_deferred_goal": (
        PlannerDefersWithoutDeferredGoal
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
    "BackgroundEngineRestartNoLeaseLeak",
    "BackgroundExitIwsDrainsAgentTasks",
    "BackgroundHeartbeatLossReapsOnlyStaleBg",
    "BackgroundManySmallWritesDoNotStarveDispatcher",
    "BackgroundMixedFgBgSamePathConflict",
    "BackgroundShellStop",
    "BackgroundShellStopDuringMaintenance",
    "BackgroundShellExhaustion",
    "BackgroundShellGolden",
    "BackgroundShellInterleave",
    "BackgroundShellLateCancelRace",
    "BackgroundShellPartialWriteCancel",
    "ComplexProjectBuild",
    "ComplexProjectBuildGrepGlob",
    "ComplexProjectBuildGrepGlobSmoke",
    "ComplexProjectBuildShellEditLsp",
    "ComplexProjectBuildShellEditLspSmoke",
    "ComplexProjectBuildSmoke",
    "EphemeralWorkspaceAllVerbs",
    "EphemeralWorkspaceCancellation",
    "EphemeralWorkspaceConcurrentWrites",
    "EphemeralWorkspaceO1Disk",
    "EphemeralWorkspacePolicy",
    "EphemeralWorkspaceSamePathConflict",
    "CorrectnessTesting",
    "DependencyBlockedDescendants",
    "DependencyDagDiamond",
    "DependencyDagMixed",
    "DependencyDagParallel",
    "DependencyDagSerial",
    "InitialMessagesCapture",
    "IterativeDeferral",
    "FullCaseUserInput",
    "FullSystemCapacityMatrix",
    "FullStackAdversarial",
    "GeneratorFailureQuiescence",
    "HeavyIoZonedConcurrent",
    "InitialGoal",
    "HighConcurrencyLayerstackOverlayOcc",
    "NestedGoal",
    "NestedGoalFailure",
    "DeferredParentPlannerTerminalRouting",
    "OccConcurrentConflicts",
    "PlannerCycleInDeps",
    "PlannerDuplicateLocalId",
    "PlannerEmptyTasks",
    "PlannerDefersWithoutDeferredGoal",
    "PlannerUnknownAgentName",
    "PlannerUnknownDep",
    "PluginIntentContract",
    "PluginIwsPolicy",
    "PluginReadOnlyLspRefresh",
    "PluginServiceEvict",
    "PluginSetupFailure",
    "PluginWriteAllowedPublish",
    "SCENARIO_REGISTRY",
]
