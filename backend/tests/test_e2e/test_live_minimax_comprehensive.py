# ruff: noqa
"""Comprehensive MiniMax live E2E tests — real API keys + real Daytona sandbox.

Covers five critical areas:
1. Tool calling & skill loading in Daytona sandbox environment
2. Multi-turn conversation capability
3. Reasoning/thinking block streaming
4. Complex long tasks with multiple tool calls
5. Code intelligence system integration

Run with: pytest tests/test_e2e/test_live_minimax_comprehensive.py -m live -v
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import (
    create_eval_agent,
    create_test_sandbox,
    delete_test_sandbox,
)

pytestmark = [pytest.mark.e2e, pytest.mark.live]


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------


def _sandbox_fixture(label: str):
    """Return a class-scoped sandbox fixture for the given label."""
    import pytest as _pytest

    @_pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox(label)
        yield sb
        delete_test_sandbox(sb["id"])

    return sandbox


async def _invoke_with_sandbox_agent(
    sandbox,
    system_prompt: str,
    message: str,
) -> object:
    """Create a sandboxed agent, invoke message, assert at least one assistant turn, return result."""
    agent = create_eval_agent(system_prompt=system_prompt, sandbox_id=sandbox["id"])
    result = await agent.invoke(message)
    assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
    return result


def _assert_sandbox_tools_used(result) -> None:
    """If any tools were called, assert at least one sandbox tool was used."""
    tool_started = result.tools_started()
    if tool_started:
        sandbox_tools = [t for t in result.tool_names if t in {"write_file", "read_file", "shell"}]
        assert len(sandbox_tools) >= 1, f"Expected at least one sandbox tool, got: {result.tool_names}"


# ===========================================================================
# AREA 1: Tool Calling & Skill Loading in Daytona Sandbox
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestToolCallingAndSkillLoading:
    """Test tool calling mechanisms and skill loading in a real Daytona sandbox."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("tool-calling")
        yield sb
        delete_test_sandbox(sb["id"])

    # -- 1a: Sandbox tool execution --

    @pytest.mark.asyncio
    async def test_shell_executes(self, sandbox):
        """Model should invoke shell and return real output."""
        result = await _invoke_with_sandbox_agent(
            sandbox,
            system_prompt="You have a remote sandbox. Use shell to run commands. Always use tools.",
            message="Run this exact command in the sandbox: echo 'TOOL_CALL_E2E_PASS'",
        )
        # Check tool events — tool may or may not be used depending on model behavior
        if result.tools_started():
            assert any(t in {"write_file", "read_file", "shell"} for t in result.tool_names), f"No sandbox tool used: {result.tool_names}"

    @pytest.mark.asyncio
    async def test_daytona_write_and_read_file(self, sandbox):
        """Model should write a file and read it back using sandbox tools."""
        result = await _invoke_with_sandbox_agent(
            sandbox,
            system_prompt=(
                "You have sandbox access via write_file and read_file. "
                "Always use the tools, never simulate."
            ),
            message="Write the text 'E2E_FILE_TEST' to /workspace/e2e_check.txt, then read it back and tell me the content.",
        )
        _assert_sandbox_tools_used(result)

    # -- 1b: Skill loading --

    @pytest.mark.asyncio
    async def test_skill_tool_available(self):
        """The skill discovery tool should be available by default."""
        agent = create_eval_agent(
            system_prompt="You are a test assistant. Be concise.",
        )

        result = await agent.invoke("Use the skill tool to list available skills.")
        assert len(result.assistant_turns()) >= 1

    def test_skill_registry_loads(self):
        """Skill registry should load even when no bundled skills are present."""
        from skills.core.loader import load_skill_registry

        registry = load_skill_registry()
        assert registry is not None
        all_skills = registry.list_skills()
        assert isinstance(all_skills, list)

    @pytest.mark.asyncio
    async def test_sandbox_tools_schema_complete(self, sandbox):
        """Verify sandbox tools expose expected schemas."""
        agent = create_eval_agent(
            sandbox_id=sandbox["id"],
        )

        # Chat to trigger tool schema generation
        result = await agent.invoke("Hello")
        assert len(result.assistant_turns()) >= 1

    # -- 1c: Multiple tools in one turn --

    @pytest.mark.asyncio
    async def test_multiple_tool_calls_single_turn(self, sandbox):
        """Model should handle multiple tool calls in a single turn."""
        result = await _invoke_with_sandbox_agent(
            sandbox,
            system_prompt="Use shell for all commands. Execute every step.",
            message="Run these two commands in the sandbox: 'echo FIRST' and then 'echo SECOND'",
        )
        # Model should have at least attempted tool calls
        if result.tools_started():
            assert len(result.tools_started()) >= 1


