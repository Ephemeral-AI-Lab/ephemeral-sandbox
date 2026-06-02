"""Ephemeral agent — short-lived runtime for one user request.

Each agent has an identity, its own API client, tool registry, and query engine.
Every run is one provider request shaped as system prompt, user prompt, and
assistant response.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, cast
from collections.abc import AsyncIterator

if TYPE_CHECKING:
    from runtime.app_factory import RuntimeConfig
    from engine.query.context import QueryContext
    from notification import NotificationRule
    from tools import ToolRegistry

from agents import AgentDefinition, AgentType
from config import Settings
from message.message import Message
from message.events import StreamEvent
from engine.background.policy import (
    is_engine_background_tool,
    needs_background_manager,
    supports_explicit_generic_background,
)
from providers.provider import make_api_client
from providers.types import UsageSnapshot
from prompt import build_runtime_system_prompt
from tools import (
    ExecutionMetadata,
    SANDBOX_CONTEXT,
    ToolFactoryContext,
    create_tool,
    create_default_tool_registry,
    has_tool,
    resolve_harness_notification_triggers,
)

logger = logging.getLogger(__name__)

_BACKGROUND_CONTROL_TOOL_NAMES = frozenset(
    {
        "cancel_background_task",
        "cancel_subagent",
        "check_background_task_result",
        "check_subagent_progress",
        "wait_background_tasks",
    }
)


@dataclass
class EphemeralAgent:
    """A short-lived agent that handles one user request then dies."""

    agent_name: str
    query_context: QueryContext
    model: str
    _messages: list[Message]
    total_usage: UsageSnapshot | None = None

    @property
    def messages(self) -> list[Message]:
        """Live view of the agent's run transcript.

        The list is owned by the agent. A run appends the current user prompt
        to any initial history, then appends provider and tool responses.
        """
        return self._messages

    async def run(
        self,
        prompt: str | None,
        *,
        auto_close: bool = True,
    ) -> AsyncIterator[StreamEvent]:
        """Execute one provider request and stream its events.

        Args:
            prompt: User prompt to append before invoking the query loop. Pass
                ``None`` to resume from the current transcript (used by the
                retry path in :func:`run_ephemeral_agent`, which injects its
                own nudge message directly into ``self._messages`` to keep
                role alternation idiomatic).
            auto_close: When ``True`` (default) the API client is released in
                ``finally``. Retry callers pass ``False`` and call
                :meth:`close` once after the final attempt.
        """
        from engine.query.loop import run_query

        if self.total_usage is None:
            self.total_usage = UsageSnapshot()
        try:
            if prompt is not None:
                self._messages = [*self._messages, Message.from_user_text(prompt)]
            messages, event_iter = await run_query(
                self.query_context, self._messages
            )
            self._messages = messages
            async for event, usage in event_iter:
                if usage:
                    self.total_usage.input_tokens += usage.input_tokens
                    self.total_usage.output_tokens += usage.output_tokens
                yield event
        finally:
            if auto_close:
                await self.close()

    async def close(self) -> None:
        """Release resources held by the agent's API client."""
        client = self.query_context.api_client
        if hasattr(client, "aclose"):
            await client.aclose()


def _finalize_tool_registry_and_prompt(
    tool_registry: ToolRegistry,
    system_prompt: str,
    *,
    agent_type: AgentType,
) -> tuple[str, bool]:
    """Finalize runtime tool registry and append terminal-tool guidance.

    This is the shared setup logic used by spawn_agent().
    Terminal tool names are derived from the registry — any tool whose class
    sets ``is_terminal_tool=True`` ends the query loop on success.

    Args:
        tool_registry: The tool registry, mutated in-place for typed subagent controls.
        system_prompt: The base system prompt.
        agent_type: Type label for the agent. Subagents cannot launch
            background tasks or spawn further subagents, so background
            management tools are withheld for ``agent_type="subagent"``.

    Returns:
        Tuple of (updated_system_prompt, has_background_tools).
    """
    from prompt.runtime_prompt import build_termination_condition_prompt
    from tools.subagent import make_subagent_control_tools

    listed_tools = tool_registry.list_tools()
    has_background_tools = (
        any(needs_background_manager(tool) for tool in listed_tools)
        and agent_type != AgentType.SUBAGENT
    )
    if has_background_tools:
        if any(is_engine_background_tool(tool) for tool in listed_tools):
            tool_registry.register_many(make_subagent_control_tools())
        if any(supports_explicit_generic_background(tool) for tool in listed_tools):
            from tools.background import (
                CancelBackgroundTaskTool,
                CheckBackgroundTaskResultTool,
            )

            tool_registry.register_many(
                [CheckBackgroundTaskResultTool(), CancelBackgroundTaskTool()]
            )

    terminal_tool_names = [
        t.name
        for t in tool_registry.list_tools()
        if getattr(t, "is_terminal_tool", False)
    ]
    assert terminal_tool_names, (
        "Agent has no terminal-capable tool registered. Every agent must "
        "declare at least one tool with is_terminal_tool=True."
    )
    termination_prompt = build_termination_condition_prompt(
        terminal_tools=terminal_tool_names,
    )
    if termination_prompt:
        system_prompt = system_prompt + "\n\n" + termination_prompt

    return system_prompt, has_background_tools


