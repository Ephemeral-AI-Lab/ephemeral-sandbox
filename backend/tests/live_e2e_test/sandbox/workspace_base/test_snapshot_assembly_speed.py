"""Phase-01 snapshot materialization metrics over imported workspace bases."""

from __future__ import annotations

import pytest

from .._harness.sandbox_fixture import SandboxHandle
from .._harness.workspace_base_metrics import write_jsonl_artifact
from .._harness.workspace_base_probe import run_workspace_base_probe


pytestmark = pytest.mark.asyncio


_SNAPSHOT_ASSEMBLY_BODY = r"""
label = "workspace_base.snapshot_assembly_speed"
case = "snapshot_assembly_speed"
started = time.perf_counter()
depths = list(__CFG__["depths"])
max_depth = max(depths)
seed = _seed_workspace_files(
    "phase01-snapshot-fixtures",
    files=max_depth + 5,
    deletes=max_depth + 5,
    overwrites=max_depth + 5,
)
workspace_inv = _inventory(WORKSPACE_ROOT)
rows = []
summary_binding = None
summary_timings = {}


def _source_file(root, name, content):
    return _source(root, name, content)


def _changes_for(workload, index, root):
    if workload == "append":
        return [
            WriteLayerChange(
                path="phase01-snapshot/append/%03d.txt" % index,
                source_path=str(_source_file(root, "append-%03d" % index, "append-%03d\n" % index)),
            )
        ]
    if workload == "overwrite":
        return [
            WriteLayerChange(
                path="phase01-snapshot-fixtures/overwrites/%03d.txt" % index,
                source_path=str(_source_file(root, "overwrite-%03d" % index, "overwrite-%03d\n" % index)),
            )
        ]
    if workload == "delete":
        return [
            DeleteLayerChange(
                path="phase01-snapshot-fixtures/deletes/%03d.txt" % index,
            )
        ]
    if workload == "symlink":
        return [
            SymlinkLayerChange(
                path="phase01-snapshot/symlinks/link-%03d" % index,
                source_path="target-%03d.txt" % index,
            )
        ]
    if workload == "opaque":
        return [
            OpaqueDirLayerChange(
                path="phase01-snapshot/opaque",
            ),
            WriteLayerChange(
                path="phase01-snapshot/opaque/%03d.txt" % index,
                source_path=str(_source_file(root, "opaque-%03d" % index, "opaque-%03d\n" % index)),
            ),
        ]
    raise AssertionError(workload)


def _verify(workload, manager, materialized, depth):
    if workload == "base_only":
        assert (materialized / "phase01-snapshot-fixtures").is_dir()
        return
    if workload == "append" and depth > 0:
        rel = "phase01-snapshot/append/%03d.txt" % (depth - 1)
        assert manager.read_text(rel) == ("append-%03d\n" % (depth - 1), True)
        assert (materialized / rel).read_text(encoding="utf-8") == "append-%03d\n" % (depth - 1)
    if workload == "overwrite" and depth > 0:
        rel = "phase01-snapshot-fixtures/overwrites/%03d.txt" % (depth - 1)
        assert manager.read_text(rel) == ("overwrite-%03d\n" % (depth - 1), True)
        assert (materialized / rel).read_text(encoding="utf-8") == "overwrite-%03d\n" % (depth - 1)
        assert (WORKSPACE_ROOT / rel).read_text(encoding="utf-8") == "overwrite-base-%03d\n" % (depth - 1)
    if workload == "delete" and depth > 0:
        rel = "phase01-snapshot-fixtures/deletes/%03d.txt" % (depth - 1)
        assert manager.read_bytes(rel) == (None, False)
        assert not (materialized / rel).exists()
        assert (WORKSPACE_ROOT / rel).exists()
    if workload == "symlink" and depth > 0:
        rel = "phase01-snapshot/symlinks/link-%03d" % (depth - 1)
        assert manager.read_symlink(rel) == ("target-%03d.txt" % (depth - 1), True)
        assert os.readlink(materialized / rel) == "target-%03d.txt" % (depth - 1)
    if workload == "opaque" and depth > 0:
        assert manager.list_dir("phase01-snapshot/opaque") == ("%03d.txt" % (depth - 1),)
        assert sorted(path.name for path in (materialized / "phase01-snapshot/opaque").iterdir()) == ["%03d.txt" % (depth - 1)]


def _measure_materialize(workload, manager, stack_root, depth, mode):
    destination = stack_root / ("materialized-%s-%s-%03d" % (workload, mode, depth))
    t0 = time.perf_counter()
    digest, inventory, elapsed = _materialize_digest(manager, destination)
    _verify(workload, manager, destination, depth)
    rows.append(_call_row(
        case,
        "%s_depth_%03d_%s" % (workload, depth, mode),
        True,
        t0,
        timings={"layer_stack.materialize.total_s": elapsed},
        extra={
            "workload": workload,
            "depth": depth,
            "mode": mode,
            "materialized_digest": digest,
            "materialized_inventory": inventory,
        },
    ))
    return elapsed


for workload in ("base_only", "append", "overwrite", "delete", "symlink", "opaque"):
    stack_root = _phase01_root(label, workload)
    binding, timings = _build_base(stack_root)
    if summary_binding is None:
        summary_binding = binding
        summary_timings = dict(timings)
    manager = LayerStackManager(stack_root)
    if workload == "base_only":
        _measure_materialize(workload, manager, stack_root, 0, "cold")
        _measure_materialize(workload, manager, stack_root, 0, "warm")
        continue
    checkpoints = set(depths)
    checkpoints.discard(0)
    for index in range(max_depth):
        _publish_changes(manager, _changes_for(workload, index, stack_root))
        depth = index + 1
        if depth not in checkpoints:
            continue
        cold = _measure_materialize(workload, manager, stack_root, depth, "cold")
        warm = _measure_materialize(workload, manager, stack_root, depth, "warm")
        assert cold >= 0
        assert warm >= 0

materialize_times = [
    float(row["timings"].get("layer_stack.materialize.total_s", 0.0))
    for row in rows
]
summary_timings.update({
    "layer_stack.materialize.total_s": max(materialize_times or [0.0]),
    "phase01.materialize.p50_s": _percentile(materialize_times, 50),
    "phase01.materialize.p99_s": _percentile(materialize_times, 99),
})
summary = _base_summary(
    case,
    summary_binding,
    workspace_inv,
    summary_timings,
    pass_bars={
        "depths": depths,
        "cold_and_warm_rows": True,
        "snapshot_outside_workspace": True,
        "cases": ["base_only", "append", "overwrite", "delete", "symlink", "opaque"],
    },
)
_emit_workspace_payload(label, started, summary, rows)
"""


async def test_snapshot_materialization_over_base_plus_layers(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    payload = await run_workspace_base_probe(
        workspace_base_sandbox,
        _SNAPSHOT_ASSEMBLY_BODY.replace("__CFG__", "json.loads(__CFG_JSON__)"),
        label="workspace_base.snapshot_assembly_speed",
        cfg={"depths": [0, 1, 5, 20, 100, 200]},
        timeout=600,
    )
    rows = payload["rows"]
    assert rows
    assert all(row["success"] for row in rows)
    assert any(row["label"].endswith("_cold") for row in rows)
    assert any(row["label"].endswith("_warm") for row in rows)
    artifact = write_jsonl_artifact(
        case="snapshot_assembly_speed",
        summary=payload["summary"],
        rows=rows,
    )
    print(f"\n[phase01:snapshot_assembly_speed] artifact={artifact}")
