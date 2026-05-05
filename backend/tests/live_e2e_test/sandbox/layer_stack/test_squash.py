"""Phase 2 native probes for layer-stack squash behavior."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_SQUASH_BODY = r"""
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.stack_manager import LayerStackManager

label = "layer_stack.squash"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")

for index in range(6):
    manager.publish_changes([
        LayerChange(
            path="pkg/value-%02d.txt" % index,
            kind="write",
            source_path=str(_source(root, "value-%02d" % index, ("value-%02d\n" % index).encode("utf-8"))),
        )
    ])

pre_squash = manager.read_active_manifest()
squashed = manager.squash(max_depth=2)
assert squashed is not None
assert squashed.depth == 2, squashed
for index in range(6):
    assert manager.read_text("pkg/value-%02d.txt" % index) == ("value-%02d\n" % index, True)

idempotent = manager.squash(max_depth=2)
assert idempotent is None

stale_staging = manager.storage_root / "staging" / "B999999-killed.staging"
stale_staging.mkdir(parents=True)
(stale_staging / "partial.txt").write_text("partial", encoding="utf-8")
os.utime(stale_staging, (0, 0))
cleanup = manager.fsck_cleanup(young_staging_age_seconds=1, now=100)
assert "B999999-killed.staging" in cleanup.orphan_staging_removed

_emit(label, started, before, {
    "pre_squash_depth": pre_squash.depth,
    "post_squash_depth": squashed.depth,
    "coalesced_layers": pre_squash.depth - squashed.depth + 1,
    "idempotent_noop": idempotent is None,
    "stale_staging_removed": "B999999-killed.staging" in cleanup.orphan_staging_removed,
})
"""


_RACE_BODY = r"""
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.stack_manager import LayerStackManager

label = "layer_stack.squash_under_race"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")

for index in range(5):
    manager.publish_changes([
        LayerChange(
            path="base/%02d.txt" % index,
            kind="write",
            source_path=str(_source(root, "base-%02d" % index, ("base-%02d\n" % index).encode("utf-8"))),
        )
    ])

barrier = threading.Barrier(2)

def append_one():
    barrier.wait(timeout=5)
    manifest = manager.publish_changes([
        LayerChange(
            path="race/appended.txt",
            kind="write",
            source_path=str(_source(root, "race-appended", b"appended\n")),
        )
    ])
    return manifest.version

def squash_once():
    barrier.wait(timeout=5)
    manifest = manager.squash(max_depth=2)
    return None if manifest is None else manifest.version

with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
    append_future = pool.submit(append_one)
    squash_future = pool.submit(squash_once)
    append_version = append_future.result(timeout=10)
    squash_version = squash_future.result(timeout=10)

manifest = manager.read_active_manifest()
for index in range(5):
    assert manager.read_text("base/%02d.txt" % index) == ("base-%02d\n" % index, True)
assert manager.read_text("race/appended.txt") == ("appended\n", True)
assert manifest.depth <= 3, manifest

_emit(label, started, before, {
    "append_version": append_version,
    "squash_version": squash_version,
    "final_depth": manifest.depth,
    "lost_appends": 0,
})
"""


async def test_squash_coalesces_idempotently_and_recovers_stale_staging(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _SQUASH_BODY,
        label="layer_stack.squash",
    )
    assert payload["post_squash_depth"] == 2
    assert payload["idempotent_noop"] is True
    assert payload["stale_staging_removed"] is True


async def test_squash_under_race_keeps_manifest_and_append_intact(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="layer_stack.squash_under_race",
    )
    assert payload["lost_appends"] == 0
    assert payload["final_depth"] <= 3
