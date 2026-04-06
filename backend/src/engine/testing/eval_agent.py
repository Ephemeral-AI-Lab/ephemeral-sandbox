"""EvalAgent — configurable test agent for e2e evaluation.

Provides a single entry point for all e2e tests to create a configured agent
with credentials loaded from ~/.ephemeralos/settings.json. Test classes
configure their specific agent via EvalAgent.create().

Usage::

    agent = EvalAgent.create(
        system_prompt="You are a developer with sandbox access.",
        sandbox_id="sb-123",
        enable_background_tasks=True,
    )
    result = await agent.invoke("Run tests in the sandbox")
    assert "daytona_bash" in result.tool_names

    # For raw client access (streaming protocol tests):
    client = agent.api_client
    async for event in client.stream_message(request):
        ...
"""

from __future__ import annotations

import logging
import re
import time
from dataclasses import dataclass, field
from typing import Any

from config.settings import Settings, load_settings
from engine.core.query import QueryContext, run_query
from message.messages import ConversationMessage
from message.stream_events import (
    AssistantTextDelta,
    AssistantTurnComplete,
    BackgroundTaskCompleted,
    BackgroundTaskStarted,
    StreamEvent,
    ThinkingDelta,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from models.core.provider import make_api_client
from models.core.types import SupportsStreamingMessages
from tools import ToolRegistry
from tools.daytona_toolkit import DaytonaToolkit

logger = logging.getLogger(__name__)

DEFAULT_SYSTEM_PROMPT = """\
You are an AI assistant with access to a remote development sandbox.
Use tools for every action. Be concise.
"""


# ---------------------------------------------------------------------------
# Result types
# ---------------------------------------------------------------------------


@dataclass
class ToolCallResult:
    """A single tool call made during evaluation."""

    name: str
    input: dict[str, Any]


@dataclass
class EvalResult:
    """Rich result from an evaluation run with event inspection helpers."""

    events: list[StreamEvent] = field(default_factory=list)
    tool_calls: list[ToolCallResult] = field(default_factory=list)
    latency_ms: float = 0.0

    # -- Text helpers --

    @property
    def text(self) -> str:
        """Concatenated assistant text from all turns, with thinking stripped."""
        from message import TextBlock

        parts: list[str] = []
        for event in self.events:
            if isinstance(event, AssistantTurnComplete):
                for block in event.message.content:
                    if isinstance(block, TextBlock):
                        parts.append(block.text)
        text = "\n".join(parts)
        return re.sub(r"<think>.*?</think>", "", text, flags=re.DOTALL).strip()

    @property
    def thinking_text(self) -> str:
        """Concatenated thinking/reasoning text."""
        return "".join(e.text for e in self.events if isinstance(e, ThinkingDelta))

    # -- Tool helpers --

    @property
    def tool_names(self) -> list[str]:
        """Names of all tools called, in order."""
        return [tc.name for tc in self.tool_calls]

    def tool_count(self, name: str) -> int:
        """Count how many times a specific tool was called."""
        return sum(1 for tc in self.tool_calls if tc.name == name)

    def has_tool(self, name: str) -> bool:
        """Check if a specific tool was called."""
        return any(tc.name == name for tc in self.tool_calls)

    # -- Event type accessors --

    def tools_started(self) -> list[ToolExecutionStarted]:
        return [e for e in self.events if isinstance(e, ToolExecutionStarted)]

    def tools_completed(self) -> list[ToolExecutionCompleted]:
        return [e for e in self.events if isinstance(e, ToolExecutionCompleted)]

    def tools_cancelled(self) -> list[ToolExecutionCancelled]:
        return [e for e in self.events if isinstance(e, ToolExecutionCancelled)]

    def background_started(self) -> list[BackgroundTaskStarted]:
        return [e for e in self.events if isinstance(e, BackgroundTaskStarted)]

    def background_completed(self) -> list[BackgroundTaskCompleted]:
        return [e for e in self.events if isinstance(e, BackgroundTaskCompleted)]

    def assistant_turns(self) -> list[AssistantTurnComplete]:
        return [e for e in self.events if isinstance(e, AssistantTurnComplete)]

    def text_deltas(self) -> list[AssistantTextDelta]:
        return [e for e in self.events if isinstance(e, AssistantTextDelta)]

    # -- Error helpers --

    @property
    def error_events(self) -> list[ToolExecutionCompleted]:
        """Tool completions that were errors."""
        return [e for e in self.tools_completed() if e.is_error]

    @property
    def has_errors(self) -> bool:
        return len(self.error_events) > 0


# ---------------------------------------------------------------------------
# EvalAgent
# ---------------------------------------------------------------------------


class EvalAgent:
    """Configurable test agent for e2e evaluation.

    Wraps QueryContext + run_query with credentials from settings.json. Test classes
    configure their specific agent (system prompt, toolkits, background
    tasks) via the create() classmethod.
    """

    def __init__(
        self,
        query_context: QueryContext,
        settings: Settings,
        model: str,
        api_client: SupportsStreamingMessages,
    ) -> None:
        self._query_context = query_context
        self._settings = settings
        self._model = model
        self._api_client_ref = api_client
        self._messages: list[ConversationMessage] = []

    # -- Static helpers for credential checks --

    @staticmethod
    def load_settings() -> Settings:
        """Load settings from ~/.ephemeralos/settings.json + env overrides."""
        return load_settings()

    @staticmethod
    def has_credentials() -> bool:
        """Check if API credentials are available."""
        try:
            s = load_settings()
            return bool(s.api_key or s.resolve_api_key())
        except Exception:
            return False

    @staticmethod
    def has_daytona() -> bool:
        """Check if Daytona sandbox credentials are available."""
        import os

        try:
            s = load_settings()
            api_key = s.daytona_api_key or os.environ.get("DAYTONA_API_KEY", "")
            api_url = s.daytona_api_url or os.environ.get("DAYTONA_API_URL", "")
            return bool(api_key and api_url)
        except Exception:
            return False

    @staticmethod
    def has_all() -> bool:
        """Check if both API and Daytona credentials are available."""
        return EvalAgent.has_credentials() and EvalAgent.has_daytona()

    # -- Properties --

    @property
    def api_client(self) -> SupportsStreamingMessages:
        """Access the raw API client for low-level streaming tests."""
        return self._api_client_ref

    @property
    def settings(self) -> Settings:
        return self._settings

    @property
    def model(self) -> str:
        return self._model

    # -- Factory methods --

    @classmethod
    def create(
        cls,
        *,
        system_prompt: str | None = None,
        sandbox_id: str | None = None,
        enable_background_tasks: bool = False,
        max_turns: int = 200,
        max_tokens: int | None = None,
        settings: Settings | None = None,
    ) -> "EvalAgent":
        """Create a configured EvalAgent.

        Uses the active model from the DB registry when available,
        falling back to settings.json. This ensures the correct
        client class and auth (e.g. auth_token for MiniMax Anthropic)
        are used automatically.

        Args:
            system_prompt: Custom system prompt. If None, uses default.
            sandbox_id: Daytona sandbox ID for sandbox tools.
            enable_background_tasks: Enable background task execution.
            max_turns: Maximum agentic loop turns.
            max_tokens: Override max_tokens from settings.
            settings: Override auto-loaded settings.

        Returns:
            Configured EvalAgent ready to invoke.
        """
        if settings is None:
            settings = load_settings()

        # Load active model from DB registry (same pattern as engine.agent).
        # Initializes the model_store with the real PostgreSQL DB if needed.
        db_kwargs: dict | None = None
        db_class_path: str | None = None
        try:
            from server.app_factory import model_store

            if not model_store.is_available and settings.database.url:
                from db.engine import initialize_db

                sf = initialize_db(settings.database)
                if sf is not None:
                    model_store.initialize(sf)

            active = model_store.get_active_resolved() if model_store.is_available else None
            if active:
                db_kwargs = active.get("kwargs")
                db_class_path = active.get("class_path")
                logger.info(
                    "[EvalAgent] Using DB model: class_path=%s model=%s",
                    db_class_path,
                    (db_kwargs or {}).get("model", "?"),
                )
        except Exception as exc:
            logger.debug("[EvalAgent] DB model registry unavailable: %s", exc)

        resolved_model = (db_kwargs or {}).get("model") or settings.model
        api_client = make_api_client(settings, db_kwargs=db_kwargs, db_class_path=db_class_path)

        tool_registry = ToolRegistry()
        daytona_toolkit = DaytonaToolkit(sandbox_id=sandbox_id)
        tool_registry.register_toolkit(daytona_toolkit)

        prompt = system_prompt or DEFAULT_SYSTEM_PROMPT

        query_context = QueryContext(
            api_client=api_client,
            tool_registry=tool_registry,
            cwd=".",
            model=resolved_model,
            system_prompt=prompt,
            max_tokens=max_tokens or settings.max_tokens,
            max_turns=max_turns,
            hook_executor=None,
            enable_background_tasks=enable_background_tasks,
        )

        return cls(
            query_context=query_context,
            settings=settings,
            model=resolved_model,
            api_client=api_client,
        )

    @classmethod
    def from_settings(cls, settings: Settings) -> "EvalAgent":
        """Construct from an explicit Settings object (backward compat)."""
        return cls.create(settings=settings)

    # -- Invocation --

    async def invoke(self, prompt: str, verbose: bool = True) -> EvalResult:
        """Send a prompt through the full agent loop and collect results.

        Each invocation starts with a clean conversation history so that
        module-scoped fixtures can reuse the same agent across tests
        without stale tool_use_ids leaking between runs.

        Args:
            prompt: The user prompt to send.
            verbose: If True, emit logs via logger.info (captured by test frameworks).
                     If False, suppress output.
        """
        self._messages.clear()
        self._messages.append(ConversationMessage.from_user_text(prompt))
        start = time.monotonic()
        events: list[StreamEvent] = []
        tool_calls: list[ToolCallResult] = []
        thinking_buf: list[str] = []
        text_buf: list[str] = []

        logger.info("[EvalAgent] prompt: %s", _truncate(prompt, 80))

        messages, event_iter = run_query(self._query_context, self._messages)
        self._messages = messages
        async for event, _usage in event_iter:
            events.append(event)

            if isinstance(event, ThinkingDelta):
                thinking_buf.append(event.text)
                continue
            elif isinstance(event, AssistantTextDelta):
                text_buf.append(event.text)
                continue

            if thinking_buf:
                logger.info("    [thinking] %s", _truncate("".join(thinking_buf), 500))
                thinking_buf.clear()
            if text_buf:
                logger.info("    [text] %s", _truncate("".join(text_buf), 500))
                text_buf.clear()

            if isinstance(event, ToolExecutionStarted):
                logger.info(
                    "    -> tool_start: %s(%s)",
                    event.tool_name,
                    _truncate(str(event.tool_input), 120),
                )
            elif isinstance(event, ToolExecutionCompleted):
                status = "ERROR" if event.is_error else "ok"
                logger.info(
                    "    <- tool_done:  %s [%s] %s",
                    event.tool_name,
                    status,
                    _truncate(event.output, 120),
                )
            elif isinstance(event, AssistantTurnComplete):
                for tb in event.message.tool_uses:
                    tool_calls.append(ToolCallResult(name=tb.name, input=tb.input))
            elif isinstance(event, BackgroundTaskStarted):
                logger.info(
                    "    >> bg_start:   %s task_id=%s",
                    event.tool_name,
                    event.task_id,
                )
            elif isinstance(event, BackgroundTaskCompleted):
                logger.info(
                    "    << bg_done:    %s %s",
                    event.tool_name,
                    _truncate(event.output, 120),
                )

        if thinking_buf:
            logger.info("    [thinking] %s", _truncate("".join(thinking_buf), 500))
        if text_buf:
            logger.info("    [text] %s", _truncate("".join(text_buf), 500))

        latency_ms = (time.monotonic() - start) * 1000

        logger.info(
            "  [EvalAgent] done: %d tool calls, %.0fms",
            len(tool_calls),
            latency_ms,
        )

        return EvalResult(
            events=events,
            tool_calls=tool_calls,
            latency_ms=latency_ms,
        )

    async def close(self) -> None:
        """Release resources held by the agent's API client."""
        closer = getattr(self._api_client_ref, "aclose", None)
        if closer is not None:
            await closer()

    async def __aenter__(self) -> "EvalAgent":
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.close()


def _truncate(s: str, max_len: int, /) -> str:
    """Truncate a string for logging."""
    if len(s) <= max_len:
        return s
    return s[:max_len] + "..."
