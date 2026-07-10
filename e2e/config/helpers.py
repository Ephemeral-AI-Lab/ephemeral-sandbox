"""config family helpers: generated YAML configs + family gateway custody.

The family never mutates the checked-in ``config/prd.yml`` / ``config/bench.yml``;
every arm runs against a config generated under pytest tmp by ``make_config``.

Gateway custody follows one rule: transitions never stop a gateway explicitly.
``bin/start-sandbox-docker-gateway`` replaces whatever is running, and the
package finalizer's baseline restore (see ``config/conftest.py``) is just one
more start. Both gateways in a transition share ``gateway_instance_id``, so a
freshly started gateway recovers the previous one's containers and the session
cleanup net can still destroy anything a test leaked.
"""

import contextlib
import copy
import json
import logging
import os
import subprocess
import time

import yaml

from core import cli as climod
from core.config import CONFIG_YAML, REPO_ROOT, START_GATEWAY
from manager.management import helpers as mgmt

_log = logging.getLogger("e2e.config")

GATEWAY_READY_TIMEOUT_S = 120
START_SCRIPT_TIMEOUT_S = 600


# --- generated configs ------------------------------------------------------


def deep_merge(base, overrides):
    """Documented sandbox-config merge semantics: objects merge recursively,
    scalars and arrays replace."""
    merged = copy.deepcopy(base)
    for key, value in overrides.items():
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = deep_merge(merged[key], value)
        else:
            merged[key] = copy.deepcopy(value)
    return merged


def baseline_config():
    """The checked-in baseline document (read-only input)."""
    with open(CONFIG_YAML, encoding="utf-8") as handle:
        return yaml.safe_load(handle)


def make_config(overrides, out_path):
    """Write baseline ⊕ overrides to ``out_path`` (pytest tmp only) and return it.

    File *generation* only — the Rust merge engine has its own unit coverage.
    """
    merged = deep_merge(baseline_config(), overrides)
    out_path.write_text(yaml.safe_dump(merged, sort_keys=False), encoding="utf-8")
    return out_path


def rewrite_daemon_yaml(daemon_yaml_path, overrides=None):
    """Regenerate the family's daemon YAML from baseline ⊕ overrides (Lane A).

    The Docker installer re-reads this file on every ``create_sandbox``, so the
    rewrite governs the *next* sandbox; existing sandboxes keep the bytes they
    were created with.
    """
    return make_config(overrides or {}, daemon_yaml_path)


# --- gateway custody (Lane B) -----------------------------------------------


def _gateway_env(config_path):
    env = dict(os.environ)
    env["SANDBOX_GATEWAY_CONFIG_YAML"] = str(config_path)
    return env


def run_start_script(config_path):
    """Run the gateway start script against ``config_path``.

    The script stops any running gateway first, so every call is a full
    replace. Returns the script's CompletedProcess; callers decide whether
    failure is an error or the expected outcome.
    """
    return subprocess.run(
        [str(START_GATEWAY)],
        cwd=str(REPO_ROOT),
        env=_gateway_env(config_path),
        capture_output=True,
        text=True,
        timeout=START_SCRIPT_TIMEOUT_S,
    )


def _responding():
    try:
        return not climod.is_error(climod.manager("list_sandboxes", timeout=15))
    except (climod.CliError, subprocess.SubprocessError):
        return False


def wait_gateway_ready(timeout=GATEWAY_READY_TIMEOUT_S):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if _responding():
            return
        time.sleep(0.5)
    raise RuntimeError("gateway did not become ready")


def start_gateway(config_path):
    """Start (replacing any running gateway) on ``config_path`` and wait ready."""
    _log.info("starting family gateway on %s", config_path)
    completed = run_start_script(config_path)
    assert completed.returncode == 0, (
        f"gateway start script failed (rc={completed.returncode}):\n"
        f"{completed.stdout}\n{completed.stderr}"
    )
    wait_gateway_ready()


def gateway_start_fails(config_path, settle_s=5.0):
    """Start a gateway expected to die on config load; True when it failed to serve.

    Process-level contract (A2-F4/F5): the start wrapper reports failure, or the
    gateway never answers. Either way no gateway is left running afterwards.
    """
    completed = run_start_script(config_path)
    if completed.returncode != 0:
        _log.info("gateway start failed as expected: rc=%s", completed.returncode)
        return True
    deadline = time.monotonic() + settle_s
    while time.monotonic() < deadline:
        if _responding():
            return False
        time.sleep(0.5)
    return True


def restore_baseline_gateway():
    """Restore the baseline gateway every other family expects."""
    start_gateway(CONFIG_YAML)


@contextlib.contextmanager
def gateway_with_config(config_path):
    """Own the gateway for a Lane B arm. No stop on exit — the next arm's (or
    the finalizer's) start replaces this gateway."""
    start_gateway(config_path)
    yield


# --- sandbox + in-sandbox observation ---------------------------------------


def create_sandbox_or_fail():
    created = mgmt.create_sandbox()
    assert isinstance(created, dict) and created.get("id"), (
        f"create_sandbox failed: {created}"
    )
    return created["id"]


@contextlib.contextmanager
def sandbox():
    """A ready sandbox on the currently running gateway, destroyed on exit."""
    sandbox_id = create_sandbox_or_fail()
    try:
        yield sandbox_id
    finally:
        mgmt.destroy_sandbox(sandbox_id)


def sandbox_ids():
    listed = climod.manager("list_sandboxes")
    assert isinstance(listed, dict) and "sandboxes" in listed, (
        f"list_sandboxes failed: {listed}"
    )
    return {entry.get("id") for entry in listed["sandboxes"]}


def exec_in_sandbox(sandbox_id, command, timeout=180):
    """One-shot ``exec_command``: runs in an automatic workspace session with
    finalize policy publish_then_destroy, so writes publish to the layerstack
    and are visible to later commands."""
    return climod.runtime(sandbox_id, "exec_command", command, timeout=timeout)


def exec_output(sandbox_id, command, timeout=180):
    """Run ``command`` in-sandbox, assert it succeeded, return its output."""
    result = exec_in_sandbox(sandbox_id, command, timeout=timeout)
    assert isinstance(result, dict) and result.get("status") == "ok", (
        f"exec_command {command!r} failed: {result}"
    )
    return result.get("output", "")


def observability_events(sandbox_id, last_n=10):
    """Per-sandbox observability events view (daemon-backed)."""
    return climod.observability("events", "--sandbox-id", sandbox_id, "--last-n", last_n)


def error_text(result):
    """Assert ``result`` is a structured error and return it as one string for
    kind/substring matching (never exact message text)."""
    assert climod.is_error(result), f"expected structured error, got: {result}"
    return json.dumps(result["error"]).lower()
