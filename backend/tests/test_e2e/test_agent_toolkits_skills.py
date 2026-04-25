"""E2E tests for empty-by-default agent and skill APIs."""

from __future__ import annotations

import pytest

pytestmark = pytest.mark.e2e

SANDBOX_TOOLS = {
    "daytona_grep",
    "daytona_glob",
    "daytona_read_file",
    "daytona_write_file",
    "daytona_edit_file",
    "daytona_delete_file",
    "daytona_move_file",
    "daytona_shell",
}


class TestInfrastructure:
    """Verify the test infrastructure and default runtime surface."""

    def test_health_check(self, app_client):
        client, _ = app_client
        resp = client.get("/api/health")

        assert resp.status_code == 200
        assert resp.json()["status"] == "ok"

    def test_state_endpoint(self, app_client):
        client, _ = app_client
        resp = client.get("/api/state")

        assert resp.status_code == 200
        data = resp.json()
        assert data["type"] == "ready"
        assert data["state"] is not None
        tool_names = {entry["name"] for entry in data["tools"]}
        assert "submit_task_success" not in tool_names
        assert "submit_plan" not in tool_names
        assert "submit_replan" not in tool_names
        assert "submit_task_plan" not in tool_names
        assert "declare_blocker" not in tool_names
        assert "load_skill" in tool_names
        assert "check_background_progress" in tool_names


class TestConfigBackedAgentApi:
    """Agent definitions are listed from backend/config and are read-only."""

    def test_list_agents_shows_config_definitions(self, app_client):
        client, _ = app_client
        resp = client.get("/api/agents/")

        assert resp.status_code == 200
        assert resp.json() == []

    def test_get_removed_config_agent_returns_not_found(self, app_client):
        client, _ = app_client
        resp = client.get("/api/agents/developer")

        assert resp.status_code == 404

    def test_list_available_tools(self, app_client):
        client, _ = app_client
        resp = client.get("/api/agents/tools/available")

        assert resp.status_code == 200
        tools = {entry["name"] for entry in resp.json()}
        assert "daytona_shell" in tools
        assert "submit_plan" not in tools
        assert "submit_replan" not in tools
        assert "submit_task_success" not in tools
        assert "request_replan" not in tools
        assert "load_skill" in tools
        assert "check_background_progress" in tools

    @pytest.mark.parametrize(
        ("method", "path"),
        [
            ("post", "/api/agents/"),
            ("put", "/api/agents/developer"),
            ("delete", "/api/agents/developer"),
            ("post", "/api/agents/developer/clone"),
        ],
    )
    def test_mutating_agent_definition_endpoints_are_read_only(
        self,
        app_client,
        method: str,
        path: str,
    ):
        client, _ = app_client
        resp = client.request(
            method.upper(),
            path,
            json={
                "name": "ignored",
                "description": "ignored",
                "model": "minimax",
                "tools": sorted(SANDBOX_TOOLS),
            },
        )

        assert resp.status_code == 405
        assert "file-backed under backend/config/agents" in resp.json()["detail"]

    def test_validate_agent_allows_former_builtin_name(self, app_client):
        client, _ = app_client
        resp = client.post(
            "/api/agents/validate",
            json={
                "name": "team_planner",
                "description": "reserved",
                "model": "inherit",
                "tools": ["ci_query_symbol"],
            },
        )

        assert resp.status_code == 200
        data = resp.json()
        assert data["valid"] is True
        assert data["errors"] == []

    def test_validate_agent_rejects_unknown_tool(self, app_client):
        client, _ = app_client
        resp = client.post(
            "/api/agents/validate",
            json={
                "name": "custom_agent",
                "description": "custom",
                "model": "inherit",
                "tools": ["does_not_exist"],
            },
        )

        assert resp.status_code == 200
        assert "Unknown tool: does_not_exist" in resp.json()["errors"]


class TestConfigBackedSkillApi:
    """Skill definitions are listed from backend/config and are read-only."""

    def test_list_skills_shows_config_playbooks(self, app_client):
        client, _ = app_client
        resp = client.get("/api/skills/")

        assert resp.status_code == 200
        assert resp.json() == []

    def test_get_removed_skill_returns_not_found(self, app_client):
        client, _ = app_client
        resp = client.get("/api/skills/removed-playbook")

        assert resp.status_code == 404

    @pytest.mark.parametrize(
        ("method", "path"),
        [
            ("post", "/api/skills/"),
            ("put", "/api/skills/removed-playbook"),
            ("delete", "/api/skills/removed-playbook"),
        ],
    )
    def test_mutating_skill_definition_endpoints_are_read_only(
        self,
        app_client,
        method: str,
        path: str,
    ):
        client, _ = app_client
        resp = client.request(
            method.upper(),
            path,
            json={"name": "ignored", "description": "ignored", "content": "ignored"},
        )

        assert resp.status_code == 405
        assert "file-backed under backend/config/skills" in resp.json()["detail"]

    def test_removed_skill_files_return_not_found(self, app_client):
        client, _ = app_client
        resp = client.get("/api/skills/removed-playbook/files")

        assert resp.status_code == 200
        assert resp.json()["tree"] == []

        file_resp = client.get("/api/skills/removed-playbook/files/SKILL.md")
        assert file_resp.status_code == 404
