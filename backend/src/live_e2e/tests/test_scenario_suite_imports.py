"""Offline conformance tests for the focused scenario suite.

Verifies every scenario in :data:`SCENARIO_REGISTRY` satisfies the
``Scenario`` protocol, declares a non-empty ``expected_event_sequence``, and
matches the dotted ``<package>.<file>`` naming convention for focused
scenarios. Pure import + structural checks; no Daytona, no Postgres.
"""

from __future__ import annotations

import pytest

from live_e2e.audit.events import EventType
from live_e2e.scenarios import SCENARIO_REGISTRY
from live_e2e.scenarios.base import Scenario, ScenarioBase
from live_e2e.scenarios.capacity import CAPACITY_PACK_SPECS

pytestmark = pytest.mark.live_e2e_offline

_COMPOSITE_NAMES = frozenset(
    {
        "correctness_testing",
        "full_case_user_input",
        "full_stack_adversarial",
    }
)


def test_registry_is_non_empty() -> None:
    assert SCENARIO_REGISTRY, "SCENARIO_REGISTRY is empty"


def test_every_scenario_implements_protocol() -> None:
    for key, cls in SCENARIO_REGISTRY.items():
        assert issubclass(cls, ScenarioBase), f"{key} is not a ScenarioBase subclass"
        instance = cls()
        assert isinstance(instance, Scenario), (
            f"{key} does not satisfy the Scenario protocol"
        )


def test_focused_scenarios_use_dotted_names() -> None:
    for key, cls in SCENARIO_REGISTRY.items():
        if key in _COMPOSITE_NAMES:
            continue
        assert "." in key, (
            f"focused scenario {key} must use dotted <package>.<file> form"
        )
        assert cls.name == key, (
            f"scenario class.name {cls.name!r} must match registry key {key!r}"
        )


def test_every_scenario_declares_expected_event_sequence() -> None:
    for key, cls in SCENARIO_REGISTRY.items():
        instance = cls()
        sequence = tuple(instance.expected_event_sequence)
        assert sequence, f"{key} declares empty expected_event_sequence"
        for event_type in sequence:
            assert isinstance(event_type, EventType), (
                f"{key}: {event_type!r} is not an EventType"
            )


def test_capacity_pack_catalog_has_coverage_anchor() -> None:
    assert CAPACITY_PACK_SPECS, "capacity pack catalog is empty"
    for spec in CAPACITY_PACK_SPECS:
        assert spec.implementation_anchor, (
            f"{spec.name} has no registry/test/superseded coverage anchor"
        )
        if spec.registry_name is not None:
            assert spec.registry_name in SCENARIO_REGISTRY, (
                f"{spec.name} points to missing registry scenario {spec.registry_name}"
            )


def test_capacity_action_contract_and_modules_import() -> None:
    from live_e2e.squad import capacity_actions  # noqa: PLC0415
    from live_e2e.squad.capacity_actions import (  # noqa: PLC0415
        audit,
        context,
        graph,
        guardrails,
        lsp,
        metrics,
        workspace,
    )

    result = capacity_actions.CapacityActionResult(
        name="smoke",
        summary="ok",
        artifact_path=None,
        expected_errors=(),
        counters={"total": 0},
    )
    assert result.counters["total"] == 0
    assert metrics.full_system_capacity_metrics_script
    assert graph.__all__ == []
    assert workspace.__all__ == []
    assert lsp.__all__ == []
    assert guardrails.__all__ == []
    assert context.__all__ == []
    assert audit.__all__ == []


def test_subpackage_imports_are_clean() -> None:
    # Smoke: each subpackage imports without side effects and exposes its
    # implemented scenarios via __all__.
    from live_e2e.scenarios import (  # noqa: PLC0415
        capacity,
        context,
        pipeline,
        planner_validation,
        sandbox,
        tools,
    )

    assert pipeline.__all__ == [
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
    assert sandbox.__all__ == [
        "AutoSquashCommitResume",
        "ComplexProjectBuild",
        "ComplexProjectBuildShellEditLsp",
        "ComplexProjectBuildShellEditLspSmoke",
        "ComplexProjectBuildSmoke",
        "OccConcurrentConflicts",
    ]
    assert planner_validation.__all__ == [
        "PlannerCycleInDeps",
        "PlannerDuplicateLocalId",
        "PlannerEmptyTasks",
        "PlannerPartialWithoutContinuationGoal",
        "PlannerUnknownAgentName",
        "PlannerUnknownDep",
    ]
    assert capacity.__all__ == [
        "CAPACITY_PACK_SPECS",
        "CapacityPackSpec",
        "FullSystemCapacityMatrix",
    ]
    # tools/ and context/ are scaffold-only in this PR.
    assert tools.__all__ == []
    assert context.__all__ == []
