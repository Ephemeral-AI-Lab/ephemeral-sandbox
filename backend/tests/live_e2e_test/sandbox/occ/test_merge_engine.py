"""Phase 1b native probes for OCC merge engine behavior."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_MERGE_BODY = r"""
import sandbox.occ.merge as merge_facade
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.occ.changeset.prepared import PreparedPathGroup, RouteDecision
from sandbox.occ.changeset.types import EditChange, FileStatus, WriteChange
from sandbox.occ.content.hashing import ContentHasher

def _publish(stack, rel, content):
    stack.publish_changes([
        LayerChange(
            path=rel,
            kind="write",
            content_hash=ContentHasher().hash_bytes(content),
            source_path=str(_source(root, rel.replace("/", "-"), content)),
        )
    ])

def _stage_write(path, content):
    return LayerChange(
        path=path,
        kind="write",
        content_hash=ContentHasher().hash_bytes(content),
        source_path=str(_source(root, "staged-" + path.replace("/", "-"), content)),
    )

label = "occ.merge_engine"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
_publish(stack, "src/app.py", b"alpha\nbeta\n")
_publish(stack, "src/crlf.txt", b"a\r\nb\r\n")
_publish(stack, "src/bin.dat", b"\x00\x01\x02")

gated = merge_facade.GatedMerge(stack)
direct = merge_facade.DirectMerge(stack)

ok_group = PreparedPathGroup(
    path="src/app.py",
    route=RouteDecision.OCC_GATED_MERGE,
    changes=(EditChange(path="src/app.py", old_text="beta", new_text="BETA"),),
)
ok_result, ok_delta = gated.stage_group(
    ok_group,
    active_manifest=stack.read_active_manifest(),
    stage_write=_stage_write,
)
assert ok_result.status is FileStatus.ACCEPTED
assert ok_delta is not None
assert Path(ok_delta.changes[0].source_path).read_bytes() == b"alpha\nBETA\n"

conflict_group = PreparedPathGroup(
    path="src/app.py",
    route=RouteDecision.OCC_GATED_MERGE,
    changes=(EditChange(path="src/app.py", old_text="missing", new_text="X"),),
)
conflict_result, conflict_delta = gated.stage_group(
    conflict_group,
    active_manifest=stack.read_active_manifest(),
    stage_write=_stage_write,
)
assert conflict_result.status is FileStatus.ABORTED_OVERLAP
assert conflict_delta is None

binary_group = PreparedPathGroup(
    path="src/bin.dat",
    route=RouteDecision.OCC_GATED_MERGE,
    changes=(EditChange(path="src/bin.dat", old_text="x", new_text="y"),),
)
binary_result, binary_delta = gated.stage_group(
    binary_group,
    active_manifest=stack.read_active_manifest(),
    stage_write=_stage_write,
)
assert binary_result.status is FileStatus.ABORTED_OVERLAP
assert binary_delta is None

crlf_group = PreparedPathGroup(
    path="src/crlf.txt",
    route=RouteDecision.OCC_GATED_MERGE,
    changes=(EditChange(path="src/crlf.txt", old_text="b\r\n", new_text="B\r\n"),),
)
crlf_result, crlf_delta = gated.stage_group(
    crlf_group,
    active_manifest=stack.read_active_manifest(),
    stage_write=_stage_write,
)
assert crlf_result.status is FileStatus.ACCEPTED
assert Path(crlf_delta.changes[0].source_path).read_bytes() == b"a\r\nB\r\n"

direct_group = PreparedPathGroup(
    path="dist/app.js",
    route=RouteDecision.OCC_SKIPPED_MERGE,
    changes=(WriteChange(path="dist/app.js", final_content=b"direct"),),
)
direct_result, direct_delta = direct.stage_group(
    direct_group,
    active_manifest=stack.read_active_manifest(),
    stage_write=_stage_write,
)
assert direct_result.status is FileStatus.ACCEPTED
assert Path(direct_delta.changes[0].source_path).read_bytes() == b"direct"

_emit(label, started, before, {
    "non_conflict": _status(ok_result.status),
    "conflict": _status(conflict_result.status),
    "binary": _status(binary_result.status),
    "crlf": _status(crlf_result.status),
    "direct": _status(direct_result.status),
})
"""


async def test_merge_engine_handles_conflict_binary_and_line_endings(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _MERGE_BODY,
        label="occ.merge_engine",
    )
    assert payload["non_conflict"] == "accepted"
    assert payload["conflict"] == "aborted_overlap"
    assert payload["binary"] == "aborted_overlap"
    assert payload["crlf"] == "accepted"
