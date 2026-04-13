"""Tests for the structured agent prompt template system."""

from __future__ import annotations

from pathlib import Path

import pytest

from agents.loader import load_agents_dir
from agents.types import AgentDefinition
from prompts.agent_templates import (
    ROLE_TEMPLATES,
    TYPE_TEMPLATES,
    build_role_section,
    build_type_section,
)

_BUILTINS_DIR = Path(__file__).resolve().parents[2] / "config" / "agents"


# ---------------------------------------------------------------------------
# build_type_section
# ---------------------------------------------------------------------------


class TestBuildTypeSection:
    @pytest.mark.parametrize("agent_type", ["agent", "subagent"])
    def test_returns_non_empty_for_known_types(self, agent_type: str) -> None:
        result = build_type_section(agent_type, "test_agent")
        assert result, f"Expected non-empty section for type={agent_type}"
        assert "# Identity" in result
        assert "# Type Constraints" in result

    @pytest.mark.parametrize("agent_type", ["agent", "subagent"])
    def test_substitutes_agent_name(self, agent_type: str) -> None:
        result = build_type_section(agent_type, "my_custom_agent")
        assert "my_custom_agent" in result
        assert "{{name}}" not in result

    def test_subagent_includes_no_spawn_constraint(self) -> None:
        result = build_type_section("subagent", "scout")
        assert "Must not spawn subagents" in result

    def test_unknown_type_returns_empty(self) -> None:
        result = build_type_section("unknown_type", "test")
        assert result == ""


# ---------------------------------------------------------------------------
# build_role_section
# ---------------------------------------------------------------------------


class TestBuildRoleSection:
    @pytest.mark.parametrize("role", ["planner", "developer", "reviewer", "explorer", "replanner"])
    def test_returns_non_empty_for_known_roles(self, role: str) -> None:
        result = build_role_section(role)
        assert result, f"Expected non-empty section for role={role}"
        assert "# Role Boundary" in result

    def test_none_role_returns_empty(self) -> None:
        assert build_role_section(None) == ""

    def test_unknown_role_returns_empty(self) -> None:
        assert build_role_section("unknown_role") == ""

    def test_developer_scope_constraint(self) -> None:
        result = build_role_section("developer")
        assert "WorkItem payload" in result

    def test_reviewer_read_only_constraint(self) -> None:
        result = build_role_section("reviewer")
        assert "Must not modify" in result

    def test_planner_no_code_constraint(self) -> None:
        result = build_role_section("planner")
        assert "Do not execute code" in result

    def test_explorer_read_only(self) -> None:
        result = build_role_section("explorer")
        assert "read-only" in result


# ---------------------------------------------------------------------------
# Template coverage — every type and role has a template
# ---------------------------------------------------------------------------


class TestTemplateCoverage:
    def test_all_agent_types_have_templates(self) -> None:
        for t in ("agent", "subagent"):
            assert t in TYPE_TEMPLATES, f"Missing type template for {t}"

    def test_all_canonical_roles_have_templates(self) -> None:
        for r in ("planner", "developer", "reviewer", "explorer", "replanner"):
            assert r in ROLE_TEMPLATES, f"Missing role template for {r}"


# ---------------------------------------------------------------------------
# Integration: all builtins load and compose without error
# ---------------------------------------------------------------------------


class TestBuiltinIntegration:
    @pytest.fixture(scope="class")
    def builtin_defs(self) -> list[AgentDefinition]:
        return load_agents_dir(_BUILTINS_DIR)

    def test_all_builtins_get_type_section(self, builtin_defs: list[AgentDefinition]) -> None:
        for d in builtin_defs:
            section = build_type_section(d.agent_type, d.name)
            assert section, f"Agent {d.name} (type={d.agent_type}) got empty type section"
            assert d.name in section

    def test_role_agents_get_role_section(self, builtin_defs: list[AgentDefinition]) -> None:
        role_agents = [d for d in builtin_defs if d.role]
        assert len(role_agents) >= 4, "Expected at least 4 agents with roles"
        for d in role_agents:
            section = build_role_section(d.role)
            assert section, f"Agent {d.name} (role={d.role}) got empty role section"

    def test_md_bodies_no_longer_contain_identity_boilerplate(
        self, builtin_defs: list[AgentDefinition]
    ) -> None:
        """Verify .md bodies were slimmed — identity is now in type templates."""
        for d in builtin_defs:
            if not d.system_prompt:
                continue
            body = d.system_prompt
            assert f"You are {d.name}" not in body, (
                f"Agent {d.name} .md body still contains identity sentence"
            )

    def test_full_prompt_assembly_order(self, builtin_defs: list[AgentDefinition]) -> None:
        """Verify type section comes before role section comes before body."""
        for d in builtin_defs:
            if not d.system_prompt:
                continue
            parts = []
            ts = build_type_section(d.agent_type, d.name)
            if ts:
                parts.append(ts)
            rs = build_role_section(d.role)
            if rs:
                parts.append(rs)
            parts.append(d.system_prompt)
            assembled = "\n\n".join(parts)

            # Type section should be first, body last
            type_pos = assembled.index("# Identity")
            body_pos = assembled.index(d.system_prompt[:40])
            assert type_pos < body_pos, (
                f"Agent {d.name}: type section should precede body"
            )

            # If role exists, it should be between type and body
            if d.role:
                role_pos = assembled.index("# Role Boundary")
                assert type_pos < role_pos < body_pos, (
                    f"Agent {d.name}: role section should be between type and body"
                )