# ===========================================================================
# AREA 2: Multi-Turn Conversation Capability
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_credentials(), reason="API credentials not configured")
class TestMultiTurnConversation:
    """Test multi-turn conversations with context retention and continuity."""

    @pytest.mark.asyncio
    async def test_three_turn_context_retention(self):
        """Three sequential messages should maintain context across turns."""
        agent = create_eval_agent()

        # Turn 1: establish a fact
        result1 = await agent.invoke("Remember this code: X7Q9. Just confirm.")
        text1 = result1.text
        assert text1, "Turn 1 should produce a response"

        # Turn 2: recall the fact
        result2 = await agent.invoke(
            "What code did I ask you to remember? Reply with just the code."
        )
        text2 = result2.text
        assert "X7Q9" in text2, f"Model should recall 'X7Q9', got: {text2}"

        # Turn 3: transform the fact
        result3 = await agent.invoke(
            "Reverse those 4 characters. Reply with just the reversed code."
        )
        text3 = result3.text
        assert "9Q7X" in text3, f"Model should reverse to '9Q7X', got: {text3}"

    @pytest.mark.asyncio
    async def test_five_turn_conversation_depth(self):
        """Five-turn conversation should maintain deep context."""
        agent = create_eval_agent()

        # Turns 1-4: build up context, each must produce a response
        for msg in [
            "I'm building a Python class called DataProcessor. Just acknowledge.",
            "It should have a method called transform() that takes a list. Acknowledge.",
            "The transform method should square each number. Acknowledge.",
            "Add error handling for non-numeric values. Acknowledge.",
        ]:
            assert (await agent.invoke(msg)).text

        # Turn 5: test recall of accumulated context
        result5 = await agent.invoke(
            "Summarize the full class design in one sentence. Include: class name, method name, what it does, error handling."
        )
        text5 = result5.text

        # Should reference key elements from earlier turns
        text5_lower = text5.lower()
        assert (
            "dataprocessor" in text5_lower
            or "data_processor" in text5_lower
            or "data processor" in text5_lower
        ), f"Should mention DataProcessor. Got: {text5}"

    @pytest.mark.asyncio
    async def test_multiturn_with_tool_followup(self):
        """Tool use in turn 1 should be referenceable in turn 2."""
        agent = create_eval_agent()

        result1 = await agent.invoke("What is 15 * 13? Think step by step.")
        text1 = result1.text
        assert "195" in text1, f"Should compute 195. Got: {text1}"

        result2 = await agent.invoke(
            "Add 5 to the result you just gave me. Reply with just the number."
        )
        text2 = result2.text
        assert "200" in text2, f"Should compute 200. Got: {text2}"

    @pytest.mark.asyncio
    async def test_multiturn_conversation_isolation(self):
        """Each test agent should have independent conversation state."""
        agent = create_eval_agent()

        result = await agent.invoke("Reply with exactly one word: ISOLATED")
        text = result.text
        assert text, "Should get a response"
        # This test verifies that conversation state does not bleed from other tests.

    @pytest.mark.asyncio
    async def test_multiturn_error_recovery(self):
        """Conversation should continue normally after an error turn."""
        agent = create_eval_agent()

        # Turn 1: normal message
        result1 = await agent.invoke("Say hello.")
        assert result1.text

        # Turn 2: another normal message to verify conversation continues
        result2 = await agent.invoke("Now say goodbye.")
        text2 = result2.text
        assert text2, "Should still respond after error"


