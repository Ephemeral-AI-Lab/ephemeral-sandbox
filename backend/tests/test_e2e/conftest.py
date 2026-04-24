# ruff: noqa
"""E2E test fixtures — in-memory DB, mock LLM, TestClient, and EvalAgent helpers."""

from __future__ import annotations

import json
import logging
import sys
import time
import types
from pathlib import Path
from typing import Any, AsyncIterator
from unittest.mock import MagicMock
from uuid import uuid4

import pytest
from dotenv import load_dotenv

# ---------------------------------------------------------------------------
# Stub heavy dependencies ONLY if they are genuinely not installed.
# ---------------------------------------------------------------------------


def _try_import_or_stub(mod_name: str, attrs: dict) -> None:
    """Import the real module if available; otherwise install a stub."""
    if mod_name in sys.modules:
        return
    try:
        __import__(mod_name)
    except ImportError:
        _stub = types.ModuleType(mod_name)
        for k, v in attrs.items():
            _stub.__dict__.setdefault(k, v)
        sys.modules[mod_name] = _stub


_try_import_or_stub(
    "anthropic",
    {
        "APIError": type("APIError", (Exception,), {}),
        "APIStatusError": type("APIStatusError", (Exception,), {}),
        "AsyncAnthropic": MagicMock,
    },
)
_try_import_or_stub("anthropic.types", {})
_try_import_or_stub(
    "daytona_sdk",
    {
        "Daytona": MagicMock,
        "DaytonaConfig": MagicMock,
        "CreateSandboxParams": MagicMock,
    },
)
_try_import_or_stub(
    "daytona_sdk.daytona",
    {
        "Daytona": MagicMock,
        "DaytonaConfig": MagicMock,
        "CreateSandboxParams": MagicMock,
    },
)

# Now safe to import project code
from sqlalchemy import create_engine
from sqlalchemy import text
from sqlalchemy.engine import make_url
from sqlalchemy.orm import sessionmaker

from db.base import Base
from engine.testing.eval_agent import EvalAgent
from message import ConversationMessage, TextBlock, ThinkingBlock, ToolUseBlock
from providers import (
    ApiMessageCompleteEvent,
    ApiTextDeltaEvent,
    ApiThinkingDeltaEvent,
    UsageSnapshot,
)
from prompt import build_runtime_system_prompt
from token_tracker.runtime import persist_run_usage

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Credential checks (powered by EvalAgent)
# ---------------------------------------------------------------------------

# Load .env BEFORE credential checks so env vars are available
_PROJECT_ROOT = Path(__file__).resolve().parents[3]
load_dotenv(_PROJECT_ROOT / ".env")

# Suppress "Event loop is closed" warnings from httpx/anthropic async cleanup.
# The async client's __del__ tries to close the transport after the loop shuts down.
import warnings
import logging

logging.getLogger("asyncio").setLevel(logging.CRITICAL)

HAS_CREDENTIALS = EvalAgent.has_credentials()
HAS_DAYTONA = EvalAgent.has_daytona()
HAS_ALL = EvalAgent.has_all()


def create_eval_agent(
    *,
    system_prompt: str | None = None,
    sandbox_id: str | None = None,
    **kwargs,
) -> EvalAgent:
    """Create an EvalAgent for e2e tests.

    Uses the active model from the DB registry (which has the correct
    client class, auth, and base_url already configured).
    """
    _reset_runtime_store_singletons()
    return EvalAgent.create(
        system_prompt=system_prompt,
        sandbox_id=sandbox_id,
        **kwargs,
    )


def _ensure_eval_agent_db_ready(settings) -> None:
    """Initialise all DB-backed stores the live eval harness depends on."""
    try:
        from db.engine import initialize_db
        from server.app_factory import (
            agent_run_store,
            model_store,
            session_store,
            usage_store,
        )

        needs_init = (
            not model_store.is_available
            or any(
                not store.is_ready
                for store in (agent_run_store, session_store)
            )
            or not _usage_store_ready(usage_store)
        )
        if not needs_init or not settings.database.url:
            return

        sf = initialize_db(settings.database)
        if sf is None:
            return

        if not model_store.is_available:
            model_store.initialize(sf)
        if not agent_run_store.is_ready:
            agent_run_store.initialize(sf)
        if not session_store.is_ready:
            session_store.initialize(sf)
        if not _usage_store_ready(usage_store):
            usage_store.initialize(sf)
    except Exception as exc:
        logger.debug("[tests.test_e2e] EvalAgent DB bootstrap unavailable: %s", exc)


