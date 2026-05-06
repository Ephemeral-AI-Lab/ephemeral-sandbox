"""Live load metrics for reading the `/testbed` workspace base."""

from __future__ import annotations

import asyncio
import json
import os
import time
from datetime import datetime, timezone
from pathlib import Path

import pytest

from .._harness.integrated_cases import (
    RuntimeCallMetric,
    emit_metric,
    percentile,
    summarize_calls,
    timed_call,
)
from .._harness.sandbox_fixture import SandboxHandle, WORKSPACE_ROOT


pytestmark = pytest.mark.asyncio

_TEXT_SUFFIXES = (
    ".cfg",
    ".css",
    ".ini",
    ".js",
    ".json",
    ".md",
    ".py",
    ".rst",
    ".toml",
    ".txt",
    ".yaml",
    ".yml",
)


async def test_workspace_base_read_load_metrics(
    integrated_sandbox: SandboxHandle,
) -> None:
    handle = integrated_sandbox
    probe = await handle.raw_exec(
        handle.sandbox_id,
        "set -e; test -d /testbed; git -C /testbed rev-parse --show-toplevel",
        timeout=30,
    )
    assert probe.exit_code == 0, probe.stderr or probe.stdout
    assert WORKSPACE_ROOT in probe.stdout

    binding = await _workspace_binding(handle)
    assert binding["workspace_root"] == WORKSPACE_ROOT
    paths = await _selected_text_paths(
        handle,
        max_files=_env_int("EPHEMERALOS_READ_LOAD_FILES", 16),
    )
    assert paths, "workspace file walk did not include readable text paths"

    total_reads = _env_int("EPHEMERALOS_READ_LOAD_CALLS", max(32, len(paths) * 2))
    concurrency = _env_int("EPHEMERALOS_READ_LOAD_CONCURRENCY", 8)
    read_paths = [paths[index % len(paths)] for index in range(total_reads)]
    semaphore = asyncio.Semaphore(max(1, concurrency))

    async def read_one(index: int, path: str):
        async with semaphore:
            return await timed_call(
                f"workspace_base_read_{index:03d}",
                handle.tool.read_file(path),
            )

    batch_start = time.perf_counter()
    rows = await asyncio.gather(
        *(read_one(index, path) for index, path in enumerate(read_paths))
    )
    batch_wall_ms = (time.perf_counter() - batch_start) * 1000.0
    metrics: list[RuntimeCallMetric] = []
    for result, metric in rows:
        assert result.success
        assert result.exists
        metrics.append(metric)

    runtime_ms = [_runtime_ms(metric) for metric in metrics]
    artifact = _write_artifact(
        paths=paths,
        metrics=metrics,
        batch_wall_ms=batch_wall_ms,
        concurrency=concurrency,
        binding=binding,
    )
    emit_metric(
        "workspace_base.read_load",
        {
            **summarize_calls(metrics),
            "batch_wall_ms": round(batch_wall_ms, 3),
            "concurrency": concurrency,
            "unique_paths": len(paths),
            "runtime_p50_ms": round(percentile(runtime_ms, 50), 3),
            "runtime_p99_ms": round(percentile(runtime_ms, 99), 3),
            "base_manifest_version": binding["base_manifest_version"],
            "base_root_hash": binding["base_root_hash"],
            "artifact": str(artifact),
        },
    )


async def _workspace_binding(handle: SandboxHandle) -> dict[str, object]:
    result = await handle.tool.layer_metrics()
    assert result["success"] is True
    assert result["workspace_bound"] is True
    binding_result = await handle.tool.workspace_binding()
    assert binding_result["success"] is True
    binding = binding_result["binding"]
    assert isinstance(binding, dict)
    return binding


async def _selected_text_paths(
    handle: SandboxHandle,
    *,
    max_files: int,
) -> list[str]:
    result = await handle.raw_exec(
        handle.sandbox_id,
        (
            "find /testbed -xdev -type f "
            r"-printf '%P\n' | sort"
        ),
        timeout=30,
    )
    assert result.exit_code == 0, result.stderr or result.stdout
    selected = [
        path
        for path in result.stdout.splitlines()
        if Path(path).suffix.lower() in _TEXT_SUFFIXES
    ]
    return selected[:max(1, max_files)]


def _runtime_ms(metric: RuntimeCallMetric) -> float:
    value = metric.timings.get("api.read.total_s")
    if value is not None:
        return float(value) * 1000.0
    return metric.elapsed_ms


def _write_artifact(
    *,
    paths: list[str],
    metrics: list[RuntimeCallMetric],
    batch_wall_ms: float,
    concurrency: int,
    binding: dict[str, object],
) -> Path:
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    artifact = (
        Path.cwd()
        / ".omc"
        / "results"
        / f"live-e2e-workspace-base-read-load-{stamp}.jsonl"
    )
    artifact.parent.mkdir(parents=True, exist_ok=True)
    with artifact.open("w", encoding="utf-8") as file:
        header = {
            "schema": "sandbox.live_e2e.workspace_base_read_load.v1",
            "kind": "summary",
            "workspace_root": binding["workspace_root"],
            "base_manifest_version": binding["base_manifest_version"],
            "base_root_hash": binding["base_root_hash"],
            "paths": paths,
            "calls": len(metrics),
            "concurrency": concurrency,
            "batch_wall_ms": round(batch_wall_ms, 3),
        }
        file.write(json.dumps(header, sort_keys=True, separators=(",", ":")))
        file.write("\n")
        for metric in metrics:
            row = {
                "schema": "sandbox.live_e2e.workspace_base_read_load.v1",
                "kind": "call",
                "label": metric.label,
                "success": metric.success,
                "status": metric.status,
                "wall_ms": round(metric.elapsed_ms, 3),
                "runtime_ms": round(_runtime_ms(metric), 3),
                "timings": {
                    key: round(float(value), 6)
                    for key, value in sorted(metric.timings.items())
                },
            }
            file.write(json.dumps(row, sort_keys=True, separators=(",", ":")))
            file.write("\n")
    return artifact


def _env_int(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw is None or raw == "":
        return default
    return int(raw)
