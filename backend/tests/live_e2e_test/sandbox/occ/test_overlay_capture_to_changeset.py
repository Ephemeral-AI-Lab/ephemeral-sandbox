"""Phase 1b native probes for overlay-capture to OCC changeset conversion."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_OVERLAY_CAPTURE_BODY = r"""
from sandbox.occ.changeset.types import DeleteChange, OpaqueDirChange, SymlinkChange, WriteChange
from sandbox.occ.capture.overlay import overlay_path_changes_to_occ_changes
from sandbox.overlay import OverlayPathChange, content_hash
from sandbox.overlay import capture_changes
from sandbox.layer_stack.manifest import Manifest
from sandbox.layer_stack.layer.index import OPAQUE_MARKER, WHITEOUT_PREFIX

label = "occ.overlay_capture_to_changeset"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
upper = root / "upper"
upper.mkdir()
(upper / "src").mkdir()
(upper / "src" / "new.py").write_text("new\n", encoding="utf-8")
(upper / "src" / f"{WHITEOUT_PREFIX}old.py").write_text("", encoding="utf-8")
(upper / "pkg").mkdir()
(upper / "pkg" / OPAQUE_MARKER).write_text("", encoding="utf-8")
(upper / "pkg" / "keep.py").write_text("keep\n", encoding="utf-8")
os.symlink("../src/new.py", upper / "current")

captured = capture_changes(upper)
changes = overlay_path_changes_to_occ_changes(captured)
by_path = {change.path: change for change in changes}
assert isinstance(by_path["src/new.py"], WriteChange)
assert by_path["src/new.py"].source == "overlay_capture"
assert by_path["src/new.py"].final_content == b"new\n"
assert isinstance(by_path["src/old.py"], DeleteChange)
assert isinstance(by_path["current"], SymlinkChange)
assert by_path["current"].target == "../src/new.py"
assert isinstance(by_path["pkg"], OpaqueDirChange)
assert by_path["pkg"].kept_children == frozenset({"keep.py"})

rename_lower = root / "rename-lower"
rename_workspace = root / "rename-workspace"
rename_upper = root / "rename-upper"
(rename_lower / "old.txt").parent.mkdir(parents=True)
(rename_lower / "old.txt").write_text("same\n", encoding="utf-8")
(rename_workspace / "new.txt").parent.mkdir(parents=True)
(rename_workspace / "new.txt").write_text("same\n", encoding="utf-8")
rename_changes = capture_changes(
    rename_upper,
    lowerdir=rename_lower,
    workspace_root=rename_workspace,
)
rename_kinds = {item.path: item.kind for item in rename_changes}
assert rename_kinds == {"new.txt": "write", "old.txt": "delete"}

_emit(label, started, before, {
    "converted": {path: type(change).__name__ for path, change in by_path.items()},
    "opaque_kept_children": sorted(by_path["pkg"].kept_children),
    "rename_pair": rename_kinds,
    "captured_count": len(captured),
})
"""


async def test_overlay_capture_converts_whiteouts_renames_and_mixed_changes(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _OVERLAY_CAPTURE_BODY,
        label="occ.overlay_capture_to_changeset",
    )
    assert payload["opaque_kept_children"] == ["keep.py"]
    assert payload["rename_pair"] == {"new.txt": "write", "old.txt": "delete"}
