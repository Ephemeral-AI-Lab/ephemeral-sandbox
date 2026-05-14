"""Phase 1b native probes for gated OCC route conflicts."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_GATED_BODY = r"""
from sandbox.layer_stack.layer.change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.types import FileStatus, WriteChange
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

label = "occ.gated_route"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
service = Service(gitignore=_Gitignore(), snapshot_reader=stack, staging=stack, publisher=stack)
_publish(stack, "src/race.py", b"base\n")
snapshot = stack.read_active_manifest()

barrier = threading.Barrier(2)
def race_write(index):
    barrier.wait(timeout=5)
    result = service.apply_changeset_sync(
        [
            WriteChange(
                path="src/race.py",
                source="overlay_capture",
                final_content=("winner-%d\n" % index).encode("utf-8"),
            )
        ],
        snapshot=snapshot,
    )
    return _status(result.files[0].status)

with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
    first_commit_wins = list(pool.map(race_write, range(2)))
assert first_commit_wins.count("accepted") == 1
assert first_commit_wins.count("aborted_version") == 1

stale_snapshot = snapshot
both_reject = [
    service.apply_changeset_sync(
        [WriteChange(path="src/race.py", source="overlay_capture", final_content=b"stale-a\n")],
        snapshot=stale_snapshot,
    ).files[0].status,
    service.apply_changeset_sync(
        [WriteChange(path="src/race.py", source="overlay_capture", final_content=b"stale-b\n")],
        snapshot=stale_snapshot,
    ).files[0].status,
]
assert [_status(status) for status in both_reject] == ["aborted_version", "aborted_version"]

fresh = stack.read_active_manifest()
_publish(stack, "src/other.py", b"changed\n")
partial = service.apply_changeset_sync(
    [
        WriteChange(path="src/other.py", source="overlay_capture", final_content=b"partial-a\n"),
        WriteChange(path="src/new.py", source="overlay_capture", final_content=b"partial-b\n"),
    ],
    snapshot=fresh,
)
partial_statuses = [_status(file.status) for file in partial.files]
assert partial_statuses == ["aborted_version", "dropped"], partial_statuses
assert partial.published_manifest_version is None
assert stack.read_bytes("src/new.py") == (None, False)

_emit(label, started, before, {
    "first_commit_wins": first_commit_wins,
    "both_reject": [_status(status) for status in both_reject],
    "partial_overlap": partial_statuses,
    "manifest_version": stack.read_active_manifest().version,
})
"""


async def test_gated_route_resolves_conflicting_concurrent_commits(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _GATED_BODY,
        label="occ.gated_route",
    )
    assert payload["first_commit_wins"].count("accepted") == 1
    assert payload["both_reject"] == ["aborted_version", "aborted_version"]
    assert payload["partial_overlap"] == ["aborted_version", "dropped"]
