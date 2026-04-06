"""Ephemeral agent — short-lived runtime for one user request.

Each agent has an identity, its own API client, tool registry, hook
executor, and query engine.  In a relay model, different agents can
serve successive turns within the same session.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, AsyncIterator

if TYPE_CHECKING:
    from server.app_factory import SessionConfig
    from utils.compact import SessionState

from agents.types import AgentDefinition
from config import Settings
from engine.core.query import QueryContext, run_query
from message.messages import ConversationMessage
from message.stream_events import StreamEvent
from hooks import make_hook_executor
from models.core.provider import make_api_client
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
    _messages: list[ConversationMessage]

    async def run(self, prompt: str) -> AsyncIterator[StreamEvent]:
        """Execute one complete tool-call loop for the given prompt."""
        try:
            self._messages.append(ConversationMessage.from_user_text(prompt))
            messages, event_iter = run_query(self.query_context, self._messages)
            self._messages = messages
            async for event, _usage in event_iter:
                yield event
        finally:
            await self.close()

    async def close(self) -> None:
        """Release resources held by the agent's API client."""
        client = self.query_context.api_client
        if hasattr(client, "aclose"):
            await client.aclose()


def spawn_agent(
    config: "SessionConfig",
    messages: list[ConversationMessage],
    *,
    agent_def: AgentDefinition | None = None,
    latest_user_prompt: str | None = None,
    session_state: "SessionState | None" = None,
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
    settings = config.resolve_settings()

    # --- Active model from DB (carries api_key, base_url, class_path) ------
    db_kwargs: dict | None = None
    db_class_path: str | None = None
    _model_store = model_store
    if _model_store is None:
        try:
            from server.app_factory import model_store

            _model_store = model_store
        except Exception as exc:
            logger.debug("DB model registry unavailable: %s", exc)
            _model_store = None

    if _model_store is not None and _model_store.is_available:
        active = _model_store.get_active_resolved()
        if active:
            db_kwargs = active.get("kwargs")
            db_class_path = active.get("class_path")

    # --- Per-agent overrides ------------------------------------------------
    resolved_model = (
        agent_def.model
        if agent_def and agent_def.model
        else (db_kwargs or {}).get("model") or settings.model
    )
    agent_name = agent_def.name if agent_def else resolved_model

    # --- API client
    api_client = make_api_client(
        settings,
        config.external_api_client,
        db_kwargs=db_kwargs,
        db_class_path=db_class_path,
    )

    # --- Tool registry
    tool_registry = create_default_tool_registry()

    # --- Instantiate toolkits requested by the agent definition via factory ---
    toolkit_ctx = ToolkitContext(
        agent_name=agent_name,
        cwd=config.cwd,
        metadata={"sandbox_id": sandbox_id or ""},
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

    # Register Daytona sandbox tools when a sandbox is selected (if not already registered above)
    if sandbox_id and tool_registry.get_toolkit("sandbox_operations") is None:
        try:
            from tools.daytona_toolkit import DaytonaToolkit

            daytona_toolkit = DaytonaToolkit(sandbox_id=sandbox_id)
            tool_registry.register_toolkit(daytona_toolkit)
            logger.info("Registered DaytonaToolkit for sandbox %s", sandbox_id)
        except Exception:
            logger.warning(
                "Failed to register DaytonaToolkit for sandbox %s", sandbox_id, exc_info=True
            )

    if agent_def and agent_def.toolkits:
        # restrict_to_toolkits([]) would clear ALL tools, so we only call
        # it when agent_def.toolkits is non-empty (truthy check above)
        tool_registry.restrict_to_toolkits(agent_def.toolkits)

    # --- Hook executor
    hook_executor = make_hook_executor(settings, config.cwd, api_client)

    # --- System prompt
    if agent_def and agent_def.system_prompt:
        system_prompt = agent_def.system_prompt
    else:
        system_prompt = build_runtime_system_prompt(
            settings,
            cwd=config.cwd,
            latest_user_prompt=latest_user_prompt,
        )

    # --- Skills toolkit — always registered so agents can discover and load skills
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

    # --- Background toolkit — register when background-capable tools exist
    bg_tool_names = [t.name for t in tool_registry.list_tools() if t.supports_background]
    has_background_tools = bool(bg_tool_names)
    if has_background_tools:
        from tools.builtins.background import make_background_toolkit

        tool_registry.register_toolkit(make_background_toolkit(bg_tool_names))

    # --- Inject toolkit and capability awareness into system prompt ---------
    from prompts.context import build_agent_capabilities_prompt

    awareness = build_agent_capabilities_prompt(
        toolkits=tool_registry.list_toolkits(),
        has_background_tools=has_background_tools,
        bg_tool_names=bg_tool_names,
    )
    if awareness:
        system_prompt = system_prompt + "\n\n" + awareness

    # --- Max turns
    max_turns = agent_def.max_turns if agent_def and agent_def.max_turns else 200

    query_context = QueryContext(
        api_client=api_client,
        tool_registry=tool_registry,
        cwd=config.cwd,
        model=resolved_model,
        system_prompt=system_prompt,
        max_tokens=settings.max_tokens,
        max_turns=max_turns,
        hook_executor=hook_executor,
        session_state=session_state,
        enable_background_tasks=has_background_tools,
    )

    return EphemeralAgent(
        agent_name=agent_name,
        query_context=query_context,
        settings=settings,
        model=resolved_model,
        _messages=messages if messages else [],
    )
