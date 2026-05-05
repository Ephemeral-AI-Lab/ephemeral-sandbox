"""Phase 2 native probes for layer-change aggregation."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_AGGREGATION_BODY = r"""
from sandbox.layer_stack.changes import LayerChange, aggregate_layer_changes
from sandbox.layer_stack.stack_manager import LayerStackManager

label = "layer_stack.changes_aggregation"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
manager.publish_changes([
    LayerChange(path="old/name.txt", kind="write", source_path=str(_source(root, "old-name", b"old\n"))),
])

changes = [
    LayerChange(path="z/out.txt", kind="write", source_path=str(_source(root, "z-out-v1", b"z1\n"))),
    LayerChange(path="a/out.txt", kind="write", source_path=str(_source(root, "a-out-v1", b"a1\n"))),
    LayerChange(path="old/name.txt", kind="delete"),
    LayerChange(path="new/name.txt", kind="write", source_path=str(_source(root, "new-name", b"renamed\n"))),
    LayerChange(path="a/out.txt", kind="write", source_path=str(_source(root, "a-out-v2", b"a2\n"))),
    LayerChange(path="z/out.txt", kind="write", source_path=str(_source(root, "z-out-v2", b"z2\n"))),
]
delta = aggregate_layer_changes(changes)
paths = [change.path for change in delta.changes]
assert paths == sorted(set(change.path for change in changes)), paths
manager.publish_changes(delta.changes)

assert manager.read_text("a/out.txt") == ("a2\n", True)
assert manager.read_text("z/out.txt") == ("z2\n", True)
assert manager.read_bytes("old/name.txt") == (None, False)
assert manager.read_text("new/name.txt") == ("renamed\n", True)

_emit(label, started, before, {
    "input_changes": len(changes),
    "aggregated_changes": len(delta.changes),
    "deduped_paths": len(changes) - len(delta.changes),
    "ordered_paths": paths,
    "rename_pair_preserved": manager.read_text("new/name.txt") == ("renamed\n", True),
    "old_name_deleted": manager.read_bytes("old/name.txt") == (None, False),
})
"""


_RACE_BODY = r"""
from sandbox.layer_stack.changes import LayerChange, aggregate_layer_changes
from sandbox.layer_stack.stack_manager import LayerStackManager

label = "layer_stack.changes_aggregation_under_race"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
n = 8
barrier = threading.Barrier(n)

def produce(index):
    barrier.wait(timeout=5)
    first = _source(root, "race-%02d-v1" % index, ("draft-%02d\n" % index).encode("utf-8"))
    final = _source(root, "race-%02d-v2" % index, ("final-%02d\n" % index).encode("utf-8"))
    return (
        LayerChange(path="race/%02d.txt" % index, kind="write", source_path=str(first)),
        LayerChange(path="race/%02d.txt" % index, kind="write", source_path=str(final)),
    )

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    rows = list(pool.map(produce, range(n)))
changes = [change for row in rows for change in row]
delta = aggregate_layer_changes(changes)
manager.publish_changes(delta.changes)

paths = [change.path for change in delta.changes]
assert paths == ["race/%02d.txt" % index for index in range(n)], paths
for index in range(n):
    assert manager.read_text("race/%02d.txt" % index) == ("final-%02d\n" % index, True)

_emit(label, started, before, {
    "producers": n,
    "input_changes": len(changes),
    "aggregated_changes": len(delta.changes),
    "dedup_invariant": len(delta.changes) == n,
    "ordered_paths": paths,
})
"""


async def test_changes_aggregation_dedups_orders_and_preserves_rename_pairs(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _AGGREGATION_BODY,
        label="layer_stack.changes_aggregation",
    )
    assert payload["aggregated_changes"] == 4
    assert payload["rename_pair_preserved"] is True
    assert payload["old_name_deleted"] is True


async def test_changes_aggregation_under_race_is_deterministic_per_path(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="layer_stack.changes_aggregation_under_race",
    )
    assert payload["producers"] == 8
    assert payload["dedup_invariant"] is True
