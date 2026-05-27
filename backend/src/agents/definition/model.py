"""Agent definition model and constants."""

from __future__ import annotations

from collections.abc import Callable
from enum import StrEnum
from pathlib import Path
from typing import Any, Protocol, runtime_checkable

from pydantic import (
    BaseModel,
    ConfigDict,
    Field,
    field_validator,
)

class AgentType(StrEnum):
    """Runtime class of an agent profile."""

    AGENT = "agent"
    SUBAGENT = "subagent"


class AgentKind(StrEnum):
    """Canonical category of an agent profile.

    Values mirror the previous free-form ``AgentDefinition.role`` strings
    byte-for-byte, so audit consumers reading the ``metadata["role"]`` key
    (emitted by ``factory.py`` and ``run_subagent.py``) continue to see the
    same string set. Planner and executor profiles can participate in
    launch-time terminal-tool routing; ADVISOR / EXPLORER are helper /
    subagent kinds.
    """

    PLANNER = "planner"
    EXECUTOR = "executor"
    VERIFIER = "verifier"
    EVALUATOR = "evaluator"
    ADVISOR = "advisor"
    EXPLORER = "explorer"


# Pydantic can't accept ``notification.NotificationRule`` directly here because
# the rule type is defined cross-package and using it would re-introduce a
# forward-reference cycle (see ``notification/rules/model.py:18-22``). This
# structural Protocol is the supported workaround; do not replace it with a
# concrete import. The @runtime_checkable decorator is load-bearing — Pydantic
# uses ``isinstance(rule, AgentNotificationRule)`` to validate the
# ``notification_rules: list[AgentNotificationRule]`` field at construction
# time and Protocol isinstance checks require the decorator.
@runtime_checkable
class AgentNotificationRule(Protocol):
    """Runtime notification rule shape consumed by agent definitions."""

    name: str
    body: Callable[..., str]
    trigger: Callable[..., bool]
    fire_once: bool


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

    # Grace allowance past ``tool_call_limit``. Hard ceiling is reached when
    # ``overshoot_units = max(0, tool_calls_used - tool_call_limit) +
    # text_only_no_terminal_turns`` exceeds this value. ``None`` disables
    # tolerance — without it the hard cap is not enforced. Only meaningful
    # when ``tool_call_limit`` is set.
    max_tolerance_after_max_tool_call: int | None = 10

    # --- agent kind ---
    # Canonical category of this profile (planner / executor / verifier /
    # evaluator / advisor / explorer). Routing logic and the planner
    # submission gate read this; audit consumers read the same set of
    # strings via ``agent_kind.value`` through the ``metadata["role"]`` key
    # emitted by ``factory.py`` and ``run_subagent.py``. Profile MDs MUST
    # declare ``agent_kind:`` in frontmatter — the loader rejects MDs that
    # omit it. The Pydantic default exists only so test fixtures that build
    # ``AgentDefinition`` directly stay terse; production agents always go
    # through the loader gate.
    agent_kind: AgentKind = AgentKind.EXECUTOR
    # Planner-submission gate. Only profiles explicitly flagged True may be
    # named as ``agent_name`` in a planner submission. Defaults False so helper
    # and subagent profiles are never planner-submittable by accident.
    dispatchable_by_planner: bool = False

    # --- agent type: regular agent or subagent (worker) ---
    agent_type: AgentType = AgentType.AGENT

    # --- run tool surface ---
    # Tools the agent may call during a run. The agent's tool registry is
    # filtered to ``allowed_tools ∪ terminals``; the LLM only sees those.
    allowed_tools: list[str] = Field(default_factory=list)
    # Terminal tools — calling any of these ends the query loop. Definitions
    # only get terminal behavior when they explicitly name a registered terminal tool.
    terminals: list[str] = Field(default_factory=list)
    # Declarative notification trigger ids resolved into NotificationRule
    # instances by runtime-specific launch code.
    notification_triggers: list[str] = Field(default_factory=list)

    # --- notification rules ---
    # Rules evaluated at the top of every model turn (see
    # the notification rule engine. Empty list = no notifications.
    notification_rules: list[AgentNotificationRule] = Field(default_factory=list)

    # --- skill (Round 3) ---
    # Absolute path to the agent's workflow SKILL.md, resolved by the loader
    # from the relative ``skill:`` frontmatter field. ``None`` when no skill is
    # declared. Skill-equipped agents get row 4 (skill +
    # terminal_tool_selection) composed at launch.
    skill: Path | None = None

    # --- context engine (AgentEntryComposer) ---
    # Recipe id resolved at compose time. Required when the agent is launched
    # via ``AgentEntryComposer``; helper / subagent definitions that pre-date
    # the context engine may keep this null.
    context_recipe: str | None = None
    model_config = ConfigDict(
        populate_by_name=True,
        arbitrary_types_allowed=True,
        extra="forbid",
    )

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

    @field_validator("max_tolerance_after_max_tool_call", mode="before")
    @classmethod
    def _coerce_nonneg_int(cls, v: Any) -> Any:
        if v is None or isinstance(v, int):
            return v if (v is None or v >= 0) else None
        try:
            n = int(v)
            return n if n >= 0 else None
        except (TypeError, ValueError):
            return None

    @field_validator("terminals")
    @classmethod
    def _check_terminals(cls, terminals: list[str]) -> list[str]:
        return [terminal for terminal in terminals if terminal.strip()]

    @field_validator("notification_triggers")
    @classmethod
    def _check_notification_triggers(cls, triggers: list[str]) -> list[str]:
        return [trigger for trigger in triggers if trigger.strip()]
