"""A3 — Lane B manager.docker knobs.

The manager section is deserialized once at gateway start, so each arm starts
its own gateway against a generated YAML (serial, slow). Arms never stop the
gateway they started: the next start — or the package finalizer's baseline
restore — replaces it.
"""

import uuid

import pytest

from config import helpers

pytestmark = [pytest.mark.config, pytest.mark.slow]


def test_container_env_probe(tmp_path, config_family_custody):
    """F1/F2 — container_env rides gateway config → Docker → daemon → command
    environment; a config without the nonce yields the plain value (control).

    The runner builds command environments from an allowlist (shell_exec
    request HOST_KEYS: PATH/HOME/proxy vars…), so an arbitrary variable never
    reaches a command. The probe therefore carries a nonce inside NO_PROXY —
    an allowlisted variable the baseline already sets — which proves the same
    end-to-end path without violating the env-sanitization contract.
    """
    nonce = f"e2e-config-probe-{uuid.uuid4().hex}.invalid"
    baseline_no_proxy = helpers.baseline_config()["manager"]["docker"]["container_env"][
        "NO_PROXY"
    ]
    probe_yaml = helpers.make_config(
        {
            "manager": {
                "docker": {"container_env": {"NO_PROXY": f"{baseline_no_proxy},{nonce}"}}
            }
        },
        tmp_path / "gateway-probe.yml",
    )
    with helpers.gateway_with_config(probe_yaml):
        with helpers.sandbox() as sandbox_id:
            value = helpers.exec_output(sandbox_id, "printenv NO_PROXY")
            assert nonce in value, f"nonce missing from NO_PROXY: {value!r}"

    control_yaml = helpers.make_config({}, tmp_path / "gateway-control.yml")
    with helpers.gateway_with_config(control_yaml):
        with helpers.sandbox() as sandbox_id:
            value = helpers.exec_output(sandbox_id, "printenv NO_PROXY")
            assert nonce not in value, f"nonce leaked into the control arm: {value!r}"
            assert value.strip() == baseline_no_proxy


def test_memory_bytes_cgroup_max(tmp_path, config_family_custody):
    """F3 — manager.docker.memory_bytes lands in the container's cgroup
    memory.max (conditional skip only when the probe file is absent)."""
    memory_bytes = 268435456
    memory_yaml = helpers.make_config(
        {"manager": {"docker": {"memory_bytes": memory_bytes}}},
        tmp_path / "gateway-memory.yml",
    )
    with helpers.gateway_with_config(memory_yaml):
        with helpers.sandbox() as sandbox_id:
            result = helpers.exec_in_sandbox(sandbox_id, "cat /sys/fs/cgroup/memory.max")
            if result.get("status") != "ok":
                pytest.skip("cgroup memory.max not readable in this image/runtime")
            assert result.get("output", "").strip() == str(memory_bytes), result


def test_explicit_image_flag_outranks_default_image(tmp_path, config_family_custody):
    """F4 — the CLI requires a non-empty --image (pinned below), so this pins
    the precedence contract instead: the explicit flag outranks
    manager.docker.default_image."""
    from core import cli as climod
    from core.config import IMAGE, WORKSPACE_ROOT

    default_image_yaml = helpers.make_config(
        {"manager": {"docker": {"default_image": "debian:12"}}},
        tmp_path / "gateway-default-image.yml",
    )
    with helpers.gateway_with_config(default_image_yaml):
        empty = climod.manager(
            "create_sandbox", "--image", "", "--workspace-bind-root", WORKSPACE_ROOT
        )
        assert "non-empty" in helpers.error_text(empty), (
            "empty --image should be rejected by the CLI contract"
        )

        with helpers.sandbox() as sandbox_id:
            os_release = helpers.exec_output(sandbox_id, "cat /etc/os-release").lower()
            flag_image_name = IMAGE.split(":")[0]
            assert f'name="{flag_image_name}' in os_release, (
                f"explicit --image {IMAGE} must outrank default_image: {os_release}"
            )


def test_privileged_arm_functional(tmp_path, config_family_custody):
    """F5 — the privileged legacy escape hatch still creates and runs commands
    (the de-privileged default is exercised by every other test)."""
    privileged_yaml = helpers.make_config(
        {"manager": {"docker": {"privileged": True}}}, tmp_path / "gateway-privileged.yml"
    )
    with helpers.gateway_with_config(privileged_yaml):
        with helpers.sandbox() as sandbox_id:
            assert helpers.exec_output(sandbox_id, "echo privileged-ok").strip() == "privileged-ok"
