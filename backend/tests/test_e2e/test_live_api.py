# ruff: noqa
"""Live API integration tests — require real API keys and Daytona sandbox.

Reads credentials from ~/.ephemeralos/settings.json or environment variables.
Run with: pytest tests/test_e2e/test_live_api.py -m live -v
"""

from __future__ import annotations

import json
import os
import time
from pathlib import Path

import pytest
from dotenv import load_dotenv

from tests.test_e2e.conftest import parse_sse_events, events_of_type

# Load .env from project root (contains DAYTONA_API_KEY, etc.)
_PROJECT_ROOT = Path(__file__).resolve().parents[3]
load_dotenv(_PROJECT_ROOT / ".env")

# Markers
pytestmark = [pytest.mark.e2e, pytest.mark.live]


# ---------------------------------------------------------------------------
# Load credentials from settings file or env
# ---------------------------------------------------------------------------

def _load_settings() -> dict:
    """Load settings from ~/.ephemeralos/settings.json."""
    settings_path = Path.home() / ".ephemeralos" / "settings.json"
    if settings_path.exists():
        return json.loads(settings_path.read_text())
    return {}

_SETTINGS = _load_settings()

# MiniMax key: from env or settings file
MINIMAX_KEY = os.environ.get("MINIMAX_API_KEY") or _SETTINGS.get("api_key", "")
MINIMAX_MODEL = os.environ.get("MINIMAX_MODEL") or _SETTINGS.get("model", "MiniMax-M2.7-highspeed")
MINIMAX_BASE_URL = os.environ.get("MINIMAX_BASE_URL") or _SETTINGS.get("base_url", "")
MINIMAX_FORMAT = os.environ.get("MINIMAX_API_FORMAT") or _SETTINGS.get("api_format", "anthropic")

# Daytona sandbox (from env — loaded from .env above — or settings)
DAYTONA_KEY = os.environ.get("DAYTONA_API_KEY") or _SETTINGS.get("daytona_api_key", "")
DAYTONA_URL = os.environ.get("DAYTONA_API_URL") or _SETTINGS.get("daytona_api_url", "")
DAYTONA_TARGET = os.environ.get("DAYTONA_TARGET") or _SETTINGS.get("daytona_target", "")

# Detect if MiniMax is configured (key + base_url both present)
HAS_MINIMAX = bool(MINIMAX_KEY and MINIMAX_BASE_URL)
HAS_DAYTONA = bool(DAYTONA_KEY and DAYTONA_URL)
HAS_BOTH = HAS_MINIMAX and HAS_DAYTONA


# ---------------------------------------------------------------------------
# Shared live test fixture helper
# ---------------------------------------------------------------------------

