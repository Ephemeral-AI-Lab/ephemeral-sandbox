"""Snapshot tests for persisted TaskCenter graph topology."""

from __future__ import annotations

import json
from collections.abc import Awaitable, Callable
from textwrap import dedent
from typing import Any

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

import db.models  # noqa: F401
from db.base import Base
from db.stores.task_center_store import TaskCenterStore
from task_center import Status
from task_center.center import TaskCenter


Action = Callable[[TaskCenter, str], Awaitable[None]]


def _memory_store() -> TaskCenterStore:
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    sf = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    task_center_store = TaskCenterStore()
    task_center_store.initialize(sf)
    return task_center_store


def _create_run(
    store: TaskCenterStore,
    *,
    request_id: str,
    run_id: str,
    prompt: str,
) -> None:
    store.create_request(
        request_id=request_id,
        cwd="/repo",
        sandbox_id=None,
        request_prompt=prompt,
    )
    store.create_run(run_id=run_id, request_id=request_id)


def _scripted_spawn(scripts: dict[str, Action]):
    async def spawn(task_id: str, tc: TaskCenter, sandbox_id: str | None) -> None:
        del sandbox_id
        action = scripts.get(task_id)
        if action is not None:
            await action(tc, task_id)

    return spawn


def _ordered_graph_ids(nodes: dict[str, dict[str, Any]]) -> list[str]:
    roots = sorted(
        node_id
        for node_id, node in nodes.items()
        if node["parent_task_id"] is None
    )
    ordered: list[str] = []
    visited: set[str] = set()

    def visit(node_id: str) -> None:
        if node_id in visited:
            return
        visited.add(node_id)
        ordered.append(node_id)
        for child_id in nodes[node_id]["children_ids"]:
            visit(child_id)

    for root_id in roots:
        visit(root_id)
    for node_id in sorted(nodes):
        visit(node_id)
    return ordered


def _graph_snapshot(store: TaskCenterStore, run_id: str) -> dict[str, Any]:
    tasks = {task["id"]: task for task in store.list_tasks_for_run(run_id)}
    nodes = {node["task_id"]: node for node in store.list_graph_for_run(run_id)}

    return {
        "run_id": run_id,
        "entry_ids": sorted(
            node_id
            for node_id, node in nodes.items()
            if node["parent_task_id"] is None
        ),
        "sink_ids": sorted(
            node_id for node_id, node in nodes.items() if not node["children_ids"]
        ),
        "nodes": [
            {
                "id": node_id,
                "role": tasks[node_id]["role"],
                "status": tasks[node_id]["status"],
                "parent": nodes[node_id]["parent_task_id"],
                "children": nodes[node_id]["children_ids"],
                "evaluator": nodes[node_id]["evaluator_id"],
                "acceptance_criteria": nodes[node_id]["acceptance_criteria"],
                "handoff_note": nodes[node_id]["handoff_note"],
            }
            for node_id in _ordered_graph_ids(nodes)
        ],
    }


def _graph_json(store: TaskCenterStore, run_id: str) -> str:
    return json.dumps(_graph_snapshot(store, run_id), indent=2)


def _row_graph_snapshots(store: TaskCenterStore, run_id: str) -> dict[str, Any]:
    tasks = {task["id"]: task for task in store.list_tasks_for_run(run_id)}
    nodes = {node["task_id"]: node for node in store.list_graph_for_run(run_id)}

    return {
        "run_id": run_id,
        "row_graphs": [
            {
                "row": index,
                "task_id": node_id,
                "parent_task_id": nodes[node_id]["parent_task_id"],
                "entry_id": node_id,
                "sink_id": node_id,
                "role": tasks[node_id]["role"],
                "status": tasks[node_id]["status"],
                "children_ids": nodes[node_id]["children_ids"],
                "evaluator_id": nodes[node_id]["evaluator_id"],
                "acceptance_criteria": nodes[node_id]["acceptance_criteria"],
                "handoff_note": nodes[node_id]["handoff_note"],
            }
            for index, node_id in enumerate(_ordered_graph_ids(nodes), start=1)
        ],
    }


def _row_graphs_json(store: TaskCenterStore, run_id: str) -> str:
    return json.dumps(_row_graph_snapshots(store, run_id), indent=2)


