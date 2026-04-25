"""Tests for TaskCenter task prompt context injection."""

from __future__ import annotations

import json

from task_center import Status, Task
from task_center.context import build_task_prompt
from task_center.graph import TaskGraph


def test_root_task_prompt_is_original_user_message() -> None:
    graph = TaskGraph()
    task = Task(
        id="t1",
        role="executor",
        title="Root",
        spec="User input message",
        status=Status.READY,
    )
    graph.add(task)

    assert build_task_prompt(task, graph) == "User input message"


def test_child_task_prompt_includes_parent_and_completed_dependencies() -> None:
    graph = TaskGraph()
    parent = Task(
        id="parent",
        role="executor",
        title="Parent",
        spec="Parent spec",
        status=Status.AWAITING,
        acceptance_criteria="Criteria",
        handoff_note="Note",
    )
    done_dep = Task(
        id="dep_done",
        role="executor",
        title="Done dependency",
        spec="Dependency spec",
        status=Status.DONE,
        summary="Dependency summary",
        parent_id="parent",
    )
    running_dep = Task(
        id="dep_running",
        role="executor",
        title="Running dependency",
        spec="Running spec",
        status=Status.RUNNING,
        parent_id="parent",
    )
    task = Task(
        id="child",
        role="executor",
        title="Child",
        spec="Child task prompt",
        status=Status.READY,
        parent_id="parent",
        needs=frozenset({"dep_done", "dep_running"}),
    )
    for node in (parent, done_dep, running_dep, task):
        graph.add(node)

    prompt = build_task_prompt(task, graph)

    assert prompt.endswith("<Task Prompt>\nChild task prompt\n</Task Prompt>")
    context_text = prompt.split("<Task Context>\n", 1)[1].split("\n</Task Context>", 1)[0]
    payload = json.loads(context_text)
    assert payload == {
        "parent": {
            "id": "parent",
            "acceptance_criteria": "Criteria",
            "handoff_note": "Note",
        },
        "dependencies": [
            {
                "id": "dep_done",
                "spec": "Dependency spec",
                "summary": "Dependency summary",
            }
        ],
    }