def _resolve_agent_identity(
    config: RuntimeConfig,
    agent_def: AgentDefinition,
) -> tuple[str, str, Any, dict[str, Any] | None]:
    """Resolve the agent's name, model id, API client, and DB model kwargs.

    Returns ``(agent_name, resolved_model, api_client, db_kwargs)``.
    """
    from config.model_config import NoActiveModelError, get_active_model_kwargs

    try:
        db_kwargs = get_active_model_kwargs()
    except NoActiveModelError as exc:
        raise RuntimeError(
            "No active model registration found — configure a model in the "
            "model_registrations DB table before spawning agents."
        ) from exc

    # ``model`` on the agent_def can be an explicit id or the ``"inherit"``
    # sentinel meaning "use the active runtime model".
    agent_model = agent_def.model
    if agent_model and agent_model.strip().lower() == "inherit":
        agent_model = None
    resolved_model = agent_model or db_kwargs.get("model")
    if not resolved_model:
        raise RuntimeError("Active model registration has no 'model' id")

    # Subagents get their own httpx pool so concurrent workers do not
    # contend over a shared connection pool.
    needs_fresh_client = agent_def.agent_type == AgentType.SUBAGENT
    api_client = make_api_client(
        None if needs_fresh_client else config.external_api_client,
        db_kwargs=db_kwargs,
    )
    return agent_def.name, resolved_model, api_client, db_kwargs


def _build_agent_tool_registry(
    config: RuntimeConfig,
    agent_def: AgentDefinition,
    sandbox_id: str | None,
    agent_name: str,
) -> ToolRegistry:
    """Build the tool registry for a spawning agent.

    Registers the tools named in ``agent_def.allowed_tools ∪
    agent_def.terminals`` and skips unknown names with a warning.
    """
    tool_registry = create_default_tool_registry()

    tool_ctx = ToolFactoryContext(
        metadata={
            "agent_name": agent_name,
            "role": agent_def.role.value,
            "cwd": config.cwd,
            "sandbox_id": sandbox_id or "",
        },
    )
    _register_requested_tools(
        tool_registry,
        sorted(set(agent_def.allowed_tools) | set(agent_def.terminals)),
        tool_ctx,
        agent_name,
    )

    return tool_registry


def _register_requested_tools(
    tool_registry: ToolRegistry,
    tool_names: list[str],
    tool_ctx: ToolFactoryContext,
    agent_name: str,
) -> None:
    """Add explicit tools into the final tool surface."""
    for name in tool_names:
        clean_name = str(name).strip()
        if not clean_name or tool_registry.get(clean_name) is not None:
            continue
        if clean_name in _BACKGROUND_CONTROL_TOOL_NAMES:
            # Retired generic background controls and typed subagent controls
            # are not ordinary user-requested tool factories.
            continue
        if not has_tool(clean_name):
            logger.warning("No tool factory for %r requested by agent %r", clean_name, agent_name)
            continue
        try:
            tool_registry.register(create_tool(clean_name, tool_ctx))
            logger.info("Registered tool %r for agent %r", clean_name, agent_name)
        except Exception:
            logger.warning(
                "Failed to create tool %r for agent %r",
                clean_name,
                agent_name,
                exc_info=True,
            )


def _attach_default_notification_rules(
    notification_rules: list[Any],
) -> None:
    """Append default notification rules if not already present.

    Every agent has terminals and a ``tool_call_limit`` by invariant, so
    these rules apply unconditionally. Dedupes by ``rule.name`` so a rule
    already resolved from the profile's ``notification_triggers`` (which run
    earlier) wins over the same-named default.
    """
    from notification import (
        make_terminal_call_reminder,
        make_terminal_tool_call_count_reminders,
    )

    existing_names = {getattr(rule, "name", "") for rule in notification_rules}
    for rule in make_terminal_tool_call_count_reminders():
        if rule.name not in existing_names:
            notification_rules.append(rule)
            existing_names.add(rule.name)
    if "terminal_call_reminder" not in existing_names:
        notification_rules.append(make_terminal_call_reminder())


