"""Configuration system for EphemeralOS.

Provides settings management, path resolution, and API key handling.
"""

from .defaults import (
    DEFAULT_MAX_RETRIES,
    DEFAULT_BASE_DELAY,
    DEFAULT_MAX_DELAY,
    DEFAULT_RETRY_STATUS_CODES,
    DEFAULT_DATABASE_POOL_SIZE,
    DEFAULT_DATABASE_MAX_OVERFLOW,
    DEFAULT_SANDBOX_CI_ROOT,
)
from .paths import (
    get_config_agents_dir,
    get_config_dir,
    get_config_file_path,
    get_config_skills_dir,
    get_data_dir,
    get_logs_dir,
    get_repo_config_dir,
)
from .settings import DatabaseSettings, Settings, load_settings, save_settings

__all__ = [
    "DatabaseSettings",
    "Settings",
    "get_config_agents_dir",
    "get_config_dir",
    "get_config_file_path",
    "get_config_skills_dir",
    "get_data_dir",
    "get_logs_dir",
    "get_repo_config_dir",
    "load_settings",
    "save_settings",
    "DEFAULT_MAX_RETRIES",
    "DEFAULT_BASE_DELAY",
    "DEFAULT_MAX_DELAY",
    "DEFAULT_RETRY_STATUS_CODES",
    "DEFAULT_DATABASE_POOL_SIZE",
    "DEFAULT_DATABASE_MAX_OVERFLOW",
    "DEFAULT_SANDBOX_CI_ROOT",
]
