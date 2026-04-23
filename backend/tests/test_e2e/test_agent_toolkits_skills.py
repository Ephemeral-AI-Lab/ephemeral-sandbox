"""E2E tests: agent toolkit/skill assignment and chat integration.

Tests the full flow:
- Create agents with toolkits and skills via API
- Chat with agents and verify tools are passed to the LLM
- Verify skill/toolkit sections stay out of the system prompt
- Verify model key resolution (minimax, anthropic-compatible)
"""

from __future__ import annotations

import pytest

pytestmark = pytest.mark.e2e


# ---------------------------------------------------------------------------
# US-001: Health check & infrastructure
# ---------------------------------------------------------------------------


class TestInfrastructure:
    """Verify the test infrastructure (app, DB, mock client) works."""

    def test_health_check(self, app_client):
        client, _ = app_client
        resp = client.get("/api/health")
        assert resp.status_code == 200
        data = resp.json()
        assert data["status"] == "ok"

    def test_state_endpoint(self, app_client):
        client, _ = app_client
        resp = client.get("/api/state")
        assert resp.status_code == 200
        data = resp.json()
        # /api/state returns a BackendEvent with type="ready"
        assert data["type"] == "ready"
        assert data["state"] is not None
        assert "model" in data["state"]
        assert data["toolkits"] is not None
        toolkits = {entry["name"]: entry["tools"] for entry in data["toolkits"]}
        assert set(toolkits["submission"]) == {
            "submit_task_success",
            "submit_plan",
            "submit_replan",
        }
        assert "submit_task_plan" not in toolkits["submission"]
        assert "declare_blocker" not in toolkits["submission"]
        assert "skills" in toolkits
        assert "background" in toolkits


# ---------------------------------------------------------------------------
# US-002: Agent CRUD with toolkits and skills
# ---------------------------------------------------------------------------


class TestAgentCRUD:
    """Create, read, update agents with toolkit and skill assignments."""

    def test_create_agent_with_toolkits_and_skills(self, app_client):
        client, _ = app_client
        resp = client.post(
            "/api/agents/",
            json={
                "name": "test-coder",
                "description": "A test coding agent",
                "model": "minimax",
                "toolkits": ["sandbox_operations"],
                "skills": ["team-planner-playbook"],
            },
        )
        assert resp.status_code == 201, resp.text
        data = resp.json()
        assert data["name"] == "test-coder"
        assert data["toolkits"] == ["sandbox_operations"]
        assert data["skills"] == ["team-planner-playbook"]
        assert data["model"] == "minimax"

    def test_get_agent_returns_toolkits_and_skills(self, app_client):
        client, _ = app_client
        # Create first
        client.post(
            "/api/agents/",
            json={
                "name": "reader-agent",
                "description": "Reads code",
                "model": "minimax",
                "toolkits": ["sandbox_operations", "code_intelligence"],
                "skills": ["team-planner-playbook"],
            },
        )
        # Fetch
        resp = client.get("/api/agents/reader-agent")
        assert resp.status_code == 200
        data = resp.json()
        assert "sandbox_operations" in data["toolkits"]
        assert "code_intelligence" in data["toolkits"]
        assert "team-planner-playbook" in data["skills"]

    def test_update_agent_toolkits(self, app_client):
        client, _ = app_client
        # Create
        client.post(
            "/api/agents/",
            json={
                "name": "updatable-agent",
                "description": "Will be updated",
                "model": "minimax",
                "toolkits": ["sandbox_operations"],
            },
        )
        # Update toolkits
        resp = client.put(
            "/api/agents/updatable-agent",
            json={
                "toolkits": ["sandbox_operations", "code_intelligence"],
            },
        )
        assert resp.status_code == 200
        data = resp.json()
        assert set(data["toolkits"]) == {"sandbox_operations", "code_intelligence"}

    def test_list_available_toolkits(self, app_client):
        client, _ = app_client
        resp = client.get("/api/agents/toolkits/available")
        assert resp.status_code == 200
        toolkits = resp.json()
        assert isinstance(toolkits, list)
        assert "sandbox_operations" in toolkits
        assert "code_intelligence" in toolkits

    def test_list_available_tools(self, app_client):
        client, _ = app_client
        resp = client.get("/api/agents/tools/available")
        assert resp.status_code == 200
        tools = {entry["name"] for entry in resp.json()}
        assert "daytona_shell" in tools
        assert "submit_plan" in tools
        assert "submit_replan" in tools
        assert "load_skill" in tools
        assert "check_background_progress" in tools

    def test_create_agent_no_toolkits(self, app_client):
        client, _ = app_client
        resp = client.post(
            "/api/agents/",
            json={
                "name": "bare-agent",
                "description": "No toolkits",
                "model": "minimax",
            },
        )
        assert resp.status_code == 201
        data = resp.json()
        assert data["toolkits"] is None or data["toolkits"] == []

    def test_create_agent_empty_skills(self, app_client):
        client, _ = app_client
        resp = client.post(
            "/api/agents/",
            json={
                "name": "no-skills-agent",
                "description": "No skills",
                "model": "minimax",
                "skills": [],
            },
        )
        assert resp.status_code == 201
        data = resp.json()
        assert data["skills"] == []

    def test_list_agents_shows_created(self, app_client):
        client, _ = app_client
        client.post(
            "/api/agents/",
            json={
                "name": "listed-agent",
                "description": "Should appear in list",
                "model": "minimax",
                "toolkits": ["sandbox_operations"],
            },
        )
        resp = client.get("/api/agents/")
        assert resp.status_code == 200
        agents = resp.json()
        names = [a["name"] for a in agents]
        assert "listed-agent" in names

    def test_delete_agent(self, app_client):
        client, _ = app_client
        client.post(
            "/api/agents/",
            json={
                "name": "deletable-agent",
                "description": "Will be deleted",
                "model": "minimax",
            },
        )
        resp = client.delete("/api/agents/deletable-agent")
        assert resp.status_code == 200
        # Should be gone from list
        resp2 = client.get("/api/agents/deletable-agent")
        assert resp2.status_code == 404


