"""Agent definition model and constants."""

from __future__ import annotations

from typing import Any, Literal

from pydantic import AliasChoices, BaseModel, Field, field_validator

#: Valid effort level strings.
EFFORT_LEVELS: tuple[str, ...] = ("low", "medium", "high")


class AgentDefinition(BaseModel):
    """Full agent definition with all configuration fields."""

    # --- required ---
    name: str
    description: str

    # --- prompt ---
    system_prompt: str | None = None

    # --- model & effort ---
    model: str | None = Field(
        default=None,
        validation_alias=AliasChoices("model", "model_key"),
        serialization_alias="model_key",
    )
    effort: str | int | None = None

    # --- agent loop control ---
    max_turns: int | None = Field(
        default=None, validation_alias=AliasChoices("max_turns", "maxTurns")
    )

    # --- skills & toolkits ---
    skills: list[str] = Field(default_factory=list)
    toolkits: list[str] = Field(default_factory=list)

    # --- hooks ---
    hooks: dict[str, Any] | None = None

    # --- lifecycle ---
    background: bool = False
    initial_prompt: str | None = Field(
        default=None, validation_alias=AliasChoices("initial_prompt", "initialPrompt")
    )

    # --- metadata ---
    critical_system_reminder: str | None = Field(
        default=None,
        validation_alias=AliasChoices("critical_system_reminder", "criticalSystemReminder"),
    )

    # --- Python-specific ---
    permissions: list[str] = Field(default_factory=list)
    source: Literal["builtin", "user", "plugin"] = "builtin"

    # --- agent type: regular agent or subagent (worker) ---
    # Descriptive label kept for logging / UI. Engine behaviour is driven
    # by the explicit capability flags below, not by this string.
    agent_type: Literal["agent", "subagent"] = "agent"

    # Capability flags (authoritative for engine behaviour).
    # ``can_spawn_subagents`` gates registration of the background toolkit
    # (subagents cannot launch their own background work or spawn further
    # subagents). ``require_fresh_client`` forces a dedicated API client
    # per agent instance — used for subagents so concurrent workers do
    # not share one httpx connection pool. ``include_skills`` opts into
    # automatic skill toolkit registration; test harnesses set it to
    # False to get a minimal tool surface.
    can_spawn_subagents: bool = True
    require_fresh_client: bool = False
    include_skills: bool = True

    # --- standalone tools ---
    # Names of standalone tools (registered via
    # ``tools.core.factory.register_standalone_tool``) to append to this
    # agent's tool registry. Lets an agent declare individual tools that
    # don't belong to any toolkit — e.g. the ``submit_plan_agent`` builtin
    # uses ``extra_tools=["submit_plan"]`` with empty ``toolkits``.
    extra_tools: list[str] = Field(default_factory=list)

    # --- posthook ---
    # Optional structured-output posthook. When set, the engine runs this
    # agent's work phase, then runs another *registered* agent (looked up
    # by ``cfg.agent_name``) whose job is to serialize the work output via
    # a single submit tool. Typed ``Any`` to avoid an import cycle with
    # ``hooks.agent_posthook``.
    posthook: Any | None = None

    model_config = {"populate_by_name": True, "arbitrary_types_allowed": True}

    @field_validator("skills", "toolkits", "permissions", mode="before")
    @classmethod
    def _split_csv(cls, v: Any) -> Any:
        if isinstance(v, str):
            return [s.strip() for s in v.split(",") if s.strip()]
        return v

    @field_validator("max_turns", mode="before")
    @classmethod
    def _coerce_positive_int(cls, v: Any) -> Any:
        if v is None or isinstance(v, int):
            return v if (v is None or v > 0) else None
        try:
            n = int(v)
            return n if n > 0 else None
        except (TypeError, ValueError):
            return None

    @field_validator("effort", mode="before")
    @classmethod
    def _validate_effort(cls, v: Any) -> Any:
        if v is None:
            return None
        if isinstance(v, int):
            return v if v > 0 else None
        if isinstance(v, str) and v in EFFORT_LEVELS:
            return v
        return None

    @field_validator("background", mode="before")
    @classmethod
    def _coerce_bool(cls, v: Any) -> Any:
        if isinstance(v, str):
            return v.lower() == "true"
        return bool(v) if v is not None else False

    def model_post_init(self, __context: Any) -> None:
        # Keep legacy ``agent_type`` strings in sync with capability flags
        # so existing callers that read ``agent_def.agent_type`` continue
        # to work without caring that the flags are now authoritative.
        if self.agent_type == "subagent":
            self.can_spawn_subagents = False
            self.require_fresh_client = True
