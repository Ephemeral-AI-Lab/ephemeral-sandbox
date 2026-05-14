"""Phase-01 live layer creation speed over an imported `/testbed` base."""

from __future__ import annotations

import pytest

from .._harness.sandbox_fixture import SandboxHandle
from .._harness.workspace_base_metrics import write_jsonl_artifact
from .._harness.workspace_base_probe import run_workspace_base_probe


pytestmark = pytest.mark.asyncio


_LAYER_CREATE_BODY = r"""
label = "workspace_base.layer_create_speed"
case = "layer_create_speed"
started = time.perf_counter()
seed = _seed_workspace_files(
    "phase01-layer-create-fixtures",
    files=10,
    deletes=60,
    overwrites=120,
)
(seed / "opaque" / "old.txt").parent.mkdir(parents=True, exist_ok=True)
(seed / "opaque" / "old.txt").write_text("old\n", encoding="utf-8")

stack_root = _phase01_root(label)
binding, base_timings = _build_base(stack_root)
workspace_inv = _inventory(WORKSPACE_ROOT)
manager = LayerStackManager(stack_root)
rows = []


def _source_file(name, content):
    return _source(stack_root, name, content)


def _publish_workload(workload, changes, check):
    before_depth = manager.read_active_manifest().depth
    before_bytes = _storage_bytes(stack_root)
    t0 = time.perf_counter()
    manifest, timings = _publish_changes(manager, changes)
    wall_ms = (time.perf_counter() - t0) * 1000.0
    after_bytes = _storage_bytes(stack_root)
    check(manifest)
    rows.append(_call_row(
        case,
        workload,
        True,
        t0,
        timings,
        extra={
            "manifest_depth_before": before_depth,
            "manifest_depth_after": manifest.depth,
            "storage_bytes_delta": after_bytes - before_bytes,
            "layer_create_wall_ms": wall_ms,
            "change_count": len(changes),
        },
    ))


def _check_one_small_file(manifest):
    del manifest
    assert manager.read_text("phase01-layer-create/small.txt") == ("small\n", True)


_publish_workload(
    "one_small_file",
    [
        WriteLayerChange(
            path="phase01-layer-create/small.txt",
            source_path=str(_source_file("small", b"small\n")),
        )
    ],
    _check_one_small_file,
)

small_changes = [
    WriteLayerChange(
        path="phase01-layer-create/small-batch/%03d.txt" % index,
        source_path=str(_source_file("small-%03d" % index, "small-%03d\n" % index)),
    )
    for index in range(100)
]


def _check_small_batch(manifest):
    del manifest
    assert manager.read_text("phase01-layer-create/small-batch/099.txt") == ("small-099\n", True)


_publish_workload(
    "one_hundred_small_files",
    small_changes,
    _check_small_batch,
)

large_size = int(__CFG__["large_file_bytes"])


def _check_large_file(manifest):
    del manifest
    assert manager.read_bytes("phase01-layer-create/large.bin")[1] is True


_publish_workload(
    "one_large_file",
    [
        WriteLayerChange(
            path="phase01-layer-create/large.bin",
            source_path=str(_source_file("large", b"x" * large_size)),
        )
    ],
    _check_large_file,
)

overwrite_changes = [
    WriteLayerChange(
        path="phase01-layer-create-fixtures/overwrites/%03d.txt" % index,
        source_path=str(_source_file("overwrite-%03d" % index, "top-%03d\n" % index)),
    )
    for index in range(100)
]


def _check_overwrites(manifest):
    del manifest
    assert manager.read_text("phase01-layer-create-fixtures/overwrites/099.txt") == ("top-099\n", True)


_publish_workload(
    "one_hundred_overwrites",
    overwrite_changes,
    _check_overwrites,
)

delete_changes = [
    DeleteLayerChange(
        path="phase01-layer-create-fixtures/deletes/%03d.txt" % index,
    )
    for index in range(50)
]


def _check_deletes(manifest):
    del manifest
    assert manager.read_bytes("phase01-layer-create-fixtures/deletes/049.txt") == (None, False)


_publish_workload(
    "fifty_deletes",
    delete_changes,
    _check_deletes,
)

mixed_changes = [
    WriteLayerChange(
        path="phase01-layer-create/mixed/new.txt",
        source_path=str(_source_file("mixed-new", b"new\n")),
    ),
    WriteLayerChange(
        path="phase01-layer-create-fixtures/overwrites/000.txt",
        source_path=str(_source_file("mixed-overwrite", b"mixed-overwrite\n")),
    ),
    DeleteLayerChange(
        path="phase01-layer-create-fixtures/deletes/050.txt",
    ),
    SymlinkLayerChange(
        path="phase01-layer-create/mixed/link.txt",
        source_path="new.txt",
    ),
    OpaqueDirLayerChange(
        path="phase01-layer-create-fixtures/opaque",
    ),
    WriteLayerChange(
        path="phase01-layer-create-fixtures/opaque/new.txt",
        source_path=str(_source_file("opaque-new", b"opaque-new\n")),
    ),
]


def _check_mixed(manifest):
    del manifest
    assert manager.read_text("phase01-layer-create/mixed/new.txt") == ("new\n", True)


_publish_workload(
    "mixed_write_overwrite_delete_symlink_opaque",
    mixed_changes,
    _check_mixed,
)

assert manager.read_text("phase01-layer-create-fixtures/overwrites/000.txt") == ("mixed-overwrite\n", True)
assert manager.read_bytes("phase01-layer-create-fixtures/deletes/050.txt") == (None, False)
assert manager.read_symlink("phase01-layer-create/mixed/link.txt") == ("new.txt", True)
assert manager.list_dir("phase01-layer-create-fixtures/opaque") == ("new.txt",)
assert (WORKSPACE_ROOT / "phase01-layer-create-fixtures/overwrites/000.txt").read_text(encoding="utf-8") == "overwrite-base-000\n"
assert (WORKSPACE_ROOT / "phase01-layer-create-fixtures/deletes/050.txt").exists()

publish_times = [
    float(row["timings"].get("layer_stack.publish.total_s", 0.0))
    for row in rows
]
summary_timings = {
    **base_timings,
    "layer_stack.publish.total_s": max(publish_times or [0.0]),
    "phase01.layer_create.publish_p50_s": _percentile(publish_times, 50),
    "phase01.layer_create.publish_p99_s": _percentile(publish_times, 99),
}
summary = _base_summary(
    case,
    binding,
    workspace_inv,
    summary_timings,
    pass_bars={
        "workloads": len(rows),
        "published_paths_visible": True,
        "overwrites_resolve_to_top_layer": True,
        "deletes_hide_base_content": True,
        "real_workspace_mutated": False,
    },
)
_emit_workspace_payload(label, started, summary, rows)
"""


async def test_layer_creation_workloads_publish_over_imported_base(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    payload = await run_workspace_base_probe(
        workspace_base_sandbox,
        _LAYER_CREATE_BODY.replace("__CFG__", "json.loads(__CFG_JSON__)"),
        label="workspace_base.layer_create_speed",
        cfg={"large_file_bytes": 2 * 1024 * 1024},
        timeout=300,
    )
    rows = payload["rows"]
    assert len(rows) == 6
    assert all(row["success"] for row in rows)
    artifact = write_jsonl_artifact(
        case="layer_create_speed",
        summary=payload["summary"],
        rows=rows,
    )
    print(f"\n[phase01:layer_create_speed] artifact={artifact}")
