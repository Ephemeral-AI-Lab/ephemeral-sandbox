"""Loose phase-01 performance redlines for workspace-base operations."""

from __future__ import annotations

import pytest

from .._harness.sandbox_fixture import SandboxHandle
from .._harness.workspace_base_metrics import env_int, write_jsonl_artifact
from .._harness.workspace_base_probe import run_workspace_base_probe


pytestmark = pytest.mark.asyncio


_BUDGET_BODY = r"""
label = "workspace_base.budget_redlines"
case = "budget_redlines"
started = time.perf_counter()
cfg = __CFG__
depths = [int(value) for value in cfg["depths"]]
max_depth = max(depths)
import_budget_s = float(cfg["import_p99_budget_ms"]) / 1000.0
materialize_budget_s = float(cfg["materialize_p99_budget_ms"]) / 1000.0
squash_budget_s = float(cfg["squash_p99_budget_ms"]) / 1000.0

seed = _seed_workspace_files(
    "phase01-budget-fixtures",
    files=max_depth + 5,
    deletes=max_depth + 5,
    overwrites=max_depth + 5,
)
workspace_inv = _inventory(WORKSPACE_ROOT)
rows = []
summary_binding = None
summary_timings = {}

import_times = []
for index in range(int(cfg["import_repeats"])):
    stack_root = _phase01_root(label, "import-%02d" % index)
    t0 = time.perf_counter()
    binding, timings = _build_base(stack_root)
    elapsed = float(timings["api.workspace_base.total_s"])
    import_times.append(elapsed)
    rows.append(_call_row(
        case,
        "import_%02d" % index,
        elapsed <= import_budget_s,
        t0,
        timings,
        extra={"budget_ms": import_budget_s * 1000.0},
    ))
    if summary_binding is None:
        summary_binding = binding
        summary_timings = dict(timings)


def _source_file(root, name, content):
    return _source(root, name, content)


def _publish_write(manager, root, index):
    _publish_changes(manager, [
        WriteLayerChange(
            path="phase01-budget/layer-%03d.txt" % index,
            source_path=str(_source_file(root, "layer-%03d" % index, "layer-%03d\n" % index)),
        )
    ])


materialize_times = []
materialize_root = _phase01_root(label, "materialize")
binding, timings = _build_base(materialize_root)
manager = LayerStackManager(materialize_root)
if summary_binding is None:
    summary_binding = binding
    summary_timings = dict(timings)
for depth in depths:
    while manager.read_active_manifest().depth < depth:
        _publish_write(manager, materialize_root, manager.read_active_manifest().depth)
    destination = materialize_root / ("materialize-%03d" % depth)
    t0 = time.perf_counter()
    _, _, elapsed = _materialize_digest(manager, destination)
    materialize_times.append(elapsed)
    rows.append(_call_row(
        case,
        "materialize_depth_%03d" % depth,
        elapsed <= materialize_budget_s,
        t0,
        {"layer_stack.materialize.total_s": elapsed},
        extra={"depth": depth, "budget_ms": materialize_budget_s * 1000.0},
    ))

squash_times = []
for depth in depths:
    if depth == 0:
        continue
    stack_root = _phase01_root(label, "squash-%03d" % depth)
    binding, timings = _build_base(stack_root)
    manager = LayerStackManager(stack_root)
    for index in range(depth):
        _publish_write(manager, stack_root, index)
    before = manager.read_active_manifest().depth
    t0 = time.perf_counter()
    squash_start = time.perf_counter()
    squashed = manager.squash(max_depth=4)
    elapsed = time.perf_counter() - squash_start
    squash_times.append(elapsed)
    assert squashed is not None
    after = manager.read_active_manifest().depth
    rows.append(_call_row(
        case,
        "squash_depth_%03d" % depth,
        elapsed <= squash_budget_s,
        t0,
        {"layer_stack.squash.total_s": elapsed, **timings},
        extra={
            "depth": depth,
            "before_depth": before,
            "after_depth": after,
            "budget_ms": squash_budget_s * 1000.0,
        },
    ))

import_p99 = _percentile(import_times, 99)
materialize_p99 = _percentile(materialize_times, 99)
squash_p99 = _percentile(squash_times, 99)
assert import_p99 <= import_budget_s, import_p99
assert materialize_p99 <= materialize_budget_s, materialize_p99
assert squash_p99 <= squash_budget_s, squash_p99
assert all(row["success"] for row in rows)

summary_timings.update({
    "phase01.import.runtime_p99_s": import_p99,
    "phase01.materialize.p99_s": materialize_p99,
    "phase01.squash.p99_s": squash_p99,
})
summary = _base_summary(
    case,
    summary_binding,
    workspace_inv,
    summary_timings,
    pass_bars={
        "depths": depths,
        "import_p99_budget_ms": import_budget_s * 1000.0,
        "materialize_p99_budget_ms": materialize_budget_s * 1000.0,
        "squash_p99_budget_ms": squash_budget_s * 1000.0,
    },
)
_emit_workspace_payload(label, started, summary, rows)
"""


async def test_workspace_base_import_materialize_and_squash_redlines(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    payload = await run_workspace_base_probe(
        workspace_base_sandbox,
        _BUDGET_BODY.replace("__CFG__", "json.loads(__CFG_JSON__)"),
        label="workspace_base.budget_redlines",
        cfg={
            "depths": [0, 20, 100],
            "import_repeats": env_int("EPHEMERALOS_PHASE01_BUDGET_IMPORT_REPEATS", 3),
            "import_p99_budget_ms": env_int(
                "EPHEMERALOS_PHASE01_IMPORT_P99_BUDGET_MS",
                2000,
            ),
            "materialize_p99_budget_ms": env_int(
                "EPHEMERALOS_PHASE01_MATERIALIZE_P99_BUDGET_MS",
                2500,
            ),
            "squash_p99_budget_ms": env_int(
                "EPHEMERALOS_PHASE01_SQUASH_P99_BUDGET_MS",
                2500,
            ),
        },
        timeout=600,
    )
    rows = payload["rows"]
    assert rows
    assert all(row["success"] for row in rows)
    artifact = write_jsonl_artifact(
        case="budget_redlines",
        summary=payload["summary"],
        rows=rows,
    )
    print(f"\n[phase01:budget_redlines] artifact={artifact}")