def _extract_new_messages(display_messages, prompt: str) -> list[dict[str, Any]]:
    for i in range(len(display_messages) - 1, -1, -1):
        msg = display_messages[i]
        if msg.role == "user" and msg.text.strip() == prompt.strip():
            return [m.model_dump(mode="json") for m in display_messages[i:]]
    return [m.model_dump(mode="json") for m in display_messages]


def _usage_store_ready(store: Any) -> bool:
    return getattr(store, "_session_factory", None) is not None


def _reset_runtime_store_singletons() -> None:
    """Detach server store singletons from per-test DB schemas."""
    try:
        from server import app_factory as _af

        for store in (
            _af.session_store,
            _af.agent_run_store,
            _af.usage_store,
            _af.model_store,
        ):
            if hasattr(store, "_session_factory"):
                store._session_factory = None
    except Exception:
        pass


def _persist_eval_agent_artifacts(agent: EvalAgent, prompt: str, result: Any | None = None) -> None:
    """Mirror the server router's post-run persistence for live eval runs."""
    try:
        from server.app_factory import agent_run_store, session_store, usage_store
    except Exception:
        return

    session_config = getattr(agent, "_session_config", None)
    if session_config is None:
        return

    display_messages = list(getattr(agent, "_display_messages", []) or [])
    api_messages = list(agent._query_context.api_messages_snapshot or [])
    session_id = getattr(session_config, "session_id", None)
    if not session_id:
        return

    full_history = list(getattr(agent, "_e2e_full_history", []) or [])
    if not full_history and session_store.is_ready:
        try:
            record = session_store.get(session_id)
            if record and record.full_message_history:
                full_history = list(record.full_message_history)
        except Exception:
            logger.debug("[tests.test_e2e] Failed to bootstrap full history", exc_info=True)

    new_messages = _extract_new_messages(display_messages, prompt)
    if new_messages:
        full_history.extend(new_messages)
    setattr(agent, "_e2e_full_history", full_history)

    tool_metadata = getattr(agent._query_context, "tool_metadata", None)
    run_id = getattr(tool_metadata, "agent_run_id", None)

    if run_id and agent_run_store.is_ready:
        record = agent_run_store.get_run(run_id)
        event_count = getattr(record, "event_count", 0) if record else 0
        status = getattr(record, "status", "completed") if record else "completed"
        error = getattr(record, "error", None) if record else None
        cancellation_reason = getattr(record, "cancellation_reason", None) if record else None
        agent_name = getattr(getattr(agent, "_agent", None), "agent_name", "eval_agent")
        agent_run_store.finish_run(
            run_id,
            status=status,
            response=new_messages or None,
            message_history=[m.model_dump(mode="json") for m in display_messages] or None,
            compacted_history=[m.model_dump(mode="json") for m in api_messages] or None,
            reasoning=(
                getattr(result, "thinking_text", "") or getattr(agent, "_e2e_reasoning", None)
            ),
            error=error,
            event_count=event_count,
            cancellation_reason=cancellation_reason,
        )

        usage = getattr(agent, "_e2e_total_usage", None) or getattr(
            getattr(agent, "_agent", None), "total_usage", None
        )
        if _usage_store_ready(usage_store) and usage_store.get_run_usage(run_id) is None:
            persist_run_usage(
                usage_store=usage_store,
                session_id=session_id,
                run_id=run_id,
                agent_name=agent_name,
                model_id=agent.model,
                usage=usage,
            )

    if session_store.is_ready:
        try:
            session_store.upsert(
                session_id=session_id,
                cwd=getattr(session_config, "cwd", "."),
                model=agent.model,
                system_prompt=build_runtime_system_prompt(
                    agent.settings,
                    cwd=getattr(session_config, "cwd", "."),
                    latest_user_prompt=prompt,
                ),
                messages=[m.model_dump(mode="json") for m in display_messages] or None,
                full_messages=full_history or None,
                usage=(
                    usage.model_dump()
                    if usage is not None
                    else None
                ),
                session_state=agent._query_context.session_state.to_dict()
                if agent._query_context.session_state
                else None,
                summary=next(
                    (
                        m.text.strip()[:80]
                        for m in display_messages
                        if m.role == "user" and m.text.strip()
                    ),
                    "",
                ),
                message_count=len(display_messages),
            )
        except Exception:
            logger.debug("[tests.test_e2e] Failed to persist session artifacts", exc_info=True)


