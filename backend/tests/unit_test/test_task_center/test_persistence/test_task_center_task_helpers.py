"""Persistence tests for attempt-scoped TaskCenter task helpers."""

from __future__ import annotations


def _upsert(
    task_store,
    *,
    task_id: str,
    attempt_id: str | None,
    role: str = "generator",
    status: str = "pending",
    needs: list[str] | None = None,
    context_packet_id: str | None = None,
) -> None:
    task_store.upsert_task(
        task_id=task_id,
        task_center_run_id="run1",
        role=role,
        agent_name=role,
        rendered_prompt=f"input-{task_id}",
        status=status,
        summaries=[],
        needs=needs or [],
        task_center_attempt_id=attempt_id,
        context_packet_id=context_packet_id,
    )


def test_get_task_returns_serialized_task(task_store):
    _upsert(task_store, task_id="g1:gen:a", attempt_id="g1")

    task = task_store.get_task("g1:gen:a")

    assert task is not None
    assert task["id"] == "g1:gen:a"
    assert task["task_center_attempt_id"] == "g1"
    assert task["agent_name"] == "generator"
    assert task["needs"] == []
    assert task["context_packet_id"] is None


def test_request_and_run_helpers_return_serialized_rows(task_store):
    request = task_store.get_request("req1")
    run = task_store.get_run("run1")

    assert request is not None
    assert request["id"] == "req1"
    assert request["cwd"] == "/tmp"
    assert run is not None
    assert run["id"] == "run1"
    assert run["status"] == "running"


def test_list_tasks_for_attempt_filters_by_attempt_id(task_store):
    _upsert(task_store, task_id="g1:planner", attempt_id="g1", role="planner")
    _upsert(task_store, task_id="g2:planner", attempt_id="g2", role="planner")

    tasks = task_store.list_tasks_for_attempt("g1")

    assert [task["id"] for task in tasks] == ["g1:planner"]


def test_set_task_status_updates_status_and_appends_summary(task_store):
    _upsert(task_store, task_id="g1:gen:a", attempt_id="g1")

    updated = task_store.set_task_status(
        "g1:gen:a", status="done", summary={"summary": "ok"}
    )

    assert updated["status"] == "done"
    assert updated["summaries"] == [{"summary": "ok"}]


def test_context_packet_id_round_trips_and_updates(task_store):
    _upsert(
        task_store,
        task_id="g1:gen:a",
        attempt_id="g1",
        context_packet_id="packet-1",
    )

    task = task_store.get_task("g1:gen:a")
    assert task is not None
    assert task["context_packet_id"] == "packet-1"

    updated = task_store.set_task_context_packet_id(
        "g1:gen:a", context_packet_id="packet-2"
    )
    assert updated["context_packet_id"] == "packet-2"


def test_list_generator_tasks_excludes_planner_and_evaluator(task_store):
    _upsert(task_store, task_id="g1:planner", attempt_id="g1", role="planner")
    _upsert(task_store, task_id="g1:gen:a", attempt_id="g1", role="generator")
    _upsert(task_store, task_id="g1:evaluator", attempt_id="g1", role="evaluator")

    tasks = task_store.list_generator_tasks_for_attempt("g1")

    assert [task["id"] for task in tasks] == ["g1:gen:a"]
