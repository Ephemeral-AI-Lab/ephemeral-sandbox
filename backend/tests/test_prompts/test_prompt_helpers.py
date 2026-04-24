from __future__ import annotations

import sys
from pathlib import Path
from types import SimpleNamespace

from agents.registry import register_definition, unregister_definition
from agents.types import AgentDefinition
from config.settings import Settings
from team.core.models import BudgetConfig, Task, TaskStatus, TeamDefinition
from team.persistence.events import make_task_added, make_team_run_created, task_to_dict
from team.definitions import register_team_definition, unregister_team_definition
_ROOT = Path(__file__).resolve().parents[3]
_SCRIPTS_DIR = _ROOT / "scripts"
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

from prompt.helpers import (  # noqa: E402
    _rendered_skill_content,
    build_agent_system_prompt_text,
    build_team_run_user_prompt_report_text_sync,
    build_team_user_prompt_report_text_sync,
    default_team_run_prompt_report_path,
    default_team_user_prompt_report_path,
    load_agent_definition,
    load_team_definition,
    register_builtins,
)
from prompt.prompt_cli import _render_team_prompt_report  # noqa: E402


def _spec(goal: str) -> dict[str, str]:
    return {
        "goal": goal,
        "detail": f"Detail for {goal}",
        "acceptance_criteria": f"Acceptance for {goal}",
    }


def test_build_team_user_prompt_report_uses_runtime_context_path(tmp_path: Path) -> None:
    register_builtins()
    team_def = TeamDefinition(
        id="team-12345678",
        name="demo team",
        description="demo",
        entry_planner="team_planner",
        roster={
            "planner": ["team_planner"],
            "developer": ["developer"],
            "reviewer": ["validator"],
        },
    )

    report, missing = build_team_user_prompt_report_text_sync(
        team_def,
        user_request="Fix the login retry behavior.",
        cwd=str(tmp_path),
        settings=Settings(),
    )

    assert missing == []
    assert "# Team User Prompts: demo team" in report
    assert "- Source: representative synthetic task graph rendered through `build_query_context`." in report
    assert "## Agent: team_planner" in report
    assert "## Available Agents" not in report
    assert "Fix the login retry behavior." in report
    assert "## Agent: developer" in report
    assert "# Goal\n\nImplement the bounded code change" in report
    assert "# Acceptance Criteria\n\n- Keep edits inside the assigned scope." in report
    assert "## Agent: validator" in report
    assert "Verify the implementation evidence and report pass or fail." in report


def _team_system_prompt_for(agent_name: str, tmp_path: Path) -> str:
    agent_def = load_agent_definition(agent_name, Settings())
    assert agent_def is not None
    return build_agent_system_prompt_text(
        agent_def,
        cwd=str(tmp_path),
        settings=Settings(),
        sandbox_id="sb-test",
        terminal_tools=set(agent_def.terminal_tools or []),
    )


def test_team_system_prompts_include_only_terminal_guidance(tmp_path: Path) -> None:
    register_builtins()

    planner = _team_system_prompt_for("team_planner", tmp_path)
    replanner = _team_system_prompt_for("team_replanner", tmp_path)
    scout = _team_system_prompt_for("scout", tmp_path)
    validator = _team_system_prompt_for("validator", tmp_path)

    for prompt in (planner, replanner, scout, validator):
        assert "<Available Skills>" not in prompt
        assert "<Background Tasks>" not in prompt
        assert "sandbox_operations" not in prompt
        assert "daytona_" not in prompt

    assert "submit_plan" in planner
    assert "submit_task_success" not in planner
    assert "submit_replan" not in planner

    assert "submit_replan" in replanner
    assert "submit_task_success" not in replanner
    assert "submit_plan" not in replanner

    assert "<Termination Condition>" not in scout
    assert "submit_file_notes" in scout
    assert "submit_task_success" not in scout
    assert "submit_plan" not in scout
    assert "submit_replan" not in scout

    assert "sandbox_operations" not in validator
    assert "daytona_edit_file" not in validator
    assert "daytona_shell" not in validator
    assert "submit_task_success" in validator


def _agent_section(report: str, agent_name: str) -> str:
    start = report.index(f"## Agent: {agent_name}")
    end = report.find("\n## Agent:", start + 1)
    return report[start:] if end == -1 else report[start:end]