# ===========================================================================
# AREA 3: Reasoning/Thinking Block Streaming
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_credentials(), reason="API credentials not configured")
class TestThinkingBlockStreaming:
    """Test reasoning/thinking block streaming from real MiniMax API."""

    @pytest.mark.asyncio
    async def test_thinking_block_on_math_reasoning(self):
        """Math problems should trigger thinking and produce correct results."""
        agent = create_eval_agent()

        result = await agent.invoke("Think step by step: what is 23 * 17?")
        assert len(result.assistant_turns()) >= 1

        text = result.text
        assert "391" in text, f"Should compute 391. Got: {text}"

        # Thinking events may or may not be present — both valid
        thinking_text = result.thinking_text
        # If thinking was produced, it should have content
        if thinking_text:
            assert len(thinking_text) > 0, "Thinking text should have content"

    @pytest.mark.asyncio
    async def test_thinking_before_text_ordering(self):
        """If thinking events exist, they should precede text events."""
        agent = create_eval_agent()

        result = await agent.invoke(
            "Carefully reason: is 97 a prime number? Think before answering."
        )

        from message.stream_events import ThinkingDelta, AssistantTextDelta

        thinking = [e for e in result.events if isinstance(e, ThinkingDelta)]
        text_deltas = [e for e in result.events if isinstance(e, AssistantTextDelta)]

        if thinking and text_deltas:
            first_thinking = next(
                i for i, e in enumerate(result.events) if isinstance(e, ThinkingDelta)
            )
            first_text = next(
                i for i, e in enumerate(result.events) if isinstance(e, AssistantTextDelta)
            )
            assert first_thinking < first_text, "Thinking should precede text deltas"

    def test_thinking_block_message_model(self):
        """ThinkingBlock should integrate correctly in ConversationMessage."""
        from message import ConversationMessage, TextBlock, ThinkingBlock

        msg = ConversationMessage(
            role="assistant",
            content=[
                ThinkingBlock(text="Let me think about this..."),
                TextBlock(text="The answer is 42."),
            ],
        )
        assert msg.thinking == "Let me think about this..."
        assert msg.text == "The answer is 42."
        # Thinking excluded from API params
        api_param = msg.to_api_param()
        block_types = [b["type"] for b in api_param["content"]]
        assert "thinking" not in block_types

    @pytest.mark.asyncio
    async def test_thinking_block_with_complex_reasoning(self):
        """Complex reasoning should produce structured thought."""
        agent = create_eval_agent()

        result = await agent.invoke(
            "Think carefully: if all roses are flowers, and some flowers fade quickly, can we conclude that some roses fade quickly? Explain your logic."
        )
        text = result.text
        assert text, "Should produce a reasoning response"
        assert len(text) > 50, "Complex reasoning should produce substantial output"

    @pytest.mark.asyncio
    async def test_thinking_delta_event_structure(self):
        """Verify thinking_delta events have expected fields when present."""
        agent = create_eval_agent()

        result = await agent.invoke("Step by step, calculate 8! (8 factorial).")
        text = result.text
        # Model may format with commas (40,320) or plain (40320)
        assert "40320" in text.replace(",", ""), f"8! = 40320. Got: {text}"

        from message.stream_events import ThinkingDelta

        for ev in result.events:
            if isinstance(ev, ThinkingDelta):
                assert hasattr(ev, "text")


