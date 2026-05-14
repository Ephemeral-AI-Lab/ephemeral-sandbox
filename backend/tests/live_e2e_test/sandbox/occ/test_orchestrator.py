"""Phase 1b native probes for OCC orchestration."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_ORCHESTRATOR_BODY = r"""
from sandbox.layer_stack.layer.change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.prepared import CommitOptions, RouteDecision
from sandbox.occ.changeset.types import FileStatus, WriteChange
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.service import Service

class _Gitignore:
    def __init__(self, ignored=()):
        self.ignored = set(ignored)
    def is_ignored(self, path):
        return path in self.ignored

def _publish(stack, rel, content):
    stack.publish_changes([
        WriteLayerChange(
            path=rel,
            content_hash=ContentHasher().hash_bytes(content),
            source_path=str(_source(root, rel.replace("/", "-"), content)),
        )
    ])

label = "occ.orchestrator"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
service = Service(gitignore=_Gitignore({"dist/app.js"}), snapshot_reader=stack, staging=stack, publisher=stack)
_publish(stack, "src/app.py", b"base\n")
snapshot = stack.read_active_manifest()

prepared = service.prepare_changeset_sync(
    [
        WriteChange(path="src/app.py", final_content=b"next\n"),
        WriteChange(path="dist/app.js", final_content=b"built\n"),
        WriteChange(path=".git/config", final_content=b"bad"),
        WriteChange(path="../escape", final_content=b"bad"),
    ],
    snapshot=snapshot,
    options=CommitOptions(),
)
routes = [(group.path, group.route.value) for group in prepared.path_groups]
assert routes == [
    ("src/app.py", RouteDecision.GATED.value),
    ("dist/app.js", RouteDecision.DIRECT.value),
    (".git/config", RouteDecision.DROP.value),
    ("../escape", RouteDecision.REJECT.value),
]
[first_change] = prepared.path_groups[0].changes
assert first_change.base_hash == ContentHasher().hash_bytes(b"base\n")

happy = service.apply_changeset_sync(
    [WriteChange(path="src/app.py", final_content=b"next\n")],
    snapshot=snapshot,
)
assert happy.files[0].status is FileStatus.ACCEPTED

conflict = service.apply_changeset_sync(
    [WriteChange(path="src/app.py", final_content=b"conflict\n")],
    snapshot=snapshot,
)
assert conflict.files[0].status is FileStatus.ABORTED_VERSION

restarted_stack = LayerStackManager(root / "stack")
restarted_service = Service(gitignore=_Gitignore(), snapshot_reader=restarted_stack, staging=restarted_stack, publisher=restarted_stack)
after_restart = restarted_service.apply_changeset_sync(
    [WriteChange(path="src/restart.py", final_content=b"ok\n")],
    snapshot=restarted_stack.read_active_manifest(),
)
assert after_restart.files[0].status is FileStatus.ACCEPTED
assert restarted_stack.read_bytes("src/restart.py") == (b"ok\n", True)

_emit(label, started, before, {
    "routes": routes,
    "happy_status": _status(happy.files[0].status),
    "conflict_status": _status(conflict.files[0].status),
    "restart_status": _status(after_restart.files[0].status),
    "manifest_version": restarted_stack.read_active_manifest().version,
})
"""


_RACE_BODY = r"""
from sandbox.layer_stack.layer.change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.types import WriteChange
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.service import Service

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

label = "occ.orchestrator_under_race"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
service = Service(gitignore=_Gitignore(), snapshot_reader=stack, staging=stack, publisher=stack)
_publish(stack, "src/app.py", b"base\n")
snapshot = stack.read_active_manifest()
n = 4
barrier = threading.Barrier(n)

def run_one(index):
    prepared = service.prepare_changeset_sync(
        [
            WriteChange(
                path="src/app.py",
                source="overlay_capture",
                final_content=("agent-%d\n" % index).encode("utf-8"),
            )
        ],
        snapshot=snapshot,
    )
    barrier.wait(timeout=5)
    result = service.apply_changeset_sync(prepared.path_groups[0].changes, snapshot=snapshot)
    return {"index": index, "status": _status(result.files[0].status)}

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    rows = list(pool.map(run_one, range(n)))
statuses = [row["status"] for row in rows]
assert statuses.count("accepted") == 1, rows
assert statuses.count("aborted_version") == n - 1, rows

_emit(label, started, before, {
    "orchestrators": n,
    "statuses": statuses,
    "accepted": statuses.count("accepted"),
    "aborted_version": statuses.count("aborted_version"),
})
"""


async def test_orchestrator_routes_happy_conflict_abort_and_restart(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _ORCHESTRATOR_BODY,
        label="occ.orchestrator",
    )
    assert payload["happy_status"] == "accepted"
    assert payload["conflict_status"] == "aborted_version"
    assert payload["restart_status"] == "accepted"


async def test_orchestrator_under_race_detects_conflicts_deterministically(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="occ.orchestrator_under_race",
    )
    assert payload["accepted"] == 1
    assert payload["aborted_version"] == 3
