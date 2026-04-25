"""Path resolution for EphemeralOS configuration and data directories.

Follows XDG-like conventions with ~/.ephemeralos/ as the default base directory.
"""

from __future__ import annotations

import os
from pathlib import Path

_DEFAULT_BASE_DIR = ".ephemeralos"
_CONFIG_FILE_NAME = "settings.json"
_REPO_CONFIG_DIR_NAME = "config"


def get_repo_config_dir() -> Path:
    """Return the repository-bundled config directory.

    In source checkouts this is ``backend/config``.  Wheels may include the
    directory as data under ``backend/config`` beside installed packages.
    """
    here = Path(__file__).resolve()
    candidates: list[Path] = []

    if len(here.parents) > 2:
        candidates.append(here.parents[2] / _REPO_CONFIG_DIR_NAME)
    if len(here.parents) > 3:
        candidates.append(here.parents[3] / "backend" / _REPO_CONFIG_DIR_NAME)
    if len(here.parents) > 1:
        candidates.append(here.parents[1] / "backend" / _REPO_CONFIG_DIR_NAME)

    for candidate in candidates:
        if candidate.is_dir():
            return candidate
    if not candidates:
        return here.parent / _REPO_CONFIG_DIR_NAME
    return candidates[0]


def get_config_agents_dir() -> Path:
    """Return the repository config agent-definition directory."""
    return get_repo_config_dir() / "agents"


def get_config_skills_dir() -> Path:
    """Return the repository config skill-definition directory."""
    return get_repo_config_dir() / "skills"


def get_config_dir() -> Path:
    """Return the configuration directory, creating it if needed.

    Resolution order:
    1. EPHEMERALOS_CONFIG_DIR environment variable
    2. ~/.ephemeralos/
    """
    env_dir = os.environ.get("EPHEMERALOS_CONFIG_DIR")
    if env_dir:
        config_dir = Path(env_dir)
    else:
        config_dir = Path.home() / _DEFAULT_BASE_DIR

    config_dir.mkdir(parents=True, exist_ok=True)
    return config_dir


def get_config_file_path() -> Path:
    """Return the path to the main settings file (~/.ephemeralos/settings.json)."""
    return get_config_dir() / _CONFIG_FILE_NAME


def get_data_dir() -> Path:
    """Return the data directory for caches, history, etc.

    Resolution order:
    1. EPHEMERALOS_DATA_DIR environment variable
    2. ~/.ephemeralos/data/
    """
    env_dir = os.environ.get("EPHEMERALOS_DATA_DIR")
    if env_dir:
        data_dir = Path(env_dir)
    else:
        data_dir = get_config_dir() / "data"

    data_dir.mkdir(parents=True, exist_ok=True)
    return data_dir


def get_logs_dir() -> Path:
    """Return the logs directory.

    Resolution order:
    1. EPHEMERALOS_LOGS_DIR environment variable
    2. ~/.ephemeralos/logs/
    """
    env_dir = os.environ.get("EPHEMERALOS_LOGS_DIR")
    if env_dir:
        logs_dir = Path(env_dir)
    else:
        logs_dir = get_config_dir() / "logs"

    logs_dir.mkdir(parents=True, exist_ok=True)
    return logs_dir


def get_feedback_dir() -> Path:
    """Return the feedback storage directory."""
    feedback_dir = get_data_dir() / "feedback"
    feedback_dir.mkdir(parents=True, exist_ok=True)
    return feedback_dir


def get_project_config_dir(cwd: str | Path) -> Path:
    """Return the per-project .ephemeralos directory."""
    project_dir = Path(cwd).resolve() / ".ephemeralos"
    project_dir.mkdir(parents=True, exist_ok=True)
    return project_dir


def get_project_issue_file(cwd: str | Path) -> Path:
    """Return the per-project issue context file."""
    return get_project_config_dir(cwd) / "issue.md"


def get_project_pr_comments_file(cwd: str | Path) -> Path:
    """Return the per-project PR comments context file."""
    return get_project_config_dir(cwd) / "pr_comments.md"
