"""Phase 1a native probes for layer-stack merged views."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_MERGED_VIEW_BODY = r"""
from sandbox.layer_stack.layer_change import (
    LayerChange,
    DeleteLayerChange,
    OpaqueDirLayerChange,
    WriteLayerChange,
)
from sandbox.layer_stack.manager import LayerStackManager

label = "layer_stack.merged_view"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")

for index in range(100):
    manager.publish_changes([
        WriteLayerChange(
            path="depth/%03d.txt" % index,
            source_path=str(_source(root, "depth-%03d" % index, ("value-%03d\n" % index).encode("utf-8"))),
        )
    ])

manager.publish_changes([
    DeleteLayerChange(path="depth/010.txt"),
    OpaqueDirLayerChange(path="opaque"),
    WriteLayerChange(path="opaque/kept.txt", source_path=str(_source(root, "kept", b"kept\n"))),
])
manager.publish_changes([
    WriteLayerChange(path="opaque/old.txt", source_path=str(_source(root, "old", b"old\n"))),
])
manager.publish_changes([
    OpaqueDirLayerChange(path="opaque"),
    WriteLayerChange(path="opaque/new.txt", source_path=str(_source(root, "new", b"new\n"))),
])

manifest = manager.read_active_manifest()
assert manifest.depth == 103
assert manager.read_text("depth/000.txt") == ("value-000\n", True)
assert manager.read_text("depth/099.txt") == ("value-099\n", True)
assert manager.read_bytes("depth/010.txt") == (None, False)
assert manager.read_bytes("opaque/old.txt") == (None, False)
assert manager.list_dir("opaque") == ("new.txt",)

materialized = root / "materialized"
manager.materialize(materialized)
assert (materialized / "depth" / "099.txt").read_text(encoding="utf-8") == "value-099\n"
assert not (materialized / "depth" / "010.txt").exists()
assert sorted(p.name for p in (materialized / "opaque").iterdir()) == ["new.txt"]

_emit(label, started, before, {
    "manifest_depth": manifest.depth,
    "checked_depth": 100,
    "whiteout_hidden": True,
    "opaque_dir_listing": manager.list_dir("opaque"),
})
"""


async def test_merged_view_resolves_depth_100_whiteouts_and_opaque_dirs(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _MERGED_VIEW_BODY,
        label="layer_stack.merged_view",
        timeout=120,
    )
    assert payload["checked_depth"] == 100
    assert payload["whiteout_hidden"] is True
    assert payload["opaque_dir_listing"] == ["new.txt"]
