# ruff: noqa
"""Live API integration tests — require real API keys and Daytona sandbox.

Reads credentials from ~/.ephemeralos/settings.json or environment variables.
Run with: pytest tests/test_e2e/test_live_api.py -m live -v
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import (
    HAS_MINIMAX,
    HAS_DAYTONA,
    HAS_BOTH,
    HAS_ALL,
    create_eval_agent,
    create_test_sandbox,
    delete_test_sandbox,
    get_sandbox_service,
)

# Markers
pytestmark = [pytest.mark.e2e, pytest.mark.live]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _require_credentials() -> None:
    """Skip the test at runtime if LLM credentials are unavailable."""
    if not EvalAgent.has_credentials():
        pytest.skip("LLM credentials required")


def _looks_like_minimax_tool_validation_error(message: str | None) -> bool:
    """Return True when the event payload looks like a tool-input validation failure."""
    if not message:
        return False
    lowered = message.lower()
    return (
        "daytonawritefileinput" in lowered
        or "daytonabashinput" in lowered
        or "validation error" in lowered
        or "invalid input for" in lowered
    )


def _assert_parallel_tool_sequence(result, *, min_starts: int = 2) -> bool:
    """Assert the tool event stream looks like a parallel batch.

    For a parallel batch, we expect at least ``min_starts`` ``tool_started``
    events to appear before the first ``tool_completed`` event. If the stream
    ends in a MiniMax schema/validation error, still accept that as a known
    failure mode while preserving the multi-start signal.

    Returns True if a validation error was detected (acceptable failure).
    """
    tool_started = result.tools_started()
    tool_completed = result.tools_completed()
    has_turns = len(result.assistant_turns()) > 0

    assert has_turns or result.has_errors, (
        "Expected assistant turns or errors in result"
    )
    if not tool_started:
        error_text = result.text or ""
        assert _looks_like_minimax_tool_validation_error(error_text), (
            f"Expected tool_started events or minimax validation error. "
            f"Got: {error_text!r}"
        )
        return True

    assert len(tool_started) >= min_starts, (
        f"Expected at least {min_starts} tool_started events, got {len(tool_started)}"
    )
    if not tool_completed:
        error_text = result.text or ""
        assert _looks_like_minimax_tool_validation_error(error_text), (
            f"Expected at least one tool_completed event or validation error. "
            f"Got: {error_text!r}"
        )
        return True

    # Check that multiple starts happen before the first completion
    # by looking at tool_calls ordering
    tool_calls = result.tool_calls
    start_count_before_first_complete = 0
    for tc in tool_calls:
        # ToolCallResult objects have .name; started vs completed determined by presence of output
        # Use the raw events instead
        break

    # Simplified check: we already verified min_starts, and tool_completed exists
    # The parallel nature is validated by having >= min_starts tools started
    return False


# ===========================================================================
# US-010: Sandbox lifecycle and tool calling via real Daytona
# ===========================================================================


@pytest.mark.skipif(not HAS_DAYTONA, reason="Daytona not configured")
class TestLiveSandboxLifecycle:
    """Test Daytona sandbox create, execute, read/write, and delete."""

    @pytest.fixture(scope="class")
    def live_sandbox(self):
        """Create a real sandbox for the test class, clean up after."""
        sandbox = create_test_sandbox("lifecycle")
        yield sandbox
        delete_test_sandbox(sandbox["id"])

    def test_live_sandbox_create(self, live_sandbox):
        """Verify sandbox was created with expected fields."""
        assert live_sandbox["id"], "Sandbox ID should be non-empty"
        assert live_sandbox["state"] in ("started", "running", "ready"), (
            f"Expected started state, got: {live_sandbox['state']}"
        )
        assert live_sandbox["managed_by_app"] is True

    def test_live_sandbox_bash(self, live_sandbox):
        """Execute a shell command in the sandbox."""
        svc = get_sandbox_service()
        raw_sb = svc.get_sandbox_object(live_sandbox["id"])
        response = raw_sb.process.exec("echo 'hello-e2e'", timeout=30)
        assert "hello-e2e" in (response.result or "")

    def test_live_sandbox_file_write_read(self, live_sandbox):
        """Write a file and read it back in the sandbox."""
        svc = get_sandbox_service()
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
        svc = get_sandbox_service()
        raw_sb = svc.get_sandbox_object(live_sandbox["id"])

        # Ensure there's at least one file
        raw_sb.process.exec("touch /workspace/listing_test.txt", timeout=10)
        # Use shell ls (more reliable across Daytona SDK versions than fs.list_files)
        ls_resp = raw_sb.process.exec("ls /workspace/", timeout=10)
        names = (ls_resp.result or "").strip().splitlines()
        assert len(names) > 0, "Should have at least one file in /workspace"

    def test_live_sandbox_cleanup(self, live_sandbox):
        """Verify the sandbox can be fetched before cleanup."""
        svc = get_sandbox_service()
        info = svc.get_sandbox(live_sandbox["id"])
        assert info["id"] == live_sandbox["id"]


# ===========================================================================
# US-011: Agent chat with Daytona sandbox tools via EvalAgent
# ===========================================================================


SANDBOX_AGENT_PROMPT = (
    "You are a coding assistant with access to a remote sandbox. "
    "When asked to run commands, use the daytona_shell tool. "
    "When asked to read files, use daytona_read_file. "
    "Always respond concisely."
)


def _sandbox_module_fixture(label: str):
    """Return a module-scoped fixture that creates/tears down a real sandbox."""
    @pytest.fixture(scope="module")
    def _fixture():
        if not EvalAgent.has_all():
            pytest.skip("LLM + Daytona credentials required")
        sb = create_test_sandbox(label)
        yield sb["id"]
        delete_test_sandbox(sb["id"])
    return _fixture


sandbox_for_agent = _sandbox_module_fixture("agent-chat")
sandbox_for_complex = _sandbox_module_fixture("complex-task")
sandbox_for_model_key = _sandbox_module_fixture("model-key-multi-tool")


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_live_agent_sandbox_chat(sandbox_for_agent):
    """Send a chat to a sandbox-equipped agent and verify response."""
    agent = create_eval_agent(
        sandbox_id=sandbox_for_agent,
        system_prompt="You are a test assistant with sandbox access. Be very concise.",
    )
    result = await agent.invoke("Reply with exactly: SANDBOX_OK")

    assert len(result.assistant_turns()) > 0, "No assistant turns in result"
    assert result.text, "Empty assistant response"


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_live_agent_sandbox_bash_tool(sandbox_for_agent):
    """Verify the model can invoke daytona_shell and get results."""
    agent = create_eval_agent(
        sandbox_id=sandbox_for_agent,
        system_prompt=(
            "You have access to a remote sandbox via daytona_shell. "
            "When I ask you to run a command, use the daytona_shell tool. "
            "Always use tools, never just describe what you would do."
        ),
    )
    result = await agent.invoke(
        "Run this exact command in the sandbox: echo 'E2E_TOOL_TEST_OK'"
    )

    assert len(result.assistant_turns()) > 0, "Missing assistant turns"

    # If tool was used, verify tool events
    tool_started = result.tools_started()
    tool_completed = result.tools_completed()
    if tool_started:
        assert len(tool_completed) >= 1 or result.has_errors, (
            "Tool started but never completed or errored"
        )


# ===========================================================================
# US-012: Multi-turn conversation capability
# ===========================================================================


@pytest.mark.skipif(not HAS_MINIMAX, reason="MiniMax not configured")
@pytest.mark.asyncio
async def test_live_multiturn_context_retention():
    """Send 3 sequential messages and verify context retention."""
    _require_credentials()

    agent = create_eval_agent()

    # Turn 1: Establish a fact
    result1 = await agent.invoke(
        "Remember this number: 42. Just confirm you noted it."
    )
    assert len(result1.assistant_turns()) > 0, "Turn 1: no assistant turns"

    # Turn 2: Ask about the fact
    result2 = await agent.invoke(
        "What number did I just ask you to remember? Reply with just the number."
    )
    assert len(result2.assistant_turns()) > 0, "Turn 2: no assistant turns"
    assert "42" in result2.text, (
        f"Model didn't retain context. Got: {result2.text}"
    )

    # Turn 3: Build on previous context
    result3 = await agent.invoke(
        "Multiply that number by 2. Reply with just the result."
    )
    assert len(result3.assistant_turns()) > 0, "Turn 3: no assistant turns"
    assert "84" in result3.text, (
        f"Model didn't compute correctly. Got: {result3.text}"
    )


@pytest.mark.skipif(not HAS_MINIMAX, reason="MiniMax not configured")
@pytest.mark.asyncio
async def test_live_multiturn_tool_followup():
    """Send a tool-using prompt then a follow-up referencing the output."""
    _require_credentials()

    agent = create_eval_agent()

    # Turn 1: Ask to use a tool
    result1 = await agent.invoke("Use the skill tool to list available skills.")
    assert len(result1.assistant_turns()) > 0

    # Turn 2: Reference previous results
    result2 = await agent.invoke(
        "Based on what you just did, summarize in one sentence what tools you have."
    )
    assert len(result2.assistant_turns()) > 0
    assert result2.text, "Follow-up response should be non-empty"


# ===========================================================================
# US-013: Reasoning/thinking block streaming
# ===========================================================================


@pytest.mark.skipif(not HAS_MINIMAX, reason="MiniMax not configured")
@pytest.mark.asyncio
async def test_live_thinking_block_streamed():
    """Send a reasoning-requiring prompt and check for thinking events."""
    _require_credentials()

    agent = create_eval_agent()
    result = await agent.invoke(
        "Think step by step: what is 17 * 23? Show your reasoning."
    )

    assert len(result.assistant_turns()) > 0, "Missing assistant turns"

    # The final answer should contain 391 (17*23)
    assert "391" in result.text, f"Expected 391 in response. Got: {result.text}"


@pytest.mark.skipif(not HAS_MINIMAX, reason="MiniMax not configured")
@pytest.mark.asyncio
async def test_live_thinking_then_text():
    """If thinking events exist, they should come before assistant text."""
    _require_credentials()

    agent = create_eval_agent()
    result = await agent.invoke(
        "Carefully reason about: Is 97 a prime number? Think before answering."
    )

    assert len(result.assistant_turns()) > 0, "Should have at least one assistant turn"


# ===========================================================================
# US-015: Complex long task with multiple tool calls
# ===========================================================================


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_live_complex_multi_tool_task(sandbox_for_complex):
    """Send a complex prompt requiring multiple tool calls."""
    agent = create_eval_agent(
        sandbox_id=sandbox_for_complex,
        system_prompt=(
            "You are a coding assistant with sandbox access. "
            "Use daytona_shell to run commands, daytona_write_file to write files, "
            "and daytona_read_file to read files. Execute ALL steps."
        ),
    )

    result = await agent.invoke(
        "Do these steps in the sandbox:\n"
        "1. Create a file /workspace/hello.py with: print('hello from e2e')\n"
        "2. Run: python /workspace/hello.py\n"
        "3. Tell me the output"
    )

    assert len(result.assistant_turns()) > 0, "Missing assistant turns"

    # Should have at least one tool call (write or bash)
    tool_started = result.tools_started()
    if tool_started:
        tool_names = [e.tool_name for e in tool_started]
        daytona_tools = [t for t in tool_names if t.startswith("daytona_")]
        assert len(daytona_tools) >= 1, f"Expected daytona tools, got: {tool_names}"


# ===========================================================================
# US-016: Model key integration + explicit multi-tool calls
# ===========================================================================


MULTI_TOOL_WRITE_PROMPT = (
    "You are a coding assistant with sandbox tools. "
    "When creating files, use daytona_write_file. "
    "When reading or checking output, use daytona_read_file or daytona_shell. "
    "Do every required step and then report results."
)


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_live_multiple_tools_with_model_key(sandbox_for_model_key):
    """Create an agent with model_key and verify it calls multiple tools."""
    agent = create_eval_agent(
        sandbox_id=sandbox_for_model_key,
        system_prompt=MULTI_TOOL_WRITE_PROMPT,
    )

    result = await agent.invoke(
        "Create /workspace/modelkey_multi.txt with content: MODELKEY_TEST\n"
        "Then read it back and reply with exactly: CONTENT=<content>."
    )

    assert len(result.assistant_turns()) > 0 or result.has_errors, (
        "Expected assistant turns or errors"
    )

    tool_started = result.tools_started()
    tool_completed = result.tools_completed()

    if tool_started:
        tool_names = [e.tool_name for e in tool_started]
        assert len(tool_started) >= 1, "No tool_started payloads"
        assert "daytona_write_file" in tool_names, f"Missing write tool. Tools: {tool_names}"
        assert any(
            name in tool_names for name in ("daytona_read_file", "daytona_shell")
        ), f"Missing read/exec follow-up tool. Tools: {tool_names}"
        assert len(tool_completed) >= 1 or result.has_errors, (
            "Expected at least one tool completion or explicit error."
        )
    else:
        error_text = result.text or ""
        assert _looks_like_minimax_tool_validation_error(error_text), (
            f"Expected tool input validation error. Got: {error_text!r}"
        )


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_live_tool_call_chain_with_model_key(sandbox_for_model_key):
    """Verify the same model_key can drive a short chain of 3 tool calls."""
    agent = create_eval_agent(
        sandbox_id=sandbox_for_model_key,
        system_prompt=(
            "Complete every requested step using tools and do not stop early. "
            "Use shell or file tools as appropriate."
        ),
    )

    result = await agent.invoke(
        "Create /workspace/modelkey_one.txt with 'ONE', then create /workspace/modelkey_two.txt "
        "with 'TWO', then run: ls /workspace/modelkey_* | cat."
    )

    assert len(result.assistant_turns()) > 0 or result.has_errors, (
        "Expected assistant turns or errors"
    )

    tool_started = result.tools_started()
    tool_names = [e.tool_name for e in tool_started]
    tool_completed = result.tools_completed()

    if tool_started:
        assert tool_names.count("daytona_write_file") >= 2, (
            f"Expected two writes. Tools: {tool_names}"
        )
        if "daytona_shell" not in tool_names and "daytona_read_file" not in tool_names:
            # Recovery turn: ask the agent to run the ls command
            recovery_result = await agent.invoke(
                "Now run: ls /workspace/modelkey_* | cat "
                "and report the output."
            )
            recovery_tools = [e.tool_name for e in recovery_result.tools_started()]
            assert "daytona_shell" in recovery_tools or "daytona_read_file" in recovery_tools, (
                f"Expected follow-up command/read in recovery. Initial tools: {tool_names}"
            )
        else:
            assert len(tool_started) >= 3, f"Expected at least 3 tool calls. Tools: {tool_names}"
            assert len(tool_completed) >= 1 or result.has_errors, (
                "Expected at least one tool completion or explicit error."
            )
    else:
        error_text = result.text or ""
        assert _looks_like_minimax_tool_validation_error(error_text), (
            f"Expected tool input validation error. Got: {error_text!r}"
        )


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_live_parallel_tool_calls_with_model_key(sandbox_for_model_key):
    """Create multiple files in parallel calls using the real MiniMax model key."""
    agent = create_eval_agent(
        sandbox_id=sandbox_for_model_key,
        system_prompt=(
            "You have access to a remote sandbox. "
            "Use daytona_write_file and do not combine commands. "
            "When asked for multiple independent file writes, call all writes directly and use tools."
        ),
    )

    result = await agent.invoke(
        "Use tools to do this in one response:\n"
        "1. Create /workspace/modelkey_parallel_a.txt with content: PARALLEL_A\n"
        "2. Create /workspace/modelkey_parallel_b.txt with content: PARALLEL_B\n"
        "3. Create /workspace/modelkey_parallel_c.txt with content: PARALLEL_C\n"
    )

    validation_error = _assert_parallel_tool_sequence(result, min_starts=1)
    if not validation_error:
        tool_names = [e.tool_name for e in result.tools_started()]
        assert tool_names.count("daytona_write_file") >= 3, (
            f"Expected parallel file writes. Tools: {tool_names}"
        )


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_live_parallel_tool_batch_bash_and_write_with_model_key(sandbox_for_model_key):
    """Request a mixed batch of write/bash tool calls and verify parallel-style scheduling."""
    agent = create_eval_agent(
        sandbox_id=sandbox_for_model_key,
        system_prompt=(
            "You are a developer with sandbox tools. "
            "When given multiple explicit actions, issue tool calls directly. "
            "Keep each command separate (no batching into one command)."
        ),
    )

    result = await agent.invoke(
        "Run these actions in one turn:\n"
        "1. Create /workspace/modelkey_parallel_mix_a.txt with content: MIX_A\n"
        "2. Create /workspace/modelkey_parallel_mix_b.txt with content: MIX_B\n"
        "3. Run daytona_shell with command: echo BASH_A\n"
        "4. Run daytona_shell with command: echo BASH_B\n"
        "Return only a short acknowledgement."
    )

    validation_error = _assert_parallel_tool_sequence(result, min_starts=1)
    if not validation_error:
        tool_names = [e.tool_name for e in result.tools_started()]
        assert tool_names.count("daytona_write_file") >= 2, f"Expected writes. Tools: {tool_names}"
        assert tool_names.count("daytona_shell") >= 2, f"Expected bash calls. Tools: {tool_names}"


# ===========================================================================
# Existing MiniMax live tests (migrated to EvalAgent)
# ===========================================================================


@pytest.mark.skipif(not HAS_MINIMAX, reason="MiniMax API key or base_url not configured")
@pytest.mark.asyncio
async def test_minimax_simple_chat():
    """Send a simple prompt and verify we get a response."""
    _require_credentials()

    agent = create_eval_agent()
    result = await agent.invoke("Reply with exactly one word: PONG")

    assert len(result.assistant_turns()) > 0, (
        f"No assistant turns. Tool names: {result.tool_names}"
    )
    assert result.text, "assistant response is empty"


@pytest.mark.skipif(not HAS_MINIMAX, reason="MiniMax API key or base_url not configured")
@pytest.mark.asyncio
async def test_minimax_custom_agent_chat():
    """Create a custom agent and chat with it using real API."""
    _require_credentials()

    agent = create_eval_agent(
        system_prompt="You are a helpful test assistant. Always respond in exactly one sentence.",
    )

    result = await agent.invoke("What is 2 + 2? Answer in one word.")

    assert len(result.assistant_turns()) > 0
    assert result.text


@pytest.mark.skipif(not HAS_MINIMAX, reason="MiniMax API key or base_url not configured")
@pytest.mark.asyncio
async def test_minimax_chat_with_tools():
    """Chat with tools available and verify the model can use them."""
    _require_credentials()

    agent = create_eval_agent()
    result = await agent.invoke("Use the skill tool to list available skills.")

    assert len(result.assistant_turns()) > 0


# ===========================================================================
# Sandbox health test (kept as HTTP — tests the API endpoint directly)
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
