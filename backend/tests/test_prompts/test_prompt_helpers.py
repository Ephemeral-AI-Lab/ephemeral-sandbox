from __future__ import annotations

import sys
from pathlib import Path
from types import SimpleNamespace

from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

from agents.db.model import AgentDefinitionRecord
from agents.db.store import AgentDefinitionStore
from agents.types import AgentDefinition
from config.settings import Settings
from team.persistence.model import TeamDefinitionRecord
from team.persistence.store import TeamDefinitionStore
from team.models import BudgetConfig, Task, TaskStatus, TeamDefinition
from team.persistence.events import make_note_posted, make_task_added, make_team_run_created, task_to_dict
from team.runtime.tool_policy import get_role_tool_policy


def default_terminal_tools_for_role(role: str | None) -> set[str]:
    policy = get_role_tool_policy(role)
    if policy is None:
        return set()
    return set(policy.allowed_submission_tools)

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
            "task_center_note_taker": ["note_taker"],
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
    assert "Goal\nImplement the bounded code change" in report
    assert "Acceptance Criteria\n- Keep edits inside the assigned scope." in report
    assert "## Agent: note_taker" in report
    assert "### Edit Trigger" in report
    assert "Call submit_task_note" in report


def _team_system_prompt_for(agent_name: str, tmp_path: Path) -> str:
    agent_def = load_agent_definition(agent_name, Settings())
    assert agent_def is not None
    return build_agent_system_prompt_text(
        agent_def,
        cwd=str(tmp_path),
        settings=Settings(),
        sandbox_id="sb-test",
        terminal_tools=default_terminal_tools_for_role(agent_def.role),
    )


def test_team_system_prompts_include_only_terminal_guidance(tmp_path: Path) -> None:
    register_builtins()

    planner = _team_system_prompt_for("team_planner", tmp_path)
    replanner = _team_system_prompt_for("team_replanner", tmp_path)
    scout = _team_system_prompt_for("scout", tmp_path)
    validator = _team_system_prompt_for("validator", tmp_path)

    for prompt in (planner, replanner, scout, validator):
        assert "<Toolkit Instructions>" not in prompt
        assert "<Available Skills>" not in prompt
        assert "<Background Tasks>" not in prompt
        assert "sandbox_operations" not in prompt
        assert "daytona_" not in prompt

    assert "submit_plan" in planner
    assert "submit_task_success" not in planner
    assert "submit_replan" not in planner
    assert "submit_task_note" not in planner

    assert "submit_replan" in replanner
    assert "submit_task_success" not in replanner
    assert "submit_plan" not in replanner
    assert "submit_task_note" not in replanner

    assert "<Termination Condition>" not in scout
    assert "submit_file_notes" in scout
    assert "submit_task_note" not in scout
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


