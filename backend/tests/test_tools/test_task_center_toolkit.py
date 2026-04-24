"""Tests for tools.task_center.tools."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest
from pydantic import ValidationError

from tools.task_center.tools import (
    ReadFileNoteTool,
    ReadTaskDetailsTool,
    ReadTaskGraphTool,
    SubmitFileNotesTool,
)
from tools.core.base import ToolExecutionContext, parse_tool_input
from team.core.models import (
    LeafSubmission,
    Plan,
    PlannerSubmission,
    SubmittedSummary,
    Task,
    TaskDefinition,
    TaskStatus,
)


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _spec(goal: str) -> dict[str, str]:
    return {
        "goal": goal,
        "detail": f"Detail for {goal}",
        "acceptance_criteria": f"Acceptance criteria for {goal}",
    }


def _task(
    task_id: str,
    *,
    parent_id: str | None,
    status: TaskStatus = TaskStatus.READY,
    agent: str = "developer",
    description: str = "",
    deps: list[str] | None = None,
    scope_paths: list[str] | None = None,
    failure_reason: str | None = None,
) -> Task:
    return Task(
        id=task_id,
        team_run_id="run-1",
        definition=TaskDefinition(
            id=task_id,
            spec=_spec(f"Goal for {task_id}"),
            agent=agent,
            description=description,
            deps=list(deps or []),
            scope_paths=list(scope_paths or []),
        ),
        status=status,
        parent_id=parent_id,
        failure_reason=failure_reason,
    )


@pytest.mark.asyncio
async def test_submit_file_notes_store_one_note_per_item():
    class _Notes:
        def __init__(self) -> None:
            self.posted = []

        async def post(self, note) -> None:
            self.posted.append(note)

    notes = _Notes()
    ctx = _ctx(
        {
            "task_center": SimpleNamespace(notes=notes),
            "work_item_id": "task-1",
            "agent_name": "scout",
        }
    )

    tool = SubmitFileNotesTool()
    result = await tool.execute(
        tool.input_model(
            notes=[
                {"path": "./src/auth.py/", "content": "Mapped auth surface."},
                {"path": "src/session.py", "content": "Mapped session surface."},
            ],
        ),
        ctx,
    )

    assert result.is_error is False
    payload = json.loads(result.output)
    assert [item["path"] for item in payload["notes"]] == ["src/auth.py", "src/session.py"]
    assert [item["content"] for item in payload["notes"]] == [
        "Mapped auth surface.",
        "Mapped session surface.",
    ]
    assert len(notes.posted) == 2
    assert notes.posted[0].paths == ["src/auth.py"]
    assert notes.posted[1].paths == ["src/session.py"]
    assert notes.posted[0].id == payload["notes"][0]["note_id"]
    assert notes.posted[1].id == payload["notes"][1]["note_id"]


@pytest.mark.asyncio
async def test_submit_file_notes_preserve_order_and_allow_scout_correct_path_content():
    class _Notes:
        def __init__(self) -> None:
            self.posted = []

        async def post(self, note) -> None:
            self.posted.append(note)

    notes = _Notes()
    ctx = _ctx(
        {
            "task_center": SimpleNamespace(notes=notes),
            "agent_name": "scout",
        }
    )

    tool = SubmitFileNotesTool()
    result = await tool.execute(
        tool.input_model(
            notes=[
                {"path": "src/auth.py", "content": "Mapped auth surface first."},
                {
                    "path": "src/missing.py",
                    "content": "Missing target; correct path appears to be src/session.py.",
                },
            ],
        ),
        ctx,
    )

    assert result.is_error is False
    payload = json.loads(result.output)
    assert [item["path"] for item in payload["notes"]] == ["src/auth.py", "src/missing.py"]
    assert notes.posted[0].content == "Mapped auth surface first."
    assert notes.posted[1].content == "Missing target; correct path appears to be src/session.py."


def test_submit_file_notes_reject_whitespace_only_content():
    with pytest.raises(ValidationError, match="content must contain non-whitespace text"):
        SubmitFileNotesTool.input_model(
            notes=[{"path": "src/a.py", "content": " \n\t"}],
        )


def test_submit_file_notes_reject_empty_batch():
    with pytest.raises(ValidationError):
        SubmitFileNotesTool.input_model(notes=[])


def test_submit_file_notes_reject_duplicate_normalized_paths():
    with pytest.raises(ValidationError, match="duplicate normalized paths"):
        SubmitFileNotesTool.input_model(
            notes=[
                {"path": "./src/a.py", "content": "first"},
                {"path": "src/a.py/", "content": "second"},
            ],
        )

def test_submit_note_schemas_are_pydantic_native():
    file_schema = SubmitFileNotesTool().to_api_schema()

    assert "notes" in file_schema["input_schema"]["properties"]
    assert file_schema["input_schema"]["additionalProperties"] is False
    assert file_schema["input_schema"]["required"] == ["notes"]
    note_item_schema = file_schema["input_schema"]["$defs"][
        file_schema["input_schema"]["properties"]["notes"]["items"]["$ref"].split("/")[-1]
    ]
    assert "path" in note_item_schema["properties"]
    assert "content" in note_item_schema["properties"]
    assert note_item_schema["additionalProperties"] is False
    assert "Posts append-only file or directory notes" in file_schema["description"]
    assert "Use for" not in file_schema["description"]
    assert "notes" in file_schema["output_schema"]["properties"]
    item_output = file_schema["output_schema"]["$defs"][
        file_schema["output_schema"]["properties"]["notes"]["items"]["$ref"].split("/")[-1]
    ]
    assert "path" in item_output["properties"]
    assert "paths" not in item_output["properties"]


@pytest.mark.asyncio
async def test_read_task_graph_defaults_to_peer_tree_json():
    graph = {
        "root": _task("root", parent_id=None, agent="planner", description="Root"),
        "parent": _task("parent", parent_id="root", agent="planner", description="Parent"),
        "self": _task(
            "self",
            parent_id="parent",
            status=TaskStatus.RUNNING,
            description="Current task",
            deps=["peer"],
            scope_paths=["src/self.py"],
        ),
        "peer": _task("peer", parent_id="parent", description="Peer task"),
        "peer-child": _task(
            "peer-child",
            parent_id="peer",
            status=TaskStatus.PENDING,
            description="Nested child",
        ),
        "other-branch": _task("other-branch", parent_id="root"),
    }
    ctx = _ctx(
        {
            "task_center": SimpleNamespace(graph=graph),
            "work_item_id": "self",
        }
    )

    result = await ReadTaskGraphTool().execute(
        ReadTaskGraphTool.input_model(),
        ctx,
    )

    assert result.is_error is False
    payload = json.loads(result.output)
    assert payload["parent"] == {
        "id": "parent",
        "agent": "planner",
        "status": "ready",
        "description": "Parent",
    }
    assert [task["id"] for task in payload["tasks"]] == ["self", "peer"]
    self_node = payload["tasks"][0]
    assert self_node["is_you"] is True
    assert self_node["deps"] == ["peer"]
    assert self_node["scope_paths"] == ["src/self.py"]
    assert payload["tasks"][1]["children"][0]["id"] == "peer-child"
    assert "other-branch" not in json.dumps(payload)


@pytest.mark.asyncio
async def test_read_task_graph_global_scope_includes_roots_and_detached_nodes():
    graph = {
        "root": _task("root", parent_id=None, agent="planner"),
        "child": _task("child", parent_id="root", description="Child"),
        "orphan": _task(
            "orphan",
            parent_id="missing-parent",
            status=TaskStatus.FAILED,
            failure_reason="parent was pruned",
        ),
    }
    ctx = _ctx(
        {
            "task_center": SimpleNamespace(graph=graph),
            "work_item_id": "child",
        }
    )

    result = await ReadTaskGraphTool().execute(
        ReadTaskGraphTool.input_model(global_scope=True),
        ctx,
    )

    assert result.is_error is False
    payload = json.loads(result.output)
    assert [task["id"] for task in payload["tasks"]] == ["root"]
    assert payload["tasks"][0]["children"][0]["id"] == "child"
    assert payload["tasks"][0]["children"][0]["is_you"] is True
    assert [task["id"] for task in payload["detached"]] == ["orphan"]
    assert payload["detached"][0]["failure_reason"] == "parent was pruned"


@pytest.mark.asyncio
async def test_read_file_note_empty_path_read_reports_known_paths():
    class _Notes:
        async def read(self, **_kwargs):
            return []

        def known_paths(self):
            return ["src/other.py"]

    ctx = _ctx({"task_center": SimpleNamespace(notes=_Notes())})

    result = await ReadFileNoteTool().execute(
        ReadFileNoteTool.input_model(file_path="src/auth.py"),
        ctx,
    )

    assert result.is_error is False
    assert "No notes found for file_path" in result.output
    assert "src/other.py" in result.output


def test_read_file_note_schema_requires_file_path_only():
    schema = ReadFileNoteTool().to_api_schema()["input_schema"]

    assert schema["required"] == ["file_path"]
    assert "keyword" not in schema["properties"]
    assert schema["properties"]["file_path"]["type"] == "string"
    assert schema["properties"]["file_path"]["minLength"] == 1
    assert "anyOf" not in schema["properties"]["file_path"]
    assert "default" not in schema["properties"]["file_path"]
    assert "free-form call context is not searched" in (
        schema["properties"]["file_path"]["description"]
    )
    assert schema["additionalProperties"] is False


def test_read_file_note_parse_rejects_missing_file_path():
    tool = ReadFileNoteTool()

    result = parse_tool_input(
        tool,
        {"context": "Reading notes for src/auth.py"},
    )

    assert result.is_error is True
    assert result.error is not None
    assert "file_path" in result.error.output


def test_read_file_note_parse_rejects_keyword_even_with_file_path():
    result = parse_tool_input(
        ReadFileNoteTool(),
        {"file_path": "src/auth.py", "keyword": "token"},
    )

    assert result.is_error is True
    assert result.error is not None
    assert "keyword" in result.error.output


def test_read_task_details_schema_requires_single_task_id():
    schema = ReadTaskDetailsTool().to_api_schema()["input_schema"]

    assert schema["required"] == ["task_id"]
    assert "task_ids" not in schema["properties"]
    assert schema["properties"]["task_id"]["type"] == "string"
    assert schema["properties"]["task_id"]["minLength"] == 1
    assert schema["additionalProperties"] is False


def test_read_task_details_description_orders_header_reads_before_graph():
    description = ReadTaskDetailsTool().to_api_schema()["description"]

    assert "Returns one Task Center task's spec" in description
    assert "submission details" in description
    assert "Use to" not in description
    assert "may use read_task_graph first" not in description


def test_read_task_details_parse_rejects_task_ids_list():
    result = parse_tool_input(
        ReadTaskDetailsTool(),
        {"task_ids": ["task-1"]},
    )

    assert result.is_error is True
    assert result.error is not None
    assert "task_id" in result.error.output


def test_read_task_details_parse_rejects_extra_task_ids_with_task_id():
    result = parse_tool_input(
        ReadTaskDetailsTool(),
        {"task_id": "task-1", "task_ids": ["task-2"]},
    )

    assert result.is_error is True
    assert result.error is not None
    assert "task_ids" in result.error.output


@pytest.mark.asyncio
async def test_read_task_details_reads_single_task():
    class _Notes:
        async def read(self, **_kwargs):
            return []

    graph = {
        "task-1": _task(
            "task-1",
            parent_id=None,
            status=TaskStatus.RUNNING,
            agent="developer",
            description="Patch parser",
            deps=["dep-1"],
            scope_paths=["src/parser.py"],
        ),
        "task-2": _task("task-2", parent_id=None, description="Other task"),
    }
    ctx = _ctx({"task_center": SimpleNamespace(graph=graph, notes=_Notes())})

    result = await ReadTaskDetailsTool().execute(
        ReadTaskDetailsTool.input_model(task_id="task-1"),
        ctx,
    )

    assert result.is_error is False
    assert "## task-1 (developer) [running]" in result.output
    assert "**Description:** Patch parser" in result.output
    assert "# Goal {Status: Running}" in result.output
    assert "Goal for task-1" in result.output
    assert "# Detail" in result.output
    assert "# Acceptance Criteria" in result.output
    assert "**Deps:** dep-1" in result.output
    assert "**Scope:** src/parser.py" in result.output
    assert "task-2" not in result.output


@pytest.mark.asyncio
async def test_read_task_details_renders_leaf_success_outcome():
    task = _task("task-1", parent_id=None, status=TaskStatus.DONE)
    task.submission = LeafSubmission(
        summary=SubmittedSummary(summary="Implemented parser retry behavior.")
    )
    ctx = _ctx({"task_center": SimpleNamespace(graph={"task-1": task})})

    result = await ReadTaskDetailsTool().execute(
        ReadTaskDetailsTool.input_model(task_id="task-1"),
        ctx,
    )

    assert "# Goal {Status: Success}" in result.output
    assert "# Outcome" in result.output
    assert "Implemented parser retry behavior." in result.output


@pytest.mark.asyncio
async def test_read_task_details_renders_terminal_reason():
    task = _task(
        "task-1",
        parent_id=None,
        status=TaskStatus.REQUEST_REPLAN,
        failure_reason="scope_expansion: owner moved",
    )
    ctx = _ctx({"task_center": SimpleNamespace(graph={"task-1": task})})

    result = await ReadTaskDetailsTool().execute(
        ReadTaskDetailsTool.input_model(task_id="task-1"),
        ctx,
    )

    assert "# Goal {Status: Request Replan}" in result.output
    assert "# Request Replan Reason" in result.output
    assert "scope_expansion: owner moved" in result.output


@pytest.mark.asyncio
async def test_read_task_details_renders_expandable_plan_and_outcome():
    task = _task("planner", parent_id=None, status=TaskStatus.DONE, agent="team_planner")
    task.submission = PlannerSubmission(
        plan=Plan(
            tasks=[
                TaskDefinition(
                    id="dev-1",
                    spec=_spec("Repair parser"),
                    agent="developer",
                    scope_paths=["src/parser.py"],
                )
            ]
        ),
        summary=SubmittedSummary(summary="Parser repair and validation completed."),
    )
    ctx = _ctx({"task_center": SimpleNamespace(graph={"planner": task})})

    result = await ReadTaskDetailsTool().execute(
        ReadTaskDetailsTool.input_model(task_id="planner"),
        ctx,
    )

    assert "# Initial Plan" in result.output
    assert '"spec": {' in result.output
    assert '"goal": "Repair parser"' in result.output
    assert "# Outcome" in result.output
    assert "Parser repair and validation completed." in result.output

