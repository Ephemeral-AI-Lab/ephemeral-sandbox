from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

import pytest

from agents.registry import get_definition
from prompt.user_prompt_templates import render_user_prompt_template
from team.definitions import register_all
from team.core.models import BudgetConfig, Task, TaskStatus
from team.runtime.agent_context import build_query_context
from team.task_center.prompts import TaskContextBuilder


_PROMPT_DIR = Path(__file__).resolve().parents[2] / "src" / "prompt" / "user_prompt"
_SUBMIT_PLAN_SCHEMA_SNIPPET = (
    "Submits initial child tasks for the current planner"
)


def _spec(goal: str) -> dict[str, str]:
    return {
        "goal": goal,
        "detail": f"Detail for {goal}",
        "acceptance_criteria": f"Acceptance for {goal}",
    }


def test_user_prompt_markdown_files_start_at_runtime_template() -> None:
    for name in (
        "developer",
        "root_task_planner",
        "task_planner",
        "task_replanner",
        "validator",
    ):
        assert (_PROMPT_DIR / f"{name}.md").read_text(encoding="utf-8").startswith(
            "Please read the following sections"
        )

def test_render_user_prompt_template_uses_markdown_file_conditionals() -> None:
    rendered = render_user_prompt_template(
        "developer",
        {
            "task_spec": "Goal\nImplement retry handling.",
            "scope_paths": "- backend/src/retry.py",
            "terminal_tools": "- submit_task_success: Submit task outcome.",
            "your_task_id": "dev-uuid-1234",
            "your_deps_ids": "`dep-a`, `dep-b`",
            "your_parent_task_id": "parent-uuid",
        },
    )

    assert rendered.startswith("Please read the following sections")
    assert "- submit_task_success: Submit task outcome." in rendered
    assert "Your task id: `dev-uuid-1234`" in rendered
    assert "Your dependency task ids: `dep-a`, `dep-b`" in rendered
    assert "Your parent task id: `parent-uuid`" in rendered
    assert (
        'Your first assistant action must contain exactly one tool call: `load_skill(skill_name="team-developer-playbook")`'
        in rendered
    )
    assert "call `read_task_details` with only one input key, `task_id`" in rendered
    assert "Do not pass `skill_name`, planner slugs" in rendered
    assert "Use `daytona_shell(command=\"...\")` for shell, build, and test commands" in rendered
    assert "daytona_shell commands already start at the sandbox repo root" in rendered
    assert "never prefix them with a host/local workspace path" in rendered
    assert "Use repo-relative paths" in rendered
    assert "Package/environment mutation is forbidden" in rendered
    assert "Do not run `pip install`, `uv add`, `uv sync`" in rendered
    assert "Mandatory daytona_shell preflight" not in rendered
    assert "Remove shell redirects and output filters entirely" not in rendered
    assert "Do not rely on sanitizer behavior as your normal workflow" not in rendered
    assert "`scope_paths` are the primary ownership surface, not a hard mutation sandbox" in rendered
    assert "Developers may write, copy, or create production files outside `scope_paths`" in rendered
    assert "outside-scope system notification" in rendered
    assert "that notification is not a stop condition" in rendered
    assert "clearly a different owner or too broad/ambiguous for this lane" in rendered
    assert "latest required runtime verification command was run after the final edit and passed" in rendered
    assert "not run due to budget" in rendered
    assert "means `request_replan(reason=...)`, not success" in rendered
    assert "Task id: `dev-uuid-1234`" not in rendered
    assert "Dependency task ids: `dep-a`, `dep-b`" not in rendered
    assert "Parent task id: `parent-uuid`" not in rendered
    assert "Follow the bundled developer playbook for workflow and rules" in rendered
    assert "## Rule to Follow" not in rendered
    assert "## Assigned coding task" in rendered
    assert "Goal\nImplement retry handling." in rendered
    assert "## scope_paths\n- backend/src/retry.py" in rendered
    assert "scope_paths" in rendered
    assert "Benchmark and verification test files in this list are read/verify-only" not in rendered
    assert "If live evidence identifies a missing module, compatibility shim" not in rendered
    assert "source path inside `scope_paths` does not by itself authorize" not in rendered
    assert "compatibility shim, re-export, bridge, test edit, or other unassigned path outside `scope_paths`" not in rendered
    assert 'request_replan(, content=...)' not in rendered
    assert "## Context from dependencies" not in rendered
    assert "## Parent context" not in rendered
    assert "Tool-name contract" not in rendered
    assert "Run daytona_shell commands directly from repo root" not in rendered

