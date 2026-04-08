"""Ephemeral agent — short-lived runtime for one user request.

Each agent has an identity, its own API client, tool registry, hook
executor, and query engine.  In a relay model, different agents can
serve successive turns within the same session.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any
from collections.abc import AsyncIterator

if TYPE_CHECKING:
    from server.app_factory import SessionConfig
    from compaction import SessionState
    from engine.core.query import QueryContext
    from tools.core.base import ToolRegistry

from agents.types import AgentDefinition
from config import Settings
from message.messages import ConversationMessage
from message.stream_events import StreamEvent
from hooks import make_hook_executor
from providers.provider import make_api_client
from providers.types import UsageSnapshot
from prompts import build_runtime_system_prompt
from tools import create_default_tool_registry
from tools.core.factory import create_toolkit, has_factory, ToolkitContext

logger = logging.getLogger(__name__)


@dataclass
class EphemeralAgent:
    """A short-lived agent that handles one user request then dies."""

    agent_name: str
    query_context: QueryContext
    settings: Settings
    model: str
    _display_messages: list[ConversationMessage]
    total_usage: UsageSnapshot | None = None

    @property
    def display_messages(self) -> list[ConversationMessage]:
        """Live view of the agent's append-only display history.

        The list is owned by the agent and grows as ``run`` drives turns —
        callers may read it (e.g. for live progress peeks) but must treat it
        as read-only.
        """
        return self._display_messages

    async def run(self, prompt: str) -> AsyncIterator[StreamEvent]:
        """Execute one complete tool-call loop for the given prompt."""
        from engine.core.query import run_query

        self.total_usage = UsageSnapshot()
        try:
            self._display_messages.append(ConversationMessage.from_user_text(prompt))
            display_messages, event_iter = await run_query(
                self.query_context, self._display_messages
            )
            self._display_messages = display_messages
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


def resolve_active_model(
    settings: Settings,
    model_store: Any = None,
) -> dict[str, Any] | None:
    """Return the active model's DB kwargs, or ``None`` if unavailable.

    Shared by :func:`spawn_agent` and the eval harness so both paths
    pick up the same active model (with its api_key, base_url, and
    client class) instead of duplicating the lookup.
    """
    _store = model_store
    if _store is None:
        try:
            from server.app_factory import model_store as default_store

            _store = default_store
        except Exception as exc:
            logger.debug("DB model registry unavailable: %s", exc)
            return None

    if _store is None or not getattr(_store, "is_available", False):
        return None
    active = _store.get_active_resolved()
    if not active:
        return None
    return active.get("kwargs")


def finalize_tool_registry_and_prompt(
    tool_registry: ToolRegistry,
    system_prompt: str,
    *,
    can_spawn_subagents: bool = True,
) -> tuple[str, bool]:
    """Register background toolkit and inject capability awareness into the system prompt.

    This is the shared setup logic used by both spawn_agent() and EvalAgent.

    Args:
        tool_registry: The tool registry (mutated in-place to add background toolkit).
        system_prompt: The base system prompt.
        can_spawn_subagents: Whether this agent is allowed to launch background
            tasks and spawn subagents. Agents that cannot (e.g. subagents
            themselves) have the background management toolkit
            (check_background_progress / wait_for_background_task / cancel)
            withheld regardless of registry contents.

    Returns:
        Tuple of (updated_system_prompt, has_background_tools).
    """
    from prompts.runtime_prompt import build_agent_capabilities_prompt
    from tools.builtins.background import make_background_toolkit

    bg_tool_names = [
        t.name
        for t in tool_registry.list_tools()
        if getattr(t, "background", "forbidden") != "forbidden"
    ]
    has_background_tools = bool(bg_tool_names) and can_spawn_subagents
    if has_background_tools:
        tool_registry.register_toolkit(make_background_toolkit(bg_tool_names))

    awareness = build_agent_capabilities_prompt(
        toolkits=tool_registry.list_toolkits(),
        has_background_tools=has_background_tools,
        bg_tool_names=bg_tool_names,
    )
    if awareness:
        system_prompt = system_prompt + "\n\n" + awareness

    return system_prompt, has_background_tools


def _resolve_agent_identity(
    config: SessionConfig,
    agent_def: AgentDefinition | None,
    settings: Settings,
    *,
    model_store: Any = None,
) -> tuple[str, str, Any, dict | None]:
    """Resolve the agent's name, model id, API client, and DB model kwargs.

    Returns ``(agent_name, resolved_model, api_client, db_kwargs)``.
    """
    db_kwargs = resolve_active_model(settings, model_store=model_store)

    # ``model`` on the agent_def can be an explicit id, an ``"inherit"``
    # sentinel meaning "use the session's active model", or absent.
    agent_model = agent_def.model if agent_def else None
    if agent_model and agent_model.strip().lower() == "inherit":
        agent_model = None
    resolved_model = (
        agent_model
        if agent_model
        else (db_kwargs or {}).get("model") or settings.model
    )
    agent_name = agent_def.name if agent_def else resolved_model

    # Agents flagged ``require_fresh_client`` (currently: subagents) get
    # their own httpx pool so concurrent workers don't contend over a
    # shared connection pool.
    needs_fresh_client = bool(agent_def and agent_def.require_fresh_client)
    api_client = make_api_client(
        settings,
        None if needs_fresh_client else config.external_api_client,
        db_kwargs=db_kwargs,
    )
    return agent_name, resolved_model, api_client, db_kwargs


def _build_agent_tool_registry(
    config: SessionConfig,
    agent_def: AgentDefinition | None,
    sandbox_id: str | None,
    agent_name: str,
) -> ToolRegistry:
    """Build the tool registry for a spawning agent.

    Registers toolkits requested by *agent_def*, the Daytona toolkit when
    a sandbox is selected, restricts to the requested set, and finally
    registers the skills toolkit unless the agent opts out.
    """
    tool_registry = create_default_tool_registry()

    toolkit_ctx = ToolkitContext(
        metadata={
            "agent_name": agent_name,
            "cwd": config.cwd,
            "sandbox_id": sandbox_id or "",
        },
    )
    if agent_def and agent_def.toolkits:
        for tk_name in agent_def.toolkits:
            if tool_registry.get_toolkit(tk_name) is not None:
                continue  # already registered
            if has_factory(tk_name):
                try:
                    tk = create_toolkit(tk_name, toolkit_ctx)
                    tool_registry.register_toolkit(tk)
                    logger.info("Registered toolkit %r for agent %r", tk_name, agent_name)
                except Exception:
                    logger.warning(
                        "Failed to create toolkit %r for agent %r",
                        tk_name,
                        agent_name,
                        exc_info=True,
                    )
            else:
                logger.warning(
                    "No factory for toolkit %r requested by agent %r", tk_name, agent_name
                )

    # Register Daytona sandbox tools when a sandbox is selected (if not
    # already registered above).
    if sandbox_id and tool_registry.get_toolkit("sandbox_operations") is None:
        try:
            from tools.daytona_toolkit import DaytonaToolkit

            daytona_toolkit = DaytonaToolkit(sandbox_id=sandbox_id)
            tool_registry.register_toolkit(daytona_toolkit)
            logger.info("Registered DaytonaToolkit for sandbox %s", sandbox_id)
        except Exception:
            logger.warning(
                "Failed to register DaytonaToolkit for sandbox %s",
                sandbox_id,
                exc_info=True,
            )

    if agent_def and agent_def.toolkits:
        # restrict_to_toolkits([]) would clear ALL tools, so we only call
        # it when agent_def.toolkits is non-empty.
        tool_registry.restrict_to_toolkits(agent_def.toolkits)

    # Standalone tools — registered after restrict_to_toolkits so they
    # survive the toolkit filter. Used by agents that need individual
    # tools outside any toolkit (e.g. submit_plan_agent → submit_plan).
    if agent_def and agent_def.extra_tools:
        from tools.core.factory import create_standalone_tool

        for tool_name in agent_def.extra_tools:
            tool = create_standalone_tool(tool_name)
            if tool is not None:
                tool_registry.register(tool)
            else:
                logger.warning(
                    "extra_tools: no standalone tool registered for %r", tool_name
                )

    # Skills toolkit — opt-out via ``include_skills=False``.
    include_skills = agent_def.include_skills if agent_def else True
    if include_skills:
        from skills.core.loader import load_skill_registry
        from tools.builtins.skills import make_skills_toolkit

        skill_filter = agent_def.skills if agent_def and agent_def.skills else None
        skill_registry = load_skill_registry(config.cwd)
        skills_toolkit = make_skills_toolkit(skill_registry, skill_filter)
        if skills_toolkit.list_tools():
            tool_registry.register_toolkit(skills_toolkit)
            logger.info(
                "Registered SkillsToolkit (%d tools) for agent %r",
                len(skills_toolkit.list_tools()),
                agent_name,
            )

    return tool_registry


def _build_agent_system_prompt(
    config: SessionConfig,
    agent_def: AgentDefinition | None,
    settings: Settings,
    latest_user_prompt: str | None,
) -> str:
    """Return the base system prompt for *agent_def*.

    If the definition provides its own prompt, use it verbatim; otherwise
    build the runtime prompt (memory, issue context, PR comments, etc.)
    that the server-side path normally produces.
    """
    if agent_def and agent_def.system_prompt:
        return agent_def.system_prompt
    return build_runtime_system_prompt(
        settings,
        cwd=config.cwd,
        latest_user_prompt=latest_user_prompt,
    )


def spawn_agent(
    config: SessionConfig,
    messages: list[ConversationMessage],
    *,
    agent_def: AgentDefinition | None = None,
    latest_user_prompt: str | None = None,
    session_state: SessionState | None = None,
    sandbox_id: str | None = None,
    model_store: Any = None,
) -> EphemeralAgent:
    """Spawn a fresh ephemeral agent with the given session history.

    If *agent_def* is provided, its fields override the session defaults:
    - ``model`` overrides the session model
    - ``system_prompt`` replaces the default system prompt
    - ``toolkits`` restricts available toolkits
    - ``max_turns`` caps the tool-call loop iterations
    """
    from pathlib import Path

    from engine.core.query import QueryContext
    from tools.core.base import ExecutionMetadata

    settings = config.resolve_settings()

    agent_name, resolved_model, api_client, _db_kwargs = _resolve_agent_identity(
        config, agent_def, settings, model_store=model_store
    )

    tool_registry = _build_agent_tool_registry(
        config, agent_def, sandbox_id, agent_name
    )

    hook_executor = make_hook_executor(settings, config.cwd, api_client)

    base_system_prompt = _build_agent_system_prompt(
        config, agent_def, settings, latest_user_prompt
    )

    can_spawn = agent_def.can_spawn_subagents if agent_def else True
    system_prompt, has_background_tools = finalize_tool_registry_and_prompt(
        tool_registry, base_system_prompt, can_spawn_subagents=can_spawn
    )

    max_turns = agent_def.max_turns if agent_def and agent_def.max_turns else 200

    # Plumb session_config through tool_metadata so tools (e.g. run_subagent)
    # that need to spawn nested agents can reach it without a Protocol layer.
    initial_tool_metadata = ExecutionMetadata(
        session_config=config,
        sandbox_id=sandbox_id or "",
    )

    query_context = QueryContext(
        api_client=api_client,
        tool_registry=tool_registry,
        cwd=Path(config.cwd),
        model=resolved_model,
        system_prompt=system_prompt,
        max_tokens=settings.max_tokens,
        max_turns=max_turns,
        hook_executor=hook_executor,
        tool_metadata=initial_tool_metadata,
        session_state=session_state,
        enable_background_tasks=has_background_tools,
    )

    return EphemeralAgent(
        agent_name=agent_name,
        query_context=query_context,
        settings=settings,
        model=resolved_model,
        _display_messages=messages if messages else [],
    )
