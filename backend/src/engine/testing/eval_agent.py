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
    assert "daytona_codeact" in result.tool_names

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

from agents.types import AgentDefinition
from config.settings import Settings, load_settings
from engine.core.query import run_query
from message.messages import ConversationMessage
from message.stream_events import (
    AssistantTextDelta,
    AssistantTurnComplete,
    BackgroundTaskCompleted,
    BackgroundTaskStarted,
    StreamEvent,
    SystemNotification,
    ThinkingDelta,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from providers.types import SupportsStreamingMessages, UsageSnapshot
from tools.core.base import ExecutionMetadata

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

    def has_tool_with_background(self, name: str) -> bool:
        """Check if a tool was called with background: true in its input."""
        return any(
            tc.name == name and tc.input.get("background") is True
            for tc in self.tool_calls
        )

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

    def system_notifications(self) -> list[SystemNotification]:
        return [e for e in self.events if isinstance(e, SystemNotification)]

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

    @property
    def non_cancel_error_events(self) -> list[ToolExecutionCompleted | BackgroundTaskCompleted]:
        """Error events excluding cancelled/killed process artifacts.

        Filters out:
        - ToolExecutionCompleted with exit_code -1 and empty stdout (process
          killed by cancellation or transient SDK failure)
        - BackgroundTaskCompleted with "Cancelled" output
        """
        def _is_killed_process(output: str) -> bool:
            """Check if output is from a killed/cancelled process (exit_code -1, empty stdout)."""
            if '"exit_code": -1' not in output:
                return False
            try:
                import json
                data = json.loads(output)
                return data.get("exit_code") == -1 and not (data.get("stdout") or "").strip()
            except (json.JSONDecodeError, AttributeError):
                return False

        results: list[ToolExecutionCompleted | BackgroundTaskCompleted] = []
        for tool_evt in self.tools_completed():
            if tool_evt.is_error and not _is_killed_process(tool_evt.output):
                results.append(tool_evt)
        for bg_evt in self.background_completed():
            if bg_evt.is_error and not bg_evt.output.startswith("Cancelled"):
                results.append(bg_evt)
        return results

    @property
    def has_non_cancel_errors(self) -> bool:
        return len(self.non_cancel_error_events) > 0

    @property
    def unrecovered_error_events(self) -> list[ToolExecutionCompleted | BackgroundTaskCompleted]:
        """Non-cancel errors where no later successful call to the same tool exists.

        An agent may hit an intermediate failure (e.g. ``cat`` on a file not yet
        flushed) and then retry successfully.  This property keeps only errors
        that were **never** followed by a success on the same tool, i.e. errors
        the agent did not recover from.
        """
        errors = self.non_cancel_error_events
        if not errors:
            return []

        all_completed = list(self.tools_completed())
        event_index: dict[int, int] = {id(e): i for i, e in enumerate(self.events)}

        results: list[ToolExecutionCompleted | BackgroundTaskCompleted] = []
        for err in errors:
            err_idx = event_index.get(id(err), len(self.events))
            recovered = False
            if isinstance(err, ToolExecutionCompleted):
                for later in all_completed:
                    if (
                        event_index.get(id(later), -1) > err_idx
                        and later.tool_name == err.tool_name
                        and not later.is_error
                    ):
                        recovered = True
                        break
            if not recovered:
                results.append(err)
        return results

    @property
    def has_unrecovered_errors(self) -> bool:
        return len(self.unrecovered_error_events) > 0


# ---------------------------------------------------------------------------
# EvalAgent
# ---------------------------------------------------------------------------


class EvalAgent:
    """Configurable test agent for e2e evaluation.

    Wraps an :class:`EphemeralAgent` (built via :func:`spawn_agent`) with
    credentials from settings.json. Test classes configure their specific
    agent (system prompt, toolkits, background tasks) via the
    :meth:`create` classmethod.
    """

    def __init__(
        self,
        ephemeral_agent: Any,
        settings: Settings,
        model: str,
        api_client: SupportsStreamingMessages,
        session_config: Any = None,
    ) -> None:
        self._agent = ephemeral_agent
        self._query_context = ephemeral_agent.query_context
        self._settings = settings
        self._model = model
        self._api_client_ref = api_client
        self._display_messages: list[ConversationMessage] = []
        self._session_config = session_config

    # -- Static helpers for credential checks --

    @staticmethod
    def load_settings() -> Settings:
        """Load settings from ~/.ephemeralos/settings.json + env overrides."""
        return load_settings()

    @staticmethod
    def has_credentials() -> bool:
        """Check if API credentials are available via the active DB model."""
        try:
            EvalAgent._ensure_db_ready(load_settings())
            from config.model_config import try_get_active_model_kwargs

            kwargs = try_get_active_model_kwargs() or {}
            return bool(kwargs.get("api_key"))
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
        tool_call_limit: int | None = None,
        max_tokens: int | None = None,
        settings: Settings | None = None,
    ) -> EvalAgent:
        """Create a configured EvalAgent.

        EvalAgent is a thin test harness wrapper around :func:`spawn_agent`.
        All production spawn semantics (model resolution, API client,
        Daytona toolkit registration, capability gating) run through the
        same code path as the server router, so tests exercise the real
        stack rather than a parallel implementation.

        Args:
            system_prompt: Custom system prompt. If None, uses default.
            sandbox_id: Daytona sandbox ID for sandbox tools.
            enable_background_tasks: Enable background task execution.
                (Effectively derived from available tools; retained as a
                no-op arg for API compatibility with older tests.)
            tool_call_limit: Optional cap on tool dispatches for the ephemeral run.
            max_tokens: Override max_tokens from settings.
            settings: Override auto-loaded settings.

        Returns:
            Configured EvalAgent ready to invoke.
        """
        del enable_background_tasks  # derived from registered tools

        if settings is None:
            settings = load_settings()

        # Ensure the DB stores the test harness needs are initialised.
        cls._ensure_db_ready(settings)

        # Resolve the active model so the test uses the same provider +
        # credentials the server would.
        from config.model_config import NoActiveModelError, try_get_active_model_kwargs
        from engine.runtime.agent import spawn_agent
        from providers.provider import make_api_client

        db_kwargs = try_get_active_model_kwargs() or {}
        if not db_kwargs:
            raise NoActiveModelError(
                "EvalAgent requires an active model_registrations row."
            )
        resolved_model = db_kwargs.get("model")
        if not resolved_model:
            raise RuntimeError("Active model registration has no 'model' id")
        api_client = make_api_client(db_kwargs=db_kwargs)
        if db_kwargs:
            logger.info(
                "[EvalAgent] Using DB model: model=%s", db_kwargs.get("model", "?")
            )

        # Build a real SessionConfig so subagents persist agent_run rows
        # with a valid parent session_id.
        from uuid import uuid4

        from server.app_factory import SessionConfig

        session_config = SessionConfig(cwd=".", session_id=uuid4().hex[:12])
        cls._ensure_parent_session_row(session_config, resolved_model)

        # Tune the AgentDefinition so spawn_agent produces the same tool
        # surface EvalAgent historically exposed: Daytona + subagent, no
        # auto-loaded skills, the raw test system prompt.
        agent_def = AgentDefinition(
            name="eval_agent",
            description="Test harness eval agent",
            system_prompt=system_prompt or DEFAULT_SYSTEM_PROMPT,
            toolkits=["sandbox_operations", "subagent"],
            tool_call_limit=tool_call_limit,
            include_skills=False,
            source="builtin",
        )

        ephemeral = spawn_agent(
            session_config,
            messages=[],
            agent_def=agent_def,
            sandbox_id=sandbox_id,
        )
        if max_tokens is not None:
            ephemeral.query_context.max_tokens = max_tokens

        # The fresh EphemeralAgent built its own API client; swap it for
        # the one we just created so eval-side observers (latency,
        # request inspection) see the same object the tests constructed.
        ephemeral.query_context.api_client = api_client

        return cls(
            ephemeral_agent=ephemeral,
            settings=settings,
            model=resolved_model,
            api_client=api_client,
            session_config=session_config,
        )

    @staticmethod
    def _ensure_db_ready(settings: Settings) -> None:
        """Initialise agent_run_store + session_store + model_store if configured.

        Safe no-op when the DB URL is missing or stores are already ready.
        Swallows errors — the eval harness tolerates an unavailable DB and
        simply skips persistence.
        """
        try:
            from server.app_factory import agent_run_store, model_store, session_store

            needs_init = any(
                not store.is_ready
                for store in (agent_run_store, session_store)
            ) or not model_store.is_available

            if needs_init and settings.database.url:
                from db.engine import initialize_db

                sf = initialize_db(settings.database)
                if sf is not None:
                    if not model_store.is_available:
                        model_store.initialize(sf)
                    if not agent_run_store.is_ready:
                        agent_run_store.initialize(sf)
                    if not session_store.is_ready:
                        session_store.initialize(sf)
        except Exception as exc:
            logger.debug("[EvalAgent] DB persistence unavailable: %s", exc)

    @staticmethod
    def _ensure_parent_session_row(session_config: Any, model: str) -> None:
        """Insert the parent sessions row so the agent_runs FK can resolve."""
        try:
            from server.app_factory import session_store

            if session_store.is_ready:
                session_store.upsert(
                    session_id=session_config.session_id,
                    cwd=session_config.cwd,
                    model=model,
                    message_count=0,
                )
        except Exception:
            logger.debug(
                "[EvalAgent] session_store.upsert failed", exc_info=True
            )

    @classmethod
    def from_settings(cls, settings: Settings) -> EvalAgent:
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
            verbose: If True, print streaming events in real-time (thinking,
                     text, tool calls). If False, suppress output.
        """
        self._display_messages.clear()
        self._display_messages.append(ConversationMessage.from_user_text(prompt))
        start = time.monotonic()
        events: list[StreamEvent] = []
        tool_calls: list[ToolCallResult] = []
        thinking_buf: list[str] = []
        text_buf: list[str] = []

        def _out(msg: str) -> None:
            if verbose:
                print(msg, flush=True)

        _out(f"  [EvalAgent] prompt: {_truncate(prompt, 80)}")

        total_usage = UsageSnapshot()
        compacted_before: int | None = None
        if self._query_context.session_state is not None:
            compacted_before = int(self._query_context.session_state.compacted)

        # Create a top-level agent_run row for this invocation so tools that
        # spawn nested runs (e.g. run_subagent) can attribute themselves to a
        # parent_run_id — mirrors server.routers.core.execute_ephemeral_agent_run.
        from agents.run_tracker import AgentRunTracker

        tracker = AgentRunTracker.create(
            session_id=getattr(self._session_config, "session_id", None),
            agent_name="eval_agent",
            input_query=prompt,
        )
        run_id = tracker.run_id

        if run_id is not None:
            if self._query_context.tool_metadata is None:
                self._query_context.tool_metadata = ExecutionMetadata()
            self._query_context.tool_metadata.agent_run_id = run_id

        messages, event_iter = await run_query(self._query_context, self._display_messages)
        self._display_messages = messages
        async for event, usage in event_iter:
            events.append(event)
            if usage:
                total_usage.input_tokens += usage.input_tokens
                total_usage.output_tokens += usage.output_tokens

            if isinstance(event, ThinkingDelta):
                thinking_buf.append(event.text)
                continue
            elif isinstance(event, AssistantTextDelta):
                text_buf.append(event.text)
                continue

            if thinking_buf:
                _out(f"    [thinking] {_truncate(''.join(thinking_buf), 500)}")
                thinking_buf.clear()
            if text_buf:
                _out(f"    [text] {_truncate(''.join(text_buf), 500)}")
                text_buf.clear()

            if isinstance(event, ToolExecutionStarted):
                _out(
                    f"    -> tool_start: {event.tool_name}"
                    f"({_truncate(str(event.tool_input), 120)})"
                )
            elif isinstance(event, ToolExecutionCompleted):
                status = "ERROR" if event.is_error else "ok"
                _out(
                    f"    <- tool_done:  {event.tool_name}"
                    f" [{status}] {event.output}"
                )
            elif isinstance(event, AssistantTurnComplete):
                for tb in event.message.tool_uses:
                    tool_calls.append(ToolCallResult(name=tb.name, input=tb.input))
            elif isinstance(event, BackgroundTaskStarted):
                _out(
                    f"    >> bg_start:   {event.tool_name}"
                    f" task_id={event.task_id}"
                )
            elif isinstance(event, BackgroundTaskCompleted):
                _out(
                    f"    << bg_done:    {event.tool_name}"
                    f" {event.output}"
                )
            elif isinstance(event, SystemNotification):
                _out(f"    [system] {event.text}")

        if thinking_buf:
            _out(f"    [thinking] {_truncate(''.join(thinking_buf), 500)}")
        if text_buf:
            _out(f"    [text] {_truncate(''.join(text_buf), 500)}")

        # Finalise the top-level agent_run row so listings/inspectors see it
        # in a terminal state (mirrors execute_ephemeral_agent_run).
        tracker.finish(status="completed", event_count=len(events))

        latency_ms = (time.monotonic() - start) * 1000

        compaction_note = ""
        st = self._query_context.session_state
        if st is not None and compacted_before is not None:
            new_compactions = int(st.compacted) - compacted_before
            compaction_note = (
                f", compactions={'+1' if new_compactions > 0 else '0'}"
                f" (compacted={st.compacted})"
            )
        _out(
            f"  [EvalAgent] done: {len(tool_calls)} tool calls, "
            f"{latency_ms:.0f}ms, "
            f"tokens in={total_usage.input_tokens} out={total_usage.output_tokens} "
            f"total={total_usage.total_tokens}, "
            f"final_context={_estimate_final_context(self._query_context.api_messages_snapshot)}"
            f"{compaction_note}"
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

    async def __aenter__(self) -> EvalAgent:
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.close()


def _estimate_final_context(messages: list[ConversationMessage] | None) -> int:
    """Estimate the context size (tokens) of the final compacted message list.

    Mirrors the token count that ``compact_for_api`` uses when deciding
    whether to auto-compact, so it reflects what would be sent to the
    provider on the next turn.
    """
    if not messages:
        return 0
    try:
        from compaction import estimate_message_tokens

        return estimate_message_tokens(messages)
    except Exception:
        return 0


def _truncate(s: str, max_len: int, /) -> str:
    """Truncate a string for logging."""
    if len(s) <= max_len:
        return s
    return s[:max_len] + "..."