def test_db_seeded_custom_team_system_prompts_hide_forbidden_tools(
    tmp_path: Path,
    monkeypatch,
) -> None:
    engine = create_engine("sqlite:///:memory:", echo=False)
    AgentDefinitionRecord.__table__.create(engine, checkfirst=True)
    TeamDefinitionRecord.__table__.create(engine, checkfirst=True)
    session_factory = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)

    agent_store = AgentDefinitionStore()
    agent_store.initialize(session_factory)
    team_store = TeamDefinitionStore()
    team_store.initialize(session_factory)

    for defn in (
        AgentDefinition(
            name="db_planner",
            description="DB planner",
            system_prompt="Plan only. Call `submit_plan(...)` when ready.",
            role="planner",
            model="inherit",
            toolkits=["code_intelligence", "submission"],
            blocked_tools=["submit_task_note"],
            include_skills=False,
        ),
        AgentDefinition(
            name="db_replanner",
            description="DB replanner",
            system_prompt="Replan only. Call `submit_replan(...)` when ready.",
            role="replanner",
            model="inherit",
            toolkits=["code_intelligence", "submission"],
            blocked_tools=["submit_task_note"],
            include_skills=False,
        ),
        AgentDefinition(
            name="db_scout",
            description="DB scout",
            system_prompt="Explore without editing and post `submit_file_notes(...)`.",
            role="explorer",
            model="inherit",
            agent_type="subagent",
            toolkits=["code_intelligence", "task_center"],
            blocked_tools=["submit_task_note"],
            include_skills=False,
        ),
        AgentDefinition(
            name="db_validator",
            description="DB validator",
            system_prompt="Verify, apply small local fixes when obvious, otherwise fail.",
            role="reviewer",
            model="inherit",
            toolkits=["sandbox_operations", "code_intelligence", "submission"],
            include_skills=False,
        ),
    ):
        agent_store.seed_builtin(defn)

    stored_team = team_store.create(
        name="db-custom-team",
        entry_planner="db_planner",
        roster={
            "planner": ["db_planner"],
            "replanner": ["db_replanner"],
            "explorer": ["db_scout"],
            "reviewer": ["db_validator"],
        },
        description="DB-backed custom prompt report team",
    )
    monkeypatch.setattr("db.engine.initialize_db", lambda *_args, **_kwargs: session_factory)

    loaded_team = load_team_definition(stored_team.id, Settings())
    assert loaded_team is not None
    report, missing = _render_team_prompt_report(
        team_def=loaded_team,
        cwd=str(tmp_path),
        sandbox_id="sb-test",
        include_runtime_sections=True,
        settings=Settings(),
    )

    assert missing == []
    planner = _agent_section(report, "db_planner")
    replanner = _agent_section(report, "db_replanner")
    scout = _agent_section(report, "db_scout")
    validator = _agent_section(report, "db_validator")

    for section in (planner, replanner, scout, validator):
        assert "<Toolkit Instructions>" not in section
        assert "<Available Skills>" not in section
        assert "<Background Tasks>" not in section
        assert "sandbox_operations" not in section
        assert "daytona_" not in section

    assert "submit_plan" in planner
    assert "submit_task_success" not in planner
    assert "submit_replan" not in planner
    assert "submit_task_note" not in planner

    assert "submit_replan" in replanner
    assert "submit_task_success" not in replanner
    assert "submit_plan" not in replanner
    assert "submit_task_note" not in replanner

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
        objective="Fix retry behavior.",
        root_id="root",
        depth=0,
    )
    dev = Task(
        id="dev-1",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.READY,
        objective="Implement the retry fix.",
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
        make_note_posted(
            "run-1",
            task_id="root",
            agent_name="team_planner",
            auto=False,
            scope_paths=["backend/src/retry.py"],
            content_preview="Planner assigned retry implementation.",
            content_bytes=39,
        ),
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
    assert "- Note previews restored: `1`" in report
    assert "### Task: dev-1" in report
    assert "Implement the retry fix." in report


def test_build_team_run_user_prompt_report_replays_legacy_task_field(tmp_path: Path) -> None:
    register_builtins()
    root = Task(
        id="root",
        team_run_id="run-legacy",
        agent_name="team_planner",
        status=TaskStatus.DONE,
        objective="Fix retry behavior.",
        root_id="root",
        depth=0,
    )
    payload = task_to_dict(root)
    payload["task"] = payload.pop("objective")
    events = [
        make_team_run_created(
            "run-legacy",
            session_id="session-1",
            user_request="Fix retry behavior.",
            goal=None,
            repo_root=str(tmp_path),
            budgets=BudgetConfig().__dict__,
            roster={"planner": ["team_planner"]},
        ),
        make_task_added("run-legacy", payload),
    ]

    report, missing = build_team_run_user_prompt_report_text_sync(
        team_run_id="run-legacy",
        events=events,
        cwd=str(tmp_path),
        settings=Settings(),
    )

    assert missing == []
    assert "Fix retry behavior." in report


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
