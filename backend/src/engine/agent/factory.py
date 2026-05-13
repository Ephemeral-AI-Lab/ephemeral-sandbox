"""Ephemeral agent — short-lived runtime for one user request.

Each agent has an identity, its own API client, tool registry, and query engine.
Every run is one provider request shaped as system prompt, user prompt, and
assistant response.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any
from collections.abc import AsyncIterator

if TYPE_CHECKING:
    from runtime.app_factory import RuntimeConfig
    from engine.query.context import QueryContext
    from tools import ToolRegistry

from agents import AgentDefinition
from config import Settings
from message.messages import ConversationMessage
from message.stream_events import StreamEvent
from providers.provider import make_api_client
from providers.types import UsageSnapshot
from prompt import build_runtime_context_message, build_runtime_system_prompt
from tools import (
    ExecutionMetadata,
    SANDBOX_CONTEXT,
    ToolFactoryContext,
    create_tool,
    create_default_tool_registry,
    has_tool,
    make_background_tools,
    make_sandbox_tools,
    resolve_harness_notification_triggers,
)

logger = logging.getLogger(__name__)

_BACKGROUND_CONTROL_TOOL_NAMES = frozenset(
    {
        "cancel_background_task",
        "check_background_task_result",
        "wait_background_tasks",
    }
)


@dataclass
class EphemeralAgent:
    """A short-lived agent that handles one user request then dies."""

    agent_name: str
    query_context: QueryContext
    settings: Settings
    model: str
    _messages: list[ConversationMessage]
    total_usage: UsageSnapshot | None = None

    @property
    def messages(self) -> list[ConversationMessage]:
        """Live view of the agent's run transcript.

        The list is owned by the agent. A run appends the current user prompt
        to any initial history, then appends provider and tool responses.
        """
        return self._messages

    async def run(self, prompt: str) -> AsyncIterator[StreamEvent]:
        """Execute one provider request for the given prompt."""
        from engine.query.loop import run_query

        self.total_usage = UsageSnapshot()
        try:
            self._messages = [*self._messages, ConversationMessage.from_user_text(prompt)]
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
            await self.close()

    async def close(self) -> None:
        """Release resources held by the agent's API client."""
        client = self.query_context.api_client
        if hasattr(client, "aclose"):
            await client.aclose()


def finalize_tool_registry_and_prompt(
    tool_registry: ToolRegistry,
    system_prompt: str,
    *,
    agent_type: str = "agent",
) -> tuple[str, bool]:
    """Finalize runtime tool registry and append terminal-tool guidance.

    This is the shared setup logic used by both spawn_agent() and EvalAgent.
    Terminal tool names are derived from the registry — any tool whose class
    sets ``is_terminal_tool=True`` ends the query loop on success.

    Args:
        tool_registry: The tool registry (mutated in-place to add background tools).
        system_prompt: The base system prompt.
        agent_type: Type label for the agent. Subagents cannot launch
            background tasks or spawn further subagents, so background
            management tools are withheld for ``agent_type="subagent"``.

    Returns:
        Tuple of (updated_system_prompt, has_background_tools).
    """
    from prompt.runtime_prompt import build_termination_condition_prompt
    bg_tool_names = [
        t.name
        for t in tool_registry.list_tools()
        if getattr(t, "background", "forbidden") != "forbidden"
    ]
    has_background_tools = bool(bg_tool_names) and agent_type != "subagent"
    if has_background_tools:
        tool_registry.register_many(make_background_tools())

    terminal_tool_names = [
        t.name
        for t in tool_registry.list_tools()
        if getattr(t, "is_terminal_tool", False)
    ]
    termination_prompt = build_termination_condition_prompt(
        terminal_tools=terminal_tool_names,
    )
    if termination_prompt:
        system_prompt = system_prompt + "\n\n" + termination_prompt

    return system_prompt, has_background_tools


def _resolve_agent_identity(
    config: RuntimeConfig,
    agent_def: AgentDefinition | None,
) -> tuple[str, str, Any, dict | None]:
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

    # ``model`` on the agent_def can be an explicit id, an ``"inherit"``
    # sentinel meaning "use the active runtime model", or absent.
    agent_model = agent_def.model if agent_def else None
    if agent_model and agent_model.strip().lower() == "inherit":
        agent_model = None
    resolved_model = agent_model or db_kwargs.get("model")
    if not resolved_model:
        raise RuntimeError("Active model registration has no 'model' id")
    agent_name = agent_def.name if agent_def else resolved_model

    # Subagents get their own httpx pool so concurrent workers do not
    # contend over a shared connection pool.
    needs_fresh_client = bool(agent_def and agent_def.agent_type == "subagent")
    api_client = make_api_client(
        None if needs_fresh_client else config.external_api_client,
        db_kwargs=db_kwargs,
    )
    return agent_name, resolved_model, api_client, db_kwargs


