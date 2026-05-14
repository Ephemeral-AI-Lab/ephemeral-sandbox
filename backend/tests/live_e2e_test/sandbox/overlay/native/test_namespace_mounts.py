"""Phase 4 native probes for snapshot mount cleanup semantics."""

from __future__ import annotations

import pytest

from ..._harness.native_cases import run_native_case
from ..._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
from sandbox.layer_stack.layer_change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.overlay import mount_snapshot

label = "overlay.native.namespace_mounts"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
manager.publish_changes([
    WriteLayerChange(path="pkg/base.txt", source_path=str(_source(root, "base", b"base\n"))),
])
manifest = manager.read_active_manifest()
run_dir = root / "run"

first = mount_snapshot(
    manifest=manifest,
    storage_root=manager.storage_root,
    run_dir=run_dir,
)
(Path(first.upperdir) / "orphan.tmp").write_text("orphan", encoding="utf-8")
(Path(first.workdir) / "dirty.tmp").write_text("dirty", encoding="utf-8")
(Path(first.workspace_root) / "pkg" / "base.txt").write_text("mutated", encoding="utf-8")

second_timings = {}
second = mount_snapshot(
    manifest=manifest,
    storage_root=manager.storage_root,
    run_dir=run_dir,
    timings=second_timings,
)
assert not (Path(second.upperdir) / "orphan.tmp").exists()
assert not (Path(second.workdir) / "dirty.tmp").exists()
assert (Path(second.workspace_root) / "pkg" / "base.txt").read_text(encoding="utf-8") == "base\n"

shutil.rmtree(second.upperdir)
shutil.rmtree(second.workdir)
third = mount_snapshot(
    manifest=manifest,
    storage_root=manager.storage_root,
    run_dir=run_dir,
)
assert Path(third.upperdir).is_dir()
assert Path(third.workdir).is_dir()

_emit(label, started, before, {
    "orphan_upperdir_removed": True,
    "dirty_workdir_removed": True,
    "double_mount_reset": True,
    "timings": second_timings,
})
"""


async def test_namespace_mounts_clean_orphans_and_dirty_workdirs(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="overlay.native.namespace_mounts",
    )
    assert payload["orphan_upperdir_removed"] is True
    assert payload["dirty_workdir_removed"] is True
    assert payload["double_mount_reset"] is True