async def _make_task_center(
    team_run_id: str,
    tasks: dict[str, Task],
) -> SimpleNamespace:
    async def _get_task(task_id: str) -> Task | None:
        return tasks.get(task_id)

    context = TaskContextBuilder(
        team_run_id=team_run_id,
        get_task_fn=_get_task,
        task_store=SimpleNamespace(graph=tasks),
    )
    return SimpleNamespace(context=context, graph=tasks)


@pytest.mark.asyncio
async def test_build_query_context_uses_developer_markdown_template() -> None:
    register_all()
    dep = Task(
        id="dep-1",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.DONE,
        spec=_spec("Prepare retry helper."),
        parent_id="root",
        root_id="root",
        depth=1,
    )
    task = Task(
        id="dev-1",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.READY,
        spec=_spec("Goal\nImplement retry handling."),
        deps=["dep-1"],
        scope_paths=["backend/src/retry.py"],
        parent_id="root",
        root_id="root",
        depth=1,
    )
    team_run = SimpleNamespace(
        id="run-1",
        user_request="Fix retry handling.",
        root_task_id="root",
        task_center=await _make_task_center("run-1", {"dep-1": dep, "dev-1": task}),
        roster={"developer": ["developer"]},
        team_definition=None,
        project_context=SimpleNamespace(repo_root="/repo"),
        coordination_metadata={},
        budgets=None,
        budget_state=None,
        sandbox_id="",
        arbiter=None,
    )

    ctx = await build_query_context(get_definition("developer"), team_run, task)

    assert ctx.user_message.startswith("Please read the following sections")
    assert "- submit_task_success:" in ctx.user_message
    assert "Your task id: `dev-1`" in ctx.user_message
    assert "Your dependency task ids: `dep-1`" in ctx.user_message
    assert "Your parent task id: `root`" in ctx.user_message
    assert (
        'Your first assistant action must contain exactly one tool call: `load_skill(skill_name="team-developer-playbook")`'
        in ctx.user_message
    )
    assert "call `read_task_details` with only one input key, `task_id`" in ctx.user_message
    assert "Do not pass `skill_name`, planner slugs" in ctx.user_message
    assert "Use `daytona_shell(command=\"...\")` for shell, build, and test commands" in ctx.user_message
    assert "daytona_shell commands already start at the sandbox repo root" in ctx.user_message
    assert "never prefix them with a host/local workspace path" in ctx.user_message
    assert "Use repo-relative paths" in ctx.user_message
    assert "Package/environment mutation is forbidden" in ctx.user_message
    assert "Do not run `pip install`, `uv add`, `uv sync`" in ctx.user_message
    assert "treat the advisory as workflow guidance" not in ctx.user_message
    assert "`scope_paths` are the primary ownership surface, not a hard mutation sandbox" in ctx.user_message
    assert "Developers may write, copy, or create production files outside `scope_paths`" in ctx.user_message
    assert "outside-scope system notification" in ctx.user_message
    assert "that notification is not a stop condition" in ctx.user_message
    assert "clearly a different owner or too broad/ambiguous for this lane" in ctx.user_message
    assert (
        "latest required runtime verification command was run after the final edit and passed"
        in ctx.user_message
    )
    assert "not run due to budget" in ctx.user_message
    assert "means `request_replan(reason=...)`, not success" in ctx.user_message
    assert "Task id: `dev-1`" not in ctx.user_message
    assert "Dependency task ids: `dep-1`" not in ctx.user_message
    assert "Parent task id: `root`" not in ctx.user_message
    assert "Follow the bundled developer playbook for workflow and rules" in ctx.user_message
    assert "## Rule to Follow" not in ctx.user_message
    assert "## Assigned coding task" in ctx.user_message
    assert "Goal\nImplement retry handling." in ctx.user_message
    assert "## scope_paths\n- backend/src/retry.py" in ctx.user_message
    assert "Benchmark and verification test files in this list are read/verify-only" not in ctx.user_message
    assert "missing module, compatibility shim, re-export, import bridge" not in ctx.user_message
    assert "source path inside `scope_paths` does not by itself authorize" not in ctx.user_message
    assert "compatibility shim, re-export, bridge, test edit, or other unassigned path outside `scope_paths`" not in ctx.user_message
    assert "observability evidence" not in ctx.user_message
    assert "## Context from dependencies" not in ctx.user_message
    assert "## Parent context" not in ctx.user_message


