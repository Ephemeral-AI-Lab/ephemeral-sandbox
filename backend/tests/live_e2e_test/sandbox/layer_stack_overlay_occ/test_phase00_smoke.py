"""Phase 00 — Tier-1 hot-path smoke (progressive-live-test-tiers §3 Tier 1).

Three sequential cells validate the daemon socket bind, capture
pipeline, OCC routing across both merge paths, and /dev/shm cleanup.
Designed to fail fast (≤60s wall budget) so a Daytona-side stall
surfaces in 30-60s instead of waiting for a multi-minute matrix to
report PASS/FAIL.

Each cell streams a row to ``.omc/results/phase00-smoke-<run_id>.jsonl``
*before* the next cell runs (per design §4). A kill-9 mid-test
preserves prior cells' rows.

The runner sets ``EOS_TIER_RUN_ID`` so every tier in one
``run_tiered.py`` invocation lands in a deterministically-named
artifact and resume-on-restart works (design §5).
"""

from __future__ import annotations

import json
import time
from collections.abc import Awaitable, Callable
from pathlib import Path

import pytest

from .._harness.phase05_public_file_ops import seed_phase05_imported_base
from .._harness.sandbox_fixture import SandboxHandle
from .._harness.streaming_artifact import (
    resolve_run_id as _resolve_run_id,
    stream_row as _stream_row,
)


pytestmark = pytest.mark.asyncio


_GATED_PATH = "tracked/smoke/probe.txt"
_DIST_PATH = "dist/smoke/probe.txt"
_SMOKE_TEXT = "hello"


def _artifact_path() -> Path:
    target = Path.cwd() / ".omc" / "results" / f"phase00-smoke-{_resolve_run_id()}.jsonl"
    target.parent.mkdir(parents=True, exist_ok=True)
    return target


def _completed_cell_ids(artifact: Path) -> set[str]:
    """Return cell_ids whose previous run finished with passed=True."""
    if not artifact.exists():
        return set()
    completed: set[str] = set()
    with artifact.open(encoding="utf-8") as fh:
        for line in fh:
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                continue
            if row.get("passed") is True and row.get("cell_id"):
                completed.add(str(row["cell_id"]))
    return completed


async def _run_noop(handle: SandboxHandle) -> tuple[bool, str | None]:
    """Cell 1 — daemon socket + capture pipeline alive."""
    result = await handle.tool.shell(
        "true", timeout=15, description="phase00 smoke noop"
    )
    if not result.success:
        return False, (
            f"shell true failed exit={result.exit_code} "
            f"stderr={(result.stderr or '')[:200]!r}"
        )
    return True, None


async def _run_write_and_read(
    handle: SandboxHandle, *, path: str
) -> tuple[bool, str | None]:
    """Cells 2 & 3 — write a single file, then read it back via tool.read_file.

    Validates one route (gated for ``tracked/``, direct for ``dist/``)
    end-to-end: shell capture → OCC commit → merged-view read.
    """
    parent = path.rsplit("/", 1)[0]
    cmd = f"mkdir -p {parent} && printf %s {_SMOKE_TEXT!r} > {path}"
    write = await handle.tool.shell(
        cmd, timeout=15, description=f"phase00 smoke write {path}"
    )
    if not write.success:
        return False, (
            f"write failed path={path} exit={write.exit_code} "
            f"stderr={(write.stderr or '')[:200]!r}"
        )
    rf = await handle.tool.read_file(path)
    if not rf.exists:
        return False, f"read_file({path!r}) returned exists=False"
    if not rf.content.startswith(_SMOKE_TEXT):
        return False, (
            f"read_file({path!r}) content prefix mismatch: "
            f"got {rf.content[: len(_SMOKE_TEXT) + 4]!r}"
        )
    return True, None


async def test_phase00_smoke(workspace_base_sandbox: SandboxHandle) -> None:
    handle = workspace_base_sandbox
    await seed_phase05_imported_base(handle)

    artifact = _artifact_path()
    completed = _completed_cell_ids(artifact)

    CellFn = Callable[[], Awaitable[tuple[bool, str | None]]]
    cells: list[tuple[str, CellFn]] = [
        ("smoke_shell_true", lambda: _run_noop(handle)),
        (
            "smoke_write_gated",
            lambda: _run_write_and_read(handle, path=_GATED_PATH),
        ),
        (
            "smoke_write_direct",
            lambda: _run_write_and_read(handle, path=_DIST_PATH),
        ),
    ]

    failures: list[str] = []
    for cell_id, runner in cells:
        if cell_id in completed:
            continue
        start = time.perf_counter()
        passed, failure_reason = await runner()
        elapsed_ms = (time.perf_counter() - start) * 1000.0
        row: dict[str, object] = {
            "schema": "phase00.smoke.v1",
            "cell_id": cell_id,
            "passed": passed,
            "elapsed_ms": round(elapsed_ms, 3),
            "failure_reason": failure_reason,
        }
        _stream_row(artifact, row)
        if not passed:
            failures.append(f"{cell_id}: {failure_reason}")

    summary = {
        "schema": "phase00.smoke.summary.v1",
        "artifact": str(artifact),
        "total_cells": len(cells),
        "skipped_resume": len(completed & {c for c, _ in cells}),
        "failed_cells": len(failures),
    }
    _stream_row(artifact, summary)
    print(f"\n[phase00:smoke] artifact={artifact}")
    assert not failures, "phase00 smoke failures:\n" + "\n".join(failures)