def get_eval_persistence(agent: EvalAgent) -> dict[str, Any]:
    """Return the persisted run/session/usage state for the given eval agent."""
    from server.app_factory import agent_run_store, session_store, usage_store

    session_config = getattr(agent, "_session_config", None)
    session_id = getattr(session_config, "session_id", None) if session_config else None
    tool_metadata = getattr(agent._query_context, "tool_metadata", None)
    run_id = getattr(tool_metadata, "agent_run_id", None)

    run = agent_run_store.get_run(run_id) if run_id and agent_run_store.is_ready else None
    subagent_runs = (
        agent_run_store.list_subagent_runs(run_id)
        if run_id and agent_run_store.is_ready
        else []
    )
    run_usage = (
        usage_store.get_run_usage(run_id)
        if run_id and _usage_store_ready(usage_store)
        else None
    )
    child_usage = (
        usage_store.get_usage_for_runs([child["id"] for child in subagent_runs])
        if subagent_runs and _usage_store_ready(usage_store)
        else {}
    )
    for child in subagent_runs:
        child["usage"] = child_usage.get(child["id"])

    parent_total_tokens = (run_usage or {}).get("total_tokens", 0)
    subagent_total_tokens = sum(
        (child.get("usage") or {}).get("total_tokens", 0)
        for child in subagent_runs
    )

    return {
        "session_id": session_id,
        "session": session_store.get(session_id) if session_id and session_store.is_ready else None,
        "session_usage": usage_store.get_session_usage(session_id)
        if session_id and _usage_store_ready(usage_store)
        else None,
        "run_id": run_id,
        "run": run,
        "run_usage": run_usage,
        "subagent_runs": subagent_runs,
        "parent_total_tokens": parent_total_tokens,
        "subagent_total_tokens": subagent_total_tokens,
        "run_tree_total_tokens": parent_total_tokens + subagent_total_tokens,
    }