# ---------------------------------------------------------------------------
# US-003: Chat verifies toolkit tools are passed to LLM
# ---------------------------------------------------------------------------


class TestChatToolkitIntegration:
    """Chat with agents and verify the LLM receives correct tool schemas."""

    def _chat_and_get_tools(self, client, mock_client, agent_name=None, sandbox_id=None):
        """Send a chat request and return the tool names from the last API call."""
        payload = {"line": "list files in the sandbox"}
        if agent_name:
            payload["agent_name"] = agent_name
        if sandbox_id:
            payload["sandbox_id"] = sandbox_id

        resp = client.post("/api/chat", json=payload)
        assert resp.status_code == 200

        # Consume SSE stream
        for line in resp.iter_lines():
            pass

        # Extract tool names from the mock's captured request
        if mock_client.last_request and mock_client.last_request.tools:
            return [t["name"] for t in mock_client.last_request.tools]
        return []

    def test_agent_with_sandbox_toolkit_gets_sandbox_tools(self, app_client):
        client, mock_client = app_client
        # Create agent with sandbox_operations toolkit
        client.post(
            "/api/agents/",
            json={
                "name": "sandbox-agent",
                "description": "Agent with sandbox tools",
                "model": "minimax",
                "toolkits": ["sandbox_operations"],
            },
        )

        tool_names = self._chat_and_get_tools(client, mock_client, agent_name="sandbox-agent")
        print(f"DEBUG tool_names: {tool_names}")
        print(f"DEBUG mock_client.last_request: {mock_client.last_request}")
        if mock_client.last_request:
            print(f"DEBUG tools field: {mock_client.last_request.tools}")
            print(
                f"DEBUG system_prompt: {mock_client.last_request.system_prompt[:100] if mock_client.last_request.system_prompt else None}"
            )

        # Sandbox tools are prefixed with 'daytona_'
        daytona_tools = [t for t in tool_names if t.startswith("daytona_")]
        assert len(daytona_tools) > 0, f"Expected daytona_* sandbox tools, got: {tool_names}"

    def test_agent_without_toolkits_gets_defaults(self, app_client):
        client, mock_client = app_client
        # Create agent with no toolkits
        client.post(
            "/api/agents/",
            json={
                "name": "default-agent",
                "description": "Default tools only",
                "model": "minimax",
            },
        )

        tool_names = self._chat_and_get_tools(client, mock_client, agent_name="default-agent")

        # Agent with no toolkits still gets skills tools (load_skill, load_skill_reference)
        skill_tools = [t for t in tool_names if "skill" in t.lower()]
        assert len(tool_names) == len(skill_tools), (
            f"Agent with no toolkits should only have skills tools, got: {tool_names}"
        )

    def test_agent_with_toolkits_restricts_tools(self, app_client):
        client, mock_client = app_client
        # Create agent restricted to sandbox_operations only
        client.post(
            "/api/agents/",
            json={
                "name": "restricted-agent",
                "description": "Only sandbox tools",
                "model": "minimax",
                "toolkits": ["sandbox_operations"],
            },
        )

        tool_names = self._chat_and_get_tools(client, mock_client, agent_name="restricted-agent")

        # Discovery toolkit tools (skill, tool_search) should NOT be present
        assert "skill" not in tool_names, (
            f"discovery tools should be restricted out, got: {tool_names}"
        )