@pytest.mark.asyncio
async def test_build_query_context_uses_validator_markdown_template_with_task_ids() -> None:
    register_all()
    dep = Task(
        id="dev-1",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.DONE,
        spec=_spec("Implement retry handling."),
        parent_id="root",
        root_id="root",
        depth=1,
    )
    task = Task(
        id="validator-1",
        team_run_id="run-1",
        agent_name="validator",
        status=TaskStatus.READY,
        spec=_spec("Validate retry handling."),
        deps=["dev-1"],
        scope_paths=["backend/src/retry.py"],
        parent_id="root",
        root_id="root",
        depth=1,
    )
    team_run = SimpleNamespace(
        id="run-1",
        user_request="Fix retry handling.",
        root_task_id="root",
        task_center=await _make_task_center("run-1", {"dev-1": dep, "validator-1": task}),
        roster={"validator": ["validator"], "developer": ["developer"]},
        team_definition=None,
        project_context=SimpleNamespace(repo_root="/repo"),
        coordination_metadata={},
        budgets=None,
        budget_state=None,
        sandbox_id="",
        arbiter=None,
    )

    ctx = await build_query_context(get_definition("validator"), team_run, task)

    assert ctx.user_message.startswith("Please read the following sections")
    assert "- submit_task_success:" in ctx.user_message
    assert "Your task id: `validator-1`" in ctx.user_message
    assert "Your dependency task ids: `dev-1`" in ctx.user_message
    assert "Your parent task id: `root`" in ctx.user_message
    assert "Context-read pre-step: after loading the validator playbook" in ctx.user_message
    assert "Use `daytona_shell(command=\"...\")` for shell, build, and test commands" in ctx.user_message
    assert "Mandatory daytona_shell preflight" not in ctx.user_message
    assert "Remove shell redirects and output filters entirely" not in ctx.user_message
    assert "Do not rely on sanitizer behavior as your normal workflow" not in ctx.user_message
    assert "correction surface for existing files, renames, moves, and deletes" in ctx.user_message
    assert "Creating a new production file with `daytona_write_file` may extend scope" in ctx.user_message
    assert "rely on the write-scope posthook to approve and record the expansion" in ctx.user_message
    assert "Do not run duplicate equivalent verification commands in parallel" in ctx.user_message
    assert "A success verdict may cite only commands actually run after the final validator edit" in ctx.user_message
    assert "load_skill_reference" not in ctx.user_message
    assert "runtime-" "verification-examples" not in ctx.user_message
    assert "Task id: `validator-1`" not in ctx.user_message
    assert "Dependency task ids: `dev-1`" not in ctx.user_message
    assert "Parent task id: `root`" not in ctx.user_message
    assert "Follow the bundled validator playbook for workflow and rules" in ctx.user_message
    assert "## Rule to Follow" not in ctx.user_message
    assert "## Assigned validation task" in ctx.user_message
    assert "Validate retry handling." in ctx.user_message
    assert "## Context from dependencies" not in ctx.user_message
    assert "## Parent context" not in ctx.user_message


