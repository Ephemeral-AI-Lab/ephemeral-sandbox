"""Diagnostic shell latency matrix for selected concurrency levels.

Skipped by default so the 3.1 gate does not pay this probe unless explicitly
requested with ``EOS_RUN_SHELL_LATENCY_MATRIX=1``. Override levels with
``EOS_SHELL_LATENCY_MATRIX_LEVELS=1,2,5`` and mark calls as background with
``EOS_SHELL_LATENCY_MATRIX_BACKGROUND=1``.
"""

from __future__ import annotations

import asyncio
import json
import os
import time
from datetime import UTC, datetime
from pathlib import Path
from statistics import median
from typing import Any

import pytest

import sandbox.api as sandbox_api
from sandbox.api import ExecCommandRequest, SandboxCaller


pytestmark = [
    pytest.mark.asyncio,
    pytest.mark.skipif(
        os.getenv("EOS_RUN_SHELL_LATENCY_MATRIX") != "1",
        reason="shell latency matrix is an explicit diagnostic",
    ),
]

_LEVELS = tuple(
    int(part.strip())
    for part in os.getenv("EOS_SHELL_LATENCY_MATRIX_LEVELS", "1,5,10").split(",")
    if part.strip()
)
_BACKGROUND = os.getenv("EOS_SHELL_LATENCY_MATRIX_BACKGROUND") == "1"
_ROOT = "/testbed/.ephemeralos/sweevo-mock/shell_concurrency_latency_matrix"
_ARTIFACT_DIR = Path(".sweevo_runs/manual_diagnostics/shell_concurrency_latency")
_TIMING_KEYS = (
    "api.exec_command.dispatch_total_s",
    "api.exec_command.total_s",
    "command_exec.capture_upperdir_s",
    "command_exec.occ_apply_s",
    "occ.apply.commit_queue_wait_s",
    "occ.apply.total_s",
    "runtime.dispatch_s",
    "runtime.read_request_s",
    "resource.layer_stack.manifest_depth",
)


@pytest.mark.timeout(900)
async def test_shell_concurrency_latency_matrix(workspace: dict[str, object]) -> None:
    sandbox_id = str(workspace["sandbox_id"])
    groups: list[dict[str, Any]] = []
    for level in _LEVELS:
        wall_start = time.monotonic()
        results = await asyncio.gather(
            *(_run_exec_command(sandbox_id, level, index) for index in range(level))
        )
        wall_s = time.monotonic() - wall_start
        groups.append(_summarize_group(level, wall_s, results))

    payload = {
        "schema": "test_runner.shell_concurrency_latency_matrix.v1",
        "generated_at": datetime.now(UTC).isoformat(),
        "sandbox_id": sandbox_id,
        "levels": list(_LEVELS),
        "background": _BACKGROUND,
        "groups": groups,
    }
    _ARTIFACT_DIR.mkdir(parents=True, exist_ok=True)
    path = _ARTIFACT_DIR / f"{datetime.now(UTC).strftime('%Y%m%dT%H%M%SZ')}.json"
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"SHELL_LATENCY_MATRIX_ARTIFACT={path.as_posix()}")
    print(json.dumps(payload, sort_keys=True))

    assert all(
        sample["success"] and sample["exit_code"] == 0
        for group in groups
        for sample in group["samples"]
    )


async def _run_exec_command(
    sandbox_id: str,
    level: int,
    index: int,
) -> dict[str, Any]:
    caller = SandboxCaller(
        agent_id=f"shell-latency-{_mode_slug()}-c{level}-{index}",
        agent_run_id=f"shell-latency-{_mode_slug()}-c{level}-{index}",
        tool_name="exec_command",
        tool_id=f"diagnostic-{_mode_slug()}-c{level}-{index}",
    )
    command = (
        f"mkdir -p {_ROOT}/c{level} && "
        f"printf 'level={level}\\nworker={index}\\n' > {_ROOT}/c{level}/worker-{index:02d}.txt && "
        f"cat {_ROOT}/c{level}/worker-{index:02d}.txt"
    )
    wall_start = time.monotonic()
    result = await sandbox_api.exec_command(
        sandbox_id,
        ExecCommandRequest(
            cmd=command,
            timeout=120,
            caller=caller,
            description=(
                f"exec_command latency diagnostic mode={_mode_slug()} "
                f"concurrency={level}"
            ),
        ),
    )
    wall_s = time.monotonic() - wall_start
    return {
        "index": index,
        "success": bool(result.success),
        "status": result.status,
        "exit_code": result.exit_code,
        "wall_s": wall_s,
        "changed_paths": list(result.changed_paths),
        "timings": {key: float(value) for key, value in result.timings.items()},
    }


def _summarize_group(
    level: int,
    wall_s: float,
    samples: list[dict[str, Any]],
) -> dict[str, Any]:
    return {
        "concurrency": level,
        "wall_s": wall_s,
        "sample_count": len(samples),
        "timing_summary": {
            key: _stats(
                sample["timings"][key]
                for sample in samples
                if key in sample["timings"]
            )
            for key in _TIMING_KEYS
        },
        "samples": samples,
    }


def _stats(values_iter: Any) -> dict[str, float | int | None]:
    values = sorted(float(value) for value in values_iter)
    if not values:
        return {"count": 0, "min": None, "mean": None, "p50": None, "p95": None, "max": None}
    return {
        "count": len(values),
        "min": values[0],
        "mean": sum(values) / len(values),
        "p50": median(values),
        "p95": values[min(len(values) - 1, int(len(values) * 0.95))],
        "max": values[-1],
    }


def _mode_slug() -> str:
    return "background" if _BACKGROUND else "foreground"