if not getattr(EvalAgent, "_tests_e2e_persistence_patched", False):
    EvalAgent._original_invoke = EvalAgent.invoke
    EvalAgent._ensure_db_ready = staticmethod(_ensure_eval_agent_db_ready)

    async def _invoke_with_persistence(self, prompt: str, verbose: bool = True):
        from agents.run_tracker import AgentRunTracker
        from engine.core.query import run_query
        from engine.testing.eval_agent import (
            EvalResult,
            ToolCallResult,
            _estimate_final_context,
            _truncate,
        )
        from message.stream_events import (
            AssistantTurnComplete,
            ThinkingDelta,
        )
        from message.event_printer import MultiAgentEventPrinter
        from tools.core.base import ExecutionMetadata

        self._display_messages.clear()
        self._display_messages.append(ConversationMessage.from_user_text(prompt))
        start = time.monotonic()
        events: list[Any] = []
        tool_calls: list[ToolCallResult] = []
        reasoning_parts: list[str] = []

        def _out(msg: str) -> None:
            if verbose:
                print(msg, flush=True)

        # Shared printer — same file used by the sweevo CLI so single-agent
        # and multi-agent runs produce the same visual format. ``sink=_out``
        # routes through the existing verbose gate.
        printer = MultiAgentEventPrinter(
            color=sys.stdout.isatty(),
            sink=_out,
        )

        _out(f"  [EvalAgent] prompt: {_truncate(prompt, 80)}")

        total_usage = UsageSnapshot()
        compacted_before: int | None = None
        if self._query_context.session_state is not None:
            compacted_before = int(self._query_context.session_state.compacted)

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

        run_error: str | None = None
        pending_exc: Exception | None = None

        try:
            messages, event_iter = await run_query(self._query_context, self._display_messages)
            self._display_messages = messages
            async for event, usage in event_iter:
                events.append(event)
                if usage:
                    total_usage.input_tokens += usage.input_tokens
                    total_usage.output_tokens += usage.output_tokens

                # Reasoning trace persistence lives outside the printer so
                # it's available even when verbose=False.
                if isinstance(event, ThinkingDelta):
                    reasoning_parts.append(event.text)

                # tool_calls must be captured for EvalResult regardless of
                # whether the printer silences the structural event.
                if isinstance(event, AssistantTurnComplete):
                    for tb in event.message.tool_uses:
                        tool_calls.append(ToolCallResult(name=tb.name, input=tb.input))

                if verbose:
                    printer.emit(event)
        except Exception as exc:
            run_error = str(exc)
            pending_exc = exc
        finally:
            if verbose:
                printer.flush()

            self._e2e_total_usage = total_usage
            self._e2e_reasoning = "".join(reasoning_parts) if reasoning_parts else None

            tracker.finish(
                status="failed" if run_error else "completed",
                display_messages=list(self._display_messages),
                api_messages_snapshot=self._query_context.api_messages_snapshot,
                response=_extract_new_messages(self._display_messages, prompt) or None,
                reasoning=self._e2e_reasoning,
                error=run_error,
                event_count=len(events),
            )

            try:
                _persist_eval_agent_artifacts(self, prompt, result=None)
            except Exception:
                logger.debug(
                    "[tests.test_e2e] Failed to persist eval artifacts after invoke",
                    exc_info=True,
                )

        if pending_exc is not None:
            raise pending_exc

        latency_ms = (time.monotonic() - start) * 1000
        compaction_note = ""
        st = self._query_context.session_state
        if st is not None and compacted_before is not None:
            new_compactions = int(st.compacted) - compacted_before
            compaction_note = (
                f", compactions={'+1' if new_compactions > 0 else '0'}"
                f" (compacted={st.compacted})"
            )
        usage_note = ""
        try:
            persisted = get_eval_persistence(self)
            parent_tokens = persisted["parent_total_tokens"] or total_usage.total_tokens
            child_tokens = persisted["subagent_total_tokens"]
            if child_tokens:
                usage_note = (
                    f", subagent_tokens={child_tokens}, "
                    f"run_tree_total={persisted['run_tree_total_tokens'] or parent_tokens + child_tokens}"
                )
        except Exception:
            logger.debug("[tests.test_e2e] Failed to load run usage for eval summary", exc_info=True)
        _out(
            f"  [EvalAgent] done: {len(tool_calls)} tool calls, "
            f"{latency_ms:.0f}ms, "
            f"tokens in={total_usage.input_tokens} out={total_usage.output_tokens} "
            f"total={total_usage.total_tokens}, "
            f"final_context={_estimate_final_context(self._query_context.api_messages_snapshot)}"
            f"{compaction_note}{usage_note}"
        )

        result = EvalResult(
            events=events,
            tool_calls=tool_calls,
            latency_ms=latency_ms,
        )
        return result

    EvalAgent.invoke = _invoke_with_persistence
    EvalAgent._tests_e2e_persistence_patched = True


# ---------------------------------------------------------------------------
# Backward-compat: credential constants used by tests not yet refactored.
# These will be removed as tests migrate to EvalAgent.create().
# ---------------------------------------------------------------------------

import os

_LIVE_SETTINGS = {}
_settings_path = Path.home() / ".ephemeralos" / "settings.json"
if _settings_path.exists():
    _LIVE_SETTINGS = json.loads(_settings_path.read_text())