def _build_agent_tool_registry(
    config: RuntimeConfig,
    agent_def: AgentDefinition | None,
    sandbox_id: str | None,
    agent_name: str,
) -> ToolRegistry:
    """Build the tool registry for a spawning agent.

    Registers tools requested by *agent_def* and sandbox tools when
    a sandbox is selected for a default agent.
    """
    tool_registry = create_default_tool_registry()

    tool_ctx = ToolFactoryContext(
        metadata={
            "agent_name": agent_name,
            "role": agent_def.role if agent_def else "",
            "cwd": config.cwd,
            "sandbox_id": sandbox_id or "",
        },
    )
    if agent_def:
        _register_requested_tools(
            tool_registry,
            _collect_agent_tool_surface(agent_def),
            tool_ctx,
            agent_name,
        )
    elif sandbox_id:
        tool_registry.register_many(make_sandbox_tools())
        logger.info("Registered sandbox tools for sandbox %s", sandbox_id)

    return tool_registry


def _collect_agent_tool_surface(agent_def: AgentDefinition) -> list[str]:
    """Return the agent's declared tool surface (allowed_tools ∪ terminals)."""
    return sorted(set(agent_def.allowed_tools) | set(agent_def.terminals))


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
            # These are synthesized by finalize_tool_registry_and_prompt when
            # the registered tools include at least one background-capable
            # tool. They are not ordinary tool factories.
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


def _tool_registry_context_requirements(tool_registry: ToolRegistry) -> set[str]:
    """Return runtime context requirements declared by registered tools."""
    requirements: set[str] = set()
    for tool in tool_registry.list_tools():
        requirements.update(getattr(tool, "context_requirements", ()))
    return requirements


def _build_context_preparers(
    tool_registry: ToolRegistry,
    sandbox_id: str | None,
) -> list[Any]:
    """Build provider/toolkit-specific context hooks for registered tools."""
    if not sandbox_id:
        return []
    requirements = _tool_registry_context_requirements(tool_registry)
    if SANDBOX_CONTEXT not in requirements:
        return []

    import sandbox.api as sandbox_api

    return [sandbox_api.context_preparer_for(sandbox_id)]


def _build_agent_system_prompt(
    config: RuntimeConfig,
    agent_def: AgentDefinition | None,
    settings: Settings,
) -> str:
    """Return the instruction-only system prompt for *agent_def*."""
    parts: list[str] = []
    base = build_runtime_system_prompt(
        settings,
        cwd=config.cwd,
    )
    if base:
        parts.append(base)
    if agent_def is not None and agent_def.system_prompt:
        parts.append(agent_def.system_prompt)
    return "\n\n".join(part for part in parts if part.strip())


def spawn_agent(
    config: RuntimeConfig,
    messages: list[ConversationMessage],
    *,
    agent_def: AgentDefinition | None = None,
    sandbox_id: str | None = None,
) -> EphemeralAgent:
    """Spawn a fresh ephemeral agent with the given message history.

    If *agent_def* is provided, its fields customize the runtime defaults:
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
    max_tokens = int((db_kwargs or {}).get("max_tokens") or 16384)

    tool_registry = _build_agent_tool_registry(
        config, agent_def, sandbox_id, agent_name
    )

    base_system_prompt = _build_agent_system_prompt(config, agent_def, settings)
    runtime_context = build_runtime_context_message(cwd=config.cwd)
    if runtime_context:
        base_system_prompt = "\n\n".join(
            part for part in (base_system_prompt, runtime_context) if part.strip()
        )

    system_prompt, has_background_tools = finalize_tool_registry_and_prompt(
        tool_registry,
        base_system_prompt,
        agent_type=agent_def.agent_type if agent_def else "agent",
    )

    tool_call_limit = agent_def.tool_call_limit if agent_def else None

    # Plumb runtime_config through tool_metadata so tools (e.g. run_subagent)
    # that need to spawn nested agents can reach it without a Protocol layer.
    initial_tool_metadata = ExecutionMetadata(
        runtime_config=config,
        sandbox_id=sandbox_id or "",
        agent_name=agent_name,
        context_preparers=_build_context_preparers(tool_registry, sandbox_id),
    )
    if agent_def is not None:
        initial_tool_metadata["agent_type"] = agent_def.agent_type
        if agent_def.role:
            initial_tool_metadata["role"] = agent_def.role

    notification_rules = list(agent_def.notification_rules) if agent_def else []
    if agent_def and agent_def.notification_triggers:
        notification_rules.extend(
            resolve_harness_notification_triggers(agent_def.notification_triggers)
        )

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
        notification_rules=notification_rules,
    )

    return EphemeralAgent(
        agent_name=agent_name,
        query_context=query_context,
        settings=settings,
        model=resolved_model,
        _messages=messages if messages else [],
    )
