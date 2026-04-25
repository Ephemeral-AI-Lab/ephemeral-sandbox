"""Agent definition model and constants."""

from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, Field, field_validator

#: Valid effort level strings.
EFFORT_LEVELS: tuple[str, ...] = ("low", "medium", "high")

AgentSource = Literal["builtin", "user", "plugin"]
AgentType = Literal["agent", "subagent"]


class AgentDefinition(BaseModel):
    """Full agent definition with all configuration fields."""

    # --- required ---
    name: str
    description: str

    # --- prompt ---
    system_prompt: str | None = None

    # --- model & effort ---
    model: str | None = None
    effort: str | int | None = None

    # --- agent loop control ---
    # Per-ephemeral-run cap on tool dispatches. ``None`` = unlimited.
    # Each ``EphemeralAgent`` spawn starts with a fresh counter, so
    # nested ``run_subagent`` calls have independent budgets and the
    # caller's counter is untouched.
    tool_call_limit: int | None = None

    # --- skills & tools ---
    skills: list[str] = Field(default_factory=list)
    tools: list[str] = Field(
        default_factory=list,
        description="Tool names available to this agent.",
    )
    terminal_tools: list[str] = Field(
        default_factory=list,
        description="Tool names that end the agent's query loop when invoked.",
    )

    # --- hooks ---
    hooks: dict[str, Any] | None = None

    # --- lifecycle ---
    background: bool = False
    initial_prompt: str | None = None

    # --- role metadata ---
    # Optional freeform label for UI display and tool-factory context.
    role: str | None = None

    # --- metadata ---
    critical_system_reminder: str | None = None

    # --- Python-specific ---
    permissions: list[str] = Field(default_factory=list)
    source: AgentSource = "builtin"

    # --- agent type: regular agent or subagent (worker) ---
    # Descriptive label kept for logging / UI. Engine behaviour is driven
    # by the explicit capability flags below, not by this string.
    agent_type: AgentType = "agent"

    # Capability flags (authoritative for engine behaviour).
    # ``can_spawn_subagents`` gates registration of background management tools
    # (subagents cannot launch their own background work or spawn further
    # subagents). ``require_fresh_client`` forces a dedicated API client
    # per agent instance — used for subagents so concurrent workers do
    # not share one httpx connection pool. ``include_skills`` opts into
    # automatic skill tool registration; test harnesses set it to
    # False to get a minimal tool surface.
    can_spawn_subagents: bool = True
    require_fresh_client: bool = False
    include_skills: bool = True
    # Whether this agent should appear as a valid ``run_subagent``
    # target in tool schemas and pass the runtime dispatch gate.
    # Defaults to False — only ``agent_type="subagent"`` agents are
    # promoted to True in ``model_post_init``.
    dispatchable_via_run_subagent: bool = False

    model_config = {"populate_by_name": True, "arbitrary_types_allowed": True}

    @field_validator(
        "skills",
        "permissions",
        "tools",
        "terminal_tools",
        mode="before",
    )
    @classmethod
    def _split_csv(cls, v: Any) -> Any:
        if isinstance(v, str):
            return [s.strip() for s in v.split(",") if s.strip()]
        return v

    @field_validator("tool_call_limit", mode="before")
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

    def model_post_init(self, _context: Any) -> None:
        # Keep ``agent_type`` strings in sync with capability flags so
        # existing callers that read ``agent_def.agent_type`` continue to
        # work without caring that the flags are now authoritative.
        if self.agent_type == "subagent":
            self.can_spawn_subagents = False
            self.require_fresh_client = True
            self.dispatchable_via_run_subagent = True