def _row_graphs_ascii(store: TaskCenterStore, run_id: str) -> str:
    tasks = {task["id"]: task for task in store.list_tasks_for_run(run_id)}
    nodes = {node["task_id"]: node for node in store.list_graph_for_run(run_id)}
    lines: list[str] = []

    for index, node_id in enumerate(_ordered_graph_ids(nodes), start=1):
        task = tasks[node_id]
        node = nodes[node_id]
        parent = node["parent_task_id"] or "<entry>"
        lines.append(f"[row {index:02d}] task_id={node_id} parent={parent}")
        lines.append(f"{node_id} ({task['role']}, {task['status']}) [entry=sink]")
        children = node["children_ids"]
        if children:
            for child_index, child_id in enumerate(children):
                connector = "`- " if child_index == len(children) - 1 else "|- "
                lines.append(f"{connector}child={child_id}")
        else:
            lines.append("`- children=<none>")
        if node["evaluator_id"] is not None:
            lines.append(f"evaluator={node['evaluator_id']}")
        if node["acceptance_criteria"] is not None:
            lines.append(f"acceptance_criteria={node['acceptance_criteria']!r}")
        if node["handoff_note"] is not None:
            lines.append(f"handoff_note={node['handoff_note']!r}")
        lines.append("")

    return "\n".join(lines).rstrip()


def _print_graph_outputs(store: TaskCenterStore, run_id: str) -> None:
    print(f"\n=== {run_id} task_center_graph row graphs json ===")
    print(_row_graphs_json(store, run_id))
    print(f"=== {run_id} task_center_graph row graphs ascii ===")
    print(_row_graphs_ascii(store, run_id))
    print(f"=== {run_id} final graph json ===")
    print(_graph_json(store, run_id))
    print(f"=== {run_id} final graph ascii ===")
    print(_graph_ascii(store, run_id))


def _graph_ascii(store: TaskCenterStore, run_id: str) -> str:
    tasks = {task["id"]: task for task in store.list_tasks_for_run(run_id)}
    nodes = {node["task_id"]: node for node in store.list_graph_for_run(run_id)}

    def label(node_id: str) -> str:
        task = tasks[node_id]
        node = nodes[node_id]
        parts = [f"{node_id} ({task['role']}, {task['status']})"]
        if node["evaluator_id"] is not None:
            parts.append(f"eval={node['evaluator_id']}")
        if node["acceptance_criteria"] is not None:
            parts.append(f"ac={node['acceptance_criteria']!r}")
        if node["handoff_note"] is not None:
            parts.append(f"handoff={node['handoff_note']!r}")
        return " ".join(parts)

    def child_lines(node_id: str, prefix: str) -> list[str]:
        lines: list[str] = []
        children = nodes[node_id]["children_ids"]
        for index, child_id in enumerate(children):
            is_last = index == len(children) - 1
            connector = "`- " if is_last else "|- "
            child_prefix = "   " if is_last else "|  "
            lines.append(f"{prefix}{connector}{label(child_id)}")
            lines.extend(child_lines(child_id, prefix + child_prefix))
        return lines

    roots = sorted(
        node_id
        for node_id, node in nodes.items()
        if node["parent_task_id"] is None
    )
    lines: list[str] = []
    for index, root_id in enumerate(roots):
        if index > 0:
            lines.append("")
        lines.append(label(root_id))
        lines.extend(child_lines(root_id, ""))
    return "\n".join(lines)


def _assert_graph_row_per_task(store: TaskCenterStore, run_id: str) -> None:
    task_ids = {task["id"] for task in store.list_tasks_for_run(run_id)}
    graph_ids = {node["task_id"] for node in store.list_graph_for_run(run_id)}
    assert graph_ids == task_ids


def _node(
    node_id: str,
    role: str,
    status: str,
    parent: str | None,
    children: list[str] | None = None,
    *,
    evaluator: str | None = None,
    acceptance_criteria: str | None = None,
    handoff_note: str | None = None,
) -> dict[str, Any]:
    return {
        "id": node_id,
        "role": role,
        "status": status,
        "parent": parent,
        "children": children or [],
        "evaluator": evaluator,
        "acceptance_criteria": acceptance_criteria,
        "handoff_note": handoff_note,
    }


