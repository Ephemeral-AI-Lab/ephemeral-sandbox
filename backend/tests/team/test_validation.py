"""Unit tests for team.planning.validation.validate_plan."""

from __future__ import annotations

from unittest.mock import patch

from team.core.models import Plan, TaskDefinition
from team.planning.validation import validate_plan


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _spec(
    id_: str,
    agent: str = "developer",
    goal: str = "do work",
    deps: list[str] | None = None,
    scope_paths: list[str] | None = None,
    description: str = "test task",
) -> TaskDefinition:
    return TaskDefinition(
        id=id_,
        spec={
            "goal": goal,
            "detail": f"Detail for {goal}",
            "acceptance_criteria": f"Acceptance for {goal}",
        },
        agent=agent,
        description=description,
        deps=deps or [],
        scope_paths=scope_paths or [],
    )


def _plan(*specs: TaskDefinition, rationale: str | None = None) -> Plan:
    return Plan(tasks=list(specs), rationale=rationale)


# We need to patch agent resolution so tests don't depend on real registry state.
# The conftest in test_team registers standard agents, but here we directly patch
# since we're in a different test directory.
_AGENT_EXISTS_PATH = "team.planning.validation._agent_exists"
_HAS_ROLE_PATH = "team.planning.validation._has_role"
_GET_DEFN_PATH = "team.planning.validation._get_definition"


def _mock_agent(agent_type: str = "agent"):
    """Return a simple namespace that looks like an AgentDefinition."""
    class _Defn:
        role = "developer"

    _Defn.agent_type = agent_type
    return _Defn()


# ---------------------------------------------------------------------------
# Empty plan
# ---------------------------------------------------------------------------


def test_empty_plan_fails_by_default():
    plan = _plan()
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)
    assert any("no tasks" in i["msg"] for i in issues)


# ---------------------------------------------------------------------------
# Max plan size
# ---------------------------------------------------------------------------


def test_plan_exceeding_max_plan_size_fails():
    specs = [_spec(f"t{i}") for i in range(10)]
    plan = _plan(*specs)
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan, max_plan_size=5)
    assert any("exceeds max_plan_size" in i["msg"] for i in issues)


def test_plan_at_max_plan_size_passes():
    specs = [_spec(f"t{i}") for i in range(5)]
    plan = _plan(*specs)
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan, max_plan_size=5)
    # Should not have a size-related issue
    assert not any("exceeds max_plan_size" in i["msg"] for i in issues)


# ---------------------------------------------------------------------------
# Duplicate IDs
# ---------------------------------------------------------------------------


def test_duplicate_task_ids_fail():
    specs = [_spec("t1"), _spec("t1")]
    plan = _plan(*specs)
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)
    assert any("duplicate task id" in i["msg"] for i in issues)


# ---------------------------------------------------------------------------
# Agent name validation
# ---------------------------------------------------------------------------


def test_missing_agent_name_fails():
    spec = TaskDefinition(id="t1", spec=_spec("template").spec, agent="")
    plan = _plan(spec)
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)
    assert any("agent is required" in i["msg"] for i in issues)


def test_unknown_agent_name_fails():
    spec = _spec("t1", agent="nonexistent_agent")
    plan = _plan(spec)
    with patch(_AGENT_EXISTS_PATH, return_value=False), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=None):
        issues = validate_plan(plan)
    assert any("unknown agent" in i["msg"] for i in issues)


def test_known_agent_passes_agent_check():
    spec = _spec("t1", agent="developer")
    plan = _plan(spec)
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)
    assert not any("unknown agent" in i["msg"] or "agent is required" in i["msg"] for i in issues)


# ---------------------------------------------------------------------------
# Description
# ---------------------------------------------------------------------------


def test_description_allows_long_labels():
    spec = _spec(
        "t1",
        description=(
            "one two three four five six seven eight nine ten eleven twelve "
            "thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty twentyone"
        ),
        scope_paths=["src/api.py"],
    )
    plan = _plan(spec)
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)

    assert not any("description has" in i["msg"] for i in issues)


# ---------------------------------------------------------------------------
# Cycle detection
# ---------------------------------------------------------------------------


def test_cycle_a_depends_on_b_b_depends_on_a_detected():
    a = _spec("A", deps=["B"])
    b = _spec("B", deps=["A"])
    plan = _plan(a, b)
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)
    assert any("cycle detected" in i["msg"] for i in issues)


def test_self_referencing_dep_creates_cycle():
    spec = _spec("t1", deps=["t1"])
    plan = _plan(spec)
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)
    assert any("cycle detected" in i["msg"] for i in issues)


def test_linear_chain_no_cycle():
    a = _spec("A")
    b = _spec("B", deps=["A"])
    c = _spec("C", deps=["B"])
    plan = _plan(a, b, c)
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)
    assert not any("cycle" in i["msg"] for i in issues)


def test_diamond_dependency_no_cycle():
    a = _spec("A")
    b = _spec("B", deps=["A"])
    c = _spec("C", deps=["A"])
    d = _spec("D", deps=["B", "C"])
    plan = _plan(a, b, c, d)
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)
    assert not any("cycle" in i["msg"] for i in issues)


