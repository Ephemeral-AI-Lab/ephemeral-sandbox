"""Phase 4 native probes for OCC edge cases."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
from sandbox.layer_stack.layer_change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.prepared import CommitOptions
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

from sandbox.occ.service import OccService

class _Gitignore:
    def is_ignored(self, path):
        return path.startswith("dist/")

label = "occ.edge_cases"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
stack.publish_changes([
    WriteLayerChange(path="tracked/shared.txt", source_path=str(_source(root, "shared", b"base\n"))),
])
service = OccService(gitignore=_Gitignore(), layer_stack=stack)

snapshot = stack.read_active_manifest()
n = 6
barrier = threading.Barrier(n)

def commit_shared(index):
    barrier.wait(timeout=10)
    result = service.apply_changeset_sync([
        write_change(
            path="tracked/shared.txt",
            source="overlay_capture",
            final_content=("writer-%02d\n" % index).encode("utf-8"),
        )
    ], snapshot=snapshot)
    return _status(result.files[0].status)

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    conflict_statuses = list(pool.map(commit_shared, range(n)))
assert conflict_statuses.count("accepted") == 1, conflict_statuses
assert conflict_statuses.count("aborted_version") == n - 1, conflict_statuses

partial = service.apply_changeset_sync([
    write_change(path="tracked/shared.txt", source="overlay_capture", final_content=b"stale\n"),
    write_change(path="dist/partial.txt", source="overlay_capture", final_content=b"direct\n"),
], snapshot=snapshot)
assert [_status(item.status) for item in partial.files] == ["aborted_version", "dropped"]
assert stack.read_text("dist/partial.txt") == ("", False)

utf8 = service.apply_changeset_sync([
    write_change(path="unicodé/边界.txt", final_content="snowman-☃\n"),
])
assert utf8.files[0].status is FileStatus.ACCEPTED
assert stack.read_text("unicodé/边界.txt") == ("snowman-☃\n", True)

huge_prepare_start = time.perf_counter()
huge = service.prepare_changeset_sync(
    [write_change(path="huge/%05d.txt" % index, final_content=b"x") for index in range(10000)],
    options=CommitOptions(),
)
huge_prepare_ms = (time.perf_counter() - huge_prepare_start) * 1000.0
assert len(huge.path_groups) == 10000

_emit(label, started, before, {
    "conflict_statuses": conflict_statuses,
    "partial_statuses": [_status(item.status) for item in partial.files],
    "utf8_status": _status(utf8.files[0].status),
    "huge_groups": len(huge.path_groups),
    "huge_prepare_ms": huge_prepare_ms,
})
"""


async def test_occ_edge_cases_huge_conflict_gitignored_and_utf8(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="occ.edge_cases",
        timeout=180,
    )
    assert payload["conflict_statuses"].count("accepted") == 1
    assert payload["huge_groups"] == 10000
