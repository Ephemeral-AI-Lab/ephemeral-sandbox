"""Phase 4 native probes for OCC edit/patch behavior."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
from sandbox.layer_stack.layer.change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.types import EditChange, FileStatus
from sandbox.occ.service import Service

class _Gitignore:
    def is_ignored(self, path):
        return False

label = "occ.patching"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
stack.publish_changes([
    WriteLayerChange(path="src/app.py", source_path=str(_source(root, "app", b"alpha\nbeta\n"))),
    WriteLayerChange(path="src/no_newline.py", source_path=str(_source(root, "nonewline", b"tail"))),
    WriteLayerChange(path="src/spaces.py", source_path=str(_source(root, "spaces", b"value = 1\n"))),
])
service = Service(gitignore=_Gitignore(), snapshot_reader=stack, staging=stack, publisher=stack)

success = service.apply_changeset_sync([
    EditChange(path="src/app.py", old_text="beta\n", new_text="gamma\n"),
])
assert success.files[0].status is FileStatus.ACCEPTED
assert stack.read_text("src/app.py") == ("alpha\ngamma\n", True)

reject = service.apply_changeset_sync([
    EditChange(path="src/app.py", old_text="missing", new_text="never"),
])
assert reject.files[0].status is FileStatus.ABORTED_OVERLAP

whitespace = service.apply_changeset_sync([
    EditChange(path="src/spaces.py", old_text="value = 1\n", new_text="value    =    1\n"),
])
assert whitespace.files[0].status is FileStatus.ACCEPTED
assert stack.read_text("src/spaces.py") == ("value    =    1\n", True)

eof = service.apply_changeset_sync([
    EditChange(path="src/no_newline.py", old_text="tail", new_text="tail-end"),
])
assert eof.files[0].status is FileStatus.ACCEPTED
assert stack.read_text("src/no_newline.py") == ("tail-end", True)

_emit(label, started, before, {
    "statuses": [_status(result.files[0].status) for result in (success, reject, whitespace, eof)],
    "success_timings": success.timings,
    "reject_message": reject.files[0].message,
})
"""


async def test_occ_patching_success_reject_whitespace_and_eof(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="occ.patching",
    )
    assert payload["statuses"] == [
        "accepted",
        "aborted_overlap",
        "accepted",
        "accepted",
    ]
