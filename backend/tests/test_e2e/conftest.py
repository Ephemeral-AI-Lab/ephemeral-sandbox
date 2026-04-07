# ruff: noqa
"""E2E test fixtures — in-memory DB, mock LLM, TestClient, and EvalAgent helpers."""

from __future__ import annotations

import json
import sys
import time
import types
from pathlib import Path
from typing import Any, AsyncIterator
from unittest.mock import MagicMock

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
    enable_background_tasks: bool = False,
    **kwargs,
) -> EvalAgent:
    """Create an EvalAgent for e2e tests.

    Uses the active model from the DB registry (which has the correct
    client class, auth, and base_url already configured).
    """
    return EvalAgent.create(
        system_prompt=system_prompt,
        sandbox_id=sandbox_id,
        enable_background_tasks=enable_background_tasks,
        **kwargs,
    )


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
        from server.app_factory import model_store as _ms

        if not _ms.is_available:
            _sf = _idb(_s.database)
            if _sf:
                _ms.initialize(_sf)
        if _ms.is_available:
            _active = _ms.get_active_resolved()
            if _active:
                _DB_MODEL_KWARGS = _active.get("kwargs", {})
except Exception:
    pass

MINIMAX_KEY = (
    _DB_MODEL_KWARGS.get("api_key")
    or os.environ.get("MINIMAX_API_KEY")
    or _LIVE_SETTINGS.get("api_key", "")
)
MINIMAX_MODEL = (
    _DB_MODEL_KWARGS.get("model")
    or os.environ.get("MINIMAX_MODEL")
    or _LIVE_SETTINGS.get("model", "MiniMax-M2.7-highspeed")
)
MINIMAX_BASE_URL = (
    _DB_MODEL_KWARGS.get("base_url")
    or os.environ.get("MINIMAX_BASE_URL")
    or _LIVE_SETTINGS.get("base_url", "")
)
# Default to anthropic format — all e2e tests use Anthropic-compatible endpoint
MINIMAX_FORMAT = (
    _DB_MODEL_KWARGS.get("api_format") or os.environ.get("MINIMAX_API_FORMAT") or "anthropic"
)

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


def make_live_client(
    db_session_factory,
    tmp_path,
    monkeypatch,
    *,
    api_key: str = "",
    model: str = "",
    base_url: str = "",
    api_format: str = "",
):
    """Create a TestClient configured with real API credentials (compat)."""
    from fastapi.testclient import TestClient
    from server.protocol import BackendHostConfig
    from server.app_factory import create_app

    api_key = api_key or MINIMAX_KEY
    model = model or MINIMAX_MODEL
    base_url = base_url or MINIMAX_BASE_URL
    api_format = api_format or MINIMAX_FORMAT

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

    monkeypatch.setattr("db.engine.initialize_db", lambda *a, **kw: db_session_factory)
    monkeypatch.setattr("hooks.make_hook_executor", lambda *a, **kw: None)

    def _patched_load_settings(*a, **kw):
        from config.settings import Settings as _S, DatabaseSettings as _DS

        return _S(
            api_key=api_key,
            model=model,
            api_format=api_format,
            base_url=base_url or None,
            daytona_api_key=DAYTONA_KEY,
            daytona_api_url=DAYTONA_URL,
            daytona_target=DAYTONA_TARGET,
            database=_DS(url=f"sqlite:///{tmp_path / 'test.db'}"),
        )

    monkeypatch.setattr("config.load_settings", _patched_load_settings)
    monkeypatch.setattr("config.settings.load_settings", _patched_load_settings)
    monkeypatch.setattr("server.app_factory.load_settings", _patched_load_settings)

    config = BackendHostConfig(
        api_key=api_key,
        model=model,
        api_format=api_format,
        base_url=base_url or None,
    )
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
                print(f"    [text] {text[:500]}", flush=True)
        elif etype == "tool_started":
            name = evt.get("tool_name", "?")
            inp = evt.get("tool_input", {})
            print(f"    -> tool_start: {name}({str(inp)[:120]})", flush=True)
        elif etype == "tool_completed":
            name = evt.get("tool_name", "?")
            is_err = evt.get("is_error", False)
            output = evt.get("output", "")
            status = "ERROR" if is_err else "ok"
            print(f"    <- tool_done:  {name} [{status}] {str(output)[:120]}", flush=True)
        elif etype == "assistant_complete":
            print("    [assistant_complete]", flush=True)


def create_test_agent(
    client,
    name: str,
    *,
    toolkits: list[str] | None = None,
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
    if toolkits:
        payload["toolkits"] = toolkits
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
    """Create a file-based SQLite DB with all tables."""
    db_path = tmp_path / "test.db"
    engine = create_engine(f"sqlite:///{db_path}", echo=False)

    import db.models  # noqa: F401
    import agents.db.model  # noqa: F401

    Base.metadata.create_all(engine)
    sf = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    return sf


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

    monkeypatch.setattr("db.engine.initialize_db", lambda *a, **kw: db_session_factory)
    monkeypatch.setattr("models.core.provider.make_api_client", lambda *a, **kw: mock_api_client)
    monkeypatch.setattr("hooks.make_hook_executor", lambda *a, **kw: None)
    monkeypatch.setattr(
        "prompts.build_runtime_system_prompt",
        lambda *a, **kw: "You are a test assistant.",
    )

    def _patched_load_settings(*a, **kw):
        from config.settings import Settings, DatabaseSettings

        return Settings(
            api_key="test-api-key",
            model="claude-sonnet-4-20250514",
            database=DatabaseSettings(url=f"sqlite:///{tmp_path / 'test.db'}"),
        )

    monkeypatch.setattr("config.load_settings", _patched_load_settings)
    monkeypatch.setattr("config.settings.load_settings", _patched_load_settings)
    monkeypatch.setattr("server.app_factory.load_settings", _patched_load_settings)

    from server.protocol import BackendHostConfig
    from server.app_factory import create_app

    config = BackendHostConfig(
        api_key="test-api-key",
        model="claude-sonnet-4-20250514",
        api_client=mock_api_client,
    )
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
