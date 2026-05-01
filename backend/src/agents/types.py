"""Agent definition model and constants."""

from __future__ import annotations

from typing import Any, Literal

from pydantic import (
    BaseModel,
    ConfigDict,
    Field,
    field_validator,
)

from notification.rules import NotificationRule

AgentType = Literal["agent", "subagent"]


class AgentSelectionBlock(BaseModel):
    """Frontmatter-safe subset of :class:`ContextBlock`.

    Variants declare these directly on the agent definition; the resolver
    converts them into real ``ContextBlock`` instances and appends them to the
    packet after recipe build.
    """

    kind: str = Field(min_length=1)
    priority: str = Field(default="required")
    text: str
    source_id: str | None = None
    source_kind: str | None = None
    metadata: dict[str, str] = Field(default_factory=dict)

    model_config = ConfigDict(extra="forbid")


class AgentVariant(BaseModel):
    """One frontmatter-declared capability variant.

    Resolution is short-circuit + first-match-wins by declared order. The
    target ``use:`` agent must be registered, must not declare its own
    ``variants:`` (no chaining), and must have a ``context_recipe``.
    """

    when: str = Field(min_length=1)
    use: str = Field(min_length=1)
    note: str = ""
    required_context_blocks: list[AgentSelectionBlock] = Field(default_factory=list)

    model_config = ConfigDict(extra="forbid")


class AgentDefinition(BaseModel):
    """Full agent definition with all configuration fields."""

    # --- required ---
    name: str
    description: str

    # --- prompt ---
    system_prompt: str | None = None

    # --- model ---
    model: str | None = None

    # --- agent loop control ---
    # Per-ephemeral-run cap on tool dispatches. ``None`` = unlimited.
    # Each ``EphemeralAgent`` spawn starts with a fresh counter, so
    # nested ``run_subagent`` calls have independent budgets and the
    # caller's counter is untouched.
    tool_call_limit: int | None = None

    # --- skills ---
    skills: list[str] = Field(default_factory=list)

    # --- lifecycle ---
    background: bool = False

    # --- role metadata ---
    # Optional freeform label for UI display and tool-factory context.
    role: str | None = None

    # --- Python-specific ---
    permissions: list[str] = Field(default_factory=list)

    # --- agent type: regular agent or subagent (worker) ---
    agent_type: AgentType = "agent"

    # --- run tool surface ---
    # Tools the agent may call during a run. The agent's tool registry is
    # filtered to ``allowed_tools ∪ terminals``; the LLM only sees those.
    allowed_tools: list[str] = Field(default_factory=list)
    # Terminal tools — calling any of these ends the query loop. The legacy
    # TaskCenter terminal package has been removed, so definitions only get
    # terminal behavior when they explicitly name a registered terminal tool.
    terminals: list[str] = Field(default_factory=list)
    # Declarative notification trigger ids resolved into NotificationRule
    # instances by runtime-specific launch code.
    notification_triggers: list[str] = Field(default_factory=list)

    # --- notification rules ---
    # Rules evaluated at the top of every model turn (see
    # ``notification.rules.dispatch_rules``). Empty list = no notifications.
    # Built via factories in ``notification.library``.
    notification_rules: list[NotificationRule] = Field(default_factory=list)

    # --- context engine (ContextComposer) ---
    # Recipe id resolved at compose time. Required when the agent is launched
    # via ``ContextComposer``; helper / subagent definitions that pre-date the
    # context engine may keep this null.
    context_recipe: str | None = None
    # Frontmatter-declared capability variants. Empty list = no variants
    # (resolver fast-paths). Variant chaining is forbidden — the
    # ``use:`` target must itself have empty ``variants``; enforced by
    # ``validate_agent_definitions_resolved``.
    variants: list[AgentVariant] = Field(default_factory=list)

    model_config = ConfigDict(
        populate_by_name=True,
        arbitrary_types_allowed=True,
        extra="forbid",
    )

    @field_validator(
        "skills",
        "permissions",
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

    @field_validator("background", mode="before")
    @classmethod
    def _coerce_bool(cls, v: Any) -> Any:
        if isinstance(v, str):
            return v.lower() == "true"
        return bool(v) if v is not None else False

    @field_validator("terminals")
    @classmethod
    def _check_terminals(cls, terminals: list[str]) -> list[str]:
        return [terminal for terminal in terminals if terminal.strip()]

    @field_validator("notification_triggers")
    @classmethod
    def _check_notification_triggers(cls, triggers: list[str]) -> list[str]:
        return [trigger for trigger in triggers if trigger.strip()]
