"""Subagent tools — spawn focused worker subagents."""

from __future__ import annotations

from typing import ClassVar

from pydantic import Field, field_validator

from agents.registry import list_dispatchable_subagent_names
from tools.core.base import BaseTool, BaseToolkit, ToolExecutionContext, ToolResult
from tools.subagent.run_subagent_tool import run_subagent


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

    async def execute(self, arguments, context: ToolExecutionContext) -> ToolResult:  # type: ignore[override]
        return await self._delegate.execute(arguments, context)

    def background_preflight(self, arguments, context: ToolExecutionContext) -> ToolResult | None:  # type: ignore[override]
        return self._delegate.background_preflight(arguments, context)


class SubagentToolkit(BaseToolkit):
    """Spawn focused worker subagents that run as background tasks."""

    def __init__(
        self,
        *,
        caller_agent: str = "",
        allowed_agent_names: tuple[str, ...] | None = None,
    ) -> None:
        allowed = (
            allowed_agent_names
            if allowed_agent_names is not None
            else _allowed_subagent_names_for_caller(caller_agent)
        )
        allowed_text = ", ".join(allowed) if allowed else "(none)"
        super().__init__(
            name="subagent",
            description="Spawn focused worker subagents.",
            tools=[RestrictedRunSubagentTool(allowed_agent_names=allowed)],
            instructions=(
                "Use `run_subagent` to delegate bounded work to a subagent.\n"
                "- Each call returns a `task_id` immediately; workers always run in the background.\n"
                "- Emit multiple `run_subagent` calls in one turn only for disjoint work and only when live scope status still admits parallel fan-out.\n"
                "- After spawning a worker, keep doing disjoint foreground work or launch other independent workers. Do not immediately block on the new task unless its result is the only remaining blocker.\n"
                f"- Valid `agent_name` values for this caller: {allowed_text}.\n"
                "- Only dispatchable subagent targets are valid.\n"
                "- Prefer foreground work or `wait_for_background_task(task_id=...)` when blocked; call `check_background_progress(task_id=...)` only when live status will change your next action. Do not poll for reassurance or to satisfy an ordering ritual.\n"
                "- Use `wait_for_background_task(task_id=...)` to join a worker when you are ready for its final answer and it has not already reached a terminal status.\n"
                "- When a subagent result is `delivered`, `[COMPLETED]`, or reports `Posted.`, stop polling that task id; consume the posted note or artifact instead. Background status tools will only repeat the delivery envelope.\n"
                "- Cancel stale or low-value workers with `cancel_background_task(task_id=...).`\n"
                "- Workers cannot spawn subagents or launch their own background tasks."
            ),
        )

    @classmethod
    def from_context(cls, ctx):  # type: ignore[override]
        caller_agent = str((ctx.metadata or {}).get("agent_name") or "").strip()
        return cls(caller_agent=caller_agent)


__all__ = ["SubagentToolkit"]