@pytest.mark.asyncio
async def test_build_query_context_uses_root_planner_markdown_template() -> None:
    register_all()
    task = Task(
        id="root",
        team_run_id="run-1",
        agent_name="root_planner",
        status=TaskStatus.READY,
        spec=_spec("Root planner task goal."),
        root_id="root",
        depth=0,
    )
    team_run = SimpleNamespace(
        id="run-1",
        user_request="Fix retry handling.",
        root_task_id="root",
        task_center=await _make_task_center("run-1", {"root": task}),
        roster={"planner": ["root_planner", "team_planner"], "developer": ["developer"]},
        team_definition=None,
        project_context=SimpleNamespace(repo_root="/repo"),
        coordination_metadata={"benchmark_test_ids": ["tests/test_retry.py::test_retry"]},
        budgets=BudgetConfig(max_depth=4),
        budget_state=None,
        sandbox_id="",
        arbiter=None,
    )

    ctx = await build_query_context(get_definition("root_planner"), team_run, task)

    assert ctx.user_message.startswith("Please read the following sections")
    assert "- submit_plan:" in ctx.user_message
    assert "Your task id:" not in ctx.user_message
    assert "Your parent task id:" not in ctx.user_message
    assert "Your dependency task ids:" not in ctx.user_message
    assert "Context-read pre-step:" not in ctx.user_message
    assert "Task id:" not in ctx.user_message
    assert "## Available Agents" not in ctx.user_message
    assert 'load_skill(skill_name="team-root-planner-playbook")' in ctx.user_message
    assert "## Planning depth" in ctx.user_message
    assert "Current depth: `0`" in ctx.user_message
    assert "Max depth: `4`" in ctx.user_message
    assert "Tasks submitted in this plan will run at depth `1`" in ctx.user_message
    assert "would need room to submit its own children at depth `2`" in ctx.user_message
    assert "For broad benchmark, fail-to-pass, migration, compatibility, or other clustering jobs" in ctx.user_message
    assert "Do not flatten multi-cluster benchmark repair into only root-level developer tasks" in ctx.user_message
    assert "## Rule to Follow" not in ctx.user_message
    assert "## User request" in ctx.user_message
    assert "Fix retry handling." in ctx.user_message
    assert "## Benchmark targets" in ctx.user_message
    assert "tests/test_retry.py::test_retry" in ctx.user_message
    assert "Benchmark targets are verification evidence only" in ctx.user_message
    assert "not put `*/tests/*`, `test_*.py`, or benchmark test paths in scout `target_paths`" in ctx.user_message
    assert "Child and validator verification commands in specs must be daytona_shell-safe" not in ctx.user_message
    assert "Prefer `python -m pytest ... -q --tb=short` over `-v`" not in ctx.user_message
    assert _SUBMIT_PLAN_SCHEMA_SNIPPET in ctx.user_message
    assert "Submit the final plan with `submit_plan(new_tasks=[...])`" not in ctx.user_message
    assert "## Parent context" not in ctx.user_message


@pytest.mark.asyncio
async def test_build_query_context_omits_planning_depth_without_budget() -> None:
    register_all()
    task = Task(
        id="root",
        team_run_id="run-1",
        agent_name="root_planner",
        status=TaskStatus.READY,
        spec=_spec("Root planner task goal."),
        root_id="root",
        depth=0,
    )
    team_run = SimpleNamespace(
        id="run-1",
        user_request="Fix retry handling.",
        root_task_id="root",
        task_center=await _make_task_center("run-1", {"root": task}),
        roster={"planner": ["root_planner", "team_planner"], "developer": ["developer"]},
        team_definition=None,
        project_context=SimpleNamespace(repo_root="/repo"),
        coordination_metadata={},
        budgets=None,
        budget_state=None,
        sandbox_id="",
        arbiter=None,
    )

    ctx = await build_query_context(get_definition("root_planner"), team_run, task)

    assert "## Planning depth" not in ctx.user_message
    assert "{{current_depth}}" not in ctx.user_message
    assert "{{max_depth}}" not in ctx.user_message


