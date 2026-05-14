"""Phase 4 native load probe for OCC orchestrator commits."""

from __future__ import annotations

import pytest

from .._harness.load_profiles import OCC_LOAD
from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
from sandbox.layer_stack.layer.change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.types import FileStatus, WriteChange
from sandbox.occ.service import OccService

class _Gitignore:
    def is_ignored(self, path):
        return path.startswith("dist/")

cfg = json.loads(__CFG_JSON__)
label = "occ.load"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
for index in range(5):
    stack.publish_changes([
        WriteLayerChange(
            path="tracked/shared-%02d.txt" % index,
            source_path=str(_source(root, "shared-%02d" % index, b"base\n")),
        )
    ])
service = OccService(gitignore=_Gitignore(), layer_stack=stack)
operation_count = int(cfg["operation_count"])
concurrency = int(cfg["concurrency"])
barrier = threading.Barrier(concurrency)

def commit_one(index):
    stale_snapshot = stack.read_active_manifest() if index % 2 == 0 else None
    changes = []
    for slot in range(5):
        if slot < 3:
            path = "tracked/shared-%02d.txt" % (slot if index % 2 == 0 else (index + slot) % 5)
            source = "overlay_capture" if index % 2 == 0 else "api_write"
        else:
            path = "dist/load-%02d-%02d.txt" % (index, slot)
            source = "overlay_capture"
        changes.append(
            WriteChange(
                path=path,
                source=source,
                final_content=("writer-%03d-%02d\n" % (index, slot)).encode("utf-8"),
            )
        )
    barrier.wait(timeout=10)
    t0 = time.perf_counter()
    result = service.apply_changeset_sync(changes, snapshot=stale_snapshot)
    elapsed_ms = (time.perf_counter() - t0) * 1000.0
    return {
        "index": index,
        "elapsed_ms": elapsed_ms,
        "statuses": [_status(file.status) for file in result.files],
        "success": result.success,
        "timings": result.timings,
    }

with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
    rows = list(pool.map(commit_one, range(operation_count)))

latencies = [row["elapsed_ms"] for row in rows]
p99 = _percentile(latencies, 99)
accepted_files = sum(status in ("accepted", "committed") for row in rows for status in row["statuses"])
rejected_files = sum(status.startswith("aborted") for row in rows for status in row["statuses"])
starved = [row for row in rows if row["elapsed_ms"] > 30000]
assert not starved
assert p99 < float(cfg["max_p99_ms"]), p99

def timing_p99(key):
    return _percentile([row["timings"].get(key, 0.0) * 1000.0 for row in rows], 99)

_emit(label, started, before, {
    "profile": cfg,
    "operations": len(rows),
    "accepted_files": accepted_files,
    "rejected_files": rejected_files,
    "p50_ms": _percentile(latencies, 50),
    "p99_ms": p99,
    "max_ms": max(latencies),
    "starvation_count": len(starved),
    "manifest_depth": stack.read_active_manifest().depth,
    "occ_apply_p99_ms": timing_p99("occ.apply.total_s"),
    "occ_prepare_p99_ms": timing_p99("occ.prepare.total_s"),
    "occ_commit_p99_ms": timing_p99("occ.commit.total_s"),
    "occ_serial_queue_p99_ms": timing_p99("occ.serial.queue_wait_s"),
    "layer_lock_wait_p99_ms": timing_p99("layer_stack.transaction.lock_wait_s"),
})
"""


async def test_occ_load_p99_queue_and_starvation_budget(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="occ.load",
        cfg={
            "operation_count": OCC_LOAD.operation_count,
            "concurrency": OCC_LOAD.concurrency,
            "max_p99_ms": OCC_LOAD.max_p99_ms,
        },
        timeout=240,
    )
    assert payload["operations"] == OCC_LOAD.operation_count
    assert payload["p99_ms"] < OCC_LOAD.max_p99_ms
    assert payload["starvation_count"] == 0
