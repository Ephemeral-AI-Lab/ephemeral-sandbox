"""Phase 4 native probes for layer-stack edge cases."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
import os
from sandbox.layer_stack.layer_change import (
    LayerChange,
    DeleteLayerChange,
    SymlinkLayerChange,
    WriteLayerChange,
)
from sandbox.layer_stack.manifest import LayerRef, Manifest, write_manifest_atomic
from sandbox.layer_stack.manager import LayerStackManager

label = "layer_stack.edge_cases"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")

empty_manifest = manager.publish_changes([])
assert empty_manifest.depth == 0

manager.publish_changes([
    WriteLayerChange(path="dir/kept.txt", source_path=str(_source(root, "kept", b"kept"))),
    WriteLayerChange(path="dir/gone.txt", source_path=str(_source(root, "gone", b"gone"))),
])
delete_manifest = manager.publish_changes([
    DeleteLayerChange(path="dir/gone.txt"),
])
assert manager.read_text("dir/gone.txt", manifest=delete_manifest) == ("", False)
assert manager.read_text("dir/kept.txt", manifest=delete_manifest) == ("kept", True)

unicode_path = "unicodé/" + ("x" * 180) + "/文件.txt"
manager.publish_changes([
    WriteLayerChange(path=unicode_path, source_path=str(_source(root, "unicode", "hello unicode"))),
])
assert manager.read_text(unicode_path) == ("hello unicode", True)

loop_manifest = manager.publish_changes([
    SymlinkLayerChange(path="links/self", source_path="../links/self"),
])
assert manager.read_symlink("links/self", manifest=loop_manifest) == ("../links/self", "symlink")

sparse_layer = manager.storage_root / "layers" / "manual-sparse"
sparse_file = sparse_layer / "huge.bin"
sparse_file.parent.mkdir(parents=True, exist_ok=True)
with sparse_file.open("wb") as file:
    file.truncate(1024 ** 3 + 1)
sparse_manifest = Manifest(
    version=manager.read_active_manifest().version + 1,
    layers=(LayerRef(layer_id="manual-sparse", path="layers/manual-sparse"), *manager.read_active_manifest().layers),
)
write_manifest_atomic(manager.storage_root / "manifest.json", sparse_manifest)
assert manager.list_dir("") == ("dir", "huge.bin", "links", "unicodé")
assert os.stat(sparse_file).st_size > 1024 ** 3

_emit(label, started, before, {
    "empty_depth": empty_manifest.depth,
    "delete_depth": delete_manifest.depth,
    "unicode_path_length": len(unicode_path),
    "symlink_loop_target": manager.read_symlink("links/self")[0],
    "sparse_size": os.stat(sparse_file).st_size,
    "manifest_depth": manager.read_active_manifest().depth,
})
"""


async def test_layer_stack_edge_cases(native_sandbox: SandboxHandle) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="layer_stack.edge_cases",
    )
    assert payload["empty_depth"] == 0
    assert payload["sparse_size"] > 1024**3
