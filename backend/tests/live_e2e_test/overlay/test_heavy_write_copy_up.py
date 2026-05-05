"""E2.2 — heavy-write copy-up performance through a deep overlay stack.

Backs §4.2's worst-case write tail: a build/script that rewrites many
files in one call. Each write to a path that lives in a lower layer
forces overlayfs to copy-up the file before applying the write, so the
per-write cost is dominated by lowerdir-stack walk + content copy.

Default workload: 1000 files spread across 100 lower layers, each write
overwrites the file with 256 bytes. Override with
``EPHEMERALOS_OVERLAY_HEAVY_WRITE_FILES`` (push to 10000 for the
architect's flagged worst-case) and ``..._DEPTH`` /
``..._BYTES`` if needed.
"""

from __future__ import annotations

import json
import os

import pytest

from .._harness.overlay_probe import (
    OVERLAY_ROOT,
    script_heavy_write_copy_up,
    wrap_unshare,
)
from .._harness.sandbox_fixture import SandboxHandle


def _env_int(name: str, default: int) -> int:
    raw = os.environ.get(name, "").strip()
    return int(raw) if raw else default


def _print_metrics(label: str, payload: dict) -> None:
    print(f"\n[{label}] {json.dumps(payload, separators=(',', ':'))}")


@pytest.mark.asyncio
async def test_heavy_write_copy_up_at_depth_100(
    overlay_sandbox: SandboxHandle,
) -> None:
    depth = _env_int("EPHEMERALOS_OVERLAY_HEAVY_WRITE_DEPTH", 100)
    files = _env_int("EPHEMERALOS_OVERLAY_HEAVY_WRITE_FILES", 1000)
    write_bytes = _env_int("EPHEMERALOS_OVERLAY_HEAVY_WRITE_BYTES", 256)

    cmd = wrap_unshare(
        script_heavy_write_copy_up(
            overlay_root=OVERLAY_ROOT,
            depth=depth,
            files=files,
            write_bytes=write_bytes,
        )
    )
    result = await overlay_sandbox.raw_exec(
        overlay_sandbox.sandbox_id, cmd, timeout=600
    )
    assert result.exit_code == 0, (
        f"heavy-write probe failed (rc={result.exit_code}): "
        f"{result.stderr or result.stdout}"
    )
    payload = json.loads(result.stdout.strip().splitlines()[-1])
    _print_metrics("E2.2.heavy_write_copy_up", payload)

    assert payload["write_failures"] == 0, payload
    assert payload["upper_files"] == files, payload

    # Pass bar: at depth 100, p99 per-write copy-up stays under 50 ms even
    # for the slowest entry, and aggregate throughput exceeds 200 writes/s.
    # These are loose budgets — copy-up costs are dominated by I/O on
    # /dev/shm and shouldn't approach this threshold; tightening them
    # later is welcome once we have more baselines.
    assert payload["p99_ms"] < 50.0, (
        f"copy-up p99={payload['p99_ms']:.3f}ms exceeds 50ms budget at "
        f"depth={depth} files={files}"
    )
    assert payload["writes_per_s"] > 200.0, (
        f"writes_per_s={payload['writes_per_s']:.1f} too low at "
        f"depth={depth} files={files}"
    )

    summary = {
        "depth": depth,
        "files": files,
        "write_bytes": write_bytes,
        "mount_ms": round(payload["mount_ms"], 3),
        "total_write_ms": round(payload["total_write_ms"], 3),
        "writes_per_s": round(payload["writes_per_s"], 1),
        "p50_ms": round(payload["p50_ms"], 4),
        "p95_ms": round(payload["p95_ms"], 4),
        "p99_ms": round(payload["p99_ms"], 4),
        "max_ms": round(payload["max_ms"], 4),
        "upper_bytes": payload["upper_bytes"],
    }
    _print_metrics("E2.2.summary", summary)