# Load active model from DB registry for correct credentials.
# Falls back to settings.json if DB is unavailable.
_DB_MODEL_KWARGS: dict = {}
try:
    from config.settings import load_settings as _ls

    _s = _ls()
    if _s.database.url:
        from db.engine import initialize_db as _idb
        from server.app_factory import (
            agent_run_store as _ars,
            model_store as _ms,
            session_store as _ss,
            usage_store as _us,
        )

        # Initialise *all* DB-backed singletons up front so EvalAgent-driven
        # tests (and the local factories that bypass EvalAgent.create()) get
        # the same persistence setup the production server bootstrap provides.
        # Without this, run_subagent usage and compacted session artifacts can
        # be dropped because the corresponding stores are unready at tool-call
        # or post-run persistence time.
        if not _ms.is_available or not _ars.is_ready or not _ss.is_ready or not _usage_store_ready(_us):
            _sf = _idb(_s.database)
            if _sf:
                if not _ms.is_available:
                    _ms.initialize(_sf)
                if not _ars.is_ready:
                    _ars.initialize(_sf)
                if not _ss.is_ready:
                    _ss.initialize(_sf)
                if not _usage_store_ready(_us):
                    _us.initialize(_sf)
        if _ms.is_available:
            _active = _ms.get_active_resolved()
            if _active:
                _DB_MODEL_KWARGS = _active.get("kwargs", {})
except Exception:
    pass

MINIMAX_KEY = _DB_MODEL_KWARGS.get("api_key") or os.environ.get("MINIMAX_API_KEY") or ""
MINIMAX_MODEL = (
    _DB_MODEL_KWARGS.get("model")
    or os.environ.get("MINIMAX_MODEL")
    or "MiniMax-M2.7"
)
MINIMAX_BASE_URL = (
    _DB_MODEL_KWARGS.get("base_url") or os.environ.get("MINIMAX_BASE_URL") or ""
)
# All e2e tests use an Anthropic-compatible endpoint.
MINIMAX_FORMAT = "anthropic"

ANTHROPIC_MINIMAX_KEY = MINIMAX_KEY
ANTHROPIC_MINIMAX_MODEL = MINIMAX_MODEL
ANTHROPIC_MINIMAX_BASE_URL = MINIMAX_BASE_URL
ANTHROPIC_MINIMAX_FORMAT = "anthropic"

DAYTONA_KEY = os.environ.get("DAYTONA_API_KEY") or _LIVE_SETTINGS.get("daytona_api_key", "")
DAYTONA_URL = os.environ.get("DAYTONA_API_URL") or _LIVE_SETTINGS.get("daytona_api_url", "")
DAYTONA_TARGET = os.environ.get("DAYTONA_TARGET") or _LIVE_SETTINGS.get("daytona_target", "")

HAS_MINIMAX = bool(MINIMAX_KEY and MINIMAX_BASE_URL)
HAS_ANTHROPIC_MINIMAX = HAS_MINIMAX
HAS_BOTH = HAS_MINIMAX and HAS_DAYTONA
HAS_ANTHROPIC_AND_DAYTONA = HAS_MINIMAX and HAS_DAYTONA


def _postgres_test_database_url() -> str:
    raw_url = os.environ.get("EPHEMERALOS_TEST_DATABASE_URL") or os.environ.get(
        "EPHEMERALOS_DATABASE_URL"
    )
    if not raw_url:
        pytest.skip(
            "PostgreSQL e2e tests require EPHEMERALOS_TEST_DATABASE_URL "
            "or EPHEMERALOS_DATABASE_URL."
        )
    url = make_url(raw_url)
    if url.get_backend_name() != "postgresql":
        pytest.skip("E2E database URL must use PostgreSQL.")
    if url.drivername in {"postgresql", "postgresql+psycopg2"}:
        url = url.set(drivername="postgresql+psycopg")
    return url.render_as_string(hide_password=False)


def _database_url_from_session_factory(factory) -> str:
    bind = factory.kw.get("bind")
    if bind is None:
        return _postgres_test_database_url()
    return bind.url.render_as_string(hide_password=False)


