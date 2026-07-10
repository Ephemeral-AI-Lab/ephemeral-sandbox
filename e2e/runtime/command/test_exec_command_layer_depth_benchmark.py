import json
import math
import os
import statistics
import subprocess
import time

import pytest

from core import cleanup
from core.cli import is_error, manager, runtime
from manager.management import helpers as mgmt


pytestmark = pytest.mark.skipif(
    os.environ.get("E2E_EXEC_BENCH") != "1",
    reason="set E2E_EXEC_BENCH=1 to run the exec_command layer-depth benchmark",
)

IMAGE = os.environ.get("E2E_IMAGE", "ephemeral-agent")
DEPTHS = [
    int(value)
    for value in os.environ.get("E2E_EXEC_BENCH_DEPTHS", "1,10,50,100").split(",")
    if value.strip()
]
SAMPLES = int(os.environ.get("E2E_EXEC_BENCH_SAMPLES", "5"))


def test_exec_command_layer_depth_shared_base_benchmark(tmp_path):
    # This benchmark is env-gated because repeated exec publishes can approach the retention cap.
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "README.md").write_text("exec benchmark\n", encoding="utf-8")

    sandbox_ids = []
    try:
        baseline = create_sandbox(str(workspace))
        shared_batch = create_sandbox(str(workspace), count=2)
        shared_sandboxes = shared_batch.get("sandboxes", [])
        assert len(shared_sandboxes) == 2, shared_batch

        baseline_id = baseline["id"]
        shared_id = shared_sandboxes[0]["id"]
        sandbox_ids.extend([baseline_id, *[record["id"] for record in shared_sandboxes]])
        for sandbox_id in sandbox_ids:
            cleanup.track(sandbox_id)

        baseline_depth = 0
        shared_depth = 0
        for depth in sorted(set(DEPTHS)):
            baseline_depth = grow_depth(baseline_id, baseline_depth, depth)
            shared_depth = grow_depth(shared_id, shared_depth, depth)

            baseline_ms = sample_exec_ms(baseline_id, SAMPLES)
            shared_ms = sample_exec_ms(shared_id, SAMPLES)
            baseline_stats = stats(baseline_ms)
            shared_stats = stats(shared_ms)

            tolerance = max(500.0, baseline_stats["p95"] * 0.15)
            assert shared_stats["p95"] <= baseline_stats["p95"] + tolerance, {
                "depth": depth,
                "baseline_ms": baseline_stats,
                "shared_base_ms": shared_stats,
            }

            row = {
                "depth": depth,
                "samples": SAMPLES,
                "baseline_ms": baseline_stats,
                "shared_base_ms": shared_stats,
                "baseline_size_rw": docker_size_rw(baseline_id),
                "shared_base_size_rw": docker_size_rw(shared_id),
            }
            assert row["shared_base_size_rw"] <= row["baseline_size_rw"] + max(
                10_000_000, int(row["baseline_size_rw"] * 0.15)
            ), row
            print(json.dumps(row, sort_keys=True), flush=True)
    finally:
        for sandbox_id in reversed(sandbox_ids):
            try:
                mgmt.destroy_sandbox(sandbox_id)
            except Exception:
                pass


def create_sandbox(workspace_root, count=None):
    args = ["create_sandbox", "--image", IMAGE, "--workspace-root", workspace_root]
    if count is not None:
        args.extend(["--count", str(count)])
    result = manager(*args, timeout=600)
    assert not is_error(result), result
    return result


def grow_depth(sandbox_id, current, target):
    for layer in range(current + 1, target + 1):
        command = f"printf 'layer-{layer}\\n' > /workspace/.eos-bench-layer-{layer}"
        result = runtime_exec(sandbox_id, command, timeout=180)
        assert result["status"] == "ok", result
        assert result["exit_code"] == 0, result
    return target


def sample_exec_ms(sandbox_id, samples):
    values = []
    for _ in range(samples):
        started = time.perf_counter()
        result = runtime_exec(
            sandbox_id,
            "pwd; test -d /workspace; ls /workspace >/dev/null",
            timeout=180,
        )
        values.append((time.perf_counter() - started) * 1000.0)
        assert result["status"] == "ok", result
        assert result["exit_code"] == 0, result
    return values


def runtime_exec(sandbox_id, command, timeout):
    return runtime(
        sandbox_id,
        "exec_command",
        "--timeout-ms",
        str(timeout * 1000),
        command,
        timeout=timeout,
    )


def stats(values):
    ordered = sorted(values)
    p95_index = max(0, math.ceil(len(ordered) * 0.95) - 1)
    return {
        "min": ordered[0],
        "p50": statistics.median(ordered),
        "p95": ordered[p95_index],
        "max": ordered[-1],
    }


def docker_size_rw(sandbox_id):
    proc = subprocess.run(
        ["docker", "inspect", "--size", "--format", "{{.SizeRw}}", sandbox_id],
        capture_output=True,
        text=True,
        timeout=30,
        check=True,
    )
    return int(proc.stdout.strip())
