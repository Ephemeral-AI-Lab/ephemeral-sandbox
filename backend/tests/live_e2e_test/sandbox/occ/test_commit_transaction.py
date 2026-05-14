"""Phase 1b native probes for OCC commit transactions."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_COMMIT_TRANSACTION_BODY = r"""
from sandbox.layer_stack.layer_change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.prepared import CommitOptions
from sandbox.occ.changeset.types import ChangesetResult, FileStatus, WriteChange
from sandbox.occ.changeset.builders import build_api_write_change, build_overlay_write_change

def write_change(*, path, final_content, source="api_write", base_hash=None):
    if source == "overlay_capture":
        return build_overlay_write_change(
            path=path,
            final_content=final_content,
        ).with_base_hash(base_hash)
    return build_api_write_change(
        path=path,
        final_content=final_content,
        base_hash=base_hash,
    )

from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.service import OccService

class _Gitignore:
    def is_ignored(self, path):
        return False

def _publish(stack, rel, content):
    stack.publish_changes([
        WriteLayerChange(
            path=rel,
            content_hash=ContentHasher().hash_bytes(content),
            source_path=str(_source(root, rel.replace("/", "-"), content)),
        )
    ])

label = "occ.commit_transaction"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
service = OccService(gitignore=_Gitignore(), layer_stack=stack)
_publish(stack, "src/app.py", b"old\n")
snapshot = stack.read_active_manifest()

result = service.apply_changeset_sync(
    [write_change(path="src/app.py", final_content=b"new\n")],
    snapshot=snapshot,
)
assert isinstance(result, ChangesetResult)
assert result.files[0].status is FileStatus.ACCEPTED
assert result.published_manifest_version == 2
assert stack.read_bytes("src/app.py") == (b"new\n", True)

stale = service.apply_changeset_sync(
    [write_change(path="src/app.py", final_content=b"stale\n")],
    snapshot=snapshot,
)
assert isinstance(stale, ChangesetResult)
assert stale.files[0].status is FileStatus.ABORTED_VERSION
assert stale.published_manifest_version is None
assert stack.read_bytes("src/app.py") == (b"new\n", True)

atomic = service.apply_changeset_sync(
    [
        write_change(path="src/ok.py", final_content=b"ok"),
        write_change(path="../escape", final_content=b"bad"),
    ],
    snapshot=stack.read_active_manifest(),
    options=CommitOptions(atomic=True),
)
assert isinstance(atomic, ChangesetResult)
assert [item.status for item in atomic.files] == [FileStatus.DROPPED, FileStatus.REJECTED]
assert atomic.published_manifest_version is None
assert stack.read_bytes("src/ok.py") == (None, False)

_emit(label, started, before, {
    "accepted_status": _status(result.files[0].status),
    "rollback_status": _status(stale.files[0].status),
    "atomic_statuses": [_status(item.status) for item in atomic.files],
    "manifest_version": stack.read_active_manifest().version,
    "timings": result.timings,
})
"""


_RACE_BODY = r"""
from sandbox.layer_stack.layer_change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.types import FileStatus, WriteChange
from sandbox.occ.changeset.builders import build_api_write_change, build_overlay_write_change

def write_change(*, path, final_content, source="api_write", base_hash=None):
    if source == "overlay_capture":
        return build_overlay_write_change(
            path=path,
            final_content=final_content,
        ).with_base_hash(base_hash)
    return build_api_write_change(
        path=path,
        final_content=final_content,
        base_hash=base_hash,
    )

from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.service import OccService

class _Gitignore:
    def is_ignored(self, path):
        return False

def _publish(stack, rel, content):
    stack.publish_changes([
        WriteLayerChange(
            path=rel,
            content_hash=ContentHasher().hash_bytes(content),
            source_path=str(_source(root, rel.replace("/", "-"), content)),
        )
    ])

label = "occ.commit_transaction_under_race"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
service = OccService(gitignore=_Gitignore(), layer_stack=stack)
_publish(stack, "src/app.py", b"base\n")
snapshot = stack.read_active_manifest()
n = 4
barrier = threading.Barrier(n)

def commit_one(index):
    barrier.wait(timeout=5)
    t0 = time.perf_counter()
    result = service.apply_changeset_sync(
        [
            write_change(
                path="src/app.py",
                source="overlay_capture",
                final_content=("agent-%d\n" % index).encode("utf-8"),
            )
        ],
        snapshot=snapshot,
    )
    return {
        "index": index,
        "status": _status(result.files[0].status),
        "published": result.published_manifest_version,
        "elapsed_ms": (time.perf_counter() - t0) * 1000.0,
    }

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    results = list(pool.map(commit_one, range(n)))
statuses = [row["status"] for row in results]
assert statuses.count("accepted") == 1, results
assert statuses.count("aborted_version") == n - 1, results
assert stack.read_active_manifest().version == 2
content, exists = stack.read_bytes("src/app.py")
assert exists and content in {("agent-%d\n" % index).encode("utf-8") for index in range(n)}
latencies = [row["elapsed_ms"] for row in results]

_emit(label, started, before, {
    "commits": n,
    "statuses": statuses,
    "accepted": statuses.count("accepted"),
    "aborted_version": statuses.count("aborted_version"),
    "commit_p50_ms": _percentile(latencies, 50),
    "commit_p99_ms": _percentile(latencies, 99),
    "commit_max_ms": max(latencies),
})
"""


async def test_commit_transaction_atomicity_rollback_and_retry(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _COMMIT_TRANSACTION_BODY,
        label="occ.commit_transaction",
    )
    assert payload["accepted_status"] == "accepted"
    assert payload["rollback_status"] == "aborted_version"


async def test_commit_transaction_under_race_is_atomic_per_commit(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="occ.commit_transaction_under_race",
    )
    assert payload["accepted"] == 1
    assert payload["aborted_version"] == 3
