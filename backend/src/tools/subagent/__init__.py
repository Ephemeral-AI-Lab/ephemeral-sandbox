"""Subagent tools — spawn focused worker subagents."""

from __future__ import annotations

from typing import ClassVar

from pydantic import Field, field_validator

from agents.registry import list_dispatchable_subagent_names
from tools.core.base import BaseTool, ToolExecutionContextService, ToolResult
from tools.subagent.run_subagent import run_subagent


def _allowed_subagent_names_for_caller(caller_agent: str) -> tuple[str, ...]:
    return tuple(list_dispatchable_subagent_names())


def _build_restricted_input_model(allowed_agent_names: tuple[str, ...]):
    allowed_list = list(allowed_agent_names)
    description = "Name of a dispatchable subagent target."
    if allowed_list:
        description += f" Allowed targets for this caller: {', '.join(allowed_list)}."
    else:
        description += " No dispatchable subagent targets are available for this caller."

    class RestrictedRunSubagentInput(run_subagent.input_model):  # type: ignore[misc, valid-type]
        _allowed_agent_names: ClassVar[tuple[str, ...]] = allowed_agent_names

        agent_name: str = Field(
            description=description,
            json_schema_extra={"enum": allowed_list},
        )

        @field_validator("agent_name")
        @classmethod
        def _validate_agent_name(cls, value: str) -> str:
            if not cls._allowed_agent_names:
                raise ValueError(
                    "No dispatchable subagent targets are available for this caller."
                )
            if value not in cls._allowed_agent_names:
                allowed = ", ".join(cls._allowed_agent_names)
                raise ValueError(
                    f"agent_name must be one of the dispatchable subagent targets: {allowed}"
                )
            return value

    RestrictedRunSubagentInput.__name__ = "RestrictedRunSubagentInput"
    return RestrictedRunSubagentInput


class RestrictedRunSubagentTool(BaseTool):
    """Caller-aware wrapper that narrows run_subagent's agent_name schema."""

    __doc__ = run_subagent.__doc__

    def __init__(self, *, allowed_agent_names: tuple[str, ...]) -> None:
        self._delegate = run_subagent
        self.name = run_subagent.name
        self.description = run_subagent.description
        self.short_description = run_subagent.short_description
        self.input_model = _build_restricted_input_model(allowed_agent_names)
        self.output_model = run_subagent.output_model
        self.background = run_subagent.background
        self.task_type = run_subagent.task_type

    async def execute(self, arguments, context: ToolExecutionContextService) -> ToolResult:  # type: ignore[override]
        return await self._delegate.execute(arguments, context)


def make_subagent_tools(*, caller_agent: str = "") -> list[BaseTool]:
    """Return caller-scoped subagent dispatch tools."""
    allowed = _allowed_subagent_names_for_caller(caller_agent)
    return [RestrictedRunSubagentTool(allowed_agent_names=allowed)]


def make_subagent_tool_from_context(ctx: object) -> BaseTool:
    """Return the caller-scoped ``run_subagent`` tool for a factory context."""
    metadata = getattr(ctx, "metadata", {}) or {}
    caller_agent = str(metadata.get("agent_name") or "").strip()
    return make_subagent_tools(caller_agent=caller_agent)[0]


__all__ = [
    "RestrictedRunSubagentTool",
    "make_subagent_tool_from_context",
    "make_subagent_tools",
]
