"""SWE-EVO data structures, constants, and pure helpers."""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

_REQUEST_TIMEOUT_MULTIPLIER = 2
_DEFAULT_SANDBOX_COMMAND_TIMEOUT = 120 * _REQUEST_TIMEOUT_MULTIPLIER
_DEFAULT_SNAPSHOT_CREATE_TIMEOUT = 600 * _REQUEST_TIMEOUT_MULTIPLIER
_DEFAULT_SANDBOX_SETUP_TIMEOUT = 180 * _REQUEST_TIMEOUT_MULTIPLIER
_DEFAULT_SWEEVO_TEST_TIMEOUT = 600 * _REQUEST_TIMEOUT_MULTIPLIER
_REPO_DIR = "/testbed"  # SWE-EVO Docker images mount repos here
_DEFAULT_DATASET_SOURCE = "Fsoft-AIC/SWE-EVO"
_DEFAULT_TARGET_BULLETS = 10
_CONDA_ACTIVATE = ". /opt/miniconda3/etc/profile.d/conda.sh && conda activate testbed"
_DEFAULT_SWEEVO_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"


# ---------------------------------------------------------------------------
# Data structures
# ---------------------------------------------------------------------------


@dataclass
class SWEEvoInstance:
    """A single SWE-EVO benchmark instance."""

    instance_id: str
    repo: str
    base_commit: str
    problem_statement: str
    patch: str
    fail_to_pass: list[str]
    pass_to_pass: list[str]
    docker_image: str
    test_cmds: str
    environment_setup_commit: str
    test_patch: str = ""
    start_version: str = ""
    end_version: str = ""
    instance_id_swe: str = ""
    pr_description: str = ""


@dataclass
class SWEEvoResult:
    """Result of running a SWE-EVO instance through EphemeralOS."""

    plan_id: str
    instance_id: str
    status: str = "pending"  # "completed" | "failed"
    agent_patch: str = ""
    resolved: bool = False
    fix_rate: float = 0.0
    fail_to_pass_passed: int = 0
    fail_to_pass_total: int = 0
    pass_to_pass_broken: int = 0
    pass_to_pass_total: int = 0
    duration_s: float = 0.0
    task_count: int = 0
    tasks_completed: int = 0
    tasks_failed: int = 0
    error: str = ""
    task_summaries: dict[str, str] = field(default_factory=dict)


@dataclass(frozen=True)
class PreContext:
    """Inputs assembled by ``setup.preflight`` and threaded into ``provision_sandbox``."""

    instance: "SWEEvoInstance"
    repo_dir: str
    snapshot_name: str
    goal: str
    audit_dir: Path
    max_duration_s: float


# ---------------------------------------------------------------------------
# Pure helpers
# ---------------------------------------------------------------------------


def _normalize_sweevo_image_ref(image_ref: str) -> str:
    """Return the dataset-provided image reference without altering versioning."""
    return (image_ref or "").strip()


def _has_explicit_sweevo_image_version(image_ref: str) -> bool:
    """Whether an image ref carries a non-``latest`` explicit version.

    Snapshot registration requires a concrete image version. Bare repository
    refs resolve to ``latest`` and explicit ``:latest`` tags are also rejected
    by the snapshot path.
    """
    ref = (image_ref or "").strip()
    if not ref:
        return False
    if "@" in ref:
        return True
    last_segment = ref.rsplit("/", 1)[-1]
    if ":" not in last_segment:
        return False
    _, tag = last_segment.rsplit(":", 1)
    return bool(tag and tag.lower() != "latest")


def _truncate_dns_label(name: str, *, limit: int = 63) -> str:
    if len(name) <= limit:
        return name
    suffix = name[-8:]
    return f"{name[: limit - 9]}-{suffix}"


def _strip_exit_code_marker(output: str) -> str:
    return re.sub(r"\n?EXIT_CODE=\d+\s*$", "", output, flags=re.S)


def _sweevo_sandbox_name(instance: SWEEvoInstance) -> str:
    """Deterministic, instance-stable container name.

    The sweevo workflow persists one container per ``instance_id`` so back-to-back
    runs reuse the same container. Docker enforces name uniqueness; the second
    invocation fails fast on "container already in use" instead of leaking.
    """
    return _truncate_dns_label(f"sweevo-{instance.instance_id}")


def _sweevo_sandbox_labels(instance: SWEEvoInstance, repo_dir: str) -> dict[str, str]:
    return {
        "purpose": "sweevo-test",
        "project_dir": repo_dir,
        "sweevo_instance": instance.instance_id,
        "sweevo_repo": instance.repo,
    }


def default_sweevo_snapshot_name(instance: SWEEvoInstance) -> str:
    """Return a stable snapshot/image name for a SWE-EVO instance."""
    name = f"sweevo-{instance.instance_id_swe or instance.instance_id}"
    return name[:63]