def _build_sandbox_context_preparers(
    tool_registry: ToolRegistry,
    sandbox_id: str | None,
) -> list[Any]:
    """Build provider/toolkit-specific context hooks for registered tools."""
    if not sandbox_id:
        return []
    if not any(
        SANDBOX_CONTEXT in getattr(tool, "context_requirements", ())
        for tool in tool_registry.list_tools()
    ):
        return []

    import sandbox.api as sandbox_api

    return [sandbox_api.context_preparer_for(sandbox_id)]


def _build_agent_system_prompt(
    config: RuntimeConfig,
    agent_def: AgentDefinition,
    settings: Settings,
) -> str:
    """Return the instruction-only system prompt for *agent_def*.

    The main-role operating contract is prepended at agent-definition load
    time for in-harness main profiles via
    ``agents/profile/main/_main_role_contract.md``. This builder therefore
    just concatenates the runtime base + the agent profile body verbatim.
    """
    parts: list[str] = []
    base = build_runtime_system_prompt(
        settings,
        cwd=config.cwd,
    )
    if base:
        parts.append(base)
    if agent_def.system_prompt:
        parts.append(agent_def.system_prompt)
    return "\n\n".join(part for part in parts if part.strip())


def spawn_agent(
    config: RuntimeConfig,
    messages: list[Message],
    *,
    agent_def: AgentDefinition,
    sandbox_id: str | None = None,
) -> EphemeralAgent:
    """Spawn a fresh ephemeral agent with the given message history.

    *agent_def* customizes the runtime:
    - ``model`` overrides the active model
    - ``system_prompt`` is appended after the runtime system prompt
    - ``allowed_tools`` + ``terminals`` declare the tool surface
    - ``tool_call_limit`` caps tool dispatches for the ephemeral run
    """
    from pathlib import Path

    from engine.query.context import QueryContext
    settings = config.resolve_settings()

    agent_name, resolved_model, api_client, db_kwargs = _resolve_agent_identity(
        config, agent_def
    )
    max_tokens = int((db_kwargs or {}).get("max_tokens") or 32768)

    tool_registry = _build_agent_tool_registry(
        config, agent_def, sandbox_id, agent_name
    )

    base_system_prompt = _build_agent_system_prompt(config, agent_def, settings)

    system_prompt, has_background_tools = _finalize_tool_registry_and_prompt(
        tool_registry,
        base_system_prompt,
        agent_type=agent_def.agent_type,
    )

    tool_call_limit = agent_def.tool_call_limit

    # Plumb runtime_config through tool_metadata so tools (e.g. run_subagent)
    # that need to spawn nested agents can reach it without a Protocol layer.
    initial_tool_metadata = ExecutionMetadata(
        runtime_config=config,
        sandbox_id=sandbox_id or "",
        agent_name=agent_name,
        context_preparers=_build_sandbox_context_preparers(tool_registry, sandbox_id),
    )
    initial_tool_metadata["agent_type"] = agent_def.agent_type.value
    initial_tool_metadata["role"] = agent_def.role.value

    notification_rules: list[Any] = []
    if agent_def.notification_triggers:
        notification_rules.extend(
            resolve_harness_notification_triggers(agent_def.notification_triggers)
        )
    _attach_default_notification_rules(notification_rules)

    query_context = QueryContext(
        api_client=api_client,
        tool_registry=tool_registry,
        cwd=Path(config.cwd),
        model=resolved_model,
        system_prompt=system_prompt,
        max_tokens=max_tokens,
        tool_call_limit=tool_call_limit,
        tool_metadata=initial_tool_metadata,
        enable_background_tasks=has_background_tools,
        agent_name=agent_name,
        notification_rules=cast("list[NotificationRule]", notification_rules),
    )

    # Default-off seam: production leaves ``event_source_factory`` unset, so
    # ``event_source`` stays ``None`` and the loop streams from ``api_client``.
    # The mock harness supplies a factory so this agent runs the real loop
    # against a scripted source.
    if config.event_source_factory is not None:
        query_context.event_source = config.event_source_factory(agent_def)

    return EphemeralAgent(
        agent_name=agent_name,
        query_context=query_context,
        model=resolved_model,
        _messages=messages if messages else [],
    )