def test_config_registered_custom_team_system_prompts_hide_forbidden_tools(
    tmp_path: Path,
) -> None:
    custom_agents = (
        AgentDefinition(
            name="config_planner",
            description="Config planner",
            system_prompt="Plan only. Call `submit_plan(...)` when ready.",
            role="planner",
            model="inherit",
            tools=["ci_query_symbol", "submit_plan", "submit_replan"],
            terminal_tools=["submit_plan"],
            include_skills=False,
        ),
        AgentDefinition(
            name="config_replanner",
            description="Config replanner",
            system_prompt="Replan only. Call `submit_replan(...)` when ready.",
            role="replanner",
            model="inherit",
            tools=["ci_query_symbol", "submit_plan", "submit_replan"],
            terminal_tools=["submit_replan"],
            include_skills=False,
        ),
        AgentDefinition(
            name="config_scout",
            description="Config scout",
            system_prompt="Explore without editing and post `submit_file_notes(...)`.",
            role="explorer",
            model="inherit",
            agent_type="subagent",
            tools=["ci_query_symbol", "submit_file_notes"],
            include_skills=False,
        ),
        AgentDefinition(
            name="config_validator",
            description="Config validator",
            system_prompt="Verify, apply small local fixes when obvious, otherwise fail.",
            role="reviewer",
            model="inherit",
            tools=[
                "daytona_read_file",
                "daytona_shell",
                "ci_query_symbol",
                "submit_task_success",
                "request_replan",
            ],
            terminal_tools=["submit_task_success", "request_replan"],
            include_skills=False,
        ),
    )
    for defn in custom_agents:
        register_definition(defn)

    team_def = TeamDefinition(
        id="config-custom-team-id",
        name="config-custom-team",
        roster={
            "planner": ["config_planner"],
            "replanner": ["config_replanner"],
            "explorer": ["config_scout"],
            "reviewer": ["config_validator"],
        },
        entry_planner="config_planner",
        description="Config-backed custom prompt report team",
    )
    register_team_definition(team_def)

    try:
        loaded_team = load_team_definition(team_def.id, Settings())
        assert loaded_team is not None
        report, missing = _render_team_prompt_report(
            team_def=loaded_team,
            cwd=str(tmp_path),
            sandbox_id="sb-test",
            include_runtime_sections=True,
            settings=Settings(),
        )
    finally:
        unregister_team_definition(team_def.name)
        for defn in custom_agents:
            unregister_definition(defn.name)

    assert missing == []
    planner = _agent_section(report, "config_planner")
    replanner = _agent_section(report, "config_replanner")
    scout = _agent_section(report, "config_scout")
    validator = _agent_section(report, "config_validator")

    for section in (planner, replanner, scout, validator):
        assert "<Available Skills>" not in section
        assert "<Background Tasks>" not in section
        assert "sandbox_operations" not in section
        assert "daytona_" not in section

    assert "submit_plan" in planner
    assert "submit_task_success" not in planner
    assert "submit_replan" not in planner

    assert "submit_replan" in replanner
    assert "submit_task_success" not in replanner
    assert "submit_plan" not in replanner

    assert "submit_file_notes" in scout
    assert "submit_task_success" not in scout
    assert "submit_plan" not in scout
    assert "submit_replan" not in scout

    assert "sandbox_operations" not in validator
    assert "daytona_edit_file" not in validator
    assert "daytona_shell" not in validator
    assert "submit_task_success" in validator


def test_default_team_user_prompt_report_path_uses_team_prefix() -> None:
    team_def = TeamDefinition(
        id="abcdef123456",
        name="Demo Team",
        description="demo",
        entry_planner="team_planner",
    )

    path = default_team_user_prompt_report_path(team_def, output_dir="/tmp")

    assert str(path) == "/tmp/team-user-prompts-Demo-Team-abcdef12.md"


def test_build_team_run_user_prompt_report_replays_persisted_tasks(tmp_path: Path) -> None:
    register_builtins()
    root = Task(
        id="root",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.DONE,
        spec=_spec("Fix retry behavior."),
        root_id="root",
        depth=0,
    )
    dev = Task(
        id="dev-1",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.READY,
        spec=_spec("Implement the retry fix."),
        deps=["root"],
        scope_paths=["backend/src/retry.py"],
        parent_id="root",
        root_id="root",
        depth=1,
    )
    events = [
        make_team_run_created(
            "run-1",
            session_id="session-1",
            user_request="Fix retry behavior.",
            goal=None,
            repo_root=str(tmp_path),
            budgets=BudgetConfig().__dict__,
            roster={
                "planner": ["team_planner"],
                "developer": ["developer"],
            },
        ),
        make_task_added("run-1", task_to_dict(root)),
        make_task_added("run-1", task_to_dict(dev)),
    ]

    report, missing = build_team_run_user_prompt_report_text_sync(
        team_run_id="run-1",
        events=events,
        cwd=str(tmp_path),
        settings=Settings(),
    )

    assert missing == []
    assert "# Team Run User Prompts: run-1" in report
    assert "- Task count: `2`" in report
    assert "### Task: dev-1" in report
    assert "Implement the retry fix." in report


def test_default_team_run_prompt_report_path_uses_run_prefix() -> None:
    path = default_team_run_prompt_report_path("run/with spaces", output_dir="/tmp")

    assert str(path) == "/tmp/team-run-user-prompts-run-with-spaces.md"


def test_rendered_skill_content_does_not_append_reference_footer() -> None:
    skill = SimpleNamespace(
        content="# Demo\n\nUse the main workflow.",
        references={"extra": "Supplementary guidance."},
    )

    rendered = _rendered_skill_content(skill)

    assert rendered == "# Demo\n\nUse the main workflow."
    assert "This skill has" not in rendered
    assert "Use `load_skill_reference` to load any of them." not in rendered
