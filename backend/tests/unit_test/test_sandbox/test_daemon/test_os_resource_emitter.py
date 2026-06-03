"""Phase 2.5 slice 4 — ``os_resource.sampled`` emitter piggybacks the
command-exec resource-metrics tick (no new threads).
"""

from __future__ import annotations

import threading
from pathlib import Path
from typing import Any

import pytest

from sandbox._shared.command_exec_resource_metrics import (
    collect_command_exec_resource_metrics,
)
from sandbox.daemon.audit_buffer import get_audit_buffer


_AUDIT_CURSOR = {"seq": -1}


def _drain_os_resource_events() -> list[dict[str, Any]]:
    buf = get_audit_buffer()
    snap = buf.pull(after_seq=_AUDIT_CURSOR["seq"], limit=10_000)
    events = snap.get("events", [])
    if events:
        _AUDIT_CURSOR["seq"] = int(events[-1]["seq"])
    return [evt for evt in events if str(evt.get("type", "")) == "os_resource.sampled"]


@pytest.fixture(autouse=True)
def _reset_audit_cursor() -> None:
    buf = get_audit_buffer()
    cursor = -1
    while True:
        snap = buf.pull(after_seq=cursor, limit=10_000)
        events = snap.get("events", [])
        if not events:
            break
        cursor = int(events[-1]["seq"])
    _AUDIT_CURSOR["seq"] = cursor
    yield


def test_os_resource_sampled_emitted_on_collect(tmp_path: Path) -> None:
    storage = tmp_path / "storage"
    storage.mkdir()
    writable = tmp_path / "writable"
    writable.mkdir()
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    upperdir = tmp_path / "upper"
    upperdir.mkdir()
    collect_command_exec_resource_metrics(
        storage_root=storage,
        writable_root=writable,
        run_dir=run_dir,
        upperdir=upperdir,
        manifest=None,
        changed_path_count=0,
    )
    events = _drain_os_resource_events()
    assert events, "expected one os_resource.sampled event"
    section = events[0]["payload"]["os_resource"]
    assert "sampled_at_monotonic_s" in section
    assert events[0]["lane"] == "sample"


def test_os_resource_sampled_adds_no_new_threads(tmp_path: Path) -> None:
    storage = tmp_path / "storage"
    storage.mkdir()
    before = threading.active_count()
    for _ in range(3):
        collect_command_exec_resource_metrics(
            storage_root=storage,
            writable_root=storage,
            run_dir=storage,
            upperdir=storage,
            manifest=None,
            changed_path_count=0,
        )
    after = threading.active_count()
    assert after == before, (
        f"unexpected thread growth before={before} after={after}"
    )