def _patch_server_database(monkeypatch, session_factory) -> None:
    def _ensure_runtime_stores_ready(*args, **kwargs):
        from server import app_factory as _af

        for store in (
            _af.session_store,
            _af.agent_run_store,
            _af.usage_store,
            _af.model_store,
        ):
            store.initialize(session_factory)
        return session_factory

    monkeypatch.setattr("db.engine.initialize_db", lambda *a, **kw: session_factory)
    monkeypatch.setattr("db.engine.get_session_factory", lambda: session_factory)
    monkeypatch.setattr("server.app_factory.initialize_db", lambda *a, **kw: session_factory)
    monkeypatch.setattr("server.app_factory.get_session_factory", lambda: session_factory)
    monkeypatch.setattr("server.app_factory.ensure_runtime_stores_ready", _ensure_runtime_stores_ready)


def make_live_client(
    db_session_factory,
    tmp_path,
    monkeypatch,
    *,
    api_key: str = "",
    model: str = "",
    base_url: str = "",
):
    """Create a TestClient configured with real API credentials (compat)."""
    from fastapi.testclient import TestClient
    from server.protocol import BackendHostConfig
    from server.app_factory import create_app

    api_key = api_key or MINIMAX_KEY
    model = model or MINIMAX_MODEL
    base_url = base_url or MINIMAX_BASE_URL

    for _var in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy", "NO_PROXY", "no_proxy"]:
        monkeypatch.delenv(_var, raising=False)
    monkeypatch.delenv("EPHEMERALOS_DATABASE_URL", raising=False)
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)

    if DAYTONA_KEY:
        monkeypatch.setenv("DAYTONA_API_KEY", DAYTONA_KEY)
    if DAYTONA_URL:
        monkeypatch.setenv("DAYTONA_API_URL", DAYTONA_URL)
    if DAYTONA_TARGET:
        monkeypatch.setenv("DAYTONA_TARGET", DAYTONA_TARGET)

    _patch_server_database(monkeypatch, db_session_factory)

    def _patched_load_settings(*a, **kw):
        from config.settings import Settings as _S, DatabaseSettings as _DS

        return _S(
            daytona_api_key=DAYTONA_KEY,
            daytona_api_url=DAYTONA_URL,
            daytona_target=DAYTONA_TARGET,
            database=_DS(url=_database_url_from_session_factory(db_session_factory)),
        )

    monkeypatch.setattr("config.load_settings", _patched_load_settings)
    monkeypatch.setattr("config.settings.load_settings", _patched_load_settings)
    monkeypatch.setattr("server.app_factory.load_settings", _patched_load_settings)

    # Seed active model registration for this test DB so DB-sourced model
    # resolution finds credentials.
    def _seed_model(sf):
        from db.stores.model_store import ModelStore as _MS

        _s = _MS()
        _s.initialize(sf)
        _s.register(
            key="test_minimax",
            label="test_minimax",
            class_path="anthropic",
            kwargs={
                "model": model,
                "api_key": api_key,
                "base_url": base_url or None,
                "max_tokens": 16384,
            },
            activate=True,
        )

    _seed_model(db_session_factory)

    config = BackendHostConfig()
    app = create_app(config)
    return TestClient(app)


def send_chat(
    client,
    line: str,
    *,
    agent_name: str | None = None,
    sandbox_id: str | None = None,
    timeout: int = 180,
    verbose: bool = True,
) -> list[dict]:
    """Send a chat message and return parsed SSE events (compat)."""
    payload: dict[str, Any] = {"line": line}
    if agent_name:
        payload["agent_name"] = agent_name
    if sandbox_id:
        payload["sandbox_id"] = sandbox_id

    if verbose:
        print(f"  [send_chat] prompt: {line[:80]}", flush=True)

    resp = client.post("/api/chat", json=payload, timeout=timeout)
    assert resp.status_code == 200, f"Chat failed: {resp.status_code} {resp.text[:500]}"
    events = parse_sse_events(resp.text)

    if verbose:
        _print_sse_events(events)

    return events


