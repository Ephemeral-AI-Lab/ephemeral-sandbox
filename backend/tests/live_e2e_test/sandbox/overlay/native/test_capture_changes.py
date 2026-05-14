"""Phase 1b native probes for copy-backed overlay change capture."""

from __future__ import annotations

import pytest

from ..._harness.native_cases import run_native_case
from ..._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_CAPTURE_CHANGES_BODY = r"""
from sandbox.layer_stack.layer_index import OPAQUE_MARKER, WHITEOUT_PREFIX
from sandbox.overlay import capture_changes

label = "overlay.native.capture_changes"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
upper = root / "upper"
upper.mkdir()
(upper / "dir").mkdir()
(upper / "dir" / OPAQUE_MARKER).write_text("", encoding="utf-8")
(upper / "dir" / "keep.txt").write_text("keep\n", encoding="utf-8")
(upper / f"{WHITEOUT_PREFIX}gone.txt").write_text("", encoding="utf-8")
(upper / "a.txt").write_text("a\n", encoding="utf-8")
(upper / "b.txt").write_text("b\n", encoding="utf-8")

changes = capture_changes(upper)
paths = [change.path for change in changes]
kinds = {change.path: change.kind for change in changes}
expected_order = ["gone.txt", "a.txt", "b.txt", "dir", "dir/keep.txt"]
assert paths == expected_order, paths
assert kinds["dir"] == "opaque_dir"
assert kinds["dir/keep.txt"] == "write"
assert kinds["gone.txt"] == "delete"

lower = root / "lower"
merged = root / "merged"
rename_upper = root / "rename-upper"
lower.mkdir()
merged.mkdir()
(lower / "old.txt").write_text("same\n", encoding="utf-8")
(merged / "new.txt").write_text("same\n", encoding="utf-8")
rename_changes = capture_changes(
    rename_upper,
    lowerdir=lower,
    workspace_root=merged,
)
rename = {change.path: change.kind for change in rename_changes}
assert rename == {"new.txt": "write", "old.txt": "delete"}

_emit(label, started, before, {
    "paths": paths,
    "kinds": kinds,
    "rename": rename,
    "ordered": paths == expected_order,
})
"""


_RACE_BODY = r"""
from sandbox.overlay import capture_changes

label = "overlay.native.capture_changes_under_race"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
upper = root / "upper"
upper.mkdir()
n = 4
barrier = threading.Barrier(n)

def producer(index):
    barrier.wait(timeout=5)
    path = upper / "same.txt"
    path.write_text("writer-%02d\n" % index, encoding="utf-8")
    return index

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    list(pool.map(producer, range(n)))
changes = capture_changes(upper)
same_changes = [change for change in changes if change.path == "same.txt"]
assert len(same_changes) == 1, changes
final_content = Path(same_changes[0].content_path).read_text(encoding="utf-8")
assert final_content.startswith("writer-")

_emit(label, started, before, {
    "producers": n,
    "captured_same_path": len(same_changes),
    "final_content": final_content,
    "ordering": [change.path for change in changes],
})
"""


async def test_capture_changes_handles_whiteouts_opaque_rename_and_ordering(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _CAPTURE_CHANGES_BODY,
        label="overlay.native.capture_changes",
    )
    assert payload["ordered"] is True
    assert payload["rename"] == {"new.txt": "write", "old.txt": "delete"}


async def test_capture_changes_under_race_deduplicates_same_path(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="overlay.native.capture_changes_under_race",
    )
    assert payload["producers"] == 4
    assert payload["captured_same_path"] == 1