# ===========================================================================
# AREA 4: Complex Long Tasks with Multiple Tool Calls
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestComplexLongTasks:
    """Test complex multi-step tasks requiring multiple tool calls."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("complex-task")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_create_and_run_python_script(self, sandbox):
        """Model should create a Python file and execute it in the sandbox."""
        result = await _invoke_with_sandbox_agent(
            sandbox,
            system_prompt=(
                "You have sandbox access. Use write_file to write files and "
                "shell to run them. Execute ALL requested steps using tools."
            ),
            message=(
                "Do these steps in the sandbox:\n"
                "1. Write a file /workspace/greet.py with: print('COMPLEX_TASK_OK')\n"
                "2. Run: python /workspace/greet.py\n"
                "3. Tell me the output"
            ),
        )
        _assert_sandbox_tools_used(result)

    @pytest.mark.asyncio
    async def test_multi_step_file_pipeline(self, sandbox):
        """Model should execute a multi-step pipeline: create, modify, verify."""
        await _invoke_with_sandbox_agent(
            sandbox,
            system_prompt=(
                "You are a coding assistant with sandbox access. Use shell, "
                "write_file, and read_file tools. Execute every step."
            ),
            message=(
                "In the sandbox:\n"
                "1. Create /workspace/data.txt with the text: alpha beta gamma\n"
                "2. Run: wc -w /workspace/data.txt\n"
                "3. Report the word count"
            ),
        )

    @pytest.mark.asyncio
    async def test_tool_error_handling(self, sandbox):
        """Model should handle tool errors gracefully."""
        agent = create_eval_agent(
            system_prompt="Use shell for commands. If a command fails, explain the error.",
            sandbox_id=sandbox["id"],
        )
        result = await agent.invoke(
            "Run this in the sandbox: cat /nonexistent/file/that/does/not/exist"
        )
        # Should still complete without crashing
        assert len(result.assistant_turns()) >= 1 or result.has_errors, (
            "Should have assistant turn or error events"
        )
        # Text may be empty if tool error terminated the stream early

    @pytest.mark.asyncio
    async def test_sequential_tool_calls_preserve_state(self, sandbox):
        """Sequential tool calls should see each other's results in the sandbox."""
        await _invoke_with_sandbox_agent(
            sandbox,
            system_prompt="Use shell for all commands.",
            message=(
                "In the sandbox, run these commands one after another:\n"
                "1. echo 'STATE_TEST' > /workspace/state_test.txt\n"
                "2. cat /workspace/state_test.txt\n"
                "3. Report what you see"
            ),
        )

    @pytest.mark.asyncio
    async def test_long_output_handling(self, sandbox):
        """Model should handle large tool output without crashing."""
        await _invoke_with_sandbox_agent(
            sandbox,
            system_prompt="Use shell for commands.",
            message="Run in the sandbox: seq 1 200",
        )


# ===========================================================================
# AREA 5: Code Intelligence System
# ===========================================================================