def _print_sse_events(events: list[dict]) -> None:
    """Print parsed SSE events for real-time test visibility."""
    for evt in events:
        etype = evt.get("type", "")
        if etype == "thinking_delta":
            text = evt.get("text", "")
            if text:
                print(f"    [thinking] {text[:500]}", flush=True)
        elif etype == "assistant_delta":
            text = evt.get("message", evt.get("text", ""))
            if text:
                print(f"    [text] {text}", flush=True)
        elif etype == "tool_started":
            name = evt.get("tool_name", "?")
            inp = evt.get("tool_input", {})
            print(f"    -> tool_start: {name}({inp})", flush=True)
        elif etype == "tool_completed":
            name = evt.get("tool_name", "?")
            is_err = evt.get("is_error", False)
            output = evt.get("output", "")
            status = "ERROR" if is_err else "ok"
            print(f"    <- tool_done:  {name} [{status}] {output}", flush=True)
        elif etype == "assistant_complete":
            print("    [assistant_complete]", flush=True)


def create_test_agent(
    client,
    name: str,
    *,
    tools: list[str] | None = None,
    skills: list[str] | None = None,
    system_prompt: str | None = None,
    model: str | None = None,
) -> dict:
    """Create an agent and return its data (compat)."""
    payload: dict[str, Any] = {
        "name": name,
        "description": f"E2E test agent: {name}",
        "model": model or MINIMAX_MODEL,
    }
    if tools:
        payload["tools"] = tools
    if skills:
        payload["skills"] = skills
    if system_prompt:
        payload["system_prompt"] = system_prompt

    resp = client.post("/api/agents/", json=payload)
    if resp.status_code == 201:
        return resp.json()
    get_resp = client.get(f"/api/agents/{name}")
    if get_resp.status_code == 200:
        return get_resp.json()
    assert False, f"Failed to create or get agent '{name}': {resp.status_code} {resp.text}"


# ---------------------------------------------------------------------------
# Mock LLM client
# ---------------------------------------------------------------------------


class MockApiClient:
    """Deterministic mock that captures what tools/system_prompt the engine sends."""

    def __init__(self) -> None:
        self.last_request: Any = None
        self.all_requests: list[Any] = []
        self.responses: list[ConversationMessage] = []
        self._call_count = 0

    def set_responses(self, *msgs: ConversationMessage) -> None:
        self.responses = list(msgs)

    async def stream_message(self, request: Any) -> AsyncIterator:
        """Capture the request and yield streaming events + final message."""
        self.last_request = request
        self.all_requests.append(request)
        idx = min(self._call_count, len(self.responses) - 1) if self.responses else 0
        msg = (
            self.responses[idx]
            if self.responses
            else ConversationMessage(role="assistant", content=[TextBlock(text="I have no tools.")])
        )
        self._call_count += 1

        for block in msg.content:
            if isinstance(block, ThinkingBlock):
                yield ApiThinkingDeltaEvent(text=block.text)

        for block in msg.content:
            if isinstance(block, TextBlock):
                yield ApiTextDeltaEvent(text=block.text)

        yield ApiMessageCompleteEvent(
            message=msg,
            usage=UsageSnapshot(input_tokens=100, output_tokens=50),
            stop_reason="end_turn",
        )


# ---------------------------------------------------------------------------
# Database fixture
# ---------------------------------------------------------------------------


@pytest.fixture()
def db_session_factory(tmp_path):
    """Create an isolated PostgreSQL schema with all tables."""
    base_url = _postgres_test_database_url()
    schema_name = f"ephemeralos_test_{uuid4().hex}"
    admin_engine = create_engine(base_url, echo=False)
    with admin_engine.begin() as conn:
        conn.execute(text(f'CREATE SCHEMA "{schema_name}"'))

    engine = create_engine(
        base_url,
        echo=False,
        connect_args={"options": f"-csearch_path={schema_name}"},
    )

    import db.models  # noqa: F401

    Base.metadata.create_all(engine)
    sf = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    try:
        yield sf
    finally:
        engine.dispose()
        with admin_engine.begin() as conn:
            conn.execute(text(f'DROP SCHEMA IF EXISTS "{schema_name}" CASCADE'))
        admin_engine.dispose()


@pytest.fixture()
def mock_api_client():
    """Return a fresh MockApiClient."""
    client = MockApiClient()
    client.set_responses(
        ConversationMessage(
            role="assistant",
            content=[TextBlock(text="Hello! I can see my tools.")],
        )
    )
    return client


