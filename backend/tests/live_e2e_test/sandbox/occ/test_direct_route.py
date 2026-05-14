"""Phase 1b native probes for direct OCC route commits."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_DIRECT_BODY = r"""
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.types import ChangesetResult, FileStatus, WriteChange
from sandbox.occ.service import Service

class _Gitignore:
    def is_ignored(self, path):
        return True

label = "occ.direct_route"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
service = Service(gitignore=_Gitignore(), snapshot_reader=stack, staging=stack, publisher=stack)

empty = service.apply_changeset_sync([], snapshot=stack.read_active_manifest())
assert isinstance(empty, ChangesetResult)
assert empty.files == ()
assert empty.published_manifest_version is None

n = 10000
t0 = time.perf_counter()
large = service.apply_changeset_sync(
    [WriteChange(path="dist/%05d.txt" % index, final_content=b"x") for index in range(n)],
    snapshot=stack.read_active_manifest(),
)
large_ms = (time.perf_counter() - t0) * 1000.0
assert len(large.files) == n
assert all(file.status is FileStatus.ACCEPTED for file in large.files)
assert stack.read_active_manifest().depth == 1
assert stack.read_bytes("dist/09999.txt") == (b"x", True)

_emit(label, started, before, {
    "empty_files": len(empty.files),
    "large_paths": n,
    "large_accepted": sum(1 for file in large.files if file.status is FileStatus.ACCEPTED),
    "large_elapsed_ms": large_ms,
    "timings": large.timings,
})
"""


_RACE_BODY = r"""
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.types import WriteChange
from sandbox.occ.service import Service

class _Gitignore:
    def is_ignored(self, path):
        return True

label = "occ.direct_route_under_race"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
service = Service(gitignore=_Gitignore(), snapshot_reader=stack, staging=stack, publisher=stack)
n = 8
barrier = threading.Barrier(n)

def direct_commit(index):
    barrier.wait(timeout=5)
    t0 = time.perf_counter()
    result = service.apply_changeset_sync(
        [WriteChange(path="dist/race-%02d.txt" % index, final_content=("value-%02d" % index).encode("utf-8"))],
        snapshot=stack.read_active_manifest(),
    )
    return {
        "index": index,
        "status": _status(result.files[0].status),
        "elapsed_ms": (time.perf_counter() - t0) * 1000.0,
    }

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    rows = list(pool.map(direct_commit, range(n)))
assert all(row["status"] == "accepted" for row in rows), rows
for index in range(n):
    assert stack.read_text("dist/race-%02d.txt" % index) == ("value-%02d" % index, True)
latencies = [row["elapsed_ms"] for row in rows]

_emit(label, started, before, {
    "commits": n,
    "accepted": sum(1 for row in rows if row["status"] == "accepted"),
    "manifest_depth": stack.read_active_manifest().depth,
    "direct_p50_ms": _percentile(latencies, 50),
    "direct_p99_ms": _percentile(latencies, 99),
    "direct_max_ms": max(latencies),
})
"""


async def test_direct_route_handles_empty_and_10k_path_changesets(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _DIRECT_BODY,
        label="occ.direct_route",
        timeout=240,
        fd_delta_max=4,
    )
    assert payload["empty_files"] == 0
    assert payload["large_paths"] == 10000
    assert payload["large_accepted"] == 10000


async def test_direct_route_under_race_commits_disjoint_paths(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="occ.direct_route_under_race",
    )
    assert payload["commits"] == 8
    assert payload["accepted"] == 8
