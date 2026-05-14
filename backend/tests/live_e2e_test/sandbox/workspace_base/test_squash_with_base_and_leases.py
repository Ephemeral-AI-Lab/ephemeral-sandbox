"""Phase-01 squash coverage for base-plus-layer stacks and leases."""

from __future__ import annotations

import pytest

from .._harness.sandbox_fixture import SandboxHandle
from .._harness.workspace_base_metrics import write_jsonl_artifact
from .._harness.workspace_base_probe import run_workspace_base_probe


pytestmark = pytest.mark.asyncio


_SQUASH_BODY = r"""
label = "workspace_base.squash_with_base_and_leases"
case = "squash_with_base_and_leases"
started = time.perf_counter()
depths = list(__CFG__["depths"])
seed = _seed_workspace_files(
    "phase01-squash-fixtures",
    files=max(depths) + 5,
    deletes=max(depths) + 5,
    overwrites=max(depths) + 5,
)
(seed / "opaque" / "old.txt").parent.mkdir(parents=True, exist_ok=True)
(seed / "opaque" / "old.txt").write_text("old\n", encoding="utf-8")
workspace_inv = _inventory(WORKSPACE_ROOT)
rows = []
summary_binding = None
summary_timings = {}


def _full_digest(root):
    root = Path(root)
    digest = hashlib.sha256()
    for current_root, dirnames, filenames in os.walk(root, topdown=True, followlinks=False):
        current = Path(current_root)
        dirnames.sort()
        filenames.sort()
        for dirname in dirnames:
            path = current / dirname
            rel = path.relative_to(root).as_posix()
            if path.is_symlink():
                digest.update(("symlink-dir\0%s\0%s\0" % (rel, os.readlink(path))).encode("utf-8"))
            else:
                digest.update(("dir\0%s\0" % rel).encode("utf-8"))
        for filename in filenames:
            path = current / filename
            rel = path.relative_to(root).as_posix()
            if path.is_symlink():
                digest.update(("symlink\0%s\0%s\0" % (rel, os.readlink(path))).encode("utf-8"))
            elif path.is_file():
                digest.update(("file\0%s\0%s\0" % (rel, _file_sha(path))).encode("utf-8"))
    return digest.hexdigest()


def _source_file(root, name, content):
    return _source(root, name, content)


def _shape_changes(root, index):
    mode = index % 5
    if mode == 0:
        return [
            WriteLayerChange(
                path="phase01-squash/append/%03d.txt" % index,
                source_path=str(_source_file(root, "append-%03d" % index, "append-%03d\n" % index)),
            )
        ]
    if mode == 1:
        return [
            WriteLayerChange(
                path="phase01-squash-fixtures/overwrites/%03d.txt" % index,
                source_path=str(_source_file(root, "overwrite-%03d" % index, "top-%03d\n" % index)),
            )
        ]
    if mode == 2:
        return [
            DeleteLayerChange(
                path="phase01-squash-fixtures/deletes/%03d.txt" % index,
            )
        ]
    if mode == 3:
        return [
            SymlinkLayerChange(
                path="phase01-squash/symlinks/link-%03d" % index,
                source_path="target-%03d.txt" % index,
            )
        ]
    return [
        OpaqueDirLayerChange(path="phase01-squash-fixtures/opaque"),
        WriteLayerChange(
            path="phase01-squash-fixtures/opaque/%03d.txt" % index,
            source_path=str(_source_file(root, "opaque-%03d" % index, "opaque-%03d\n" % index)),
        ),
    ]


def _publish_depth(manager, root, depth):
    for index in range(depth):
        _publish_changes(manager, _shape_changes(root, index))


def _squash_no_lease(depth):
    stack_root = _phase01_root(label, "no-lease-%03d" % depth)
    binding, timings = _build_base(stack_root)
    manager = LayerStackManager(stack_root)
    _publish_depth(manager, stack_root, depth)
    before_manifest = manager.read_active_manifest()
    before_dest = stack_root / "before-squash"
    manager.materialize(before_dest)
    before_digest = _full_digest(before_dest)
    t0 = time.perf_counter()
    squash_start = time.perf_counter()
    squashed = manager.squash(max_depth=4)
    squash_elapsed = time.perf_counter() - squash_start
    assert squashed is not None, before_manifest
    after_manifest = manager.read_active_manifest()
    after_dest = stack_root / "after-squash"
    manager.materialize(after_dest)
    after_digest = _full_digest(after_dest)
    assert before_digest == after_digest
    assert after_manifest.depth < before_manifest.depth
    assert list((stack_root / "staging").iterdir()) == []
    rows.append(_call_row(
        case,
        "no_lease_depth_%03d" % depth,
        True,
        t0,
        timings={"layer_stack.squash.total_s": squash_elapsed, **timings},
        extra={
            "depth": depth,
            "pre_squash_depth": before_manifest.depth,
            "post_squash_depth": after_manifest.depth,
            "view_digest": after_digest,
            "staging_dirs_after_squash": 0,
        },
    ))
    return binding, timings


for depth in depths:
    binding, timings = _squash_no_lease(depth)
    if summary_binding is None:
        summary_binding = binding
        summary_timings = dict(timings)


lease_stack = _phase01_root(label, "lease")
lease_binding, lease_timings = _build_base(lease_stack)
manager = LayerStackManager(lease_stack)
manifest_a, _ = _publish_changes(manager, [
    WriteLayerChange(
        path="phase01-squash-lease/value.txt",
        source_path=str(_source_file(lease_stack, "lease-a", b"A\n")),
    )
])
lease = manager.acquire_snapshot_lease("phase01-lease-reader")
assert lease.manifest == manifest_a
leased_layers = lease.manifest.layers
_publish_changes(manager, [
    WriteLayerChange(
        path="phase01-squash-lease/value.txt",
        source_path=str(_source_file(lease_stack, "lease-b", b"B\n")),
    )
])
for index in range(30):
    _publish_changes(manager, [
        WriteLayerChange(
            path="phase01-squash-lease/value.txt",
            source_path=str(_source_file(lease_stack, "lease-%03d" % index, ("N%03d\n" % index).encode("utf-8"))),
        )
    ])

assert manager.read_text("phase01-squash-lease/value.txt", manifest=lease.manifest) == ("A\n", True)
assert manager.read_text("phase01-squash-lease/value.txt") == ("N029\n", True)
pre_release_t0 = time.perf_counter()
squash_start = time.perf_counter()
squashed = manager.squash(max_depth=4)
squash_elapsed = time.perf_counter() - squash_start
assert squashed is not None
assert manager.read_text("phase01-squash-lease/value.txt", manifest=lease.manifest) == ("A\n", True)
assert manager.read_text("phase01-squash-lease/value.txt") == ("N029\n", True)
assert all((manager.storage_root / layer.path).is_dir() for layer in leased_layers)
assert manager.read_text("phase01-squash-lease/value.txt", manifest=lease.manifest) == ("A\n", True)
released = manager.release_lease(lease.lease_id)
assert released is True
assert manager.read_text("phase01-squash-lease/value.txt") == ("N029\n", True)
assert all(not (manager.storage_root / layer.path).exists() for layer in leased_layers)
rows.append(_call_row(
    case,
    "lease_preserved_until_release",
    True,
    pre_release_t0,
    timings={"layer_stack.squash.total_s": squash_elapsed, **lease_timings},
    extra={
        "lease_id": lease.lease_id,
        "lease_manifest_depth": lease.manifest.depth,
        "active_depth_after_squash": manager.read_active_manifest().depth,
        "leased_layers_removed_after_release": True,
    },
))

squash_times = [
    float(row["timings"].get("layer_stack.squash.total_s", 0.0))
    for row in rows
]
summary_timings.update({
    "layer_stack.squash.total_s": max(squash_times or [0.0]),
    "phase01.squash.p50_s": _percentile(squash_times, 50),
    "phase01.squash.p99_s": _percentile(squash_times, 99),
})
summary = _base_summary(
    case,
    summary_binding or lease_binding,
    workspace_inv,
    summary_timings,
    pass_bars={
        "depths": depths,
        "no_lease_view_preserved": True,
        "depth_decreases": True,
        "lease_preserved_until_release": True,
        "leased_layers_removed_after_release": True,
    },
)
_emit_workspace_payload(label, started, summary, rows)
"""


async def test_squash_preserves_base_views_and_active_leases(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    payload = await run_workspace_base_probe(
        workspace_base_sandbox,
        _SQUASH_BODY.replace("__CFG__", "json.loads(__CFG_JSON__)"),
        label="workspace_base.squash_with_base_and_leases",
        cfg={"depths": [5, 20, 100, 200]},
        timeout=600,
    )
    rows = payload["rows"]
    assert len(rows) == 5
    assert all(row["success"] for row in rows)
    artifact = write_jsonl_artifact(
        case="squash_with_base_and_leases",
        summary=payload["summary"],
        rows=rows,
    )
    print(f"\n[phase01:squash_with_base_and_leases] artifact={artifact}")
