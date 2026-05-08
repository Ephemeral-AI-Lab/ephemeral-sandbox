"""Phase 08 — /dev/shm bounded regression test (Verification E).

Improvement #1 added ``shutil.rmtree(run_dir, ignore_errors=True)`` to
the daemon's outer ``finally`` in ``shell_runner._execute_shell``. This
test asserts the cleanup actually keeps ``/dev/shm/eos-command-exec/``
bounded across long-running daemon sessions — the prerequisite for
every concurrency / soak test downstream.

The test loops ``tool.shell("true")`` 200 times sequentially, probes
``/dev/shm/eos-command-exec/`` via ``raw_exec`` after every 50 calls,
and asserts:

- Run-dir entry count ≤ 5 at every sample point (only currently-active
  calls; sequential runs should leave ≤ 1 between iterations).
- Total ``du -sh`` size ≤ 5 MiB at every sample point.

Pre-improvement #1: every call leaves a run-dir tree behind, so 200
calls yield 200+ entries — pytest fails with /dev/shm exhaustion.

Each probe emits a JSONL row to
``.omc/results/phase08-dev-shm-bounded-<run_id>.jsonl`` so the bound
trajectory is mechanically diff-able across runs.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from .._harness.phase05_public_file_ops import seed_phase05_imported_base
from .._harness.sandbox_fixture import SandboxHandle
from .._harness.streaming_artifact import (
    resolve_run_id as _resolve_run_id,
    stream_row as _stream_row,
)


pytestmark = pytest.mark.asyncio

_PROBE_DIR = "/dev/shm/eos-command-exec"
_RUN_DIR_LIMIT = 5
_TOTAL_BYTES_LIMIT = 5 * 1024 * 1024  # 5 MiB
_TOTAL_CALLS = 200
_PROBE_EVERY = 50


def _artifact_path() -> Path:
    target = (
        Path.cwd()
        / ".omc"
        / "results"
        / f"phase08-dev-shm-bounded-{_resolve_run_id()}.jsonl"
    )
    target.parent.mkdir(parents=True, exist_ok=True)
    return target


async def _probe_dev_shm(handle: SandboxHandle) -> tuple[int, int, str]:
    """Return (run_dir_count, total_bytes, raw_listing) from the live sandbox.

    Uses raw_exec so the probe travels through the same Daytona path
    the daemon uses — no fixture-side mocking. ``find -mindepth 2 -maxdepth 2``
    counts run-dir entries (one level under the storage_root key dir).
    ``du -sb`` reports total bytes; missing dir → 0.
    """
    cmd = (
        f"set -e; "
        f"if [ -d {_PROBE_DIR} ]; then "
        f"  count=$(find {_PROBE_DIR} -mindepth 2 -maxdepth 2 "
        f"          -type d 2>/dev/null | wc -l); "
        f"  bytes=$(du -sb {_PROBE_DIR} 2>/dev/null | awk '{{print $1}}'); "
        f"  listing=$(ls -la {_PROBE_DIR}/*/ 2>/dev/null | head -20 || true); "
        f"  printf 'count=%s\\nbytes=%s\\n---\\n%s\\n' "
        f"    \"$count\" \"$bytes\" \"$listing\"; "
        f"else "
        f"  printf 'count=0\\nbytes=0\\n---\\n(absent)\\n'; "
        f"fi"
    )
    result = await handle.raw_exec(handle.sandbox_id, cmd, timeout=30)
    assert result.exit_code == 0, (
        f"probe failed: stderr={result.stderr!r} stdout={result.stdout[:400]!r}"
    )
    count = 0
    total_bytes = 0
    listing = ""
    saw_separator = False
    for line in result.stdout.splitlines():
        if line == "---":
            saw_separator = True
            continue
        if saw_separator:
            listing = listing + ("\n" if listing else "") + line
            continue
        if line.startswith("count="):
            count = int(line[len("count=") :].strip() or "0")
        elif line.startswith("bytes="):
            total_bytes = int(line[len("bytes=") :].strip() or "0")
    return count, total_bytes, listing


async def test_phase08_dev_shm_stays_bounded(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    # Seed the layer-stack workspace binding so the daemon's first
    # ``tool.shell`` doesn't fault on a missing workspace.json.
    await seed_phase05_imported_base(handle)
    artifact = _artifact_path()
    # Truncate any prior partial artifact so each invocation starts fresh.
    # Phase08 measures /dev/shm bounds across one continuous loop; resume
    # is meaningless because the loop's invariant is sequential.
    if artifact.exists():
        artifact.unlink()
    rows: list[dict[str, object]] = []

    # Pre-flight probe — record the starting state so the assertion has
    # context if it triggers right at probe #1.
    initial_count, initial_bytes, initial_listing = await _probe_dev_shm(handle)
    initial_row: dict[str, object] = {
        "schema": "phase08.dev_shm_bounded.v1",
        "call_index": 0,
        "run_dir_count": initial_count,
        "total_bytes": initial_bytes,
        "limit_run_dir": _RUN_DIR_LIMIT,
        "limit_bytes": _TOTAL_BYTES_LIMIT,
        "listing_excerpt": initial_listing[:1024],
    }
    _stream_row(artifact, initial_row)
    rows.append(initial_row)

    failures: list[str] = []

    for call_index in range(1, _TOTAL_CALLS + 1):
        result = await handle.tool.shell(
            "true", timeout=10, description=f"phase08 noop {call_index}"
        )
        assert result.success, (
            f"unexpected shell failure at call {call_index}: "
            f"exit={result.exit_code} stderr={result.stderr!r}"
        )

        if call_index % _PROBE_EVERY != 0 and call_index != _TOTAL_CALLS:
            continue

        count, total_bytes, listing = await _probe_dev_shm(handle)
        row: dict[str, object] = {
            "schema": "phase08.dev_shm_bounded.v1",
            "call_index": call_index,
            "run_dir_count": count,
            "total_bytes": total_bytes,
            "limit_run_dir": _RUN_DIR_LIMIT,
            "limit_bytes": _TOTAL_BYTES_LIMIT,
            "listing_excerpt": listing[:1024],
        }
        _stream_row(artifact, row)
        rows.append(row)

        if count > _RUN_DIR_LIMIT:
            failures.append(
                f"call={call_index}: run_dir_count={count} > {_RUN_DIR_LIMIT}"
            )
        if total_bytes > _TOTAL_BYTES_LIMIT:
            failures.append(
                f"call={call_index}: total_bytes={total_bytes} > {_TOTAL_BYTES_LIMIT}"
            )

    print(f"\n[phase08:dev_shm] artifact={artifact}")

    assert not failures, (
        "Phase 3 improvement #1 regression — /dev/shm exceeded bounds:\n"
        + "\n".join(failures)
        + f"\nfull artifact={artifact}"
    )