@pytest.mark.asyncio
async def test_single_task_persists_one_graph_node_as_single_node_diamond() -> None:
    store = _memory_store()
    _create_run(
        store,
        request_id="req-single",
        run_id="run-single",
        prompt="finish directly",
    )

    async def root_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_task_completion(task_id, "root done")

    tc = TaskCenter(
        spawn_func=_scripted_spawn({"t1": root_action}),
        request_id="req-single",
        run_id="run-single",
        task_center_store=store,
    )
    root = await tc.run_query("finish directly")

    assert root.summary == "root done"
    _assert_graph_row_per_task(store, "run-single")
    _print_graph_outputs(store, "run-single")
    assert json.loads(_graph_json(store, "run-single")) == {
        "run_id": "run-single",
        "entry_ids": ["run-single:t1"],
        "sink_ids": ["run-single:t1"],
        "nodes": [
            _node("run-single:t1", "executor", "done", None),
        ],
    }
    assert _graph_ascii(store, "run-single") == "run-single:t1 (executor, done)"


def test_plan_handoff_persists_children_acceptance_criteria_and_note() -> None:
    store = _memory_store()
    _create_run(
        store,
        request_id="req-plan",
        run_id="run-plan",
        prompt="plan root",
    )
    tc = TaskCenter(
        request_id="req-plan",
        run_id="run-plan",
        task_center_store=store,
    )
    root = tc._create_root_executor("plan root")
    tc.graph.transition(root.id, Status.RUNNING)
    tc._persist_task(root)

    tc.submit_plan_handoff(
        root.id,
        [{"id": "left"}, {"id": "right", "deps": ["left"]}],
        {
            "left": {"title": "Left", "task_input": "left work"},
            "right": {"title": "Right", "task_input": "right work"},
        },
        "children pass",
        "handoff note",
    )

    _assert_graph_row_per_task(store, "run-plan")
    _print_graph_outputs(store, "run-plan")
    assert json.loads(_graph_json(store, "run-plan")) == {
        "run_id": "run-plan",
        "entry_ids": ["run-plan:t1"],
        "sink_ids": ["run-plan:left", "run-plan:right"],
        "nodes": [
            _node(
                "run-plan:t1",
                "executor",
                "handoff",
                None,
                ["run-plan:left", "run-plan:right"],
                acceptance_criteria="children pass",
                handoff_note="handoff note",
            ),
            _node("run-plan:left", "executor", "ready", "run-plan:t1"),
            _node("run-plan:right", "executor", "pending", "run-plan:t1"),
        ],
    }
    assert _graph_ascii(store, "run-plan") == dedent(
        """\
        run-plan:t1 (executor, handoff) ac='children pass' handoff='handoff note'
        |- run-plan:left (executor, ready)
        `- run-plan:right (executor, pending)
        """
    ).strip()


@pytest.mark.asyncio
async def test_dispatcher_adds_evaluator_after_direct_children_finish() -> None:
    store = _memory_store()
    _create_run(
        store,
        request_id="req-eval",
        run_id="run-eval",
        prompt="evaluate after children",
    )
    spawn_order: list[str] = []

    async def root_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_plan_handoff(
            task_id,
            [{"id": "left"}, {"id": "right"}],
            {
                "left": {"title": "Left", "task_input": "left work"},
                "right": {"title": "Right", "task_input": "right work"},
            },
            "children pass",
            "handoff note",
        )

    async def child_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_task_completion(task_id, f"{task_id} done")

    async def eval_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_task_completion(task_id, "accepted")

    scripts = {
        "t1": root_action,
        "left": child_action,
        "right": child_action,
        "t1-eval": eval_action,
    }

    async def spawn(task_id: str, tc: TaskCenter, sandbox_id: str | None) -> None:
        del sandbox_id
        spawn_order.append(task_id)
        await scripts[task_id](tc, task_id)

    tc = TaskCenter(
        spawn_func=spawn,
        request_id="req-eval",
        run_id="run-eval",
        task_center_store=store,
    )
    root = await tc.run_query("evaluate after children")

    assert root.summary == "accepted"
    assert spawn_order[0] == "t1"
    assert set(spawn_order[1:3]) == {"left", "right"}
    assert spawn_order[-1] == "t1-eval"
    _assert_graph_row_per_task(store, "run-eval")
    _print_graph_outputs(store, "run-eval")
    assert json.loads(_graph_json(store, "run-eval")) == {
        "run_id": "run-eval",
        "entry_ids": ["run-eval:t1"],
        "sink_ids": ["run-eval:left", "run-eval:right", "run-eval:t1-eval"],
        "nodes": [
            _node(
                "run-eval:t1",
                "executor",
                "done",
                None,
                ["run-eval:left", "run-eval:right", "run-eval:t1-eval"],
                evaluator="run-eval:t1-eval",
                acceptance_criteria="children pass",
                handoff_note="handoff note",
            ),
            _node("run-eval:left", "executor", "done", "run-eval:t1"),
            _node("run-eval:right", "executor", "done", "run-eval:t1"),
            _node(
                "run-eval:t1-eval",
                "evaluator",
                "done",
                "run-eval:t1",
                acceptance_criteria="children pass",
                handoff_note="handoff note",
            ),
        ],
    }
    assert _graph_ascii(store, "run-eval") == dedent(
        """\
        run-eval:t1 (executor, done) eval=run-eval:t1-eval ac='children pass' handoff='handoff note'
        |- run-eval:left (executor, done)
        |- run-eval:right (executor, done)
        `- run-eval:t1-eval (evaluator, done) ac='children pass' handoff='handoff note'
        """
    ).strip()


