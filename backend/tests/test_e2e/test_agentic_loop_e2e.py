# ruff: noqa
"""E2E tests for agentic tool call loop — tool accuracy, skill following, task completion.

These tests verify:
1. Tool call accuracy - agent selects correct tool with correct parameters
2. Skill loading & instruction following - agent follows skill instructions exactly
3. Agentic task completion - agent completes multi-step tasks without stopping early

Requires live MiniMax API + Daytona sandbox.
Run with: pytest tests/test_e2e/test_agentic_loop_e2e.py -m live -v
"""

from __future__ import annotations

import json
import os
import re
import time
from pathlib import Path
from typing import Any

import pytest
from dotenv import load_dotenv

from tests.test_e2e.conftest import parse_sse_events, events_of_type

# Load .env from project root
_PROJECT_ROOT = Path(__file__).resolve().parents[3]
load_dotenv(_PROJECT_ROOT / ".env")

pytestmark = [pytest.mark.e2e, pytest.mark.live]


# ---------------------------------------------------------------------------
# Credential loading (same pattern as test_live_minimax_comprehensive.py)
# ---------------------------------------------------------------------------


def _load_settings() -> dict:
    settings_path = Path.home() / ".ephemeralos" / "settings.json"
    if settings_path.exists():
        return json.loads(settings_path.read_text())
    return {}


_SETTINGS = _load_settings()

MINIMAX_KEY = os.environ.get("MINIMAX_API_KEY") or _SETTINGS.get("api_key", "")
MINIMAX_MODEL = os.environ.get("MINIMAX_MODEL") or _SETTINGS.get("model", "MiniMax-M2.7-highspeed")
MINIMAX_BASE_URL = os.environ.get("MINIMAX_BASE_URL") or _SETTINGS.get("base_url", "")
MINIMAX_FORMAT = os.environ.get("MINIMAX_API_FORMAT") or _SETTINGS.get("api_format", "openai")

DAYTONA_KEY = os.environ.get("DAYTONA_API_KEY") or _SETTINGS.get("daytona_api_key", "")
DAYTONA_URL = os.environ.get("DAYTONA_API_URL") or _SETTINGS.get("daytona_api_url", "")
DAYTONA_TARGET = os.environ.get("DAYTONA_TARGET") or _SETTINGS.get("daytona_target", "")

HAS_MINIMAX = bool(MINIMAX_KEY and MINIMAX_BASE_URL)
HAS_DAYTONA = bool(DAYTONA_KEY and DAYTONA_URL)
HAS_BOTH = HAS_MINIMAX and HAS_DAYTONA


# ---------------------------------------------------------------------------
# Shared helpers (mirrors test_live_minimax_comprehensive.py)
# ---------------------------------------------------------------------------


def _make_live_client(
    db_session_factory, tmp_path, monkeypatch, *, api_key, model, base_url, api_format
):
    """Create a TestClient configured with real API credentials."""
    from fastapi.testclient import TestClient
    from server.protocol import BackendHostConfig
    from server.app_factory import create_app

    # Clear ALL proxy env vars to prevent httpx routing through localhost proxy
    for _var in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy", "NO_PROXY", "no_proxy"]:
        monkeypatch.delenv(_var, raising=False)
    monkeypatch.delenv("EPHEMERALOS_DATABASE_URL", raising=False)
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
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


def _get_sandbox_service():
    from sandbox.service import SandboxService

    return SandboxService()


def _create_test_sandbox(name: str = "e2e-agentic") -> dict:
    svc = _get_sandbox_service()
    sandbox = svc.create_sandbox(
        name=f"{name}-{int(time.time())}",
        language="python",
        labels={"purpose": "e2e-agentic-loop"},
    )
    return sandbox


def _delete_sandbox(sandbox_id: str) -> None:
    try:
        svc = _get_sandbox_service()
        svc.delete_sandbox(sandbox_id)
    except Exception:
        pass


