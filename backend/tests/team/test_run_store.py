from __future__ import annotations

from team.persistence.events import TeamRunEvent
from team.persistence.run_store import TeamRunStore, build_default_store


def _event(run_id: str, status: str) -> TeamRunEvent:
    return TeamRunEvent(
        team_run_id=run_id,
        kind="team_run_status",
        data={"status": status},
    )


def test_team_run_store_without_base_dir_is_noop() -> None:
    store = TeamRunStore()
    event = _event("run-1", "running")

    store.append(event)

    assert event.seq == 0
    assert store.load_run("run-1") == []
    assert store.list_runs() == []


def test_team_run_store_persists_events_when_configured(tmp_path) -> None:
    store = TeamRunStore(tmp_path)

    store.append(_event("run-1", "running"))
    store.append(_event("run-1", "done"))

    loaded = store.load_run("run-1")

    assert [event.seq for event in loaded] == [1, 2]
    assert [event.data["status"] for event in loaded] == ["running", "done"]
    assert store.list_runs() == ["run-1"]


def test_build_default_store_uses_env_dir(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("EPHEMERALOS_TEAM_RUN_DIR", str(tmp_path))
    store = build_default_store()

    store.append(_event("run-2", "running"))

    assert store.list_runs() == ["run-2"]