@pytest.mark.asyncio
async def test_build_query_context_uses_child_planner_structured_spec_contract() -> None:
    register_all()
    root = Task(
        id="root",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED,
        spec=_spec("Root task."),
        root_id="root",
        depth=0,
    )
    dep = Task(
        id="prep-1",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.DONE,
        spec=_spec("Prepare retry owner evidence."),
        parent_id="root",
        root_id="root",
        depth=1,
    )
    child_planner = Task(
        id="planner-1",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.READY,
        spec=_spec("Decompose retry handling."),
        deps=["prep-1"],
        parent_id="root",
        root_id="root",
        depth=1,
    )
    tasks = {"root": root, "prep-1": dep, "planner-1": child_planner}
    team_run = SimpleNamespace(
        id="run-1",
        user_request="Fix retry handling.",
        root_task_id="root",
        task_center=await _make_task_center("run-1", tasks),
        roster={"planner": ["team_planner"], "developer": ["developer"]},
        team_definition=None,
        project_context=SimpleNamespace(repo_root="/repo"),
        coordination_metadata={},
        budgets=BudgetConfig(max_depth=4),
        budget_state=None,
        sandbox_id="",
        arbiter=None,
    )

    ctx = await build_query_context(get_definition("team_planner"), team_run, child_planner)

    assert ctx.user_message.startswith("Please read the following sections")
    assert "- submit_plan:" in ctx.user_message
    assert "Your task id: `planner-1`" in ctx.user_message
    assert "Your dependency task ids: `prep-1`" in ctx.user_message
    assert "Your parent task id: `root`" in ctx.user_message
    assert "## Planning depth" in ctx.user_message
    assert "Current depth: `1`" in ctx.user_message
    assert "Max depth: `4`" in ctx.user_message
    assert "Tasks submitted in this plan will run at depth `2`" in ctx.user_message
    assert "would need room to submit its own children at depth `3`" in ctx.user_message
    assert "For broad benchmark, fail-to-pass, migration, compatibility, or other clustering jobs" in ctx.user_message
    assert "Do not flatten multi-cluster benchmark repair into only current-layer developer tasks" in ctx.user_message
    assert "Context-read pre-step: this applies to child planners only" in ctx.user_message
    assert "then call `read_task_graph()` to enumerate siblings" in ctx.user_message
    assert "Task id: `planner-1`" not in ctx.user_message
    assert "Dependency task ids: `prep-1`" not in ctx.user_message
    assert "Parent task id: `root`" not in ctx.user_message
    assert "Follow the bundled team-planner playbook for workflow and rules" in ctx.user_message
    assert "## Rule to Follow" not in ctx.user_message
    assert "## Assigned planner task" in ctx.user_message
    assert "Decompose retry handling." in ctx.user_message
    assert _SUBMIT_PLAN_SCHEMA_SNIPPET in ctx.user_message
    assert "Submit the final child plan with `submit_plan(new_tasks=[...])`" not in ctx.user_message
    assert "## Context from dependencies" not in ctx.user_message
    assert "## Parent context" not in ctx.user_message


