"""Agent definition model and constants."""

from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, Field

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

#: Valid effort level strings.
EFFORT_LEVELS: tuple[str, ...] = ("low", "medium", "high")


# ---------------------------------------------------------------------------
# AgentDefinition model
# ---------------------------------------------------------------------------


class AgentDefinition(BaseModel):
    """Full agent definition with all configuration fields.

    First-class fields:
    - ``name``          — unique agent identifier
    - ``description``   — when-to-use description
    - ``system_prompt`` — agent instructions
    - ``model``         — LLM model override (alias: ``model_key``)
    - ``skills``        — list of skill slugs
    - ``toolkits``      — list of toolkit names
    - ``subagent_type`` — routing key / agent type (alias: ``type``)
    """

    # --- required ---
    name: str
    description: str

    # --- prompt ---
    system_prompt: str | None = None

    # --- model & effort ---
    model: str | None = Field(default=None, alias="model_key")  # accepts both 'model' and 'model_key'
    effort: str | int | None = None  # "low" | "medium" | "high" or positive int

    # --- agent loop control ---
    max_turns: int | None = None  # maximum agentic turns before stopping

    # --- skills & toolkits ---
    skills: list[str] = Field(default_factory=list)
    toolkits: list[str] = Field(default_factory=list)  # allowed toolkit names

    # --- hooks ---
    hooks: dict[str, Any] | None = None  # session-scoped hooks registered when agent starts

    # --- lifecycle ---
    background: bool = False  # always run as background task when spawned
    initial_prompt: str | None = None  # prepended to the first user turn

    # --- metadata ---
    filename: str | None = None  # original filename without .md extension
    base_dir: str | None = None  # directory the agent definition was loaded from
    critical_system_reminder: str | None = None  # short message re-injected at every user turn
    pending_snapshot_update: dict[str, Any] | None = None
    omit_claude_md: bool = False  # skip CLAUDE.md injection for this agent

    # --- Python-specific ---
    permissions: list[str] = Field(default_factory=list)
    subagent_type: str = Field(default="general-purpose", alias="type")  # accepts both
    source: Literal["builtin", "user", "plugin"] = "builtin"

    model_config = {"populate_by_name": True}  # allow both field name and alias


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def parse_str_list(raw: Any) -> list[str] | None:
    """Parse a comma-separated string or list into a list of strings."""
    if raw is None:
        return None
    if isinstance(raw, list):
        return [str(item).strip() for item in raw if str(item).strip()]
    if isinstance(raw, str):
        items = [t.strip() for t in raw.split(",") if t.strip()]
        return items if items else None
    return None


def parse_positive_int(raw: Any) -> int | None:
    """Parse a positive integer, returning None if invalid."""
    if raw is None:
        return None
    try:
        val = int(raw)
        return val if val > 0 else None
    except (TypeError, ValueError):
        return None