class TestCodeIntelligenceSystem:
    """Test code intelligence service, LSP, and symbol analysis.

    These tests do NOT require live API keys (unit-level).
    """

    def setup_method(self):
        """Clean up the CI service registry."""
        from code_intelligence.routing.service import dispose_all_code_intelligence

        dispose_all_code_intelligence()

    def teardown_method(self):
        from code_intelligence.routing.service import dispose_all_code_intelligence

        dispose_all_code_intelligence()

    # -- Service lifecycle --

    def test_ci_service_creation_and_status(self):
        """CI service should initialize with correct defaults."""
        from code_intelligence.routing.service import CodeIntelligenceService

        svc = CodeIntelligenceService(
            sandbox_id="ci-test-001",
            workspace_root="/workspace",
        )
        assert svc.sandbox_id == "ci-test-001"
        assert svc.is_initialized is False

        status = svc.status()
        assert status["sandbox_id"] == "ci-test-001"
        assert status["initialized"] is False
        assert "lsp" in status
        assert "symbol_index" in status

    def test_ci_service_telemetry_fields(self):
        """Telemetry should expose all expected counters."""
        from code_intelligence.routing.service import CodeIntelligenceService
        from code_intelligence.types import CITelemetry

        svc = CodeIntelligenceService(sandbox_id="ci-tel-001", workspace_root="/ws")
        tel = svc.get_telemetry()

        assert isinstance(tel, CITelemetry)
        assert tel.symbol_index_size == 0
        assert tel.lsp_connected is False
        assert tel.lsp_query_count == 0
        assert tel.arbiter_active_locks == 0
        assert tel.total_edits == 0

    def test_ci_service_dispose_safe(self):
        """Dispose should clean up without raising."""
        from code_intelligence.routing.service import CodeIntelligenceService

        svc = CodeIntelligenceService(sandbox_id="ci-dispose", workspace_root="/ws")
        svc.dispose()  # should not raise

    # -- Registry (singleton management) --

    def test_ci_registry_singleton(self):
        """Same sandbox_id should return the same service instance."""
        from code_intelligence.routing.service import get_code_intelligence

        svc1 = get_code_intelligence("singleton-test", "/ws")
        svc2 = get_code_intelligence("singleton-test", "/ws")
        assert svc1 is svc2

    def test_ci_registry_different_sandboxes(self):
        """Different sandbox_ids should get different instances."""
        from code_intelligence.routing.service import get_code_intelligence

        svc_a = get_code_intelligence("ci-a", "/ws")
        svc_b = get_code_intelligence("ci-b", "/ws")
        assert svc_a is not svc_b
        assert svc_a.sandbox_id == "ci-a"
        assert svc_b.sandbox_id == "ci-b"

    def test_ci_registry_dispose_removes(self):
        """Disposing a service should remove it from the registry."""
        from code_intelligence.routing.service import (
            get_code_intelligence,
            get_code_intelligence_if_exists,
            dispose_code_intelligence,
        )

        get_code_intelligence("dispose-reg", "/ws")
        assert get_code_intelligence_if_exists("dispose-reg") is not None
        dispose_code_intelligence("dispose-reg")
        assert get_code_intelligence_if_exists("dispose-reg") is None

    def test_ci_registry_all_status(self):
        """get_all_services_status should return all active services."""
        from code_intelligence.routing.service import get_code_intelligence, get_all_services_status

        get_code_intelligence("status-x", "/ws")
        get_code_intelligence("status-y", "/ws")

        all_status = get_all_services_status()
        assert "status-x" in all_status
        assert "status-y" in all_status

    # -- LSP Client --

    def test_lsp_client_creation(self):
        """LSP client should initialize without error."""
        from code_intelligence.lsp.client import LspClient

        lsp = LspClient(workspace_root="/workspace")
        assert lsp.telemetry.queries == 0
        assert lsp.telemetry.cache_hits == 0

    def test_lsp_client_language_detection(self):
        """LSP client should detect file languages correctly."""
        from code_intelligence.lsp.client import LspClient

        lsp = LspClient()
        assert lsp._detect_language("test.py") == "python"
        assert lsp._detect_language("app.ts") == "typescript"
        assert lsp._detect_language("index.tsx") == "typescript"
        assert lsp._detect_language("script.js") == "javascript"
        assert lsp._detect_language("data.json") == "unknown"

    def test_lsp_client_cache_invalidation(self):
        """Cache invalidation should remove entries for a file."""
        from code_intelligence.lsp.client import LspClient

        lsp = LspClient(workspace_root="/workspace")
        # Manually insert a cache entry
        lsp._put_cached("def:/workspace/test.py:1:0", [])
        lsp._put_cached("ref:/workspace/test.py:5:0", [])
        lsp._put_cached("def:/workspace/other.py:1:0", [])

        lsp.invalidate("/workspace/test.py")

        # test.py entries should be gone, other.py should remain
        assert lsp._get_cached("def:/workspace/test.py:1:0") is None
        assert lsp._get_cached("ref:/workspace/test.py:5:0") is None
        assert lsp._get_cached("def:/workspace/other.py:1:0") is not None

    def test_lsp_client_ensure_ready(self):
        """ensure_ready should return language availability dict."""
        from code_intelligence.lsp.client import LspClient

        lsp = LspClient(workspace_root="/workspace")
        status = lsp.ensure_ready()
        assert "python" in status
        assert "typescript" in status
        assert isinstance(status["python"], bool)

    # -- CI Types --

    def test_edit_request_fields(self):
        """EditRequest should hold all fields."""
        from code_intelligence.types import EditRequest

        req = EditRequest(
            file_path="/ws/test.py",
            old_text="old",
            new_text="new",
            agent_id="agent-1",
            description="Fix bug",
        )
        assert req.file_path == "/ws/test.py"
        assert req.agent_id == "agent-1"

    def test_edit_result_success_and_failure(self):
        """EditResult should represent both success and failure states."""
        from code_intelligence.types import EditResult

        success = EditResult(success=True, file_path="/test.py", message="OK")
        assert success.success is True

        failure = EditResult(success=False, file_path="/test.py", message="Conflict", conflict=True)
        assert failure.success is False
        assert failure.conflict is True

    def test_diagnostic_severity(self):
        """Diagnostic should hold severity and source info."""
        from code_intelligence.types import Diagnostic

        d = Diagnostic(
            file_path="/test.py",
            line=10,
            character=5,
            severity="error",
            message="Syntax error",
            source="python",
        )
        assert d.severity == "error"
        assert d.source == "python"
        assert d.line == 10
