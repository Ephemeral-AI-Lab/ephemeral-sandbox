"""Lease-budget and publish-backpressure tests for layer stacks."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.layer_stack import (
    CommitBackpressureError,
    LayerChange,
    LayerRef,
    LayerStackManager,
    LeaseBudgetWorker,
    LeaseSnapshot,
)


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def test_lease_budget_backpressures_when_active_depth_reaches_limit() -> None:
    worker = LeaseBudgetWorker(max_active_depth=2)

    decision = worker.evaluate(active_depth=2, snapshots=[])

    assert decision.kind == "backpressure_commits"
    assert decision.lease_id is None


def test_lease_budget_zero_depth_is_closed() -> None:
    worker = LeaseBudgetWorker(max_active_depth=0)

    decision = worker.evaluate(active_depth=0, snapshots=[])

    assert decision.kind == "backpressure_commits"
    assert decision.lease_id is None


def test_lease_budget_marks_oldest_expired_lease_for_kill() -> None:
    worker = LeaseBudgetWorker(kill_lease_age_seconds=10, clock=lambda: 25)
    snapshot = LeaseSnapshot(
        lease_id="lease-a",
        owner_id="request-a",
        manifest_version=3,
        pinned_layers=(LayerRef(layer_id="L1", path="layers/L1"),),
        pinned_bytes=1,
        acquired_at=10,
    )

    decision = worker.evaluate(active_depth=1, snapshots=[snapshot])

    assert decision.kind == "kill_lease"
    assert decision.lease_id == "lease-a"


def test_manager_reports_pinned_bytes_for_active_lease(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            LayerChange(
                path="payload.txt",
                kind="write",
                source_path=_source(tmp_path, "payload.txt", b"bytes"),
            )
        ]
    )

    lease = manager.acquire_snapshot_lease("request-a")

    assert manager.lease_snapshots() == (
        LeaseSnapshot(
            lease_id=lease.lease_id,
            owner_id="request-a",
            manifest_version=1,
            pinned_layers=lease.manifest.layers,
            pinned_bytes=5,
            acquired_at=lease.acquired_at,
        ),
    )


def test_publish_backpressure_blocks_before_staging(tmp_path: Path) -> None:
    manager = LayerStackManager(
        tmp_path / "stack",
        lease_budget=LeaseBudgetWorker(max_active_depth=1),
    )
    manager.publish_changes(
        [
            LayerChange(
                path="first.txt",
                kind="write",
                source_path=_source(tmp_path, "first.txt", b"first"),
            )
        ]
    )

    with pytest.raises(CommitBackpressureError):
        manager.publish_changes(
            [
                LayerChange(
                    path="second.txt",
                    kind="write",
                    source_path=_source(tmp_path, "second.txt", b"second"),
                )
            ]
        )

    assert tuple((manager.storage_root / "staging").iterdir()) == ()
    assert manager.read_text("second.txt") == ("", False)
