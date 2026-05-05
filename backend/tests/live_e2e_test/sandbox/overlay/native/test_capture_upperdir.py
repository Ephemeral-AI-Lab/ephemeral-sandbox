"""Phase 1b native probes for raw upperdir capture."""

from __future__ import annotations

import pytest

from ..._harness.native_cases import run_native_case
from ..._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_CAPTURE_UPPERDIR_BODY = r"""
from sandbox.layer_stack.manifest import Manifest
from sandbox.overlay.capture.upperdir import capture_changes

label = "overlay.native.capture_upperdir"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
upper = root / "upper"
upper.mkdir()

(upper / "binary.bin").write_bytes(b"\x00\xff\x01")
sparse = upper / "sparse.bin"
with sparse.open("wb") as fh:
    fh.seek((1024 * 1024) - 1)
    fh.write(b"\0")
(upper / "target.txt").write_text("target\n", encoding="utf-8")
os.symlink("target.txt", upper / "link.txt")
os.link(upper / "target.txt", upper / "hardlink.txt")
long_dir = upper / ("nested-" + "a" * 40) / ("child-" + "b" * 40)
long_dir.mkdir(parents=True)
(long_dir / "file.txt").write_text("long\n", encoding="utf-8")
(upper / "unicode-\u2603.txt").write_text("snow\n", encoding="utf-8")

changes = capture_changes(upper, snapshot_manifest=Manifest(version=1, layers=()))
by_path = {change.path: change for change in changes}
assert by_path["binary.bin"].kind == "write"
assert by_path["binary.bin"].final_hash == _sha(b"\x00\xff\x01")
assert by_path["sparse.bin"].kind == "write"
assert by_path["sparse.bin"].final_hash == _sha(sparse.read_bytes())
assert by_path["link.txt"].kind == "symlink"
assert by_path["hardlink.txt"].kind == "write"
assert by_path["unicode-\u2603.txt"].kind == "write"
long_rel = (long_dir / "file.txt").relative_to(upper).as_posix()
assert by_path[long_rel].kind == "write"

_emit(label, started, before, {
    "captured_paths": sorted(by_path),
    "binary_hash": by_path["binary.bin"].final_hash,
    "sparse_size": sparse.stat().st_size,
    "symlink_target": os.readlink(upper / "link.txt"),
    "hardlink_nlink": (upper / "hardlink.txt").stat().st_nlink,
    "long_path_length": len(long_rel),
    "unicode_path_present": "unicode-\u2603.txt" in by_path,
})
"""


async def test_capture_upperdir_handles_binary_sparse_links_long_and_unicode(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _CAPTURE_UPPERDIR_BODY,
        label="overlay.native.capture_upperdir",
    )
    assert payload["sparse_size"] == 1024 * 1024
    assert payload["unicode_path_present"] is True
    assert payload["symlink_target"] == "target.txt"
