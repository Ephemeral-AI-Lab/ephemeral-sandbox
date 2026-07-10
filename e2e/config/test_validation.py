"""A2 — invalid config rejection on both lanes.

Lane A negatives target the ``runtime`` section, which the gateway itself
deserializes and validates on every ``create_sandbox`` (host-side), so a bad
value surfaces as a structured create error with rollback — no container, no
registry record.

Lane B negatives (``manager`` section) are load-at-start: the gateway must
fail to serve. That assertion is process-level (start wrapper exit status /
readiness never answering) — the one place log-free verification is
impossible pre-RPC. These run last in the family and deliberately leave no
gateway running; the package finalizer's baseline restore covers them.
"""

import pytest

from config import helpers

pytestmark = pytest.mark.config


def test_unknown_daemon_key_fails_create(lane_a_daemon_yaml):
    """F1 — deny_unknown_fields surfaces through create_sandbox, with rollback.
    F6 — restoring a valid YAML recovers with no gateway restart."""
    helpers.rewrite_daemon_yaml(
        lane_a_daemon_yaml, {"runtime": {"workspace": {"bogus_knob": 1}}}
    )
    before = helpers.sandbox_ids()
    from manager.management import helpers as mgmt

    result = mgmt.create_sandbox()
    error = helpers.error_text(result)
    assert "bogus_knob" in error or "unknown field" in error, error
    assert helpers.sandbox_ids() == before, "failed create must not leave a record"

    helpers.rewrite_daemon_yaml(lane_a_daemon_yaml)
    with helpers.sandbox() as recovered_id:
        assert recovered_id


@pytest.mark.parametrize(
    ("overrides", "expected_substring"),
    [
        pytest.param(
            {"runtime": {"workspace": {"setup_timeout_s": 0}}},
            "setup_timeout_s",
            id="setup-timeout-zero",
        ),
        pytest.param(
            {"runtime": {"workspace": {"exit_grace_s": -1}}},
            "exit_grace_s",
            id="exit-grace-negative",
        ),
        pytest.param(
            {"runtime": {"workspace": {"scratch_root": "relative/workspace"}}},
            "scratch_root",
            id="relative-scratch-root",
        ),
        pytest.param(
            {"runtime": {"workspace": {"layer_stack_root": "/"}}},
            "filesystem root",
            id="filesystem-root-guard",
        ),
    ],
)
def test_invalid_daemon_values_fail_create(lane_a_daemon_yaml, overrides, expected_substring):
    """F2/F3 — semantic validation failures surface structurally and roll back."""
    helpers.rewrite_daemon_yaml(lane_a_daemon_yaml, overrides)
    before = helpers.sandbox_ids()
    from manager.management import helpers as mgmt

    result = mgmt.create_sandbox()
    error = helpers.error_text(result)
    assert expected_substring in error, f"missing {expected_substring!r} in: {error}"
    assert helpers.sandbox_ids() == before, "failed create must not leave a record"


def test_valid_config_recovers(lane_a_daemon_yaml):
    """F6 — after the invalid arms above, a valid rewrite creates cleanly on the
    same gateway (Lane A reload also recovers, no restart)."""
    helpers.rewrite_daemon_yaml(lane_a_daemon_yaml)
    with helpers.sandbox() as sandbox_id:
        assert helpers.exec_output(sandbox_id, "echo recovered").strip() == "recovered"


@pytest.mark.slow
def test_unknown_manager_key_fails_gateway_start(tmp_path, config_family_custody):
    """F4 — an unknown manager.docker key must fail gateway start (Lane B)."""
    bad = helpers.make_config(
        {"manager": {"docker": {"bogus_manager_knob": True}}}, tmp_path / "gateway.yml"
    )
    assert helpers.gateway_start_fails(bad), (
        "gateway served despite an unknown manager.docker key"
    )


@pytest.mark.slow
def test_invalid_manager_value_fails_gateway_start(tmp_path, config_family_custody):
    """F5 — a semantic manager.docker violation must fail gateway start (Lane B)."""
    bad = helpers.make_config(
        {"manager": {"docker": {"readiness_timeout_ms": 0}}}, tmp_path / "gateway.yml"
    )
    assert helpers.gateway_start_fails(bad), (
        "gateway served despite readiness_timeout_ms: 0"
    )
