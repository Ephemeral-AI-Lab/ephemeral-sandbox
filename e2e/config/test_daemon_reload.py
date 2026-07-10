"""A1 — Lane A config delivery mechanics + daemon-side behavior knobs.

Lane A contract: the Docker installer re-reads the daemon YAML from
``manager.docker.daemon_config_yaml_path`` on every ``create_sandbox`` and
uploads those bytes into the new container. A rewrite therefore governs the
next sandbox — never a running one — with no gateway restart.

Every probe is CLI-driven and asserts structured operation JSON. In-sandbox
observation uses one-shot ``exec_command``, which runs inside an automatic
workspace session (mount mask applied, publish_then_destroy finalize policy).
"""

import pytest

from config import helpers

pytestmark = pytest.mark.config

# Ubuntu images ship /root with dotfiles (.bashrc, .profile), so an empty
# listing is only explainable by the mount mask's empty tmpfs.
MASK_PROBE_DIR = "/root"


def test_rewrite_applies_to_next_sandbox(lane_a_daemon_yaml):
    """F1 per-create reload + F2 create-time binding, one flow.

    Sandbox A is created while the mask hides /root; the rewrite that unhides
    /root is observed by sandbox B (F1) while A keeps its create-time mask (F2).
    """
    helpers.rewrite_daemon_yaml(
        lane_a_daemon_yaml,
        {"runner": {"mount_mask": {"hidden_paths": ["/eos", MASK_PROBE_DIR]}}},
    )
    with helpers.sandbox() as sandbox_a:
        masked = helpers.exec_output(sandbox_a, f"ls -A {MASK_PROBE_DIR}")
        assert masked.strip() == "", f"expected masked {MASK_PROBE_DIR}: {masked!r}"

        helpers.rewrite_daemon_yaml(lane_a_daemon_yaml)
        with helpers.sandbox() as sandbox_b:
            visible = helpers.exec_output(sandbox_b, f"ls -A {MASK_PROBE_DIR}")
            assert visible.strip() != "", (
                "rewrite not observed by the next sandbox (gateway-level caching?)"
            )

        still_masked = helpers.exec_output(sandbox_a, f"ls -A {MASK_PROBE_DIR}")
        assert still_masked.strip() == "", (
            "config must bind at create: the first sandbox saw a later rewrite"
        )


@pytest.mark.parametrize(
    ("hidden_paths", "probe_dir", "expect_masked"),
    [
        pytest.param(["/eos"], "/eos", True, id="baseline-hides-eos"),
        pytest.param(["/eos"], MASK_PROBE_DIR, False, id="baseline-shows-root"),
        pytest.param(["/eos", MASK_PROBE_DIR], MASK_PROBE_DIR, True, id="extended-hides-root"),
    ],
)
def test_mount_mask_hides_paths(lane_a_daemon_yaml, hidden_paths, probe_dir, expect_masked):
    """F3 — runner.mount_mask.hidden_paths governs in-session visibility."""
    helpers.rewrite_daemon_yaml(
        lane_a_daemon_yaml, {"runner": {"mount_mask": {"hidden_paths": hidden_paths}}}
    )
    with helpers.sandbox() as sandbox_id:
        listing = helpers.exec_output(sandbox_id, f"ls -A {probe_dir}").strip()
        if expect_masked:
            assert listing == "", f"{probe_dir} should be masked: {listing!r}"
        else:
            assert listing != "", f"{probe_dir} should be visible"


def test_setup_timeout_tiny_fails_session(lane_a_daemon_yaml):
    """F4 — a tiny runtime.workspace.setup_timeout_s fails workspace-session
    setup with a timeout-classed error; the default arm succeeds.

    The cleanest "a runtime float from YAML changed daemon behavior" probe.
    The budget is 1e-9 s, not 1 ms: on fast hosts the ns-holder can win a
    1 ms race, while a sub-poll-resolution budget expires before the first
    readiness read, deterministically. The deadline surfaces as the setup
    step that missed its signal (kind operation_failed, "workspace setup
    failed at ns_holder did not signal ns-up"), asserted by kind/substring
    per the family convention.
    """
    helpers.rewrite_daemon_yaml(
        lane_a_daemon_yaml, {"runtime": {"workspace": {"setup_timeout_s": 1.0e-9}}}
    )
    with helpers.sandbox() as sandbox_id:
        result = helpers.exec_in_sandbox(sandbox_id, "true")
        error = helpers.error_text(result)
        assert "workspace setup failed" in error, error
        assert "did not signal" in error or "timed out" in error, error

    helpers.rewrite_daemon_yaml(lane_a_daemon_yaml)
    with helpers.sandbox() as control_id:
        assert helpers.exec_output(control_id, "echo setup-ok").strip() == "setup-ok"


def test_relocated_roots_functional(lane_a_daemon_yaml):
    """F5 — relocated scratch roots stay fully functional.

    The paths are container-internal, so functional invariance is the
    observable: create ready, a command runs, and a file write round-trips
    through publish (one-shot sessions publish to the layerstack on finalize).

    layer_stack_root deliberately stays at its default: the manager pins the
    shared-base mount target to the container layer-stack root constant
    (create_sandbox.rs CONTAINER_LAYER_STACK_ROOT), so relocating it makes
    daemon boot panic at workspace-base initialization today. Recorded as a
    config-consolidation finding; this family pins present-day behavior.
    """
    helpers.rewrite_daemon_yaml(
        lane_a_daemon_yaml,
        {
            "runtime": {
                "workspace": {"scratch_root": "/eos/workspace-alt"},
                "namespace_execution": {"scratch_root": "/eos/namespace-execution-alt"},
            }
        },
    )
    with helpers.sandbox() as sandbox_id:
        helpers.exec_output(sandbox_id, "echo relocated > /workspace/relocated.txt")
        round_trip = helpers.exec_output(sandbox_id, "cat /workspace/relocated.txt")
        assert round_trip.strip() == "relocated"


def test_observability_toggle(lane_a_daemon_yaml):
    """F6 — observability.enabled governs the views while operations still work.

    Disabled: the daemon's observer is a no-op, so the events view answers
    with an empty set even after commands ran, while operations succeed.
    Enabled: the same flow populates the view.
    """
    helpers.rewrite_daemon_yaml(lane_a_daemon_yaml, {"observability": {"enabled": False}})
    with helpers.sandbox() as disabled_id:
        assert helpers.exec_output(disabled_id, "echo obs-off").strip() == "obs-off"
        events = helpers.observability_events(disabled_id)
        assert isinstance(events, dict) and events.get("events") == [], (
            f"disabled arm must report no events (no-op observer): {events}"
        )

    helpers.rewrite_daemon_yaml(lane_a_daemon_yaml, {"observability": {"enabled": True}})
    with helpers.sandbox() as enabled_id:
        assert helpers.exec_output(enabled_id, "echo obs-on").strip() == "obs-on"
        events = helpers.observability_events(enabled_id)
        assert isinstance(events, dict) and events.get("events"), (
            f"enabled arm must return populated events: {events}"
        )


def test_single_worker_thread_functional(lane_a_daemon_yaml):
    """F7 — daemon.server.max_worker_threads: 1 is accepted and functional
    (no stronger observable exists via the CLI)."""
    helpers.rewrite_daemon_yaml(lane_a_daemon_yaml, {"daemon": {"server": {"max_worker_threads": 1}}})
    with helpers.sandbox() as sandbox_id:
        for step in ("one", "two", "three"):
            assert helpers.exec_output(sandbox_id, f"echo {step}").strip() == step
