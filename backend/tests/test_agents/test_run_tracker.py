"""Tests for agents.run_tracker."""

from __future__ import annotations

from types import SimpleNamespace

from agents.run_tracker import AgentRunTracker


def test_create_retries_on_duplicate_auto_run_id(monkeypatch):
    class DuplicateKeyError(Exception):
        pass

    calls: list[str] = []

    class FakeStore:
        def create_run(self, **kwargs):
            calls.append(kwargs["run_id"])
            if len(calls) == 1:
                raise DuplicateKeyError("duplicate key value violates unique constraint")
            return None

    ids = iter(
        [
            SimpleNamespace(hex="duplicate000000"),
            SimpleNamespace(hex="fresh-run-id-01"),
        ]
    )

    monkeypatch.setattr("agents.run_tracker._get_agent_run_store", lambda: FakeStore())
    monkeypatch.setattr("agents.run_tracker.uuid4", lambda: next(ids))

    tracker = AgentRunTracker.create(
        session_id="s1",
        agent_name="developer",
        input_query="payload",
    )

    assert tracker.run_id == "fresh-run-id-01"[:16]
    assert calls == ["duplicate000000"[:16], "fresh-run-id-01"[:16]]
