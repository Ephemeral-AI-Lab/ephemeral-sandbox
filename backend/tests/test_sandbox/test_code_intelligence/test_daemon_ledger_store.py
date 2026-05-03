"""Unit tests for ``storage.LedgerStore`` (Phase 3 SQLite WAL ledger)."""

from __future__ import annotations

import inspect
import sqlite3
import threading
from datetime import timezone
from pathlib import Path

import pytest

from sandbox.code_intelligence.daemon.storage import LedgerStore
from sandbox.code_intelligence.mutations.edit_history_ledger import (
    ContentionHotspot,
    EditHistoryLedger,
    EditRecord,
)


@pytest.fixture()
def store(tmp_path: Path) -> LedgerStore:
    return LedgerStore(state_dir_path=tmp_path)


def test_wal_pragma_applied(store: LedgerStore) -> None:
    with sqlite3.connect(store.path) as conn:
        mode = conn.execute("PRAGMA journal_mode").fetchone()[0]
    assert mode.lower() == "wal"


def test_schema_created_on_first_open(store: LedgerStore, tmp_path: Path) -> None:
    with sqlite3.connect(tmp_path / "ledger.sqlite3") as conn:
        rows = conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table'"
        ).fetchall()
    names = {row[0] for row in rows}
    assert "edits" in names

    with sqlite3.connect(tmp_path / "ledger.sqlite3") as conn:
        idx = conn.execute(
            "SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_edits_%'"
        ).fetchall()
    assert {row[0] for row in idx} >= {
        "idx_edits_file",
        "idx_edits_ts",
        "idx_edits_run",
        "idx_edits_agent_run",
    }


def test_record_round_trip(store: LedgerStore) -> None:
    rec = store.record(
        run_id="run-1",
        file_path="/workspace/foo.py",
        agent_run_id="agent-a",
        task_id="task-1",
        edit_type="write_file",
        old_hash="aaa",
        new_hash="bbb",
        description="add foo",
    )
    assert isinstance(rec, EditRecord)
    assert rec.id >= 1
    assert rec.run_id == "run-1"
    assert rec.created_at.tzinfo is timezone.utc

    fetched = store.who_changed("/workspace/foo.py")
    assert len(fetched) == 1
    assert fetched[0].file_path == "/workspace/foo.py"
    assert fetched[0].new_hash == "bbb"


def test_recent_edits_returns_within_window(store: LedgerStore) -> None:
    store.record(run_id="r", file_path="/a.py")
    rows = store.recent_edits(seconds=60.0)
    assert len(rows) == 1
    assert rows[0].file_path == "/a.py"

    # Ancient cutoff returns nothing.
    rows = store.recent_edits(seconds=0.0)
    assert rows == []


def test_changes_in_scope_filters_run_and_prefix(store: LedgerStore) -> None:
    store.record(run_id="r1", file_path="/a/x.py", agent_run_id="ag-a")
    store.record(run_id="r1", file_path="/b/y.py", agent_run_id="ag-b")
    store.record(run_id="r2", file_path="/a/z.py", agent_run_id="ag-c")

    rows = store.changes_in_scope("r1", ["/a"], since=0.0)
    paths = sorted(r.file_path for r in rows)
    assert paths == ["/a/x.py"]


def test_external_changes_excludes_same_agent_run(store: LedgerStore) -> None:
    store.record(run_id="r1", file_path="/a/x.py", agent_run_id="ag-a")
    store.record(run_id="r1", file_path="/a/y.py", agent_run_id="ag-b")

    rows = store.external_changes_in_scope(
        "r1", ["/a"], since=0.0, exclude_run_id="ag-a"
    )
    assert [r.file_path for r in rows] == ["/a/y.py"]


def test_hotspots_returns_top_files(store: LedgerStore) -> None:
    store.record(run_id="r", file_path="/a.py")
    store.record(run_id="r", file_path="/a.py")
    store.record(run_id="r", file_path="/b.py")

    hotspots = store.hotspots(limit=5)
    assert hotspots == [("/a.py", 2), ("/b.py", 1)]


def test_changes_by_agent_run_filters(store: LedgerStore) -> None:
    store.record(run_id="r", file_path="/x.py", agent_run_id="ag-a")
    store.record(run_id="r", file_path="/y.py", agent_run_id="ag-b")

    rows = store.changes_by_agent_run("r", "ag-a")
    assert [r.file_path for r in rows] == ["/x.py"]
    assert store.changes_by_agent_run("r", "") == []


def test_contention_hotspots_requires_multiple_contributors(
    store: LedgerStore,
) -> None:
    store.record(run_id="r", file_path="/hot.py", task_id="t1")
    store.record(run_id="r", file_path="/hot.py", task_id="t2")
    store.record(run_id="r", file_path="/cold.py", task_id="t1")

    rows = store.contention_hotspots(scope_prefixes=["/"], limit=5)
    assert len(rows) == 1
    assert isinstance(rows[0], ContentionHotspot)
    assert rows[0].file_path == "/hot.py"
    assert rows[0].contributor_count == 2
    assert rows[0].edit_count == 2


def test_concurrent_record_inserts_distinct_rows(store: LedgerStore) -> None:
    threads = []
    errors: list[BaseException] = []

    def worker(i: int) -> None:
        try:
            store.record(run_id="r", file_path=f"/thread_{i}.py")
        except BaseException as exc:  # noqa: BLE001
            errors.append(exc)

    for i in range(10):
        t = threading.Thread(target=worker, args=(i,))
        threads.append(t)
        t.start()
    for t in threads:
        t.join()

    assert errors == []
    paths = {r.file_path for r in store.recent_edits(seconds=10.0)}
    assert len(paths) == 10
    assert paths == {f"/thread_{i}.py" for i in range(10)}


def test_integrity_failure_rotates_and_recreates(tmp_path: Path) -> None:
    bad = tmp_path / "ledger.sqlite3"
    bad.write_bytes(b"not a real sqlite database")

    store = LedgerStore(state_dir_path=tmp_path)
    rotations = list(tmp_path.glob("ledger.corrupt.*.sqlite3"))
    assert rotations, "corrupt file was not rotated"

    rec = store.record(run_id="r", file_path="/post-rotation.py")
    assert rec.id >= 1
    assert store.recent_edits(seconds=60.0)[0].file_path == "/post-rotation.py"


def test_interface_matches_edit_history_ledger() -> None:
    """LedgerStore must duck-type match the in-memory ledger interface."""
    expected_methods = [
        "record",
        "changes_in_scope",
        "external_changes_in_scope",
        "changes_since",
        "recent_edits",
        "hotspots",
        "who_changed",
        "changes_by_agent_run",
        "contention_hotspots",
    ]
    for name in expected_methods:
        ledger_sig = inspect.signature(getattr(EditHistoryLedger, name))
        store_sig = inspect.signature(getattr(LedgerStore, name))

        ledger_params = [p.name for p in ledger_sig.parameters.values()]
        store_params = [p.name for p in store_sig.parameters.values()]
        assert ledger_params == store_params, (
            f"{name} parameter list mismatch: "
            f"ledger={ledger_params!r} store={store_params!r}"
        )

    assert hasattr(LedgerStore, "initialized")
    assert hasattr(EditHistoryLedger, "initialized")


def test_close_releases_connection(store: LedgerStore) -> None:
    store.record(run_id="r", file_path="/x.py")
    store.close()
    # After close, sqlite raises ProgrammingError for further use.
    with pytest.raises(sqlite3.ProgrammingError):
        store.recent_edits(seconds=10.0)