# ---------------------------------------------------------------------------
# US-004: Chat omits skill/toolkit sections from system prompt
# ---------------------------------------------------------------------------


class TestChatPromptSections:
    """Verify skills and toolkit metadata do not inflate the system prompt."""

    def _chat_and_get_system_prompt(self, client, mock_client, agent_name=None):
        """Send a chat request and return the system_prompt from the last API call."""
        payload = {"line": "hello"}
        if agent_name:
            payload["agent_name"] = agent_name

        resp = client.post("/api/chat", json=payload)
        assert resp.status_code == 200
        for line in resp.iter_lines():
            pass

        if mock_client.last_request:
            return mock_client.last_request.system_prompt or ""
        return ""

    def test_agent_with_toolkits_omits_toolkit_sections(self, app_client):
        client, mock_client = app_client
        client.post(
            "/api/agents/",
            json={
                "name": "aware-agent",
                "description": "Should know its tools",
                "model": "minimax",
                "toolkits": ["sandbox_operations"],
            },
        )

        system_prompt = self._chat_and_get_system_prompt(
            client, mock_client, agent_name="aware-agent"
        )

        assert "<Toolkit Instructions>" not in system_prompt
        assert "sandbox_operations" not in system_prompt

    def test_agent_with_custom_system_prompt_omits_toolkit_sections(self, app_client):
        client, mock_client = app_client
        client.post(
            "/api/agents/",
            json={
                "name": "custom-prompt-agent",
                "description": "Has custom prompt",
                "model": "minimax",
                "system_prompt": "You are a specialized coding assistant.",
                "toolkits": ["sandbox_operations"],
            },
        )

        system_prompt = self._chat_and_get_system_prompt(
            client, mock_client, agent_name="custom-prompt-agent"
        )

        assert system_prompt.startswith("You are a specialized coding assistant.")
        assert "<Toolkit Instructions>" not in system_prompt

    def test_default_agent_omits_available_skills_section(self, app_client):
        client, mock_client = app_client
        system_prompt = self._chat_and_get_system_prompt(client, mock_client)

        assert "<Available Skills>" not in system_prompt

    def test_agent_with_declared_skills_omits_available_skills_section(self, app_client):
        client, mock_client = app_client
        client.post(
            "/api/agents/",
            json={
                "name": "skills-agent",
                "description": "Has explicit skills",
                "model": "minimax",
                "skills": ["team-planner-playbook"],
            },
        )

        system_prompt = self._chat_and_get_system_prompt(
            client, mock_client, agent_name="skills-agent"
        )

        assert "<Available Skills>" not in system_prompt
        assert "team-planner-playbook" not in system_prompt
        assert "plan-json-contract" not in system_prompt

    def test_agent_without_skills_no_skills_section(self, app_client):
        client, mock_client = app_client
        client.post(
            "/api/agents/",
            json={
                "name": "no-skills-agent-2",
                "description": "No skills assigned",
                "model": "minimax",
                "skills": [],
            },
        )

        system_prompt = self._chat_and_get_system_prompt(
            client, mock_client, agent_name="no-skills-agent-2"
        )

        assert "<Available Skills>" not in system_prompt


# ---------------------------------------------------------------------------
# US-005: Minimax model key integration
# ---------------------------------------------------------------------------


class TestModelKeyIntegration:
    """Verify model key resolution for minimax and anthropic models."""

    def test_create_agent_with_minimax_model(self, app_client):
        client, _ = app_client
        resp = client.post(
            "/api/agents/",
            json={
                "name": "minimax-agent",
                "description": "Uses minimax model",
                "model": "minimax",
                "toolkits": ["sandbox_operations"],
            },
        )
        assert resp.status_code == 201
        assert resp.json()["model"] == "minimax"

    def test_create_agent_with_anthropic_model(self, app_client):
        client, _ = app_client
        resp = client.post(
            "/api/agents/",
            json={
                "name": "anthropic-agent",
                "description": "Uses anthropic model",
                "model": "claude-sonnet",
            },
        )
        assert resp.status_code == 201
        assert resp.json()["model"] == "claude-sonnet"

    def test_chat_uses_agent_model_key(self, app_client):
        """When chatting with a named agent, the engine should use the agent's model."""
        client, mock_client = app_client
        client.post(
            "/api/agents/",
            json={
                "name": "model-test-agent",
                "description": "Test model resolution",
                "model": "minimax",
            },
        )

        resp = client.post(
            "/api/chat",
            json={
                "line": "hello",
                "agent_name": "model-test-agent",
            },
        )
        assert resp.status_code == 200
        for line in resp.iter_lines():
            pass

        # The mock captured the request — check the model
        if mock_client.last_request:
            assert mock_client.last_request.model == "minimax"