@pytest.mark.asyncio
async def test_build_query_context_uses_replanner_template_with_task_ids() -> None:
    register_all()
    root = Task(
        id="root",
        team_run_id="run-1",
        agent_name="team_planner",
        status=TaskStatus.EXPANDED,
        spec=_spec("Root task."),
        root_id="root",
        depth=0,
    )
    dep = Task(
        id="prep-1",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.DONE,
        spec=_spec("Prepare retry owner evidence."),
        parent_id="root",
        root_id="root",
        depth=1,
    )
    failed = Task(
        id="failed-1",
        team_run_id="run-1",
        agent_name="developer",
        status=TaskStatus.FAILED,
        spec=_spec("Goal\nImplement retry handling."),
        failure_reason="unit test still fails",
        root_id="root",
        depth=1,
    )
    replanner = Task(
        id="replanner-1",
        team_run_id="run-1",
        agent_name="team_replanner",
        status=TaskStatus.READY,
        spec=_spec("Recover from failed-1."),
        fired_by_task_id="failed-1",
        deps=["prep-1"],
        parent_id="root",
        root_id="root",
        depth=1,
    )
    tasks = {"root": root, "prep-1": dep, "failed-1": failed, "replanner-1": replanner}
    team_run = SimpleNamespace(
        id="run-1",
        user_request="Fix retry handling.",
        root_task_id="root",
        task_center=await _make_task_center("run-1", tasks),
        roster={"replanner": ["team_replanner"], "developer": ["developer"]},
        team_definition=None,
        project_context=SimpleNamespace(repo_root="/repo"),
        coordination_metadata={},
        budgets=None,
        budget_state=None,
        sandbox_id="",
        arbiter=None,
    )

    ctx = await build_query_context(get_definition("team_replanner"), team_run, replanner)

    assert ctx.user_message.startswith("Please read the following sections")
    assert "- submit_replan:" in ctx.user_message
    assert "Your task id: `replanner-1`" in ctx.user_message
    assert "Your dependency task ids: `prep-1`" in ctx.user_message
    assert "Your parent task id: `root`" in ctx.user_message
    assert "Context-read pre-step: after loading the replanner playbook" in ctx.user_message
    assert "then call `read_task_graph()` to enumerate siblings" in ctx.user_message
    assert "Task id: `replanner-1`" not in ctx.user_message
    assert "Failed task id: `failed-1`" in ctx.user_message
    assert "Dependency task ids: `prep-1`" not in ctx.user_message
    assert "Parent task id: `root`" not in ctx.user_message
    assert "Follow the bundled team-replanner playbook for workflow and rules" in ctx.user_message
    assert "## Rule to Follow" not in ctx.user_message
    assert "## Assigned replanning task" in ctx.user_message
    assert "## Failure context" not in ctx.user_message
    assert "Original task: failed-1" not in ctx.user_message
    assert "Failed reason: unit test still fails" not in ctx.user_message
    assert "## Context from dependencies" not in ctx.user_message
    assert "## Parent context" not in ctx.user_message


@pytest.mark.asyncio
async def test_build_query_context_does_not_use_removed_scout_task_template() -> None:
    register_all()
    task = Task(
        id="scout-1",
        team_run_id="run-1",
        agent_name="scout",
        status=TaskStatus.READY,
        spec=_spec("Map retry module ownership."),
        scope_paths=["backend/src/retry.py"],
        parent_id="root",
        root_id="root",
        depth=1,
    )
    team_run = SimpleNamespace(
        id="run-1",
        user_request="Fix retry handling.",
        root_task_id="root",
        task_center=await _make_task_center("run-1", {"scout-1": task}),
        roster={"explorer": ["scout"]},
        team_definition=None,
        project_context=SimpleNamespace(repo_root="/repo"),
        coordination_metadata={},
        budgets=None,
        budget_state=None,
        sandbox_id="",
        arbiter=None,
    )

    ctx = await build_query_context(get_definition("scout"), team_run, task)

    assert "## Scout note override" not in ctx.user_message
    assert "your required post action is one `submit_file_notes(...)` tool call" not in ctx.user_message
    assert "Your task id:" not in ctx.user_message
    assert "read_task_details" not in ctx.user_message
    assert "read_task_graph" not in ctx.user_message
    assert "Follow the bundled scout playbook for workflow and rules" not in ctx.user_message
    assert "## Assigned exploration task" not in ctx.user_message
    assert "Map retry module ownership." in ctx.user_message
