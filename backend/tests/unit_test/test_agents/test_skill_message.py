"""Round 3 Phase 5: row-4 composite message format."""

from __future__ import annotations

from pathlib import Path

from agents import AgentDefinition, AgentKind
from task_center.context_engine.core import build_skill_message
from tools._terminals.registry import render_terminal_catalog


def _make_planner_def(terminals: list[str] | None = None) -> AgentDefinition:
    return AgentDefinition(
        name="planner",
        description="planner",
        agent_kind=AgentKind.PLANNER,
        context_recipe="planner",
        terminals=terminals
        if terminals is not None
        else ["submit_plan_closes_goal", "submit_plan_continues_goal"],
    )


def test_build_skill_message_returns_none_when_no_skill():
    msg = build_skill_message(None, _make_planner_def())
    assert msg is None


def test_build_skill_message_has_load_header_skill_block_and_terminal_block(
    tmp_path: Path,
):
    skill_file = tmp_path / "planner" / "SKILL.md"
    skill_file.parent.mkdir()
    skill_file.write_text(
        "---\nname: planner\n---\n\n# Planner skill\n\nWorkflow text."
    )
    msg = build_skill_message(skill_file, _make_planner_def())
    assert msg is not None

    assert msg.startswith("Load skill: planner\n")
    assert "<skill>" in msg and "</skill>" in msg
    assert "<terminal_selection>" in msg and "</terminal_selection>" in msg

    # Frontmatter is stripped — the `name:` key MUST NOT appear in the body.
    skill_block = msg.split("<skill>", 1)[1].split("</skill>", 1)[0]
    assert "name: planner" not in skill_block
    assert "# Planner skill" in skill_block
    assert "Workflow text." in skill_block


def test_terminal_selection_block_matches_registry_catalog(tmp_path: Path):
    """Row 4's <terminal_selection> renders from the same registry call as row 3."""
    skill_file = tmp_path / "planner" / "SKILL.md"
    skill_file.parent.mkdir()
    skill_file.write_text("# body")
    planner_def = _make_planner_def(
        terminals=["submit_plan_closes_goal", "submit_plan_continues_goal"]
    )

    msg = build_skill_message(skill_file, planner_def)
    assert msg is not None

    expected_catalog = render_terminal_catalog(
        list(planner_def.terminals), focus="selection_guidance"
    )
    block = msg.split("<terminal_selection>", 1)[1].split(
        "</terminal_selection>", 1
    )[0]
    assert expected_catalog in block, (
        "row-4 <terminal_selection> must include the same catalog text as row 3"
    )


def test_terminal_selection_block_skipped_when_no_terminals(tmp_path: Path):
    skill_file = tmp_path / "planner" / "SKILL.md"
    skill_file.parent.mkdir()
    skill_file.write_text("# body")
    plain_def = AgentDefinition(
        name="plain",
        description="plain",
        agent_kind=AgentKind.PLANNER,
        terminals=[],
    )

    msg = build_skill_message(skill_file, plain_def)
    assert msg is not None
    assert "<terminal_selection>" not in msg


def test_skill_name_derives_from_parent_folder(tmp_path: Path):
    """Load skill: <name> uses skill_path.parent.name as the slug."""
    skill_file = tmp_path / "planner_full_only" / "SKILL.md"
    skill_file.parent.mkdir()
    skill_file.write_text("# body")
    msg = build_skill_message(skill_file, _make_planner_def())
    assert msg is not None
    assert msg.startswith("Load skill: planner_full_only\n")
