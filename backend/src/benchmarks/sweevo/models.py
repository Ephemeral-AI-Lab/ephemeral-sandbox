"""SWE-EVO data structures and constants."""

from __future__ import annotations

import re
from dataclasses import dataclass, field


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

_REQUEST_TIMEOUT_MULTIPLIER = 2
_DEFAULT_SANDBOX_COMMAND_TIMEOUT = 120 * _REQUEST_TIMEOUT_MULTIPLIER
_DEFAULT_SNAPSHOT_CREATE_TIMEOUT = 600 * _REQUEST_TIMEOUT_MULTIPLIER
_DEFAULT_SANDBOX_SETUP_TIMEOUT = 180 * _REQUEST_TIMEOUT_MULTIPLIER
_DEFAULT_SWEEVO_TEST_TIMEOUT = 600 * _REQUEST_TIMEOUT_MULTIPLIER
_DEFAULT_SWEEVO_PLANNING_TIMEOUT = 240.0 * _REQUEST_TIMEOUT_MULTIPLIER
_DEFAULT_WORKER_TIMEOUT = 600 * _REQUEST_TIMEOUT_MULTIPLIER  # 20 min per agent
_DEFAULT_AGENT_NAME = "python-developer"
_DEFAULT_TEAM_ID = "sweevo"
_REPO_DIR = "/testbed"  # SWE-EVO Docker images mount repos here
_DEFAULT_DATASET_SOURCE = "Fsoft-AIC/SWE-EVO"
_DEFAULT_TARGET_BULLETS = 10
_MAX_PRECOMPUTED_TEST_FILES = 6
_MAX_PRECOMPUTED_TOP_LEVEL_PATHS = 8
_MAX_PRECOMPUTED_FOCUS_PATHS = 6
# SWE-EVO Docker images use conda envs with the correct Python version.
# All commands must be prefixed with conda activation.
_CONDA_ACTIVATE = ". /opt/miniconda3/etc/profile.d/conda.sh && conda activate testbed"


# ---------------------------------------------------------------------------
# Data structures
# ---------------------------------------------------------------------------


@dataclass
class SWEEvoInstance:
    """A single SWE-EVO benchmark instance."""

    instance_id: str  # e.g. "iterative__dvc_1.0.0a1_1.0.0a2"
    repo: str  # e.g. "iterative/dvc"
    base_commit: str
    problem_statement: str  # changelog / release notes
    patch: str  # gold solution diff
    fail_to_pass: list[str]  # test IDs that must flip to pass
    pass_to_pass: list[str]  # test IDs that must stay passing
    docker_image: str  # e.g. "xingyaoww/sweb.eval.x86_64.iterative_s_dvc-3760"
    test_cmds: str  # e.g. "pytest --continue-on-collection-errors -rA"
    environment_setup_commit: str
    test_patch: str = ""  # diff that adds F2P tests (applied during evaluation)
    # Optional metadata
    start_version: str = ""
    end_version: str = ""
    instance_id_swe: str = ""  # SWE-bench compatible ID


@dataclass
class SWEEvoResult:
    """Result of running a SWE-EVO instance through EphemeralOS."""

    plan_id: str
    instance_id: str
    status: str = "pending"  # "completed" | "failed"
    agent_patch: str = ""  # combined git diff from all agents
    resolved: bool = False  # all F2P pass, no P2P broken
    fix_rate: float = 0.0  # fraction of F2P tests passing (partial credit)
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


def _normalize_sweevo_image_ref(image_ref: str) -> str:
    """Add an explicit ``:latest`` tag when an image ref omits tag/digest."""
    normalized = (image_ref or "").strip()
    if not normalized:
        return normalized
    image_tail = normalized.rsplit("/", 1)[-1]
    if ":" in image_tail or "@" in image_tail:
        return normalized
    return f"{normalized}:latest"


def _truncate_dns_label(name: str, *, limit: int = 63) -> str:
    if len(name) <= limit:
        return name
    suffix = name[-8:]
    return f"{name[: limit - 9]}-{suffix}"


def _strip_exit_code_marker(output: str) -> str:
    return re.sub(r"\n?EXIT_CODE=\d+\s*$", "", output, flags=re.S)
