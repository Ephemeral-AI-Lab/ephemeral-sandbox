"""Ephemeral agent runtime for EphemeralOS.

Each user request spawns a fresh agent (engine + API client + tools) that
inherits session history, executes one complete tool-call loop, then dies.
No in-memory state persists between requests — only durable config and
the session ID survive.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, AsyncIterator, Awaitable, Callable

if TYPE_CHECKING:
    from ephemeralos.utils.compact import SessionContext

from ephemeralos.agents.types import AgentDefinition
from ephemeralos.config import Settings, load_settings
from ephemeralos.engine import QueryEngine
from ephemeralos.engine.messages import ConversationMessage
from ephemeralos.engine.stream_events import AssistantTurnComplete, StreamEvent
from ephemeralos.hooks import HookExecutionContext, HookExecutor, load_hook_registry
from ephemeralos.models.clients.anthropic import AnthropicApiClient
from ephemeralos.models.clients.openai_compat import OpenAICompatibleClient
from ephemeralos.models.types import SupportsStreamingMessages
from ephemeralos.prompts import build_runtime_system_prompt
from ephemeralos.tools import create_default_tool_registry

logger = logging.getLogger(__name__)

SystemPrinter = Callable[[str], Awaitable[None]]
StreamRenderer = Callable[[StreamEvent], Awaitable[None]]
ClearHandler = Callable[[], Awaitable[None]]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_api_client(
    settings: Settings,
    external: SupportsStreamingMessages | None = None,
    *,
    db_kwargs: dict | None = None,
    db_class_path: str | None = None,
) -> SupportsStreamingMessages:
    """Build an API client from settings, or return the external one.

    When *db_kwargs* / *db_class_path* are provided (from the active model
    registration in the DB) they supply ``api_key``, ``base_url``, and the
    provider type — falling back to ``settings`` only when a value is absent.
    """
    if external is not None:
        return external

    # Resolve from DB-registered model first, then settings
    api_key = (db_kwargs or {}).get("api_key") or settings.resolve_api_key()
    base_url = (db_kwargs or {}).get("base_url") or settings.base_url

    # Determine provider: DB class_path > settings.api_format
    is_openai = (
        (db_class_path or "").lower() in ("openai", "openai_compat")
        or settings.api_format == "openai"
    )

    if is_openai:
        return OpenAICompatibleClient(api_key=api_key, base_url=base_url)
    return AnthropicApiClient(api_key=api_key, base_url=base_url)


def _make_hook_executor(settings: Settings, cwd: str, api_client: SupportsStreamingMessages) -> HookExecutor:
    """Build a hook executor from settings."""
    return HookExecutor(
        load_hook_registry(settings, []),
        HookExecutionContext(
            cwd=Path(cwd).resolve(),
            api_client=api_client,
            default_model=settings.model,
        ),
    )


# ---------------------------------------------------------------------------
# SessionConfig — durable configuration that survives across requests
# ---------------------------------------------------------------------------


@dataclass
class SessionConfig:
    """Durable session configuration — persists across ephemeral agents."""

    cwd: str
    session_id: str
    # CLI overrides (take precedence over settings.json)
    model_override: str | None = None
    base_url_override: str | None = None
    system_prompt_override: str | None = None
    api_key_override: str | None = None
    api_format_override: str | None = None
    # If an external API client was injected, store it for reuse
    external_api_client: SupportsStreamingMessages | None = None
    # Messages to restore on first spawn (from session restore)
    _initial_messages: list[dict] | None = field(default=None, repr=False)

    def resolve_settings(self) -> Settings:
        """Load settings and apply any CLI overrides."""
        return load_settings().merge_cli_overrides(
            model=self.model_override,
            base_url=self.base_url_override,
            system_prompt=self.system_prompt_override,
            api_key=self.api_key_override,
            api_format=self.api_format_override,
        )


# ---------------------------------------------------------------------------
# EphemeralAgent — short-lived runtime for one request
# ---------------------------------------------------------------------------


@dataclass
class EphemeralAgent:
    """A short-lived agent that handles one user request then dies.

    Each agent has an identity (``agent_name``) and its own API client,
    tool registry, hook executor, and query engine.  In a relay model,
    different agents can serve successive turns within the same session,
    each with their own model, tools, and system prompt.
    """

    agent_name: str
    api_client: SupportsStreamingMessages
    engine: QueryEngine
    settings: Settings
    model: str

    async def run(self, prompt: str) -> AsyncIterator[StreamEvent]:
        """Execute one complete tool-call loop for the given prompt."""
        async for event in self.engine.submit_message(prompt):
            yield event


def spawn_agent(
    config: SessionConfig,
    messages: list[ConversationMessage],
    *,
    agent_def: AgentDefinition | None = None,
    latest_user_prompt: str | None = None,
    session_context: "SessionContext | None" = None,
) -> EphemeralAgent:
    """Spawn a fresh ephemeral agent with the given session history.

    If *agent_def* is provided, its fields override the session defaults:
    - ``model`` overrides the session model
    - ``system_prompt`` replaces the default system prompt
    - ``toolkits`` restricts available toolkits
    - ``max_turns`` caps the tool-call loop iterations

    This enables **relay mode**: different agents can handle successive
    turns in the same session, each with different capabilities.
    """
    settings = config.resolve_settings()

    # --- Active model from DB (carries api_key, base_url, class_path) ------
    db_kwargs: dict | None = None
    db_class_path: str | None = None
    try:
        from ephemeralos.server.app_factory import model_store
        active = model_store.get_active_resolved() if model_store.is_available else None
        if active:
            db_kwargs = active.get("kwargs")
            db_class_path = active.get("class_path")
    except Exception:
        active = None

    # --- Per-agent overrides ------------------------------------------------
    resolved_model = (
        agent_def.model if agent_def and agent_def.model
        else (db_kwargs or {}).get("model") or settings.model
    )
    agent_name = agent_def.name if agent_def else resolved_model

    # --- API client
    api_client = _make_api_client(
        settings, config.external_api_client,
        db_kwargs=db_kwargs, db_class_path=db_class_path,
    )

    # --- Tool registry
    tool_registry = create_default_tool_registry()
    if agent_def and agent_def.toolkits:
        tool_registry.restrict_to_toolkits(agent_def.toolkits)

    # --- Hook executor
    hook_executor = _make_hook_executor(settings, config.cwd, api_client)

    # --- System prompt
    if agent_def and agent_def.system_prompt:
        system_prompt = agent_def.system_prompt
    else:
        system_prompt = build_runtime_system_prompt(
            settings, cwd=config.cwd, latest_user_prompt=latest_user_prompt,
        )

    # --- Max turns
    max_turns = agent_def.max_turns if agent_def and agent_def.max_turns else 200

    # --- Query engine
    engine = QueryEngine(
        api_client=api_client,
        tool_registry=tool_registry,
        cwd=config.cwd,
        model=resolved_model,
        system_prompt=system_prompt,
        max_tokens=settings.max_tokens,
        hook_executor=hook_executor,
        session_context=session_context,
    )
    if messages:
        engine.load_messages(messages)

    return EphemeralAgent(
        agent_name=agent_name,
        api_client=api_client,
        engine=engine,
        settings=settings,
        model=resolved_model,
    )


# ---------------------------------------------------------------------------
# Session config lifecycle
# ---------------------------------------------------------------------------


def build_session_config(
    *,
    model: str | None = None,
    base_url: str | None = None,
    system_prompt: str | None = None,
    api_key: str | None = None,
    api_format: str | None = None,
    api_client: SupportsStreamingMessages | None = None,
    restore_messages: list[dict] | None = None,
) -> SessionConfig:
    """Build durable session config. Called once at server startup."""
    from uuid import uuid4

    return SessionConfig(
        cwd=str(Path.cwd()),
        session_id=uuid4().hex[:12],
        model_override=model,
        base_url_override=base_url,
        system_prompt_override=system_prompt,
        api_key_override=api_key,
        api_format_override=api_format,
        external_api_client=api_client,
        _initial_messages=restore_messages,
    )


# ---------------------------------------------------------------------------
# handle_line — spawn, run, die
# ---------------------------------------------------------------------------


def _load_session_state(config: SessionConfig) -> tuple[list[ConversationMessage], "SessionContext"]:
    """Load conversation history and session context from DB.

    Returns (messages, session_context).  The session context is the
    source of truth for context-window management across ephemeral agents.
    """
    from ephemeralos.server.app_factory import session_store
    from ephemeralos.utils.compact import SessionContext

    ctx = SessionContext()

    if session_store._session_factory is not None:
        record = session_store.get(config.session_id)
        if record:
            ctx = SessionContext.from_dict(record.session_context)
            if record.message_history:
                try:
                    msgs = [ConversationMessage.model_validate(m) for m in record.message_history]
                    return msgs, ctx
                except Exception:
                    logger.warning("Failed to deserialize messages from DB — starting fresh", exc_info=True)

    # Fallback: initial restore messages (consumed once)
    if config._initial_messages:
        try:
            msgs = [ConversationMessage.model_validate(m) for m in config._initial_messages]
            config._initial_messages = None
            return msgs, ctx
        except Exception:
            logger.warning("Failed to load initial restore messages — starting fresh", exc_info=True)

    return [], ctx


async def handle_line(
    config: SessionConfig,
    line: str,
    *,
    print_system: SystemPrinter,
    render_event: StreamRenderer,
    clear_output: ClearHandler,
    agent_def: AgentDefinition | None = None,
) -> bool:
    """Spawn an ephemeral agent, run it, let it die.

    1. Load conversation history from persistence
    2. Spawn a fresh agent (optionally configured by *agent_def*)
    3. Execute the user's request (full tool-call loop)
    4. Record the agent run + token usage to DB
    5. Save updated history back to DB
    6. Agent goes out of scope — dies

    In relay mode, successive calls can pass different ``agent_def``
    values so different agents handle different turns within the same
    session.
    """
    from ephemeralos.server.app_factory import agent_run_store, session_store, usage_store

    db_available = agent_run_store._session_factory is not None

    # 1. Load history + session context from DB
    messages, session_context = _load_session_state(config)

    # 2. Spawn ephemeral agent (inherits session context)
    agent = spawn_agent(
        config, messages,
        agent_def=agent_def, latest_user_prompt=line,
        session_context=session_context,
    )
    logger.info("Spawned agent %r (model=%s, session=%s)", agent.agent_name, agent.model, config.session_id)

    # 3. Ensure session record exists (agent_runs FK requires it)
    run_id: str | None = None
    if db_available:
        try:
            session_store.upsert(
                session_id=config.session_id,
                cwd=config.cwd,
                model=agent.model,
                message_count=0,
            )
        except Exception:
            logger.debug("Failed to ensure session record", exc_info=True)

    # 4. Create agent run record
    if db_available:
        from uuid import uuid4

        run_id = uuid4().hex[:12]
        try:
            agent_run_store.create_run(
                run_id=run_id,
                session_id=config.session_id,
                agent_name=agent.agent_name,
                input_query=line[:2000],
            )
        except Exception:
            logger.debug("Failed to create agent run record", exc_info=True)
            run_id = None

    # 5. Run the agent
    event_count = 0
    run_error: str | None = None
    usage_snapshot = None

    try:
        async for event in agent.run(line):
            event_count += 1
            if isinstance(event, AssistantTurnComplete):
                usage_snapshot = event.usage
            await render_event(event)
    except Exception as exc:
        run_error = str(exc)
        raise
    finally:
        # Finish agent run record
        if run_id and db_available:
            try:
                agent_run_store.finish_run(
                    run_id,
                    status="failed" if run_error else "completed",
                    error=run_error,
                    event_count=event_count,
                )
            except Exception:
                logger.debug("Failed to finish agent run record", exc_info=True)

        # Record token usage
        if db_available and usage_snapshot and (usage_snapshot.input_tokens or usage_snapshot.output_tokens):
            try:
                usage_store.record(
                    session_id=config.session_id,
                    agent_name=agent.agent_name,
                    model_id=agent.model,
                    prompt_tokens=usage_snapshot.input_tokens,
                    completion_tokens=usage_snapshot.output_tokens,
                )
            except Exception:
                logger.debug("Failed to record token usage", exc_info=True)

    # 6. Save updated history to DB
    if db_available:
        try:
            session_store.upsert(
                session_id=config.session_id,
                cwd=config.cwd,
                model=agent.model,
                system_prompt=build_runtime_system_prompt(
                    agent.settings, cwd=config.cwd, latest_user_prompt=line,
                ),
                messages=[m.model_dump(mode="json") for m in agent.engine.messages],
                usage=agent.engine.total_usage.model_dump(),
                session_context=agent.engine.session_context.to_dict() if agent.engine.session_context else None,
                summary=next(
                    (m.text.strip()[:80] for m in agent.engine.messages if m.role == "user" and m.text.strip()),
                    "",
                ),
                message_count=len(agent.engine.messages),
            )
        except Exception:
            logger.debug("Failed to save session to DB", exc_info=True)

    logger.info("Agent %r finished (events=%d, status=%s)", agent.agent_name, event_count, "failed" if run_error else "completed")

    # 7. Agent goes out of scope — ephemeral lifecycle complete
    return True