# ---------------------------------------------------------------------------
# Valid plan returns empty issues
# ---------------------------------------------------------------------------


def test_valid_simple_plan_returns_no_issues():
    a = _spec("A")
    b = _spec("B", deps=["A"])
    plan = _plan(a, b)
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)
    # May have validator policy issues but not structural issues
    structural_issues = [
        i for i in issues
        if any(kw in i["msg"] for kw in ["duplicate", "unknown agent", "cycle", "no tasks", "agent is required"])
    ]
    assert structural_issues == []


# ---------------------------------------------------------------------------
# External dep refs
# ---------------------------------------------------------------------------


def test_unknown_dep_reference_fails():
    spec = _spec("t1", deps=["external-ghost"])
    plan = _plan(spec)
    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)
    assert any("unknown dep" in i["msg"] for i in issues)


# ---------------------------------------------------------------------------
# Extra validators
# ---------------------------------------------------------------------------


def _mock_validator_agent():
    """Return a namespace that looks like a validator AgentDefinition."""
    class _Defn:
        role = "reviewer"
        agent_type = "agent"
    return _Defn()


def test_validator_plan_passes_without_policy_field():
    """Validators no longer need a separate failure-policy field in the plan."""
    dev = _spec("dev-1")
    val = TaskDefinition(
        id="val-root",
        spec={
            "goal": "validate",
            "detail": "Validate the upstream task.",
            "acceptance_criteria": "Report pass/fail evidence.",
        },
        agent="validator",
        deps=["dev-1"],
    )
    plan = _plan(dev, val)

    def side_effect_exists(name):
        return True

    def side_effect_role(name, role):
        return name == "validator" and role == "reviewer"

    def side_effect_defn(name):
        if name == "validator":
            return _mock_validator_agent()
        return _mock_agent()

    with patch(_AGENT_EXISTS_PATH, side_effect=side_effect_exists), \
         patch(_HAS_ROLE_PATH, side_effect=side_effect_role), \
         patch(_GET_DEFN_PATH, side_effect=side_effect_defn):
        issues = validate_plan(plan)
    assert not any("validator task" in i["msg"] and "continue" in i["msg"] for i in issues)


def test_three_concrete_tasks_do_not_require_validator():
    plan = _plan(_spec("a"), _spec("b"), _spec("c"))

    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)

    assert not any("must include at least one terminal validator" in i["msg"] for i in issues)


def test_multiple_terminal_validators_are_allowed():
    dev = _spec("dev")
    val_a = _spec("val-a", agent="validator", deps=["dev"])
    val_b = _spec("val-b", agent="validator", deps=["dev"])
    plan = _plan(dev, val_a, val_b)

    def side_effect_role(name, role):
        return name == "validator" and role == "reviewer"

    def side_effect_defn(name):
        if name == "validator":
            return _mock_validator_agent()
        return _mock_agent()

    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, side_effect=side_effect_role), \
         patch(_GET_DEFN_PATH, side_effect=side_effect_defn):
        issues = validate_plan(plan)

    assert not any("exactly one validator" in i["msg"] for i in issues)


# ---------------------------------------------------------------------------
# Extra validators
# ---------------------------------------------------------------------------


def test_extra_validators_are_called():
    a = _spec("A")
    plan = _plan(a)
    called = []

    def extra(items):
        called.append(True)
        return [{"field": "tasks", "msg": "custom error"}]

    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan, extra_validators=[extra])
    assert called
    assert any("custom error" in i["msg"] for i in issues)


def test_parallel_tasks_with_shared_scope_paths_pass_without_sequencing():
    left = _spec("dev-plot", scope_paths=["dvc/command/plot.py", "dvc/repo/plot/data.py"])
    right = _spec("dev-cli", scope_paths=["dvc/command/plot.py", "dvc/command/update.py"])
    plan = _plan(left, right)

    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)

    assert not any("share overlapping scope_paths" in i["msg"] for i in issues)


def test_parallel_tasks_with_parent_child_scope_paths_pass_without_sequencing():
    left = _spec("dev-hdf", scope_paths=["dask/dataframe/io/hdf.py"])
    right = _spec("dev-cli-config-compat", scope_paths=["dask"])
    plan = _plan(left, right)

    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)

    assert not any("share overlapping scope_paths" in i["msg"] for i in issues)


def test_sequenced_tasks_with_shared_scope_paths_pass():
    left = _spec("dev-plot", scope_paths=["dvc/command/plot.py"])
    right = _spec("dev-cli", deps=["dev-plot"], scope_paths=["dvc/command/plot.py"])
    plan = _plan(left, right)

    with patch(_AGENT_EXISTS_PATH, return_value=True), \
         patch(_HAS_ROLE_PATH, return_value=False), \
         patch(_GET_DEFN_PATH, return_value=_mock_agent()):
        issues = validate_plan(plan)

    assert not any("share overlapping scope_paths" in i["msg"] for i in issues)
