"""Phase 4 native probes for overlay runtime edge cases."""

from __future__ import annotations

import pytest

from ..._harness.native_cases import run_native_case
from ..._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
import errno
from unittest import mock
from sandbox.layer_stack.layer.change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.overlay import mount_snapshot
import sandbox.overlay.mounts as mount_mod

label = "overlay.native.overlay_edge_cases"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")

empty = mount_snapshot(
    manifest=manager.read_active_manifest(),
    storage_root=manager.storage_root,
    run_dir=root / "empty",
)
assert Path(empty.workspace_root).is_dir()

manager.publish_changes([
    WriteLayerChange(path="depth/value-000.txt", source_path=str(_source(root, "value-000", b"0"))),
])
depth_one = mount_snapshot(
    manifest=manager.read_active_manifest(),
    storage_root=manager.storage_root,
    run_dir=root / "depth-one",
)
assert (Path(depth_one.workspace_root) / "depth" / "value-000.txt").read_text(encoding="utf-8") == "0"

for index in range(1, 26):
    manager.publish_changes([
        WriteLayerChange(
            path="depth/value-%03d.txt" % index,
            source_path=str(_source(root, "value-%03d" % index, ("%d" % index).encode("utf-8"))),
        )
    ])
deep = mount_snapshot(
    manifest=manager.read_active_manifest(),
    storage_root=manager.storage_root,
    run_dir=root / "deep",
)
assert (Path(deep.workspace_root) / "depth" / "value-025.txt").read_text(encoding="utf-8") == "25"

missing_layer_detected = False
missing = manager.read_active_manifest().layers[0]
shutil.rmtree(manager.storage_root / missing.path)
try:
    mount_snapshot(
        manifest=manager.read_active_manifest(),
        storage_root=manager.storage_root,
        run_dir=root / "missing",
    )
except Exception:
    missing_layer_detected = True
assert missing_layer_detected

injected = {}
for name, err_no in (("enospc", errno.ENOSPC), ("ebusy", errno.EBUSY), ("enomem", errno.ENOMEM)):
    try:
        with mock.patch.object(mount_mod.MergedView, "materialize", side_effect=OSError(err_no, name)):
            mount_snapshot(
                manifest=manager.read_active_manifest(),
                storage_root=manager.storage_root,
                run_dir=root / name,
            )
    except OSError as exc:
        injected[name] = exc.errno
assert injected == {"enospc": errno.ENOSPC, "ebusy": errno.EBUSY, "enomem": errno.ENOMEM}

_emit(label, started, before, {
    "depths": [0, 1, 26],
    "missing_lowerdir_detected": missing_layer_detected,
    "injected_errors": injected,
})
"""


async def test_overlay_edge_cases_cover_depths_missing_layers_and_injected_errors(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="overlay.native.overlay_edge_cases",
    )
    assert payload["missing_lowerdir_detected"] is True
    assert payload["injected_errors"]["enospc"] == 28
