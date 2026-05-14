"""Phase 1b native probes for OCC serial merger fairness."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_SERIAL_MERGER_BODY = r"""
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import PreparedChangeset, PreparedPathGroup, RouteDecision
from sandbox.occ.changeset.types import ChangesetResult, FileResult, FileStatus, WriteChange
from sandbox.occ.merge.serial import OccSerialMerger

class _RecordingTransaction:
    def __init__(self):
        self.lock = threading.Lock()
        self.order = []
    def revalidate_and_publish(self, prepared):
        with self.lock:
            self.order.extend(group.path for group in prepared.path_groups)
        time.sleep(0.005)
        return ChangesetResult(
            files=tuple(FileResult(path=group.path, status=FileStatus.ACCEPTED) for group in prepared.path_groups),
            timings={"fake.commit_s": 0.005},
            published_manifest_version=1,
        )

def _prepared(path, *, atomic=True):
    return PreparedChangeset(
        snapshot=Manifest(version=0, layers=()),
        path_groups=(PreparedPathGroup(
            path=path,
            route=RouteDecision.DIRECT,
            changes=(WriteChange(path=path, final_content=b"x"),),
        ),),
        atomic=atomic,
    )

label = "occ.serial_merger"
before = sample_resource()
started = time.perf_counter()
txn = _RecordingTransaction()
merger = OccSerialMerger(txn, max_batch_size=1, batch_window_s=0.0)

futures = [merger.submit(_prepared("fifo/%02d.txt" % index)) for index in range(4)]
cancelled = merger.submit(_prepared("fifo/cancelled.txt"))
assert cancelled.cancel() is True
results = [future.result(timeout=5) for future in futures]
assert cancelled.cancelled() is True
assert txn.order == ["fifo/%02d.txt" % index for index in range(4)], txn.order
assert all(result.files[0].status is FileStatus.ACCEPTED for result in results)

_emit(label, started, before, {
    "processed_order": txn.order,
    "cancelled_mid_wait": cancelled.cancelled(),
    "result_count": len(results),
})
"""


_RACE_BODY = r"""
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import PreparedChangeset, PreparedPathGroup, RouteDecision
from sandbox.occ.changeset.types import ChangesetResult, FileResult, FileStatus, WriteChange
from sandbox.occ.merge.serial import OccSerialMerger

class _RecordingTransaction:
    def __init__(self):
        self.lock = threading.Lock()
        self.order = []
    def revalidate_and_publish(self, prepared):
        with self.lock:
            self.order.extend(group.path for group in prepared.path_groups)
        time.sleep(0.002)
        return ChangesetResult(
            files=tuple(FileResult(path=group.path, status=FileStatus.ACCEPTED) for group in prepared.path_groups),
            timings={"fake.commit_s": 0.002},
            published_manifest_version=1,
        )

def _prepared(path):
    return PreparedChangeset(
        snapshot=Manifest(version=0, layers=()),
        path_groups=(PreparedPathGroup(
            path=path,
            route=RouteDecision.DIRECT,
            changes=(WriteChange(path=path, final_content=b"x"),),
        ),),
        atomic=True,
    )

label = "occ.serial_merger_under_race"
before = sample_resource()
started = time.perf_counter()
txn = _RecordingTransaction()
merger = OccSerialMerger(txn, max_batch_size=1, batch_window_s=0.0)
n = 16
paths = ["wait/%02d.txt" % index for index in range(n)]
futures = [merger.submit(_prepared(path)) for path in paths]

def wait_one(index):
    t0 = time.perf_counter()
    result = futures[index].result(timeout=30)
    return {
        "path": paths[index],
        "status": _status(result.files[0].status),
        "wait_ms": (time.perf_counter() - t0) * 1000.0,
    }

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    rows = list(pool.map(wait_one, range(n)))

waits = [row["wait_ms"] for row in rows]
assert txn.order == paths, txn.order
assert all(row["status"] == "accepted" for row in rows), rows
assert max(waits) < 30000.0, waits

_emit(label, started, before, {
    "waiters": n,
    "fifo_upheld": txn.order == paths,
    "max_wait_ms": max(waits),
    "wait_p50_ms": _percentile(waits, 50),
    "wait_p99_ms": _percentile(waits, 99),
})
"""


async def test_serial_merger_orders_commits_and_honors_cancellation(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _SERIAL_MERGER_BODY,
        label="occ.serial_merger",
    )
    assert payload["cancelled_mid_wait"] is True
    assert payload["processed_order"] == [f"fifo/{index:02d}.txt" for index in range(4)]


async def test_serial_merger_under_race_preserves_fifo_without_starvation(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="occ.serial_merger_under_race",
    )
    assert payload["waiters"] == 16
    assert payload["fifo_upheld"] is True
    assert payload["max_wait_ms"] < 30000