@pytest.fixture()
def override_compaction_threshold(monkeypatch):
    """Temporarily override the auto-compaction threshold for targeted tests."""

    def _apply(threshold: int) -> None:
        monkeypatch.setattr(
            "compaction.compactor.get_autocompact_threshold",
            lambda _model: threshold,
        )
        monkeypatch.setattr(
            "compaction.get_autocompact_threshold",
            lambda _model: threshold,
        )

    return _apply


# ---------------------------------------------------------------------------
# App + TestClient fixture (for mock tests)
# ---------------------------------------------------------------------------


@pytest.fixture()
def app_client(db_session_factory, mock_api_client, tmp_path, monkeypatch):
    """Create a FastAPI TestClient with real DB and mock LLM."""
    from fastapi.testclient import TestClient

    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    monkeypatch.delenv("EPHEMERALOS_DATABASE_URL", raising=False)

    _patch_server_database(monkeypatch, db_session_factory)
    monkeypatch.setattr("providers.provider.make_api_client", lambda *a, **kw: mock_api_client)
    monkeypatch.setattr(
        "prompt.build_runtime_system_prompt",
        lambda *a, **kw: "You are a test assistant.",
    )

    def _patched_load_settings(*a, **kw):
        from config.settings import Settings, DatabaseSettings

        return Settings(
            database=DatabaseSettings(url=_database_url_from_session_factory(db_session_factory)),
        )

    monkeypatch.setattr("config.load_settings", _patched_load_settings)
    monkeypatch.setattr("config.settings.load_settings", _patched_load_settings)
    monkeypatch.setattr("server.app_factory.load_settings", _patched_load_settings)

    # Seed active model registration so DB-based model resolution works.
    from db.stores.model_store import ModelStore as _MS

    _ms = _MS()
    _ms.initialize(db_session_factory)
    _ms.register(
        key="test_mock",
        label="test_mock",
        class_path="anthropic",
        kwargs={
            "model": "claude-sonnet-4-20250514",
            "api_key": "test-api-key",
            "base_url": None,
            "max_tokens": 16384,
        },
        activate=True,
    )

    from server.protocol import BackendHostConfig
    from server.app_factory import create_app

    config = BackendHostConfig(api_client=mock_api_client)
    app = create_app(config)

    with TestClient(app) as client:
        yield client, mock_api_client


from sandbox.testing import (
    EVAL_SANDBOX_FILES,
    create_test_sandbox,
    delete_test_sandbox,
    get_sandbox_service,
    populate_sandbox_files,
)


# ---------------------------------------------------------------------------
# SSE parsing helpers (for tests that still use TestClient)
# ---------------------------------------------------------------------------


def parse_sse_events(raw: str) -> list[dict[str, Any]]:
    """Parse SSE text into a list of JSON-decoded BackendEvent dicts."""
    events = []
    for line in raw.split("\n"):
        line = line.strip()
        if line.startswith("data: "):
            payload = line[6:]
            if payload == "[DONE]":
                continue
            try:
                events.append(json.loads(payload))
            except json.JSONDecodeError:
                pass
    return events


def events_of_type(events: list[dict], event_type: str) -> list[dict]:
    """Filter parsed SSE events by their 'type' field."""
    return [e for e in events if e.get("type") == event_type]


def get_assistant_text(events: list[dict]) -> str:
    """Extract assistant text from SSE events."""
    import re

    parts: list[str] = []
    for evt in events_of_type(events, "assistant_complete"):
        msg = evt.get("message", "")
        if msg:
            parts.append(msg)

    if not parts:
        for evt in events_of_type(events, "assistant_delta"):
            msg = evt.get("message", "")
            if msg:
                parts.append(msg)

    text = "\n".join(parts)
    text = re.sub(r"<think>.*?</think>", "", text, flags=re.DOTALL).strip()
    return text


def get_event_types(events: list[dict]) -> set[str]:
    return {e["type"] for e in events}


def get_tool_started_events(events: list[dict]) -> list[dict]:
    return events_of_type(events, "tool_started")


def get_tool_completed_events(events: list[dict]) -> list[dict]:
    return events_of_type(events, "tool_completed")


def get_tool_cancelled_events(events: list[dict]) -> list[dict]:
    return events_of_type(events, "tool_cancelled")