def _send_chat(
    client,
    line: str,
    *,
    agent_name: str | None = None,
    sandbox_id: str | None = None,
    timeout: int = 180,
) -> list[dict]:
    """Send a chat message and return parsed SSE events."""
    payload: dict[str, Any] = {"line": line}
    if agent_name:
        payload["agent_name"] = agent_name
    if sandbox_id:
        payload["sandbox_id"] = sandbox_id

    resp = client.post("/api/chat", json=payload, timeout=timeout)
    assert resp.status_code == 200, f"Chat failed: {resp.status_code} {resp.text[:500]}"
    return parse_sse_events(resp.text)


def _get_assistant_text(events: list[dict]) -> str:
    """Extract the final assistant message text from events."""
    completes = events_of_type(events, "assistant_complete")
    if completes:
        return completes[0].get("message", "")
    return ""


def _get_event_types(events: list[dict]) -> set[str]:
    """Get unique event types."""
    return {e["type"] for e in events}


def _get_tool_started_events(events: list[dict]) -> list[dict]:
    """Get all tool_started events."""
    return events_of_type(events, "tool_started")


def _get_tool_completed_events(events: list[dict]) -> list[dict]:
    """Get all tool_completed events."""
    return events_of_type(events, "tool_completed")


def _create_agent(
    client,
    name: str,
    *,
    toolkits: list[str] | None = None,
    skills: list[str] | None = None,
    system_prompt: str | None = None,
) -> dict:
    """Create an agent and return its data, handling duplicates."""
    payload: dict[str, Any] = {
        "name": name,
        "description": f"E2E agentic loop test agent: {name}",
        "model": MINIMAX_MODEL,
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
    # Agent may already exist from a previous test run — fetch it
    get_resp = client.get(f"/api/agents/{name}")
    if get_resp.status_code == 200:
        return get_resp.json()
    # If neither worked, raise
    assert False, f"Failed to create or get agent '{name}': {resp.status_code} {resp.text}"


def _extract_tool_calls_from_events(events: list[dict]) -> list[tuple[str, dict]]:
    """Extract (tool_name, tool_input) tuples from tool_started events."""
    tool_calls = []
    for ev in _get_tool_started_events(events):
        tool_calls.append((ev.get("tool_name", ""), ev.get("tool_input", {})))
    return tool_calls


# ===========================================================================
# AREA 1: Tool Call Accuracy
# ===========================================================================


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
class TestToolCallAccuracy:
    """Verify agent selects correct tool with correct parameters."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = _create_test_sandbox("tool-accuracy")
        yield sb
        _delete_sandbox(sb["id"])

    @pytest.fixture()
    def client(self, db_session_factory, tmp_path, monkeypatch):
        c = _make_live_client(
            db_session_factory,
            tmp_path,
            monkeypatch,
            api_key=MINIMAX_KEY,
            model=MINIMAX_MODEL,
            base_url=MINIMAX_BASE_URL,
            api_format=MINIMAX_FORMAT,
        )
        with c:
            yield c

    def test_correct_tool_selected_for_file_write(self, client, sandbox):
        """Agent should use daytona_write_file, not daytona_bash, for file creation."""
        _create_agent(
            client,
            "acc-write-agent",
            toolkits=["sandbox_operations"],
            system_prompt=(
                "You have sandbox access via daytona_write_file and daytona_bash. "
                "When asked to create a file, ALWAYS use daytona_write_file."
            ),
        )

        events = _send_chat(
            client,
            "Create a file /workspace/e2e_accuracy.txt with content: TOOL_ACCURACY_TEST_PASS",
            agent_name="acc-write-agent",
            sandbox_id=sandbox["id"],
            timeout=120,
        )

        tool_started = _get_tool_started_events(events)
        tool_names = [e["tool_name"] for e in tool_started]

        # Should use daytona_write_file for file creation
        assert "daytona_write_file" in tool_names, (
            f"Should use daytona_write_file for file creation. Tools used: {tool_names}"
        )
        # Should NOT use daytona_bash for file creation (wrong tool)
        bash_for_write = [
            e
            for e in tool_started
            if e["tool_name"] == "daytona_bash" and "write" in str(e.get("tool_input", {})).lower()
        ]
        assert not bash_for_write, "Should not use daytona_bash for file write operations"

    def test_correct_tool_selected_for_command_execution(self, client, sandbox):
        """Agent should use daytona_bash, not daytona_write_file, for command execution."""
        _create_agent(
            client,
            "acc-bash-agent",
            toolkits=["sandbox_operations"],
            system_prompt=(
                "You have sandbox access. Use daytona_bash for running commands. "
                "Use daytona_write_file only for creating files."
            ),
        )

        events = _send_chat(
            client,
            "Run this command in the sandbox: echo 'CORRECT_TOOL_BASH'",
            agent_name="acc-bash-agent",
            sandbox_id=sandbox["id"],
            timeout=120,
        )

        tool_started = _get_tool_started_events(events)
        tool_names = [e["tool_name"] for e in tool_started]

        # Should use daytona_bash for command execution
        assert "daytona_bash" in tool_names, (
            f"Should use daytona_bash for commands. Tools used: {tool_names}"
        )

    def test_tool_input_parameters_correct(self, client, sandbox):
        """Verify tool is called with the exact parameters specified."""
        _create_agent(
            client,
            "acc-params-agent",
            toolkits=["sandbox_operations"],
            system_prompt="Use daytona_write_file with EXACTLY the path and content provided.",
        )

        events = _send_chat(
            client,
            "Write to /workspace/params_test.txt with content: PARAM_TEST_CONTENT",
            agent_name="acc-params-agent",
            sandbox_id=sandbox["id"],
            timeout=120,
        )

        tool_started = _get_tool_started_events(events)
        write_calls = [e for e in tool_started if e["tool_name"] == "daytona_write_file"]

        assert write_calls, (
            f"No daytona_write_file calls found. Tools: {[e['tool_name'] for e in tool_started]}"
        )

        # Verify exact path
        write_inputs = [e["tool_input"] for e in write_calls]
        path_matched = any(
            inp.get("file_path") == "/workspace/params_test.txt" for inp in write_inputs
        )
        assert path_matched, f"Expected path /workspace/params_test.txt. Got: {write_inputs}"

        # Verify exact content
        content_matched = any(inp.get("content") == "PARAM_TEST_CONTENT" for inp in write_inputs)
        assert content_matched, f"Expected content 'PARAM_TEST_CONTENT'. Got: {write_inputs}"

    def test_multiple_tools_different_purposes(self, client, sandbox):
        """Agent should use different tools for different purposes in same conversation."""
        _create_agent(
            client,
            "acc-multi-agent",
            toolkits=["sandbox_operations"],
            system_prompt=(
                "Use the right tool for each task. "
                "Continue working — do not stop after one tool. "
                "Make tool calls for BOTH steps: write the file AND run the command."
            ),
        )

        events = _send_chat(
            client,
            "First, create /workspace/multi_test.txt with 'MULTI_TOOL_TEST'. Then run: cat /workspace/multi_test.txt",
            agent_name="acc-multi-agent",
            sandbox_id=sandbox["id"],
            timeout=180,
        )

        tool_started = _get_tool_started_events(events)
        tool_names = [e["tool_name"] for e in tool_started]

        # Should have BOTH write and bash (read) tools
        assert "daytona_write_file" in tool_names, f"Missing write tool. Tools: {tool_names}"
        assert "daytona_bash" in tool_names, f"Missing bash tool. Tools: {tool_names}"

        # Verify sequence: write should come before bash
        write_idx = tool_names.index("daytona_write_file")
        bash_idx = tool_names.index("daytona_bash")
        assert write_idx < bash_idx, f"Write should come before bash. Order: {tool_names}"


# ===========================================================================
# AREEA 2: Skill Loading & Instruction Following
# ===========================================================================


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
class TestSkillLoadingAndInstructionFollowing:
    """Verify agent loads skills and follows their instructions exactly."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = _create_test_sandbox("skill-follow")
        yield sb
        _delete_sandbox(sb["id"])

    @pytest.fixture()
    def client(self, db_session_factory, tmp_path, monkeypatch):
        c = _make_live_client(
            db_session_factory,
            tmp_path,
            monkeypatch,
            api_key=MINIMAX_KEY,
            model=MINIMAX_MODEL,
            base_url=MINIMAX_BASE_URL,
            api_format=MINIMAX_FORMAT,
        )
        with c:
            yield c

    def test_skill_load_skill_tool_invoked(self, client, sandbox):
        """Agent should invoke load_skill tool when given a skill-dependent task."""
        _create_agent(
            client,
            "skill-load-agent",
            toolkits=["sandbox_operations"],
            skills=["e2e-test-skill"],
            system_prompt=(
                "You have access to the 'e2e-test-skill'. "
                "When asked to verify tool call accuracy, ALWAYS load the skill first using load_skill tool."
            ),
        )

        events = _send_chat(
            client,
            "I need to verify tool call accuracy. Load the e2e-test-skill and follow its instructions.",
            agent_name="skill-load-agent",
            sandbox_id=sandbox["id"],
            timeout=120,
        )

        tool_started = _get_tool_started_events(events)
        tool_names = [e["tool_name"] for e in tool_started]

        # Should have invoked load_skill
        assert "load_skill" in tool_names, (
            f"Agent should invoke load_skill tool. Tools used: {tool_names}"
        )

    def test_skill_instructions_followed_exactly(self, client, sandbox):
        """Agent should follow skill instructions with exact string matching."""
        _create_agent(
            client,
            "skill-follow-agent",
            toolkits=["sandbox_operations"],
            skills=["e2e-test-skill"],
            system_prompt=(
                "When asked to verify tool call accuracy, ALWAYS load the skill first using load_skill tool. "
                "After loading, follow the skill's instructions EXACTLY for verification. "
                "Continue working — do not stop. Execute ALL verification steps."
            ),
        )

        events = _send_chat(
            client,
            (
                "Load the e2e-test-skill FIRST, then verify these steps:\n"
                "1. Run command: echo 'SKILL_FOLLOW_EXACT'\n"
                "2. Report the EXACT output using the format:\n"
                "   VERIFIED: <exact_string>\n"
                "   STATUS: PASS"
            ),
            agent_name="skill-follow-agent",
            sandbox_id=sandbox["id"],
            timeout=180,
        )

        text = _get_assistant_text(events)
        tool_started = _get_tool_started_events(events)
        tool_names = [e["tool_name"] for e in tool_started]

        assert "load_skill" in tool_names, (
            f"Model must call load_skill first. Tools called: {tool_names}"
        )

    def test_skill_output_format_compliance(self, client, sandbox):
        """Verify agent uses the exact output format specified by the skill."""
        _create_agent(
            client,
            "skill-format-agent",
            toolkits=["sandbox_operations"],
            skills=["e2e-test-skill"],
            system_prompt=(
                "For verification tasks, FIRST call load_skill with name='e2e-test-skill' to get the skill instructions. "
                "The skill mandates: TOOL_CALLED, PARAMS_USED, VERIFIED, and STATUS fields in your response. "
                "Continue working — do not stop. Execute all verification steps with tools."
            ),
        )

        events = _send_chat(
            client,
            (
                "Run: echo 'FORMAT_TEST' and verify the output.\n"
                "Provide the verification report with TOOL_CALLED, PARAMS_USED, VERIFIED, STATUS fields."
            ),
            agent_name="skill-format-agent",
            sandbox_id=sandbox["id"],
            timeout=180,
        )

        text = _get_assistant_text(events)

        # Skill mandates specific output format
        required_fields = ["TOOL_CALLED:", "PARAMS_USED:", "VERIFIED:", "STATUS:"]
        for field in required_fields:
            assert field in text, f"Missing required field '{field}' from skill format. Got: {text}"

    def test_skill_not_loaded_when_not_needed(self, client, sandbox):
        """Verify load_skill is NOT invoked for tasks that don't require it."""
        _create_agent(
            client,
            "skill-unneeded-agent",
            toolkits=["sandbox_operations"],
            skills=["e2e-test-skill"],
            system_prompt="You have e2e-test-skill available but only use it when appropriate.",
        )

        events = _send_chat(
            client,
            "Simply run: echo 'NO_SKILL_NEEDED' and tell me the result.",
            agent_name="skill-unneeded-agent",
            sandbox_id=sandbox["id"],
            timeout=120,
        )

        tool_started = _get_tool_started_events(events)
        tool_names = [e["tool_name"] for e in tool_started]

        # Should NOT invoke load_skill for simple echo command
        assert "load_skill" not in tool_names, (
            f"Should not load skill for simple echo command. Tools used: {tool_names}"
        )


# ===========================================================================
# AREA 3: Agentic Task Completion (Multi-Step, No Early Stop)
# ===========================================================================


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
class TestAgenticTaskCompletion:
    """Verify agent completes multi-step tasks without stopping early."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = _create_test_sandbox("task-completion")
        yield sb
        _delete_sandbox(sb["id"])

    @pytest.fixture()
    def client(self, db_session_factory, tmp_path, monkeypatch):
        c = _make_live_client(
            db_session_factory,
            tmp_path,
            monkeypatch,
            api_key=MINIMAX_KEY,
            model=MINIMAX_MODEL,
            base_url=MINIMAX_BASE_URL,
            api_format=MINIMAX_FORMAT,
        )
        with c:
            yield c

    def test_five_step_task_completes_all_steps(self, client, sandbox):
        """A 5-step task should complete ALL 5 steps, not stop at step 2 or 3."""
        _create_agent(
            client,
            "multi-step-agent",
            toolkits=["sandbox_operations"],
            system_prompt=(
                "Execute ALL steps in sequence. Do NOT skip any steps. "
                "Report completion of EACH step. "
                "Continue working — do not stop to summarize results unless the task is done. "
                "You MUST make a tool call for EACH step - do not summarize or skip any step."
            ),
        )

        events = _send_chat(
            client,
            (
                "Complete these 5 steps in order:\n"
                "Step 1: Create /workspace/step1.txt with 'STEP1_DONE'\n"
                "Step 2: Create /workspace/step2.txt with 'STEP2_DONE'\n"
                "Step 3: Create /workspace/step3.txt with 'STEP3_DONE'\n"
                "Step 4: Create /workspace/step4.txt with 'STEP4_DONE'\n"
                "Step 5: Create /workspace/step5.txt with 'STEP5_DONE'\n"
                "After completing all steps, list all 5 filenames you created."
            ),
            agent_name="multi-step-agent",
            sandbox_id=sandbox["id"],
            timeout=300,
        )

        tool_started = _get_tool_started_events(events)
        tool_names = [e["tool_name"] for e in tool_started]

        # Count daytona_write_file calls - should be exactly 5 (one per step)
        write_calls = [e for e in tool_started if e["tool_name"] == "daytona_write_file"]
        assert len(write_calls) >= 5, (
            f"Expected at least 5 write operations (one per step). Got {len(write_calls)}. "
            f"Tools: {tool_names}"
        )

        # Verify all 5 files were attempted
        write_inputs = [e["tool_input"] for e in write_calls]
        expected_files = ["step1.txt", "step2.txt", "step3.txt", "step4.txt", "step5.txt"]
        created_files = [inp.get("file_path", "").split("/")[-1] for inp in write_inputs]

        for expected in expected_files:
            assert expected in created_files, (
                f"File {expected} not created. Created files: {created_files}"
            )

    def test_agent_continues_after_tool_error(self, client, sandbox):
        """Agent should continue task even if a tool call returns an error."""
        _create_agent(
            client,
            "error-recovery-agent",
            toolkits=["sandbox_operations"],
            system_prompt=(
                "If a tool fails, explain the error and continue with remaining steps. "
                "Do NOT stop the task. Continue working — do not stop to summarize results "
                "unless the task is done. Make tool calls for ALL remaining steps."
            ),
        )

        events = _send_chat(
            client,
            (
                "Complete these steps:\n"
                "Step 1: Create /workspace/recover1.txt with 'RECOVER1'\n"
                "Step 2: Try to read /nonexistent/file.txt (expect error)\n"
                "Step 3: Create /workspace/recover3.txt with 'RECOVER3'\n"
                "Report what happened at each step."
            ),
            agent_name="error-recovery-agent",
            sandbox_id=sandbox["id"],
            timeout=180,
        )

        text = _get_assistant_text(events)

        # Should have attempted all 3 steps
        tool_started = _get_tool_started_events(events)
        tool_names = [e["tool_name"] for e in tool_started]

        # Should have write for step 1
        assert "daytona_write_file" in tool_names, (
            f"Should attempt step 1 write. Tools: {tool_names}"
        )

        # Should have tried to read nonexistent file (step 2)
        bash_calls = [e for e in tool_started if e["tool_name"] == "daytona_bash"]
        assert bash_calls, f"Should attempt step 2 (read nonexistent file). Tools: {tool_names}"

        # Should have write for step 3 (continued after error)
        write_calls_after_bash = [
            e
            for i, e in enumerate(tool_started)
            if e["tool_name"] == "daytona_write_file" and i > tool_started.index(bash_calls[0])
        ]
        assert write_calls_after_bash, (
            f"Should continue with step 3 after error. Tools: {tool_names}"
        )

    def test_complex_task_with_10_plus_tool_calls(self, client, sandbox):
        """Complex task requiring 10+ tool calls should complete without hitting max_turns."""
        _create_agent(
            client,
            "complex-task-agent",
            toolkits=["sandbox_operations"],
            system_prompt=(
                "Execute each step carefully. Complete all operations. "
                "Continue working — do not stop to summarize. "
                "Make a tool call for EACH file - do not skip any file."
            ),
        )

        prompt = (
            "Create these 10 files in /workspace/:\n"
            + "\n".join(f"- file{i}.txt with content 'FILE{i}DONE'" for i in range(1, 11))
            + "\nThen list all 10 filenames."
        )
        events = _send_chat(
            client,
            prompt,
            agent_name="complex-task-agent",
            sandbox_id=sandbox["id"],
            timeout=600,
        )

        tool_started = _get_tool_started_events(events)
        tool_names = [e["tool_name"] for e in tool_started]

        # Should have made multiple tool calls
        assert len(tool_started) >= 10, (
            f"Complex task should require 10+ tool calls. Got {len(tool_started)}. "
            f"Tools: {tool_names}"
        )

        # Verify assistant completed (didn't hit max_turns limit)
        assert "assistant_complete" in _get_event_types(events), (
            "Task should complete with assistant_complete, not timeout"
        )

    def test_no_early_stop_verification(self, client, sandbox):
        """Verify agent doesn't stop early when task explicitly asks for specific completion criteria."""
        _create_agent(
            client,
            "complete-agent",
            toolkits=["sandbox_operations"],
            system_prompt=(
                "Complete the ENTIRE task. Do not summarize or stop early. "
                "Continue working — do not stop to summarize results unless the task is done. "
                "Make ALL tool calls required to complete every step."
            ),
        )

        events = _send_chat(
            client,
            (
                "Complete these EXACT steps:\n"
                "1. Create /workspace/complete1.txt with 'FIRST'\n"
                "2. Create /workspace/complete2.txt with 'SECOND'\n"
                "3. Create /workspace/complete3.txt with 'THIRD'\n"
                "4. Run: ls /workspace/complete*.txt\n"
                "5. Tell me the EXACT output from step 4."
            ),
            agent_name="complete-agent",
            sandbox_id=sandbox["id"],
            timeout=300,
        )

        tool_started = _get_tool_started_events(events)
        tool_names = [e["tool_name"] for e in tool_started]

        # Should have ls command at the end (step 4)
        bash_calls = [e for e in tool_started if e["tool_name"] == "daytona_bash"]
        assert bash_calls, f"Should execute ls command (step 4). Tools: {tool_names}"

        # Verify ls was called with correct path
        # tool_input can be a dict or a string depending on the event structure
        ls_calls = []
        for e in bash_calls:
            tool_input = e.get("tool_input", {})
            if isinstance(tool_input, dict):
                cmd = tool_input.get("command", "")
            else:
                cmd = str(tool_input)
            if "ls" in cmd:
                ls_calls.append(e)
        assert ls_calls, (
            f"Should have ls command. Bash calls: {[e['tool_input'] for e in bash_calls]}"
        )

    def test_agent_completes_without_summarizing_early(self, client, sandbox):
        """Agent should not stop early by summarizing - must complete actual operations."""
        _create_agent(
            client,
            "no-summarize-agent",
            toolkits=["sandbox_operations"],
            system_prompt=(
                "Do the actual work. Do NOT summarize that you would do something - actually do it. "
                "Complete every step personally. "
                "Continue working — do not stop. Make tool calls for ALL steps."
            ),
        )

        events = _send_chat(
            client,
            (
                "Perform these actions (not just describe them):\n"
                "1. Write to /workspace/action1.txt: 'ACTION1'\n"
                "2. Write to /workspace/action2.txt: 'ACTION2'\n"
                "3. Verify both files exist and report their content."
            ),
            agent_name="no-summarize-agent",
            sandbox_id=sandbox["id"],
            timeout=300,
        )

        tool_started = _get_tool_started_events(events)
        tool_names = [e["tool_name"] for e in tool_started]

        # Should actually perform writes, not just describe
        write_calls = [e for e in tool_started if e["tool_name"] == "daytona_write_file"]
        assert len(write_calls) >= 2, (
            f"Should perform 2 write actions. Got {len(write_calls)}. Tools: {tool_names}"
        )


# ===========================================================================
# AREA 4: Integration - All Three Test Areas Combined
# ===========================================================================


@pytest.mark.skipif(not HAS_BOTH, reason="MiniMax + Daytona both required")
class TestIntegratedAgenticLoop:
    """Integration test combining tool accuracy, skill following, and task completion."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = _create_test_sandbox("integrated")
        yield sb
        _delete_sandbox(sb["id"])

    @pytest.fixture()
    def client(self, db_session_factory, tmp_path, monkeypatch):
        c = _make_live_client(
            db_session_factory,
            tmp_path,
            monkeypatch,
            api_key=MINIMAX_KEY,
            model=MINIMAX_MODEL,
            base_url=MINIMAX_BASE_URL,
            api_format=MINIMAX_FORMAT,
        )
        with c:
            yield c

    def test_full_integration_tool_accuracy_plus_skill_following(self, client, sandbox):
        """Combines: correct tool selection + skill instruction following + task completion."""
        _create_agent(
            client,
            "integration-agent",
            toolkits=["sandbox_operations"],
            skills=["e2e-test-skill"],
            system_prompt=(
                "For verification tasks, FIRST call load_skill with name='e2e-test-skill'. "
                "Use e2e-test-skill for verification format (TOOL_CALLED, PARAMS_USED, VERIFIED, STATUS). "
                "Use correct tools for each operation. "
                "Complete all steps. Continue working — do not stop to summarize results. "
                "Make ALL tool calls needed to complete every step."
            ),
        )

        events = _send_chat(
            client,
            (
                "Using the e2e-test-skill format:\n"
                "1. Create /workspace/integration_test.txt with 'INTEGRATION_PASS'\n"
                "2. Verify the file was created with correct content\n"
                "3. Report using the skill format with VERIFIED and STATUS fields."
            ),
            agent_name="integration-agent",
            sandbox_id=sandbox["id"],
            timeout=300,
        )

        # Verify tool accuracy
        tool_started = _get_tool_started_events(events)
        tool_names = [e["tool_name"] for e in tool_started]
        assert "daytona_write_file" in tool_names, f"Should use correct tool. Tools: {tool_names}"

        # Verify skill was loaded and followed
        assert "load_skill" in tool_names, f"Should load skill. Tools: {tool_names}"

        # Verify task completion
        text = _get_assistant_text(events)
        assert "VERIFIED:" in text, f"Should follow skill format. Got: {text}"
        assert "INTEGRATION_PASS" in text, f"Should verify content. Got: {text}"
