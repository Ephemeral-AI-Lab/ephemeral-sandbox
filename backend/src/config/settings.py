"""Settings model and loading logic for EphemeralOS.

Settings are resolved with the following precedence (highest first):
1. CLI arguments
2. Environment variables (ANTHROPIC_API_KEY, EPHEMERALOS_MODEL, etc.)
3. Config file (~/.ephemeralos/settings.json)
4. Defaults
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

from pydantic import BaseModel, Field

from ephemeralos.hooks.schemas import HookDefinition
from ephemeralos.mcp.types import McpServerConfig


class DatabaseSettings(BaseModel):
    """PostgreSQL database configuration."""

    url: str = ""
    pool_pre_ping: bool = True
    pool_size: int = 5
    max_overflow: int = 10
    echo: bool = False

    def is_configured(self) -> bool:
        """Return True if a database URL has been set."""
        return bool(self.url)


class MemorySettings(BaseModel):
    """Memory system configuration."""

    enabled: bool = True
    max_files: int = 5
    max_entrypoint_lines: int = 200


class Settings(BaseModel):
    """Main settings model for EphemeralOS."""

    # API configuration
    api_key: str = ""
    model: str = "claude-sonnet-4-20250514"
    max_tokens: int = 16384
    base_url: str | None = None
    api_format: str = "anthropic"  # "anthropic" or "openai"

    # Behavior
    system_prompt: str | None = None
    hooks: dict[str, list[HookDefinition]] = Field(default_factory=dict)
    memory: MemorySettings = Field(default_factory=MemorySettings)
    enabled_plugins: dict[str, bool] = Field(default_factory=dict)
    mcp_servers: dict[str, McpServerConfig] = Field(default_factory=dict)

    # Database
    database: DatabaseSettings = Field(default_factory=DatabaseSettings)

    # Daytona sandbox
    daytona_api_key: str = ""
    daytona_api_url: str = ""
    daytona_target: str = ""

    # UI
    theme: str = "default"
    fast_mode: bool = False
    effort: str = "medium"
    passes: int = 1
    verbose: bool = False

    def resolve_api_key(self) -> str:
        """Resolve API key with precedence: instance value > env var > empty.

        Returns the API key string. Raises ValueError if no key is found.
        """
        if self.api_key:
            return self.api_key

        env_key = os.environ.get("ANTHROPIC_API_KEY", "")
        if env_key:
            return env_key

        # Also check OPENAI_API_KEY for openai-format providers
        openai_key = os.environ.get("OPENAI_API_KEY", "")
        if openai_key:
            return openai_key

        raise ValueError(
            "No API key found. Set ANTHROPIC_API_KEY (or OPENAI_API_KEY for openai-format "
            "providers) environment variable, or configure api_key in "
            "~/.ephemeralos/settings.json"
        )

    def merge_cli_overrides(self, **overrides: Any) -> Settings:
        """Return a new Settings with CLI overrides applied (non-None values only)."""
        updates = {k: v for k, v in overrides.items() if v is not None}
        return self.model_copy(update=updates)


def _apply_env_overrides(settings: Settings) -> Settings:
    """Apply supported environment variable overrides over loaded settings."""
    updates: dict[str, Any] = {}
    model = os.environ.get("ANTHROPIC_MODEL") or os.environ.get("EPHEMERALOS_MODEL")
    if model:
        updates["model"] = model

    base_url = os.environ.get("ANTHROPIC_BASE_URL") or os.environ.get("EPHEMERALOS_BASE_URL")
    if base_url:
        updates["base_url"] = base_url

    max_tokens = os.environ.get("EPHEMERALOS_MAX_TOKENS")
    if max_tokens:
        updates["max_tokens"] = int(max_tokens)

    api_key = os.environ.get("ANTHROPIC_API_KEY") or os.environ.get("OPENAI_API_KEY")
    if api_key:
        updates["api_key"] = api_key

    api_format = os.environ.get("EPHEMERALOS_API_FORMAT")
    if api_format:
        updates["api_format"] = api_format

    database_url = os.environ.get("EPHEMERALOS_DATABASE_URL")
    if database_url:
        db = settings.database.model_copy(update={"url": database_url})
        updates["database"] = db

    if not updates:
        return settings
    return settings.model_copy(update=updates)


def load_settings(config_path: Path | None = None) -> Settings:
    """Load settings from config file, merging with defaults.

    Args:
        config_path: Path to settings.json. If None, uses the default location.

    Returns:
        Settings instance with file values merged over defaults.
    """
    if config_path is None:
        from ephemeralos.config.paths import get_config_file_path

        config_path = get_config_file_path()

    if config_path.exists():
        raw = json.loads(config_path.read_text(encoding="utf-8"))
        return _apply_env_overrides(Settings.model_validate(raw))

    return _apply_env_overrides(Settings())


def save_settings(settings: Settings, config_path: Path | None = None) -> None:
    """Persist settings to the config file.

    Args:
        settings: Settings instance to save.
        config_path: Path to write. If None, uses the default location.
    """
    if config_path is None:
        from ephemeralos.config.paths import get_config_file_path

        config_path = get_config_file_path()

    config_path.parent.mkdir(parents=True, exist_ok=True)
    config_path.write_text(
        settings.model_dump_json(indent=2) + "\n",
        encoding="utf-8",
    )
