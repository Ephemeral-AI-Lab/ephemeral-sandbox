"""Tests for tools.task_center.toolkit."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest
from pydantic import ValidationError

from tools.task_center.toolkit import (
    ReadFileNoteTool,
    ReadTaskDetailsTool,
    ReadTaskGraphTool,
    SubmitFileNotesTool,
    SubmitTaskNoteTool,
)
from tools.core.base import ToolExecutionContext, parse_tool_input
from team.models import Note, Task, TaskStatus


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


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
        agent_name=agent,
        status=status,
        objective=f"Objective for {task_id}",
        description=description,
        deps=list(deps or []),
        scope_paths=list(scope_paths or []),
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
    assert notes.posted[0].task_id == ""
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


@pytest.mark.asyncio
async def test_submit_task_note_requires_task_id_and_paths():
    class _Notes:
        def __init__(self) -> None:
            self.posted = []

        async def post(self, note) -> None:
            self.posted.append(note)

    notes = _Notes()
    ctx = _ctx(
        {
            "task_center": SimpleNamespace(notes=notes),
            "agent_name": "note_taker",
        }
    )

    tool = SubmitTaskNoteTool()
    result = await tool.execute(
        tool.input_model(
            content="Blocker on dep migration.",
            task_id="task-42",
            paths=["src/auth.py"],
            tags=["blocker"],
        ),
        ctx,
    )

    assert result.is_error is False
    payload = json.loads(result.output)
    assert payload["task_id"] == "task-42"
    assert payload["paths"] == ["src/auth.py"]


def test_submit_task_note_rejects_whitespace_only_content():
    with pytest.raises(ValidationError, match="content must contain non-whitespace text"):
        SubmitTaskNoteTool.input_model(content=" \n\t", task_id="task-1", paths=["src/a.py"])


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


def test_submit_task_note_rejects_missing_task_id():
    with pytest.raises(ValidationError):
        SubmitTaskNoteTool.input_model(content="note", paths=["src/a.py"])


def test_submit_note_schemas_are_pydantic_native():
    file_schema = SubmitFileNotesTool().to_api_schema()
    task_schema = SubmitTaskNoteTool().to_api_schema()

    assert "notes" in file_schema["input_schema"]["properties"]
    assert file_schema["input_schema"]["additionalProperties"] is False
    assert file_schema["input_schema"]["required"] == ["notes"]
    note_item_schema = file_schema["input_schema"]["$defs"][
        file_schema["input_schema"]["properties"]["notes"]["items"]["$ref"].split("/")[-1]
    ]
    assert "path" in note_item_schema["properties"]
    assert "content" in note_item_schema["properties"]
    assert note_item_schema["additionalProperties"] is False
    assert "batched file-scoped notes" in file_schema["description"]
    assert "notes" in file_schema["output_schema"]["properties"]
    item_output = file_schema["output_schema"]["$defs"][
        file_schema["output_schema"]["properties"]["notes"]["items"]["$ref"].split("/")[-1]
    ]
    assert "path" in item_output["properties"]
    assert "paths" not in item_output["properties"]

    assert "task_id" in task_schema["input_schema"]["properties"]
    assert "paths" in task_schema["input_schema"]["properties"]
    assert "task-scoped note" in task_schema["description"]
    assert task_schema["output_schema"]["properties"]["task_id"]["description"]


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

    assert "must read their prompt header ids first" in description
    assert "then may use read_task_graph" in description
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
    assert "**Deps:** dep-1" in result.output
    assert "**Scope:** src/parser.py" in result.output
    assert "task-2" not in result.output


@pytest.mark.asyncio
async def test_read_task_details_labels_initial_plan_and_replan_json():
    class _Notes:
        async def read(self, **_kwargs):
            return [
                Note(
                    id="plan-note",
                    task_id="task-1",
                    agent_name="team_planner",
                    content='[{"id": "dev-1", "agent": "developer"}]',
                    tags=["initial_planned_tasks"],
                ),
                Note(
                    id="replan-note",
                    task_id="task-1",
                    agent_name="team_replanner",
                    content='[{"id": "fix-1", "agent": "developer"}]',
                    tags=["initial_replanned_tasks"],
                ),
                Note(
                    id="summary-note",
                    task_id="task-1",
                    agent_name="parent_summarizer",
                    content="dev-1 delivered parser retry behavior.",
                    tags=["implementation", "parent_summary"],
                ),
            ]

    graph = {
        "task-1": _task(
            "task-1",
            parent_id=None,
            status=TaskStatus.EXPANDED_AWAITING_SUMMARY,
            agent="team_planner",
            description="Plan parser work",
        ),
    }
    ctx = _ctx({"task_center": SimpleNamespace(graph=graph, notes=_Notes())})

    result = await ReadTaskDetailsTool().execute(
        ReadTaskDetailsTool.input_model(task_id="task-1"),
        ctx,
    )

    assert result.is_error is False
    assert "**Initial Plan:**\n```json\n[{\"id\": \"dev-1\", \"agent\": \"developer\"}]\n```" in result.output
    assert "**Initial Replan:**\n```json\n[{\"id\": \"fix-1\", \"agent\": \"developer\"}]\n```" in result.output
    assert "**Summary:**\ndev-1 delivered parser retry behavior." in result.output
    assert "### team_planner [initial_planned_tasks]" not in result.output
    assert "### team_replanner [initial_replanned_tasks]" not in result.output
