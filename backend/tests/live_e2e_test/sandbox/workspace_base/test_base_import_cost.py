"""Phase-01 live cost metrics for importing `/testbed` as a base layer."""

from __future__ import annotations

import hashlib
import shlex

import pytest

from .._harness.sandbox_fixture import SandboxHandle, WORKSPACE_ROOT
from .._harness.workspace_base_metrics import (
    base_summary,
    call_row,
    env_int,
    monotonic_ms,
    path_sha256,
    percentile,
    phase01_stack_root,
    reset_layer_stack_root,
    runtime_call,
    selected_text_paths,
    workspace_inventory,
    write_jsonl_artifact,
)


pytestmark = pytest.mark.asyncio


async def test_base_import_cost_metrics(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    case = "base_import_cost"
    repeats = env_int("EPHEMERALOS_PHASE01_BASE_IMPORT_REPEATS", 5)
    inventory = await workspace_inventory(handle)
    assert int(inventory["files"]) > 0
    assert int(inventory["dirs"]) > 0
    assert int(inventory["bytes"]) > 0

    text_paths = await selected_text_paths(handle, max_files=8)
    assert text_paths, "workspace file walk did not include readable text paths"

    rows: list[dict[str, object]] = []
    wall_values: list[float] = []
    runtime_values: list[float] = []
    last_binding: dict[str, object] | None = None
    last_timings: dict[str, float] = {}

    for index in range(repeats):
        root = phase01_stack_root(case, f"{index:02d}")
        await reset_layer_stack_root(handle, root)
        before = await handle.raw_exec(
            handle.sandbox_id,
            f"test ! -e {root}/workspace.json",
            timeout=15,
        )
        assert before.exit_code == 0, before.stderr or before.stdout

        start_ms = monotonic_ms()
        result = await runtime_call(
            handle,
            "api.build_workspace_base",
            {"workspace_root": WORKSPACE_ROOT},
            layer_stack_root=root,
            timeout=240,
        )
        wall_ms = monotonic_ms() - start_ms
        assert result["success"] is True
        binding = result["binding"]
        assert isinstance(binding, dict)
        timings = {
            str(key): float(value)
            for key, value in dict(result.get("timings") or {}).items()
        }
        runtime_ms = timings.get("api.workspace_base.total_s", 0.0) * 1000.0
        rows.append(
            call_row(
                case=case,
                label=f"import_{index + 1:03d}",
                success=True,
                wall_ms=wall_ms,
                runtime_ms=runtime_ms,
                timings=timings,
                extra={
                    "files_per_s": _rate(inventory["files"], runtime_ms),
                    "bytes_per_s": _rate(inventory["bytes"], runtime_ms),
                },
            )
        )
        wall_values.append(wall_ms)
        runtime_values.append(runtime_ms)

        assert binding["base_manifest_version"] == 1
        assert binding["active_manifest_version"] == 1
        assert binding["base_root_hash"] == binding["active_root_hash"]
        assert binding["workspace_root"] == WORKSPACE_ROOT
        assert str(binding["layer_stack_root"]).startswith("/eos-mount-scratch/")
        assert not str(binding["layer_stack_root"]).startswith(WORKSPACE_ROOT)

        exists = await handle.raw_exec(
            handle.sandbox_id,
            f"test -f {shlex.quote(root)}/workspace.json && "
            "python3 -c "
            + shlex.quote(
                "import json,sys;"
                "print(json.load(open(sys.argv[1]))['active_manifest_version'])"
            )
            + f" {shlex.quote(root)}/workspace.json",
            timeout=15,
        )
        assert exists.exit_code == 0, exists.stderr or exists.stdout
        assert exists.stdout.strip() == "1"

        metrics = await runtime_call(
            handle,
            "api.layer_metrics",
            {"agent_id": handle.caller.agent_id},
            layer_stack_root=root,
            timeout=60,
        )
        assert metrics["success"] is True
        assert metrics["manifest_version"] == 1
        assert metrics["workspace_bound"] is True

        await _assert_selected_hashes_match(handle, root, text_paths[:4])
        last_binding = binding
        last_timings = timings

    assert last_binding is not None
    summary_timings = {
        **last_timings,
        "phase01.import.wall_p50_s": percentile(wall_values, 50) / 1000.0,
        "phase01.import.wall_p99_s": percentile(wall_values, 99) / 1000.0,
        "phase01.import.runtime_p50_s": percentile(runtime_values, 50) / 1000.0,
        "phase01.import.runtime_p99_s": percentile(runtime_values, 99) / 1000.0,
    }
    artifact = write_jsonl_artifact(
        case=case,
        summary=base_summary(
            case=case,
            binding=last_binding,
            workspace_inventory=inventory,
            timings=summary_timings,
            pass_bars={
                "sequential_rebuilds": repeats,
                "hard_budget": None,
                "baseline_only": True,
            },
        ),
        rows=rows,
    )
    print(f"\n[phase01:{case}] artifact={artifact}")


async def _assert_selected_hashes_match(
    handle: SandboxHandle,
    layer_stack_root: str,
    paths: list[str],
) -> None:
    for path in paths:
        raw_hash = await path_sha256(handle, f"{WORKSPACE_ROOT}/{path}")
        read = await runtime_call(
            handle,
            "api.read_file",
            {"path": path},
            layer_stack_root=layer_stack_root,
            timeout=60,
        )
        assert read["success"] is True
        assert read["exists"] is True
        content = str(read["content"]).encode("utf-8")
        assert hashlib.sha256(content).hexdigest() == raw_hash


def _rate(value: object, runtime_ms: float) -> float:
    elapsed_s = max(runtime_ms / 1000.0, 0.001)
    return round(float(value) / elapsed_s, 3)
