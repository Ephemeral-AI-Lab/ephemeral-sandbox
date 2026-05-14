"""Phase 4 native probes for OCC changeset model edge cases."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.prepared import CommitOptions, RouteDecision
from sandbox.occ.changeset.types import DeleteChange, EditChange, FileStatus, WriteChange
from sandbox.occ.service import OccService

class _Gitignore:
    def is_ignored(self, path):
        return path.startswith("dist/")

label = "occ.changeset_model"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
service = OccService(gitignore=_Gitignore(), layer_stack=stack)

empty = service.apply_changeset_sync([])
assert empty.files == ()

mixed = service.prepare_changeset_sync([
    WriteChange(path="src/new.txt", final_content="new\n"),
    WriteChange(path="dist/cache.txt", final_content="cache\n"),
    DeleteChange(path=".git/config"),
    WriteChange(path="../escape", final_content="bad"),
    WriteChange(path="unicodé/文件.txt", final_content="utf8\n"),
], options=CommitOptions(atomic=True))
routes = [(group.path, group.route.value) for group in mixed.path_groups]
assert routes == [
    ("src/new.txt", RouteDecision.GATED.value),
    ("dist/cache.txt", RouteDecision.DIRECT.value),
    (".git/config", RouteDecision.DROP.value),
    ("../escape", RouteDecision.REJECT.value),
    ("unicodé/文件.txt", RouteDecision.GATED.value),
]

max_changes = [WriteChange(path="bulk/%05d.txt" % index, final_content="x") for index in range(2000)]
prepared = service.prepare_changeset_sync(max_changes)
assert len(prepared.path_groups) == 2000

applied = service.apply_changeset_sync([
    WriteChange(path="src/new.txt", final_content="new\n"),
    WriteChange(path="dist/cache.txt", final_content="cache\n"),
])
assert [item.status for item in applied.files] == [FileStatus.ACCEPTED, FileStatus.ACCEPTED]

_emit(label, started, before, {
    "empty_files": len(empty.files),
    "routes": routes,
    "max_groups": len(prepared.path_groups),
    "applied_statuses": [_status(item.status) for item in applied.files],
    "prepare_timings": prepared.timings,
})
"""


async def test_changeset_model_empty_max_mixed_and_unicode(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="occ.changeset_model",
        timeout=120,
    )
    assert payload["empty_files"] == 0
    assert payload["max_groups"] == 2000