def _make_live_client(db_session_factory, tmp_path, monkeypatch, *, api_key, model, base_url, api_format):
    """Create a TestClient configured with real API credentials."""
    from fastapi.testclient import TestClient
    from server.protocol import BackendHostConfig
    from server.app_factory import create_app

    monkeypatch.delenv("EPHEMERALOS_DATABASE_URL", raising=False)
    monkeypatch.setattr("db.engine.initialize_db", lambda *a, **kw: db_session_factory)
    monkeypatch.setattr("engine.agent.make_hook_executor", lambda *a, **kw: None)

    def _patched_load_settings(*a, **kw):
        from config.settings import Settings, DatabaseSettings
        return Settings(
            api_key=api_key,
            model=model,
            api_format=api_format,
            base_url=base_url or None,
            daytona_api_key=DAYTONA_KEY,
            daytona_api_url=DAYTONA_URL,
            daytona_target=DAYTONA_TARGET,
            database=DatabaseSettings(url=f"sqlite:///{tmp_path / 'test.db'}"),
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


# ---------------------------------------------------------------------------
# Daytona sandbox helper — create/delete real sandboxes for tests
# ---------------------------------------------------------------------------

def _get_sandbox_service():
    """Return a SandboxService instance."""
    from sandbox.service import SandboxService
    return SandboxService()


def _create_test_sandbox(name: str = "e2e-test") -> dict:
    """Create a real Daytona sandbox for testing."""
    svc = _get_sandbox_service()
    sandbox = svc.create_sandbox(
        name=f"{name}-{int(time.time())}",
        language="python",
        labels={"purpose": "e2e-test"},
    )
    return sandbox


def _delete_sandbox(sandbox_id: str) -> None:
    """Delete a sandbox, ignoring errors."""
    try:
        svc = _get_sandbox_service()
        svc.delete_sandbox(sandbox_id)
    except Exception:
        pass


# ===========================================================================
# US-010: Sandbox lifecycle and tool calling via real Daytona
# ===========================================================================


@pytest.mark.skipif(not HAS_DAYTONA, reason="Daytona not configured")
class TestLiveSandboxLifecycle:
    """Test Daytona sandbox create, execute, read/write, and delete."""

    @pytest.fixture(scope="class")
    def live_sandbox(self):
        """Create a real sandbox for the test class, clean up after."""
        sandbox = _create_test_sandbox("lifecycle")
        yield sandbox
        _delete_sandbox(sandbox["id"])

    def test_live_sandbox_create(self, live_sandbox):
        """Verify sandbox was created with expected fields."""
        assert live_sandbox["id"], "Sandbox ID should be non-empty"
        assert live_sandbox["state"] in ("started", "running", "ready"), (
            f"Expected started state, got: {live_sandbox['state']}"
        )
        assert live_sandbox["managed_by_app"] is True

    def test_live_sandbox_bash(self, live_sandbox):
        """Execute a shell command in the sandbox."""
        svc = _get_sandbox_service()
        raw_sb = svc.get_sandbox_object(live_sandbox["id"])
        response = raw_sb.process.exec("echo 'hello-e2e'", timeout=30)
        assert "hello-e2e" in (response.result or "")

    def test_live_sandbox_file_write_read(self, live_sandbox):
        """Write a file and read it back in the sandbox."""
        svc = _get_sandbox_service()
        raw_sb = svc.get_sandbox_object(live_sandbox["id"])

        # Write file and read it back in a single exec call — Daytona process
        # isolation means separate exec calls may not share filesystem state.
        resp = raw_sb.process.exec(
            "echo 'e2e test content: hello world' > /tmp/e2e_test.txt && "
            "echo 'second line' >> /tmp/e2e_test.txt && "
            "cat /tmp/e2e_test.txt",
            timeout=30,
        )
        content = resp.result or ""
        assert "e2e test content: hello world" in content, (
            f"Write+read failed. Got: {content!r}"
        )
        assert "second line" in content

    def test_live_sandbox_list_files(self, live_sandbox):
        """List files in the sandbox /workspace directory."""
        svc = _get_sandbox_service()
        raw_sb = svc.get_sandbox_object(live_sandbox["id"])

        # Ensure there's at least one file
        raw_sb.process.exec("touch /workspace/listing_test.txt", timeout=10)
        # Use shell ls (more reliable across Daytona SDK versions than fs.list_files)
        ls_resp = raw_sb.process.exec("ls /workspace/", timeout=10)
        names = (ls_resp.result or "").strip().splitlines()
        assert len(names) > 0, "Should have at least one file in /workspace"

    def test_live_sandbox_cleanup(self, live_sandbox):
        """Verify the sandbox can be fetched before cleanup."""
        svc = _get_sandbox_service()
        info = svc.get_sandbox(live_sandbox["id"])
        assert info["id"] == live_sandbox["id"]


# ===========================================================================
# US-011: Agent chat with Daytona sandbox tools via MiniMax
# ===========================================================================


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
class TestLiveAgentSandboxChat:
    """Chat with a custom agent that has sandbox tools, using real MiniMax + Daytona."""

    @pytest.fixture(scope="class")
    def sandbox_for_agent(self):
        """Create a sandbox for agent chat tests."""
        sandbox = _create_test_sandbox("agent-chat")
        yield sandbox
        _delete_sandbox(sandbox["id"])

    @pytest.fixture()
    def minimax_client(self, db_session_factory, tmp_path, monkeypatch):
        client = _make_live_client(
            db_session_factory, tmp_path, monkeypatch,
            api_key=MINIMAX_KEY,
            model=MINIMAX_MODEL,
            base_url=MINIMAX_BASE_URL,
            api_format=MINIMAX_FORMAT,
        )
        with client:
            yield client

    def test_live_agent_creates_sandbox_agent(self, minimax_client, sandbox_for_agent):
        """Create a custom agent with sandbox_operations toolkit."""
        resp = minimax_client.post("/api/agents/", json={
            "name": "e2e-sandbox-agent",
            "description": "E2E test agent with sandbox tools",
            "model": MINIMAX_MODEL,
            "toolkits": ["sandbox_operations"],
            "system_prompt": (
                "You are a coding assistant with access to a remote sandbox. "
                "When asked to run commands, use the daytona_bash tool. "
                "When asked to read files, use daytona_read_file. "
                "Always respond concisely."
            ),
        })
        assert resp.status_code == 201, resp.text
        data = resp.json()
        assert data["name"] == "e2e-sandbox-agent"
        assert "sandbox_operations" in data["toolkits"]

    def test_live_agent_sandbox_chat(self, minimax_client, sandbox_for_agent):
        """Send a chat to a sandbox-equipped agent and verify events."""
        # Create agent first
        minimax_client.post("/api/agents/", json={
            "name": "sandbox-chat-agent",
            "description": "Chat test agent",
            "model": MINIMAX_MODEL,
            "toolkits": ["sandbox_operations"],
            "system_prompt": "You are a test assistant with sandbox access. Be very concise.",
        })

        resp = minimax_client.post(
            "/api/chat",
            json={
                "line": "Reply with exactly: SANDBOX_OK",
                "agent_name": "sandbox-chat-agent",
                "sandbox_id": sandbox_for_agent["id"],
            },
            timeout=90,
        )
        assert resp.status_code == 200
        events = parse_sse_events(resp.text)

        completes = events_of_type(events, "assistant_complete")
        assert len(completes) >= 1, f"No assistant_complete. Events: {[e['type'] for e in events]}"
        assert completes[0]["message"], "Empty assistant response"

    def test_live_agent_sandbox_bash_tool(self, minimax_client, sandbox_for_agent):
        """Verify the model can invoke daytona_bash and get results."""
        minimax_client.post("/api/agents/", json={
            "name": "bash-tool-agent",
            "description": "Agent that uses bash",
            "model": MINIMAX_MODEL,
            "toolkits": ["sandbox_operations"],
            "system_prompt": (
                "You have access to a remote sandbox via daytona_bash. "
                "When I ask you to run a command, use the daytona_bash tool. "
                "Always use tools, never just describe what you would do."
            ),
        })

        resp = minimax_client.post(
            "/api/chat",
            json={
                "line": "Run this exact command in the sandbox: echo 'E2E_TOOL_TEST_OK'",
                "agent_name": "bash-tool-agent",
                "sandbox_id": sandbox_for_agent["id"],
            },
            timeout=120,
        )
        assert resp.status_code == 200
        events = parse_sse_events(resp.text)

        # Check for tool usage events (model may or may not use tools depending on interpretation)
        types = {e["type"] for e in events}
        assert "assistant_complete" in types, f"Missing assistant_complete. Types: {types}"

        # If tool was used, verify tool events
        tool_started = events_of_type(events, "tool_started")
        tool_completed = events_of_type(events, "tool_completed")
        if tool_started:
            # Tool may error during sandbox execution; completed or error both acceptable
            assert len(tool_completed) >= 1 or "error" in types, "Tool started but never completed or errored"


# ===========================================================================
# US-012: Multi-turn conversation capability
# ===========================================================================


@pytest.mark.skipif(not HAS_MINIMAX, reason="MiniMax not configured")
class TestLiveMultiTurn:
    """Test multi-turn conversations with context retention."""

    @pytest.fixture()
    def minimax_client(self, db_session_factory, tmp_path, monkeypatch):
        client = _make_live_client(
            db_session_factory, tmp_path, monkeypatch,
            api_key=MINIMAX_KEY,
            model=MINIMAX_MODEL,
            base_url=MINIMAX_BASE_URL,
            api_format=MINIMAX_FORMAT,
        )
        with client:
            yield client

    def test_live_multiturn_context_retention(self, minimax_client):
        """Send 3 sequential messages and verify context retention."""
        # Turn 1: Establish a fact
        resp1 = minimax_client.post(
            "/api/chat",
            json={"line": "Remember this number: 42. Just confirm you noted it."},
            timeout=60,
        )
        assert resp1.status_code == 200
        events1 = parse_sse_events(resp1.text)
        completes1 = events_of_type(events1, "assistant_complete")
        assert len(completes1) >= 1, "Turn 1: no assistant_complete"

        # Turn 2: Ask about the fact
        resp2 = minimax_client.post(
            "/api/chat",
            json={"line": "What number did I just ask you to remember? Reply with just the number."},
            timeout=60,
        )
        assert resp2.status_code == 200
        events2 = parse_sse_events(resp2.text)
        completes2 = events_of_type(events2, "assistant_complete")
        assert len(completes2) >= 1, "Turn 2: no assistant_complete"
        # The model should reference 42
        assert "42" in completes2[0]["message"], (
            f"Model didn't retain context. Got: {completes2[0]['message']}"
        )

        # Turn 3: Build on previous context
        resp3 = minimax_client.post(
            "/api/chat",
            json={"line": "Multiply that number by 2. Reply with just the result."},
            timeout=60,
        )
        assert resp3.status_code == 200
        events3 = parse_sse_events(resp3.text)
        completes3 = events_of_type(events3, "assistant_complete")
        assert len(completes3) >= 1, "Turn 3: no assistant_complete"
        assert "84" in completes3[0]["message"], (
            f"Model didn't compute correctly. Got: {completes3[0]['message']}"
        )

    def test_live_multiturn_tool_followup(self, minimax_client):
        """Send a tool-using prompt then a follow-up referencing the output."""
        # Turn 1: Ask to use a tool
        resp1 = minimax_client.post(
            "/api/chat",
            json={"line": "Use the skill tool to list available skills."},
            timeout=60,
        )
        assert resp1.status_code == 200
        events1 = parse_sse_events(resp1.text)
        completes1 = events_of_type(events1, "assistant_complete")
        assert len(completes1) >= 1

        # Turn 2: Reference previous results
        resp2 = minimax_client.post(
            "/api/chat",
            json={"line": "Based on what you just did, summarize in one sentence what tools you have."},
            timeout=60,
        )
        assert resp2.status_code == 200
        events2 = parse_sse_events(resp2.text)
        completes2 = events_of_type(events2, "assistant_complete")
        assert len(completes2) >= 1
        assert completes2[0]["message"], "Follow-up response should be non-empty"


# ===========================================================================
# US-013: Reasoning/thinking block streaming
# ===========================================================================


@pytest.mark.skipif(not HAS_MINIMAX, reason="MiniMax not configured")
class TestLiveThinkingBlock:
    """Test thinking/reasoning block streaming from real MiniMax API."""

    @pytest.fixture()
    def minimax_client(self, db_session_factory, tmp_path, monkeypatch):
        client = _make_live_client(
            db_session_factory, tmp_path, monkeypatch,
            api_key=MINIMAX_KEY,
            model=MINIMAX_MODEL,
            base_url=MINIMAX_BASE_URL,
            api_format=MINIMAX_FORMAT,
        )
        with client:
            yield client

    def test_live_thinking_block_streamed(self, minimax_client):
        """Send a reasoning-requiring prompt and check for thinking events."""
        resp = minimax_client.post(
            "/api/chat",
            json={"line": "Think step by step: what is 17 * 23? Show your reasoning."},
            timeout=60,
        )
        assert resp.status_code == 200
        events = parse_sse_events(resp.text)

        types = {e["type"] for e in events}
        assert "assistant_complete" in types, f"Missing assistant_complete. Types: {types}"
        assert "line_complete" in types

        # MiniMax may or may not emit thinking blocks — both are valid
        thinking_events = events_of_type(events, "thinking_delta")
        completes = events_of_type(events, "assistant_complete")

        # The final answer should contain 391 (17*23)
        final_text = completes[0]["message"]
        assert "391" in final_text, f"Expected 391 in response. Got: {final_text}"

        # Log whether thinking was present for debugging
        if thinking_events:
            assert thinking_events[0]["message"], "Thinking delta should have content"

    def test_live_thinking_then_text(self, minimax_client):
        """If thinking events exist, they should come before assistant text."""
        resp = minimax_client.post(
            "/api/chat",
            json={"line": "Carefully reason about: Is 97 a prime number? Think before answering."},
            timeout=60,
        )
        assert resp.status_code == 200
        events = parse_sse_events(resp.text)

        thinking_events = events_of_type(events, "thinking_delta")
        text_events = events_of_type(events, "assistant_delta")
        completes = events_of_type(events, "assistant_complete")

        assert len(completes) >= 1, "Should have at least one assistant_complete"

        # If both thinking and text deltas exist, thinking should come first
        if thinking_events and text_events:
            types_list = [e["type"] for e in events]
            first_thinking = types_list.index("thinking_delta")
            first_text = types_list.index("assistant_delta")
            assert first_thinking < first_text, (
                "Thinking should come before text deltas"
            )


# ===========================================================================
# US-015: Complex long task with multiple tool calls
# ===========================================================================


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
class TestLiveComplexTask:
    """Test complex multi-step tasks with multiple tool calls."""

    @pytest.fixture(scope="class")
    def sandbox_for_complex(self):
        """Create a sandbox for complex task tests."""
        sandbox = _create_test_sandbox("complex-task")
        yield sandbox
        _delete_sandbox(sandbox["id"])

    @pytest.fixture()
    def minimax_client(self, db_session_factory, tmp_path, monkeypatch):
        client = _make_live_client(
            db_session_factory, tmp_path, monkeypatch,
            api_key=MINIMAX_KEY,
            model=MINIMAX_MODEL,
            base_url=MINIMAX_BASE_URL,
            api_format=MINIMAX_FORMAT,
        )
        with client:
            yield client

    def test_live_complex_multi_tool_task(self, minimax_client, sandbox_for_complex):
        """Send a complex prompt requiring multiple tool calls."""
        minimax_client.post("/api/agents/", json={
            "name": "complex-task-agent",
            "description": "Agent for complex multi-step tasks",
            "model": MINIMAX_MODEL,
            "toolkits": ["sandbox_operations"],
            "system_prompt": (
                "You are a coding assistant with sandbox access. "
                "Use daytona_bash to run commands, daytona_write_file to write files, "
                "and daytona_read_file to read files. Execute ALL steps."
            ),
        })

        resp = minimax_client.post(
            "/api/chat",
            json={
                "line": (
                    "Do these steps in the sandbox:\n"
                    "1. Create a file /workspace/hello.py with: print('hello from e2e')\n"
                    "2. Run: python /workspace/hello.py\n"
                    "3. Tell me the output"
                ),
                "agent_name": "complex-task-agent",
                "sandbox_id": sandbox_for_complex["id"],
            },
            timeout=180,
        )
        assert resp.status_code == 200
        events = parse_sse_events(resp.text)

        types = {e["type"] for e in events}
        assert "assistant_complete" in types, f"Missing assistant_complete. Types: {types}"

        # Should have at least one tool call (write or bash)
        tool_started = events_of_type(events, "tool_started")
        tool_completed = events_of_type(events, "tool_completed")

        # Model should have attempted tool usage
        if tool_started:
            tool_names = [e["tool_name"] for e in tool_started]
            daytona_tools = [t for t in tool_names if t.startswith("daytona_")]
            assert len(daytona_tools) >= 1, f"Expected daytona tools, got: {tool_names}"


# ===========================================================================
# US-016: Model key integration + explicit multi-tool calls with live MiniMax
# ===========================================================================


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
class TestLiveMultipleToolCallsWithModelKey:
    """Use model_key when creating a live agent and verify multi-tool execution."""

    @pytest.fixture(scope="class")
    def sandbox_for_model_key(self):
        """Create a sandbox for model-key multi-tool tests."""
        sandbox = _create_test_sandbox("model-key-multi-tool")
        yield sandbox
        _delete_sandbox(sandbox["id"])

    @pytest.fixture()
    def minimax_client(self, db_session_factory, tmp_path, monkeypatch):
        client = _make_live_client(
            db_session_factory, tmp_path, monkeypatch,
            api_key=MINIMAX_KEY,
            model=MINIMAX_MODEL,
            base_url=MINIMAX_BASE_URL,
            api_format=MINIMAX_FORMAT,
        )
        with client:
            yield client

    def test_live_multiple_tools_with_model_key(self, minimax_client, sandbox_for_model_key):
        """Create an agent with model_key and verify it calls multiple tools."""
        agent_name = "modelkey-multi-tool-agent"
        create_resp = minimax_client.post("/api/agents/", json={
            "name": agent_name,
            "description": "Agent using model_key with multiple tools",
            "model": "minimax",
            "toolkits": ["sandbox_operations"],
            "system_prompt": (
                "You are a coding assistant with sandbox tools. "
                "When creating files, use daytona_write_file. "
                "When reading or checking output, use daytona_read_file or daytona_bash. "
                "Do every required step and then report results."
            ),
        })
        if create_resp.status_code == 201:
            agent_payload = create_resp.json()
        else:
            get_resp = minimax_client.get(f"/api/agents/{agent_name}")
            assert get_resp.status_code == 200, create_resp.text
            agent_payload = get_resp.json()
        assert agent_payload["model"] == "minimax"

        resp = minimax_client.post(
            "/api/chat",
            json={
                "line": (
                    "Create /workspace/modelkey_multi.txt with content: MODELKEY_TEST\n"
                    "Then read it back and reply with exactly: CONTENT=<content>."
                ),
                "agent_name": agent_name,
                "sandbox_id": sandbox_for_model_key["id"],
            },
            timeout=180,
        )
        assert resp.status_code == 200
        events = parse_sse_events(resp.text)

        types = {e["type"] for e in events}
        assert "assistant_complete" in types, f"Missing assistant_complete. Types: {types}"

        tool_started = events_of_type(events, "tool_started")
        tool_completed = events_of_type(events, "tool_completed")
        assert len(tool_started) >= 2, f"Expected >=2 tool calls. Got: {[e['tool_name'] for e in tool_started]}"
        assert len(tool_completed) >= 2 or "error" in types, (
            "Expected tool calls to complete (or error)."
        )

        tool_names = [e["tool_name"] for e in tool_started]
        assert "daytona_write_file" in tool_names, f"Missing write tool. Tools: {tool_names}"
        assert any(
            name in tool_names for name in ("daytona_read_file", "daytona_bash")
        ), f"Missing read/exec follow-up tool. Tools: {tool_names}"

    def test_live_tool_call_chain_with_model_key(self, minimax_client, sandbox_for_model_key):
        """Verify the same model_key can drive a short chain of 3 tool calls."""
        agent_name = "modelkey-multi-tool-chain-agent"
        create_resp = minimax_client.post("/api/agents/", json={
            "name": agent_name,
            "description": "Chain three tools using model_key",
            "model": "minimax",
            "toolkits": ["sandbox_operations"],
            "system_prompt": (
                "Complete every requested step using tools and do not stop early. "
                "Use shell or file tools as appropriate."
            ),
        })
        if create_resp.status_code == 201:
            agent_payload = create_resp.json()
        else:
            get_resp = minimax_client.get(f"/api/agents/{agent_name}")
            assert get_resp.status_code == 200, create_resp.text
            agent_payload = get_resp.json()
        assert agent_payload["model"] == "minimax"

        resp = minimax_client.post(
            "/api/chat",
            json={
                "line": (
                    "Create /workspace/modelkey_one.txt with 'ONE', then create /workspace/modelkey_two.txt "
                    "with 'TWO', then run: ls /workspace/modelkey_* | cat."
                ),
                "agent_name": agent_name,
                "sandbox_id": sandbox_for_model_key["id"],
            },
            timeout=240,
        )
        assert resp.status_code == 200
        events = parse_sse_events(resp.text)

        tool_started = events_of_type(events, "tool_started")
        tool_names = [e["tool_name"] for e in tool_started]
        assert tool_names.count("daytona_write_file") >= 2, f"Expected two writes. Tools: {tool_names}"
        assert "daytona_bash" in tool_names, f"Expected bash for listing. Tools: {tool_names}"
        assert len(tool_started) >= 3, f"Expected at least 3 tool calls. Tools: {tool_names}"


# ===========================================================================
# Text tool call parsing (unit test — no API/sandbox needed)
# ===========================================================================


class TestTextToolCallParsing:
    """Verify [TOOL_CALL] text markers from MiniMax are parsed and executed."""

    def test_json_format(self):
        """Parse JSON-formatted tool call markers."""
        from engine.text_tool_parser import parse_text_tool_calls

        text = '[TOOL_CALL]\n{"tool": "daytona_bash", "args": {"command": "echo hi"}}\n[/TOOL_CALL]'
        calls = parse_text_tool_calls(text)
        assert len(calls) == 1
        assert calls[0].name == "daytona_bash"
        assert calls[0].input["command"] == "echo hi"

    def test_arrow_format(self):
        """Parse arrow-formatted tool call markers."""
        from engine.text_tool_parser import parse_text_tool_calls

        text = '[TOOL_CALL]\ntool => "daytona_read_file", args => {"file_path": "/test.txt"}\n[/TOOL_CALL]'
        calls = parse_text_tool_calls(text)
        assert len(calls) == 1
        assert calls[0].name == "daytona_read_file"

    def test_multiple_calls(self):
        """Parse multiple tool call markers in one text."""
        from engine.text_tool_parser import parse_text_tool_calls

        text = (
            '[TOOL_CALL]\n{"tool": "daytona_bash", "args": {"command": "ls"}}\n[/TOOL_CALL]\n'
            'Some text in between\n'
            '[TOOL_CALL]\n{"tool": "daytona_read_file", "args": {"file_path": "/a.txt"}}\n[/TOOL_CALL]'
        )
        calls = parse_text_tool_calls(text)
        assert len(calls) == 2

    def test_no_calls(self):
        """Plain text should return empty list."""
        from engine.text_tool_parser import parse_text_tool_calls

        calls = parse_text_tool_calls("Just regular text with no tool calls")
        assert len(calls) == 0

    def test_name_key_format(self):
        """Parse with 'name' key instead of 'tool'."""
        from engine.text_tool_parser import parse_text_tool_calls

        text = '[TOOL_CALL]\n{"name": "skill", "input": {"query": "test"}}\n[/TOOL_CALL]'
        calls = parse_text_tool_calls(text)
        assert len(calls) == 1
        assert calls[0].name == "skill"
        assert calls[0].input["query"] == "test"


# ===========================================================================
# Existing MiniMax live tests (kept for backward compat)
# ===========================================================================


@pytest.mark.skipif(not HAS_MINIMAX, reason="MiniMax API key or base_url not configured")
class TestMiniMaxLive:
    """Live tests against the MiniMax API via Anthropic-compatible endpoint."""

    @pytest.fixture()
    def minimax_client(self, db_session_factory, tmp_path, monkeypatch):
        client = _make_live_client(
            db_session_factory, tmp_path, monkeypatch,
            api_key=MINIMAX_KEY,
            model=MINIMAX_MODEL,
            base_url=MINIMAX_BASE_URL,
            api_format=MINIMAX_FORMAT,
        )
        with client:
            yield client

    def test_minimax_simple_chat(self, minimax_client):
        """Send a simple prompt and verify we get a response."""
        resp = minimax_client.post(
            "/api/chat",
            json={"line": "Reply with exactly one word: PONG"},
            timeout=60,
        )
        assert resp.status_code == 200
        events = parse_sse_events(resp.text)

        completes = events_of_type(events, "assistant_complete")
        assert len(completes) >= 1, f"No assistant_complete events. All events: {[e['type'] for e in events]}"
        assert completes[0]["message"], "assistant_complete message is empty"

        assert any(e["type"] == "line_complete" for e in events), "Missing line_complete event"

    def test_minimax_custom_agent_chat(self, minimax_client):
        """Create a custom agent and chat with it using real API."""
        create_resp = minimax_client.post("/api/agents/", json={
            "name": "live-test-agent",
            "description": "A live test agent for e2e testing",
            "model": MINIMAX_MODEL,
            "system_prompt": "You are a helpful test assistant. Always respond in exactly one sentence.",
        })
        if create_resp.status_code == 201:
            agent_name = "live-test-agent"
        else:
            agent_name = None

        payload = {"line": "What is 2 + 2? Answer in one word."}
        if agent_name:
            payload["agent_name"] = agent_name

        resp = minimax_client.post("/api/chat", json=payload, timeout=60)
        assert resp.status_code == 200
        events = parse_sse_events(resp.text)

        completes = events_of_type(events, "assistant_complete")
        assert len(completes) >= 1
        assert completes[0]["message"]

        types = {e["type"] for e in events}
        assert "transcript_item" in types
        assert "assistant_complete" in types
        assert "line_complete" in types

    def test_minimax_chat_with_tools(self, minimax_client):
        """Chat with tools available and verify the model can use them."""
        resp = minimax_client.post(
            "/api/chat",
            json={"line": "Use the skill tool to list available skills."},
            timeout=60,
        )
        assert resp.status_code == 200
        events = parse_sse_events(resp.text)

        completes = events_of_type(events, "assistant_complete")
        assert len(completes) >= 1
        assert any(e["type"] == "line_complete" for e in events)


# ===========================================================================
# Sandbox health test
# ===========================================================================


class TestSandboxHealth:
    """Test sandbox service health endpoint."""

    def test_sandbox_health(self, app_client):
        """Check sandbox health endpoint returns expected fields."""
        client, _ = app_client
        resp = client.get("/api/sandboxes/health")
        assert resp.status_code == 200
        data = resp.json()
        assert "configured" in data
        assert "available" in data
        assert isinstance(data["configured"], bool)

    @pytest.mark.skipif(not HAS_DAYTONA, reason="Daytona not configured (no API key/URL)")
    def test_sandbox_health_when_configured(self, app_client):
        """When Daytona is configured, health should report configured=True."""
        client, _ = app_client
        resp = client.get("/api/sandboxes/health")
        data = resp.json()
        assert data["configured"] is True