@pytest.mark.asyncio
async def test_nested_plan_handoffs_persist_deep_task_graph() -> None:
    store = _memory_store()
    _create_run(
        store,
        request_id="req-nested",
        run_id="run-nested",
        prompt="deep graph",
    )

    async def root_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_plan_handoff(
            task_id,
            [{"id": "discovery"}, {"id": "delivery", "deps": ["discovery"]}],
            {
                "discovery": {"title": "Discovery", "task_input": "discover"},
                "delivery": {"title": "Delivery", "task_input": "deliver"},
            },
            "root accepted",
            "root handoff",
        )

    async def discovery_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_plan_handoff(
            task_id,
            [{"id": "scan"}, {"id": "synthesize", "deps": ["scan"]}],
            {
                "scan": {"title": "Scan", "task_input": "scan"},
                "synthesize": {"title": "Synthesize", "task_input": "synthesize"},
            },
            "discovery accepted",
            "discovery handoff",
        )

    async def synthesize_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_plan_handoff(
            task_id,
            [
                {"id": "synthesize-draft"},
                {"id": "synthesize-review", "deps": ["synthesize-draft"]},
            ],
            {
                "synthesize-draft": {"title": "Draft", "task_input": "draft"},
                "synthesize-review": {"title": "Review", "task_input": "review"},
            },
            "synthesis accepted",
            "synthesis handoff",
        )

    async def synthesize_draft_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_plan_handoff(
            task_id,
            [{"id": "draft-outline"}, {"id": "draft-body", "deps": ["draft-outline"]}],
            {
                "draft-outline": {"title": "Draft outline", "task_input": "outline"},
                "draft-body": {"title": "Draft body", "task_input": "body"},
            },
            "draft accepted",
            "draft handoff",
        )

    async def draft_body_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_plan_handoff(
            task_id,
            [
                {"id": "body-section-a"},
                {"id": "body-section-b", "deps": ["body-section-a"]},
            ],
            {
                "body-section-a": {"title": "Body section A", "task_input": "section a"},
                "body-section-b": {"title": "Body section B", "task_input": "section b"},
            },
            "body accepted",
            "body handoff",
        )

    async def body_section_b_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_plan_handoff(
            task_id,
            [{"id": "body-final-copy"}],
            {
                "body-final-copy": {
                    "title": "Body final copy",
                    "task_input": "final copy",
                },
            },
            "final body accepted",
            "final body handoff",
        )

    async def delivery_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_plan_handoff(
            task_id,
            [
                {"id": "implementation"},
                {"id": "verification", "deps": ["implementation"]},
            ],
            {
                "implementation": {"title": "Implementation", "task_input": "build"},
                "verification": {"title": "Verification", "task_input": "verify"},
            },
            "delivery accepted",
            "delivery handoff",
        )

    async def complete_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_task_completion(task_id, f"{task_id} done")

    async def accept_action(tc: TaskCenter, task_id: str) -> None:
        tc.submit_task_completion(task_id, f"{task_id} accepted")

    scripts = {
        "t1": root_action,
        "discovery": discovery_action,
        "scan": complete_action,
        "synthesize": synthesize_action,
        "synthesize-draft": synthesize_draft_action,
        "draft-outline": complete_action,
        "draft-body": draft_body_action,
        "body-section-a": complete_action,
        "body-section-b": body_section_b_action,
        "body-final-copy": complete_action,
        "body-section-b-eval": accept_action,
        "draft-body-eval": accept_action,
        "synthesize-draft-eval": accept_action,
        "synthesize-review": complete_action,
        "synthesize-eval": accept_action,
        "discovery-eval": accept_action,
        "delivery": delivery_action,
        "implementation": complete_action,
        "verification": complete_action,
        "delivery-eval": accept_action,
        "t1-eval": accept_action,
    }
    tc = TaskCenter(
        spawn_func=_scripted_spawn(scripts),
        request_id="req-nested",
        run_id="run-nested",
        task_center_store=store,
    )
    root = await tc.run_query("deep graph")

    assert root.summary == "t1-eval accepted"
    _assert_graph_row_per_task(store, "run-nested")
    _print_graph_outputs(store, "run-nested")
    assert json.loads(_graph_json(store, "run-nested")) == {
        "run_id": "run-nested",
        "entry_ids": ["run-nested:t1"],
        "sink_ids": [
            "run-nested:body-final-copy",
            "run-nested:body-section-a",
            "run-nested:body-section-b-eval",
            "run-nested:delivery-eval",
            "run-nested:discovery-eval",
            "run-nested:draft-body-eval",
            "run-nested:draft-outline",
            "run-nested:implementation",
            "run-nested:scan",
            "run-nested:synthesize-draft-eval",
            "run-nested:synthesize-eval",
            "run-nested:synthesize-review",
            "run-nested:t1-eval",
            "run-nested:verification",
        ],
        "nodes": [
            _node(
                "run-nested:t1",
                "executor",
                "done",
                None,
                [
                    "run-nested:discovery",
                    "run-nested:delivery",
                    "run-nested:t1-eval",
                ],
                evaluator="run-nested:t1-eval",
                acceptance_criteria="root accepted",
                handoff_note="root handoff",
            ),
            _node(
                "run-nested:discovery",
                "executor",
                "done",
                "run-nested:t1",
                [
                    "run-nested:scan",
                    "run-nested:synthesize",
                    "run-nested:discovery-eval",
                ],
                evaluator="run-nested:discovery-eval",
                acceptance_criteria="discovery accepted",
                handoff_note="discovery handoff",
            ),
            _node("run-nested:scan", "executor", "done", "run-nested:discovery"),
            _node(
                "run-nested:synthesize",
                "executor",
                "done",
                "run-nested:discovery",
                [
                    "run-nested:synthesize-draft",
                    "run-nested:synthesize-review",
                    "run-nested:synthesize-eval",
                ],
                evaluator="run-nested:synthesize-eval",
                acceptance_criteria="synthesis accepted",
                handoff_note="synthesis handoff",
            ),
            _node(
                "run-nested:synthesize-draft",
                "executor",
                "done",
                "run-nested:synthesize",
                [
                    "run-nested:draft-outline",
                    "run-nested:draft-body",
                    "run-nested:synthesize-draft-eval",
                ],
                evaluator="run-nested:synthesize-draft-eval",
                acceptance_criteria="draft accepted",
                handoff_note="draft handoff",
            ),
            _node(
                "run-nested:draft-outline",
                "executor",
                "done",
                "run-nested:synthesize-draft",
            ),
            _node(
                "run-nested:draft-body",
                "executor",
                "done",
                "run-nested:synthesize-draft",
                [
                    "run-nested:body-section-a",
                    "run-nested:body-section-b",
                    "run-nested:draft-body-eval",
                ],
                evaluator="run-nested:draft-body-eval",
                acceptance_criteria="body accepted",
                handoff_note="body handoff",
            ),
            _node(
                "run-nested:body-section-a",
                "executor",
                "done",
                "run-nested:draft-body",
            ),
            _node(
                "run-nested:body-section-b",
                "executor",
                "done",
                "run-nested:draft-body",
                [
                    "run-nested:body-final-copy",
                    "run-nested:body-section-b-eval",
                ],
                evaluator="run-nested:body-section-b-eval",
                acceptance_criteria="final body accepted",
                handoff_note="final body handoff",
            ),
            _node(
                "run-nested:body-final-copy",
                "executor",
                "done",
                "run-nested:body-section-b",
            ),
            _node(
                "run-nested:body-section-b-eval",
                "evaluator",
                "done",
                "run-nested:body-section-b",
                acceptance_criteria="final body accepted",
                handoff_note="final body handoff",
            ),
            _node(
                "run-nested:draft-body-eval",
                "evaluator",
                "done",
                "run-nested:draft-body",
                acceptance_criteria="body accepted",
                handoff_note="body handoff",
            ),
            _node(
                "run-nested:synthesize-draft-eval",
                "evaluator",
                "done",
                "run-nested:synthesize-draft",
                acceptance_criteria="draft accepted",
                handoff_note="draft handoff",
            ),
            _node(
                "run-nested:synthesize-review",
                "executor",
                "done",
                "run-nested:synthesize",
            ),
            _node(
                "run-nested:synthesize-eval",
                "evaluator",
                "done",
                "run-nested:synthesize",
                acceptance_criteria="synthesis accepted",
                handoff_note="synthesis handoff",
            ),
            _node(
                "run-nested:discovery-eval",
                "evaluator",
                "done",
                "run-nested:discovery",
                acceptance_criteria="discovery accepted",
                handoff_note="discovery handoff",
            ),
            _node(
                "run-nested:delivery",
                "executor",
                "done",
                "run-nested:t1",
                [
                    "run-nested:implementation",
                    "run-nested:verification",
                    "run-nested:delivery-eval",
                ],
                evaluator="run-nested:delivery-eval",
                acceptance_criteria="delivery accepted",
                handoff_note="delivery handoff",
            ),
            _node(
                "run-nested:implementation",
                "executor",
                "done",
                "run-nested:delivery",
            ),
            _node(
                "run-nested:verification",
                "executor",
                "done",
                "run-nested:delivery",
            ),
            _node(
                "run-nested:delivery-eval",
                "evaluator",
                "done",
                "run-nested:delivery",
                acceptance_criteria="delivery accepted",
                handoff_note="delivery handoff",
            ),
            _node(
                "run-nested:t1-eval",
                "evaluator",
                "done",
                "run-nested:t1",
                acceptance_criteria="root accepted",
                handoff_note="root handoff",
            ),
        ],
    }
    assert _graph_ascii(store, "run-nested") == dedent(
        """\
        run-nested:t1 (executor, done) eval=run-nested:t1-eval ac='root accepted' handoff='root handoff'
        |- run-nested:discovery (executor, done) eval=run-nested:discovery-eval ac='discovery accepted' handoff='discovery handoff'
        |  |- run-nested:scan (executor, done)
        |  |- run-nested:synthesize (executor, done) eval=run-nested:synthesize-eval ac='synthesis accepted' handoff='synthesis handoff'
        |  |  |- run-nested:synthesize-draft (executor, done) eval=run-nested:synthesize-draft-eval ac='draft accepted' handoff='draft handoff'
        |  |  |  |- run-nested:draft-outline (executor, done)
        |  |  |  |- run-nested:draft-body (executor, done) eval=run-nested:draft-body-eval ac='body accepted' handoff='body handoff'
        |  |  |  |  |- run-nested:body-section-a (executor, done)
        |  |  |  |  |- run-nested:body-section-b (executor, done) eval=run-nested:body-section-b-eval ac='final body accepted' handoff='final body handoff'
        |  |  |  |  |  |- run-nested:body-final-copy (executor, done)
        |  |  |  |  |  `- run-nested:body-section-b-eval (evaluator, done) ac='final body accepted' handoff='final body handoff'
        |  |  |  |  `- run-nested:draft-body-eval (evaluator, done) ac='body accepted' handoff='body handoff'
        |  |  |  `- run-nested:synthesize-draft-eval (evaluator, done) ac='draft accepted' handoff='draft handoff'
        |  |  |- run-nested:synthesize-review (executor, done)
        |  |  `- run-nested:synthesize-eval (evaluator, done) ac='synthesis accepted' handoff='synthesis handoff'
        |  `- run-nested:discovery-eval (evaluator, done) ac='discovery accepted' handoff='discovery handoff'
        |- run-nested:delivery (executor, done) eval=run-nested:delivery-eval ac='delivery accepted' handoff='delivery handoff'
        |  |- run-nested:implementation (executor, done)
        |  |- run-nested:verification (executor, done)
        |  `- run-nested:delivery-eval (evaluator, done) ac='delivery accepted' handoff='delivery handoff'
        `- run-nested:t1-eval (evaluator, done) ac='root accepted' handoff='root handoff'
        """
    ).strip()
